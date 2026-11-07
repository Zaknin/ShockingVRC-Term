use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::random;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

use shocking_vrc_rendezvous::pairing::{DeviceGrantV1, device_auth_transcript, device_id_for_public_key, encode};

type HmacSha256 = Hmac<Sha256>;
const ACCESS_TTL: Duration = Duration::from_secs(900);
const HEARTBEAT_LOSS: Duration = Duration::from_secs(15);
const RESERVATION_TTL: Duration = Duration::from_secs(30);
const RESUME_REPLAY_TTL: Duration = Duration::from_secs(15);
const MAX_RESUME_REPLAYS: usize = 8;
const INVITATION_TTL: Duration = Duration::from_secs(15 * 60);
const DEVICE_CHALLENGE_TTL: Duration = Duration::from_secs(60);
pub const MAX_ACTIVE_INVITATIONS: usize = 20;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("authentication failed")]
    Failed,
    #[error("remote session is busy")]
    Busy,
    #[error("remote session is unavailable")]
    Unavailable,
    #[error("remote session has expired")]
    Expired,
    #[error("resume ticket is invalid")]
    InvalidResume,
    #[error("Terminal device is not paired with this Host")]
    DeviceNotPaired,
    #[error("pairing invitation is invalid, expired, or already used")]
    InvalidInvitation,
    #[error("the Host supports up to 20 active invitations; revoke or wait for one to expire")]
    InvitationLimitReached,
    #[error("device authentication challenge is invalid or expired")]
    InvalidChallenge,
    #[error("trusted Terminal storage failed: {0}")]
    TrustStorage(std::io::Error),
    #[error("session state is unavailable")]
    State,
}

#[derive(Clone, Serialize)]
pub struct LoginResponse {
    pub session_id: Uuid,
    pub resume_ticket: String,
    pub access_token: String,
    pub access_expires_in: u16,
    pub transport_generation: u64,
    pub server_epoch: Uuid,
}

#[derive(Clone, Serialize)]
pub struct RefreshResponse {
    pub access_token: String,
    pub access_expires_in: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Free,
    Active,
    Reserved,
}

impl SessionPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Free => "FREE",
            Self::Active => "ACTIVE",
            Self::Reserved => "RESERVED",
        }
    }

    pub fn status_description(self) -> &'static str {
        match self {
            Self::Free => "No authenticated Terminal API session is active.",
            Self::Active => "Authenticated API session; the Host has received recent Terminal API traffic.",
            Self::Reserved => {
                "No recent Terminal API traffic; the session is temporarily available for same-session resume."
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionStatus {
    pub phase: SessionPhase,
    pub transport_generation: Option<u64>,
    pub last_contact: Option<Instant>,
}

struct Session {
    id: Uuid,
    device_id: Option<String>,
    phase: SessionPhase,
    access_hash: [u8; 32],
    previous_access_hash: Option<([u8; 32], Instant)>,
    resume_hash: [u8; 32],
    generation: u64,
    access_expires: Instant,
    last_contact: Instant,
    reserved_at: Option<Instant>,
    resume_replays: VecDeque<ResumeReplay>,
}

struct ResumeReplay {
    request_id: Uuid,
    consumed_ticket_hash: [u8; 32],
    response: LoginResponse,
    at: Instant,
}

struct SessionInner {
    session: Option<Session>,
}

/// Process-local, single-slot session manager shared by REST and WSS.
pub struct SessionManager {
    epoch: Uuid,
    digest_key: Zeroizing<[u8; 32]>,
    inner: Mutex<SessionInner>,
    pairing: Mutex<PairingState>,
    trust_path: PathBuf,
}

struct PairingState {
    invitations: HashMap<Uuid, StoredInvitation>,
    challenges: HashMap<Uuid, StoredChallenge>,
    trusted: BTreeMap<String, TrustedTerminal>,
    history: BTreeMap<Uuid, InvitationHistoryRecord>,
}

struct StoredInvitation {
    secret_hash: [u8; 32],
    expires_at: u64,
}

struct StoredChallenge {
    host_id: String,
    request_id: Uuid,
    device_id: String,
    public_key: [u8; 32],
    invitation_id: Option<Uuid>,
    nonce: [u8; 32],
    expires_at: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedTerminal {
    pub device_id: String,
    pub public_key: String,
    pub paired_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friendly_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitation_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum InvitationStatus {
    Active,
    Accepted,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InvitationHistoryRecord {
    pub id: Uuid,
    pub created_at: u64,
    pub expires_at: u64,
    pub status: InvitationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_by_device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustedTerminalFile {
    version: u8,
    terminals: BTreeMap<String, TrustedTerminal>,
    #[serde(default)]
    invitations: BTreeMap<Uuid, InvitationHistoryRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceAuthChallenge {
    pub request_id: Uuid,
    pub challenge_id: Uuid,
    pub device_id: String,
    pub server_nonce: [u8; 32],
}

impl SessionManager {
    pub fn load(server_epoch: Uuid) -> Result<Self, AuthError> {
        Self::load_from_path(server_epoch, trust_path()?)
    }

    pub fn load_from_path(server_epoch: Uuid, trust_path: PathBuf) -> Result<Self, AuthError> {
        let (trusted, mut history, version) = load_trusted_terminals(&trust_path)?;
        let now = unix_time().ok_or(AuthError::State)?;
        let mut history_changed = false;
        for record in history.values_mut().filter(|record| record.status == InvitationStatus::Active) {
            record.status = if record.expires_at <= now {
                record.expired_at = Some(now);
                InvitationStatus::Expired
            } else {
                record.revoked_at = Some(now);
                InvitationStatus::Revoked
            };
            history_changed = true;
        }
        if version != 2 || history_changed {
            persist_trusted_terminals(&trust_path, &trusted, &history)?;
        }
        Ok(Self {
            epoch: server_epoch,
            digest_key: Zeroizing::new(random()),
            inner: Mutex::new(SessionInner { session: None }),
            pairing: Mutex::new(PairingState {
                invitations: HashMap::new(),
                challenges: HashMap::new(),
                trusted,
                history,
            }),
            trust_path,
        })
    }

    pub fn create_invitation(&self, invitation_id: Uuid, secret_hash: [u8; 32]) -> Result<u64, AuthError> {
        let created_at = unix_time().ok_or(AuthError::State)?;
        let expires_at = created_at.saturating_add(INVITATION_TTL.as_secs());
        self.activate_invitation(invitation_id, secret_hash, created_at, expires_at, None)?;
        Ok(expires_at)
    }

    pub fn activate_invitation(
        &self,
        invitation_id: Uuid,
        secret_hash: [u8; 32],
        created_at: u64,
        expires_at: u64,
        description: Option<String>,
    ) -> Result<(), AuthError> {
        if expires_at <= created_at || expires_at - created_at != INVITATION_TTL.as_secs() {
            return Err(AuthError::InvalidInvitation);
        }
        let now = unix_time().ok_or(AuthError::State)?;
        if created_at > now || expires_at <= now {
            return Err(AuthError::InvalidInvitation);
        }
        let description = description
            .map(|value| value.trim().chars().take(80).collect::<String>())
            .filter(|value| !value.is_empty());
        let mut pairing = self.pairing.lock().map_err(|_| AuthError::State)?;
        self.expire_invitations_locked(&mut pairing, now)?;
        if pairing.history.contains_key(&invitation_id) {
            return Err(AuthError::InvalidInvitation);
        }
        if pairing.invitations.len() >= MAX_ACTIVE_INVITATIONS {
            return Err(AuthError::InvitationLimitReached);
        }
        let record = InvitationHistoryRecord {
            id: invitation_id,
            created_at,
            expires_at,
            status: InvitationStatus::Active,
            accepted_at: None,
            expired_at: None,
            revoked_at: None,
            consumed_by_device_id: None,
            description,
        };
        let mut next_history = pairing.history.clone();
        next_history.insert(invitation_id, record);
        persist_trusted_terminals(&self.trust_path, &pairing.trusted, &next_history)?;
        pairing.history = next_history;
        pairing.invitations.insert(invitation_id, StoredInvitation { secret_hash, expires_at });
        Ok(())
    }

    pub fn cancel_invitation(&self, invitation_id: Uuid) -> Result<(), AuthError> {
        let mut pairing = self.pairing.lock().map_err(|_| AuthError::State)?;
        let removed = pairing.invitations.remove(&invitation_id).is_some();
        pairing
            .challenges
            .retain(|_, challenge| challenge.invitation_id != Some(invitation_id));
        if removed {
            let now = unix_time().ok_or(AuthError::State)?;
            if let Some(record) = pairing.history.get_mut(&invitation_id) {
                record.status = InvitationStatus::Revoked;
                record.revoked_at = Some(now);
            }
            persist_trusted_terminals(&self.trust_path, &pairing.trusted, &pairing.history)?;
        }
        Ok(())
    }

    pub fn cancel_all_invitations(&self) -> Result<(), AuthError> {
        let mut pairing = self.pairing.lock().map_err(|_| AuthError::State)?;
        let now = unix_time().ok_or(AuthError::State)?;
        for invitation_id in pairing.invitations.keys().copied().collect::<Vec<_>>() {
            if let Some(record) = pairing.history.get_mut(&invitation_id) {
                record.status = InvitationStatus::Revoked;
                record.revoked_at = Some(now);
            }
        }
        pairing.invitations.clear();
        pairing.challenges.clear();
        persist_trusted_terminals(&self.trust_path, &pairing.trusted, &pairing.history)?;
        Ok(())
    }

    pub fn active_invitations(&self) -> Vec<InvitationHistoryRecord> {
        self.invitation_history().into_iter().filter(|record| record.status == InvitationStatus::Active).collect()
    }

    pub fn invitation_history(&self) -> Vec<InvitationHistoryRecord> {
        let Ok(mut pairing) = self.pairing.lock() else { return Vec::new(); };
        let now = unix_time().unwrap_or_default();
        if self.expire_invitations_locked(&mut pairing, now).is_err() {
            log::warn!("[remote] could not persist expired invitation history");
        }
        let mut records = pairing.history.values().cloned().collect::<Vec<_>>();
        records.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
        records
    }

    pub fn set_terminal_name(&self, device_id: &str, friendly_name: Option<String>) -> Result<(), AuthError> {
        let friendly_name = friendly_name
            .map(|value| value.trim().chars().take(80).collect::<String>())
            .filter(|value| !value.is_empty());
        let mut pairing = self.pairing.lock().map_err(|_| AuthError::State)?;
        let mut next_trusted = pairing.trusted.clone();
        let terminal = next_trusted.get_mut(device_id).ok_or(AuthError::DeviceNotPaired)?;
        terminal.friendly_name = friendly_name;
        persist_trusted_terminals(&self.trust_path, &next_trusted, &pairing.history)?;
        pairing.trusted = next_trusted;
        Ok(())
    }

    fn expire_invitations_locked(&self, pairing: &mut PairingState, now: u64) -> Result<(), AuthError> {
        let expired = pairing.invitations.iter()
            .filter_map(|(id, invitation)| (invitation.expires_at <= now).then_some(*id))
            .collect::<Vec<_>>();
        if expired.is_empty() { return Ok(()); }
        for id in &expired {
            self.expire_invitation_locked(pairing, *id, now)?;
        }
        Ok(())
    }

    fn expire_invitation_locked(&self, pairing: &mut PairingState, invitation_id: Uuid, now: u64) -> Result<(), AuthError> {
        pairing.invitations.remove(&invitation_id);
        pairing.challenges.retain(|_, challenge| challenge.invitation_id != Some(invitation_id));
        if let Some(record) = pairing.history.get_mut(&invitation_id) {
            record.status = InvitationStatus::Expired;
            record.expired_at = Some(now);
        }
        persist_trusted_terminals(&self.trust_path, &pairing.trusted, &pairing.history)
    }

    pub fn trusted_terminals(&self) -> Vec<TrustedTerminal> {
        self.pairing
            .lock()
            .map(|state| state.trusted.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn device_grant(
        &self,
        device_id: &str,
        host_id: &str,
        sign: impl FnOnce(&[u8]) -> Result<Vec<u8>, AuthError>,
    ) -> Result<String, AuthError> {
        let pairing = self.pairing.lock().map_err(|_| AuthError::State)?;
        let terminal = pairing.trusted.get(device_id).ok_or(AuthError::DeviceNotPaired)?;
        let grant = DeviceGrantV1 {
            version: 1,
            host_id: host_id.to_owned(),
            device_id: device_id.to_owned(),
            terminal_public_key: terminal.public_key.clone(),
            signature: String::new(),
        };
        let signature = sign(&grant.signing_bytes())?;
        let grant = DeviceGrantV1 {
            signature: encode(&signature),
            ..grant
        };
        grant.to_authorization().map_err(|_| AuthError::State)
    }

    pub fn revoke_terminal(&self, device_id: &str) -> Result<(), AuthError> {
        let mut pairing = self.pairing.lock().map_err(|_| AuthError::State)?;
        if pairing.trusted.remove(device_id).is_none() {
            return Err(AuthError::DeviceNotPaired);
        }
        if let Err(error) = persist_trusted_terminals(&self.trust_path, &pairing.trusted, &pairing.history) {
            // Fail closed in memory too; after a storage failure the process must not
            // continue accepting a key whose revocation could not be made durable.
            self.kick_device(device_id);
            return Err(error);
        }
        pairing
            .challenges
            .retain(|_, challenge| challenge.device_id != device_id);
        drop(pairing);
        self.kick_device(device_id);
        Ok(())
    }

    fn kick_device(&self, device_id: &str) {
        if let Ok(mut inner) = self.inner.lock()
            && inner
                .session
                .as_ref()
                .is_some_and(|session| session.device_id.as_deref() == Some(device_id))
        {
            inner.session = None;
        }
    }

    pub fn issue_device_challenge(
        &self,
        host_id: &str,
        request_id: Uuid,
        device_id: String,
        public_key: [u8; 32],
        invitation_id: Option<Uuid>,
    ) -> Result<DeviceAuthChallenge, AuthError> {
        if device_id_for_public_key(&public_key).map_err(|_| AuthError::DeviceNotPaired)? != device_id {
            return Err(AuthError::DeviceNotPaired);
        }
        let now_utc = unix_time().ok_or(AuthError::State)?;
        let mut pairing = self.pairing.lock().map_err(|_| AuthError::State)?;
        pairing
            .challenges
            .retain(|_, challenge| challenge.expires_at > Instant::now());
        if let Some(invitation_id) = invitation_id {
            let invitation = pairing
                .invitations
                .get(&invitation_id)
                .ok_or(AuthError::InvalidInvitation)?;
            if invitation.expires_at <= now_utc {
                self.expire_invitation_locked(&mut pairing, invitation_id, now_utc)?;
                return Err(AuthError::InvalidInvitation);
            }
        } else {
            let trusted = pairing.trusted.get(&device_id).ok_or(AuthError::DeviceNotPaired)?;
            if decode_public_key(&trusted.public_key).as_ref() != Some(&public_key) {
                return Err(AuthError::DeviceNotPaired);
            }
        }
        if pairing.challenges.len() >= 256 {
            return Err(AuthError::Unavailable);
        }
        let challenge_id = Uuid::new_v4();
        let nonce = random();
        pairing.challenges.insert(
            challenge_id,
            StoredChallenge {
                host_id: host_id.to_owned(),
                request_id,
                device_id: device_id.clone(),
                public_key,
                invitation_id,
                nonce,
                expires_at: Instant::now() + DEVICE_CHALLENGE_TTL,
            },
        );
        Ok(DeviceAuthChallenge {
            request_id,
            challenge_id,
            device_id,
            server_nonce: nonce,
        })
    }

    pub fn authenticate_device(
        &self,
        host_id: &str,
        challenge_id: Uuid,
        request_id: Uuid,
        device_id: &str,
        public_key: [u8; 32],
        signature: &[u8],
        invitation_id: Option<Uuid>,
        pairing_secret: Option<&str>,
    ) -> Result<LoginResponse, AuthError> {
        let now_utc = unix_time().ok_or(AuthError::State)?;
        let mut pairing = self.pairing.lock().map_err(|_| AuthError::State)?;
        let challenge = pairing
            .challenges
            .remove(&challenge_id)
            .ok_or(AuthError::InvalidChallenge)?;
        if challenge.expires_at <= Instant::now()
            || challenge.host_id != host_id
            || challenge.request_id != request_id
            || challenge.device_id != device_id
            || challenge.public_key != public_key
            || challenge.invitation_id != invitation_id
        {
            return Err(AuthError::InvalidChallenge);
        }
        let secret_hash = match (invitation_id, pairing_secret) {
            (Some(_), Some(secret)) => {
                let secret = Zeroizing::new(
                    URL_SAFE_NO_PAD.decode(secret).map_err(|_| AuthError::InvalidInvitation)?,
                );
                if secret.len() != 32 || secret.iter().all(|byte| *byte == 0) {
                    return Err(AuthError::InvalidInvitation);
                }
                Some(Sha256::digest(secret.as_slice()).into())
            }
            (None, None) => None,
            _ => return Err(AuthError::InvalidInvitation),
        };
        let transcript = device_auth_transcript(
            host_id,
            self.epoch,
            request_id,
            challenge_id,
            challenge.nonce,
            device_id,
            public_key,
            invitation_id,
            secret_hash,
        );
        UnparsedPublicKey::new(&ED25519, &public_key)
            .verify(&transcript, signature)
            .map_err(|_| AuthError::Failed)?;
        if let Some(invitation_id) = invitation_id {
            let invitation = pairing
                .invitations
                .get(&invitation_id)
                .ok_or(AuthError::InvalidInvitation)?;
            if invitation.expires_at <= now_utc {
                self.expire_invitation_locked(&mut pairing, invitation_id, now_utc)?;
                return Err(AuthError::InvalidInvitation);
            }
            if !secret_hash.is_some_and(|provided| bool::from(provided.ct_eq(&invitation.secret_hash))) {
                return Err(AuthError::InvalidInvitation);
            }
        } else {
            let trusted = pairing.trusted.get(device_id).ok_or(AuthError::DeviceNotPaired)?;
            if decode_public_key(&trusted.public_key).as_ref() != Some(&public_key) {
                return Err(AuthError::DeviceNotPaired);
            }
        }

        // Keep the pairing lock through session admission and persistence. This is
        // the one-use boundary for both invitation consumption and trust creation.
        let now = Instant::now();
        let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
        self.expire_locked(&mut inner, now);
        // A proven device may replace only its own reserved session. This
        // recovers the one-use invitation case where the Host consumed the
        // proof but the Terminal lost the login response before saving a ticket.
        match inner.session.as_ref() {
            Some(session)
                if session.phase == SessionPhase::Reserved
                    && session.device_id.as_deref() == Some(device_id) => {}
            Some(_) => return Err(AuthError::Busy),
            None => {}
        }
        let first_pair = invitation_id.is_some();
        if first_pair {
            let previous_name = pairing.trusted.get(device_id).and_then(|terminal| terminal.friendly_name.clone());
            let invitation_id = invitation_id.expect("first pair has invitation");
            let mut next_history = pairing.history.clone();
            let record = next_history.get_mut(&invitation_id).ok_or(AuthError::InvalidInvitation)?;
            if record.status != InvitationStatus::Active { return Err(AuthError::InvalidInvitation); }
            let invitation_name = record.description.clone();
            record.status = InvitationStatus::Accepted;
            record.accepted_at = Some(now_utc);
            record.consumed_by_device_id = Some(device_id.to_owned());
            let terminal = TrustedTerminal {
                device_id: device_id.to_owned(),
                public_key: encode(&public_key),
                paired_at: now_utc,
                friendly_name: previous_name.or(invitation_name).or_else(|| Some("Terminal".to_owned())),
                invitation_id: Some(invitation_id),
            };
            let mut next_trusted = pairing.trusted.clone();
            next_trusted.insert(device_id.to_owned(), terminal);
            if let Err(error) = persist_trusted_terminals(&self.trust_path, &next_trusted, &next_history) {
                return Err(error);
            }
            pairing.trusted = next_trusted;
            pairing.history = next_history;
            pairing.invitations.remove(&invitation_id);
            pairing
                .challenges
                .retain(|_, pending| pending.invitation_id != Some(invitation_id));
        }
        let response = self.new_login_response(Uuid::new_v4(), 1);
        let access_hash = self.digest(response.access_token.as_bytes());
        let resume_hash = self.digest(response.resume_ticket.as_bytes());
        inner.session = Some(Session {
            id: response.session_id,
            device_id: Some(device_id.to_owned()),
            phase: SessionPhase::Active,
            access_hash,
            previous_access_hash: None,
            resume_hash,
            generation: 1,
            access_expires: now + ACCESS_TTL,
            last_contact: now,
            reserved_at: None,
            resume_replays: VecDeque::new(),
        });
        Ok(response)
    }

    pub fn resume(&self, session_id: Uuid, ticket: &str, request_id: Uuid) -> Result<LoginResponse, AuthError> {
        let now = Instant::now();
        let ticket_hash = self.digest(ticket.as_bytes());
        let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
        self.expire_locked(&mut inner, now);
        let session = inner.session.as_mut().ok_or(AuthError::InvalidResume)?;
        if session.id != session_id {
            return Err(AuthError::InvalidResume);
        }
        while session
            .resume_replays
            .front()
            .is_some_and(|entry| now.duration_since(entry.at) > RESUME_REPLAY_TTL)
        {
            session.resume_replays.pop_front();
        }
        if let Some(replay) = session
            .resume_replays
            .iter()
            .find(|entry| entry.request_id == request_id && bool::from(entry.consumed_ticket_hash.ct_eq(&ticket_hash)))
        {
            return Ok(replay.response.clone());
        }
        if !bool::from(session.resume_hash.ct_eq(&ticket_hash))
            || !matches!(session.phase, SessionPhase::Active | SessionPhase::Reserved)
        {
            return Err(AuthError::InvalidResume);
        }

        let previous_phase = session.phase;
        let next_generation = session.generation.saturating_add(1);
        let response = self.new_login_response(session.id, next_generation);
        session.resume_replays.push_back(ResumeReplay {
            request_id,
            consumed_ticket_hash: ticket_hash,
            response: response.clone(),
            at: now,
        });
        while session.resume_replays.len() > MAX_RESUME_REPLAYS {
            session.resume_replays.pop_front();
        }
        session.access_hash = self.digest(response.access_token.as_bytes());
        session.previous_access_hash = None;
        session.resume_hash = self.digest(response.resume_ticket.as_bytes());
        session.generation = next_generation;
        session.phase = SessionPhase::Active;
        session.last_contact = now;
        session.reserved_at = None;
        session.access_expires = now + ACCESS_TTL;
        log::info!(
            "[remote] API session phase={} -> ACTIVE reason=same_session_resume generation={next_generation}",
            previous_phase.as_str()
        );
        Ok(response)
    }

    pub fn logout_generation(&self, session_id: Uuid, generation: u64) -> Result<(), AuthError> {
        let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
        self.expire_locked(&mut inner, Instant::now());
        let session = inner.session.as_ref().ok_or(AuthError::Unavailable)?;
        if session.id != session_id || session.generation != generation || session.phase != SessionPhase::Active {
            return Err(AuthError::Expired);
        }
        log::info!("[remote] API session phase=ACTIVE -> FREE reason=terminal_logout generation={generation}");
        inner.session = None;
        Ok(())
    }

    pub fn refresh_generation(&self, session_id: Uuid, generation: u64) -> Result<RefreshResponse, AuthError> {
        let now = Instant::now();
        let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
        self.expire_locked(&mut inner, now);
        let session = inner.session.as_mut().ok_or(AuthError::Unavailable)?;
        if session.id != session_id || session.generation != generation || session.phase != SessionPhase::Active {
            return Err(AuthError::Expired);
        }
        let new_token = random_token();
        session.previous_access_hash = Some((session.access_hash, now + Duration::from_secs(10)));
        session.access_hash = self.digest(new_token.as_bytes());
        session.access_expires = now + ACCESS_TTL;
        session.last_contact = now;
        Ok(RefreshResponse {
            access_token: new_token,
            access_expires_in: ACCESS_TTL.as_secs() as u16,
        })
    }

    pub fn heartbeat(&self, token: &str) -> Result<(), AuthError> {
        self.with_bearer(token, |session| {
            if session.phase == SessionPhase::Reserved {
                return Err(AuthError::Expired);
            }
            session.last_contact = Instant::now();
            session.reserved_at = None;
            Ok(())
        })
    }

    pub fn refresh(&self, token: &str) -> Result<RefreshResponse, AuthError> {
        let supplied = self.digest(token.as_bytes());
        self.with_bearer(token, |session| {
            if session.phase != SessionPhase::Active {
                return Err(AuthError::Expired);
            }
            if !bool::from(session.access_hash.ct_eq(&supplied)) {
                return Err(AuthError::Failed);
            }
            let now = Instant::now();
            let new_token = random_token();
            session.previous_access_hash = Some((session.access_hash, now + Duration::from_secs(10)));
            session.access_hash = self.digest(new_token.as_bytes());
            session.access_expires = now + ACCESS_TTL;
            session.last_contact = now;
            Ok(RefreshResponse {
                access_token: new_token,
                access_expires_in: ACCESS_TTL.as_secs() as u16,
            })
        })
    }

    pub fn logout(&self, token: &str) -> Result<(), AuthError> {
        let supplied = self.digest(token.as_bytes());
        let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
        let session = inner.session.as_ref().ok_or(AuthError::Unavailable)?;
        if !self.token_matches(session, &supplied, Instant::now()) {
            return Err(AuthError::Failed);
        }
        log::info!(
            "[remote] API session phase={} -> FREE reason=terminal_logout generation={}",
            session.phase.as_str(),
            session.generation
        );
        inner.session = None;
        Ok(())
    }

    pub fn kick(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(session) = inner.session.as_ref() {
                log::info!(
                    "[remote] API session phase={} -> FREE reason=host_reset generation={}",
                    session.phase.as_str(),
                    session.generation
                );
            }
            inner.session = None;
        }
    }

    pub fn status(&self) -> SessionStatus {
        let now = Instant::now();
        if let Ok(mut inner) = self.inner.lock() {
            self.expire_locked(&mut inner, now);
            if let Some(session) = &inner.session {
                return SessionStatus {
                    phase: session.phase,
                    transport_generation: Some(session.generation),
                    last_contact: Some(session.last_contact),
                };
            }
        }
        SessionStatus {
            phase: SessionPhase::Free,
            transport_generation: None,
            last_contact: None,
        }
    }

    fn with_bearer<T>(
        &self,
        token: &str,
        operation: impl FnOnce(&mut Session) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        let now = Instant::now();
        let supplied = self.digest(token.as_bytes());
        let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
        self.expire_locked(&mut inner, now);
        let session = inner.session.as_mut().ok_or(AuthError::Unavailable)?;
        if !self.token_matches(session, &supplied, now) {
            return Err(AuthError::Failed);
        }
        operation(session)
    }

    pub fn with_generation<T>(
        &self,
        session_id: Uuid,
        generation: u64,
        operation: impl FnOnce() -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        let now = Instant::now();
        let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
        self.expire_locked(&mut inner, now);
        let session = inner.session.as_mut().ok_or(AuthError::Unavailable)?;
        if session.id != session_id || session.generation != generation || session.phase != SessionPhase::Active {
            return Err(AuthError::Expired);
        }
        operation()
    }

    pub fn admit_bearer<T>(
        &self,
        token: &str,
        operation: impl FnOnce() -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        self.admit_bearer_with_session(token, |_| operation())
    }

    pub fn admit_bearer_with_session<T>(
        &self,
        token: &str,
        operation: impl FnOnce(Uuid) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        self.with_bearer(token, |session| {
            if session.phase != SessionPhase::Active {
                return Err(AuthError::Expired);
            }
            session.last_contact = Instant::now();
            operation(session.id)
        })
    }

    /// Mark only the matching active WSS transport as lost. A stale socket can
    /// never reserve or otherwise mutate a newer generation.
    pub fn mark_generation_lost(&self, session_id: Uuid, generation: u64) {
        let now = Instant::now();
        if let Ok(mut inner) = self.inner.lock() {
            self.expire_locked(&mut inner, now);
            if let Some(session) = inner.session.as_mut()
                && session.id == session_id
                && session.generation == generation
                && session.phase == SessionPhase::Active
            {
                log::info!(
                    "[remote] API session phase=ACTIVE -> RESERVED reason=transport_closed generation={generation} last_contact_age_seconds={}",
                    now.duration_since(session.last_contact).as_secs()
                );
                session.phase = SessionPhase::Reserved;
                session.reserved_at = Some(now);
            }
        }
    }

    pub fn touch_generation(&self, session_id: Uuid, generation: u64) -> Result<(), AuthError> {
        let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
        self.expire_locked(&mut inner, Instant::now());
        if let Some(session) = inner.session.as_mut()
            && session.id == session_id
            && session.generation == generation
            && session.phase == SessionPhase::Active
        {
            session.last_contact = Instant::now();
            return Ok(());
        }
        Err(AuthError::Expired)
    }

    pub fn active_generation(&self) -> Option<(Uuid, u64)> {
        let mut inner = self.inner.lock().ok()?;
        self.expire_locked(&mut inner, Instant::now());
        inner
            .session
            .as_ref()
            .filter(|session| session.phase == SessionPhase::Active)
            .map(|session| (session.id, session.generation))
    }

    pub fn maintain(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            self.expire_locked(&mut inner, Instant::now());
        }
    }

    fn expire_locked(&self, inner: &mut SessionInner, now: Instant) {
        let Some(session) = inner.session.as_mut() else {
            return;
        };
        if session.phase == SessionPhase::Active && now.duration_since(session.last_contact) >= HEARTBEAT_LOSS {
            log::warn!(
                "[remote] API session phase=ACTIVE -> RESERVED reason=heartbeat_timeout last_contact_age_seconds={}",
                now.duration_since(session.last_contact).as_secs()
            );
            session.phase = SessionPhase::Reserved;
            session.reserved_at = Some(now);
        }
        if session.phase == SessionPhase::Reserved
            && session
                .reserved_at
                .is_some_and(|started| now.duration_since(started) >= RESERVATION_TTL)
        {
            log::info!("[remote] API session phase=RESERVED -> FREE reason=resume_window_expired");
            inner.session = None;
            return;
        }
    }

    fn token_matches(&self, session: &Session, supplied: &[u8; 32], now: Instant) -> bool {
        (now <= session.access_expires && bool::from(session.access_hash.ct_eq(supplied)))
            || session
                .previous_access_hash
                .as_ref()
                .is_some_and(|(previous, expires)| now <= *expires && bool::from(previous.ct_eq(supplied)))
    }

    fn digest(&self, value: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(self.digest_key.as_ref()).expect("HMAC accepts any key length");
        mac.update(value);
        mac.finalize().into_bytes().into()
    }

    fn new_login_response(&self, session_id: Uuid, generation: u64) -> LoginResponse {
        LoginResponse {
            session_id,
            resume_ticket: random_token(),
            access_token: random_token(),
            access_expires_in: ACCESS_TTL.as_secs() as u16,
            transport_generation: generation,
            server_epoch: self.epoch,
        }
    }
}

fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(random::<[u8; 32]>())
}

fn unix_time() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn trust_path() -> Result<PathBuf, AuthError> {
    #[cfg(windows)]
    {
        let app_data = std::env::var_os("APPDATA").ok_or(AuthError::State)?;
        Ok(PathBuf::from(app_data)
            .join("ShockingVRC")
            .join("trusted_terminals.json"))
    }
    #[cfg(not(windows))]
    {
        Ok(PathBuf::from("trusted_terminals.json"))
    }
}

fn load_trusted_terminals(
    path: &Path,
) -> Result<(BTreeMap<String, TrustedTerminal>, BTreeMap<Uuid, InvitationHistoryRecord>, u8), AuthError> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((BTreeMap::new(), BTreeMap::new(), 2));
        }
        Err(error) => return Err(AuthError::TrustStorage(error)),
    };
    let stored: TrustedTerminalFile = serde_json::from_slice(&raw).map_err(|_| AuthError::State)?;
    if !matches!(stored.version, 1 | 2)
        || stored.terminals.iter().any(|(id, terminal)| {
            id != &terminal.device_id
                || decode_public_key(&terminal.public_key)
                    .and_then(|key| device_id_for_public_key(&key).ok())
                    .as_deref()
                    != Some(id.as_str())
        })
    {
        return Err(AuthError::State);
    }
    if stored.invitations.iter().any(|(id, record)| id != &record.id) {
        return Err(AuthError::State);
    }
    Ok((stored.terminals, stored.invitations, stored.version))
}

fn decode_public_key(value: &str) -> Option<[u8; 32]> {
    URL_SAFE_NO_PAD.decode(value).ok()?.try_into().ok()
}

fn persist_trusted_terminals(
    path: &Path,
    terminals: &BTreeMap<String, TrustedTerminal>,
    invitations: &BTreeMap<Uuid, InvitationHistoryRecord>,
) -> Result<(), AuthError> {
    let directory = path.parent().ok_or(AuthError::State)?;
    fs::create_dir_all(directory).map_err(AuthError::TrustStorage)?;
    let bytes = serde_json::to_vec(&TrustedTerminalFile {
        version: 2,
        terminals: terminals.clone(),
        invitations: invitations.clone(),
    })
    .map_err(|_| AuthError::State)?;
    let temporary = path.with_file_name(format!("trusted_terminals.{}.tmp", Uuid::new_v4()));
    let write_result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(AuthError::TrustStorage)?;
        file.write_all(&bytes).map_err(AuthError::TrustStorage)?;
        file.sync_all().map_err(AuthError::TrustStorage)?;
        replace_file(&temporary, path).map_err(AuthError::TrustStorage)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(test)]
mod session_status_tests {
    use super::*;

    fn manager_with_active_session(last_contact: Instant) -> (SessionManager, Uuid) {
        let epoch = Uuid::new_v4();
        let id = Uuid::new_v4();
        let session = Session {
            id,
            device_id: None,
            phase: SessionPhase::Active,
            access_hash: [0; 32],
            previous_access_hash: None,
            resume_hash: [0; 32],
            generation: 1,
            access_expires: Instant::now() + ACCESS_TTL,
            last_contact,
            reserved_at: None,
            resume_replays: VecDeque::new(),
        };
        let manager = SessionManager {
            epoch,
            digest_key: Zeroizing::new([0; 32]),
            inner: Mutex::new(SessionInner { session: Some(session) }),
            pairing: Mutex::new(PairingState {
                invitations: HashMap::new(),
                challenges: HashMap::new(),
                trusted: BTreeMap::new(),
                history: BTreeMap::new(),
            }),
            trust_path: std::env::temp_dir().join(format!("svrc-status-{}.json", Uuid::new_v4())),
        };
        (manager, id)
    }

    #[test]
    fn transport_loss_moves_active_api_session_to_resume_reservation() {
        let now = Instant::now();
        let (manager, id) = manager_with_active_session(now);

        manager.mark_generation_lost(id, 1);

        let status = manager.status();
        assert_eq!(status.phase, SessionPhase::Reserved);
        assert!(status.last_contact.is_some());
    }

    #[test]
    fn heartbeat_timeout_moves_active_api_session_to_reservation() {
        let last_contact = Instant::now() - HEARTBEAT_LOSS;
        let (manager, _) = manager_with_active_session(last_contact);

        let mut inner = manager.inner.lock().expect("session lock");
        manager.expire_locked(&mut inner, Instant::now());
        drop(inner);

        assert_eq!(manager.status().phase, SessionPhase::Reserved);
    }

    #[test]
    fn reading_status_does_not_refresh_last_api_contact() {
        let last_contact = Instant::now() - Duration::from_secs(7);
        let (manager, _) = manager_with_active_session(last_contact);

        let status = manager.status();

        assert_eq!(status.last_contact, Some(last_contact));
        assert_eq!(status.phase, SessionPhase::Active);
    }

    #[test]
    fn phase_descriptions_distinguish_api_activity_from_transport_resume() {
        assert!(SessionPhase::Active.status_description().contains("API session"));
        assert!(
            SessionPhase::Reserved
                .status_description()
                .contains("same-session resume")
        );
    }
}

#[cfg(test)]
mod device_auth_tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{Arc, Barrier},
    };

    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::{AuthError, DeviceAuthChallenge, SessionManager};
    use shocking_vrc_rendezvous::pairing::{device_auth_transcript, device_id_for_public_key};

    fn key_pair() -> Ed25519KeyPair {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("Terminal key");
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("parse Terminal key")
    }

    fn manager_path() -> PathBuf {
        std::env::temp_dir().join(format!("svrc-device-auth-{}.json", Uuid::new_v4()))
    }

    fn secret() -> ([u8; 32], String) {
        let bytes = [0x3a_u8; 32];
        (bytes, URL_SAFE_NO_PAD.encode(bytes))
    }

    fn challenge(
        manager: &SessionManager,
        host_id: &str,
        key: &Ed25519KeyPair,
        invitation_id: Option<Uuid>,
    ) -> DeviceAuthChallenge {
        let public_key: [u8; 32] = key.public_key().as_ref().try_into().expect("Ed25519 public key");
        let device_id = device_id_for_public_key(&public_key).expect("device id");
        manager
            .issue_device_challenge(host_id, Uuid::new_v4(), device_id, public_key, invitation_id)
            .expect("challenge")
    }

    fn authenticate(
        manager: &SessionManager,
        host_id: &str,
        challenge: &DeviceAuthChallenge,
        key: &Ed25519KeyPair,
        invitation_id: Option<Uuid>,
        pairing_secret: Option<&str>,
    ) -> Result<super::LoginResponse, AuthError> {
        let public_key: [u8; 32] = key.public_key().as_ref().try_into().expect("Ed25519 public key");
        let secret_hash = pairing_secret.map(|value| {
            let secret = URL_SAFE_NO_PAD.decode(value).expect("test secret encoding");
            Sha256::digest(secret).into()
        });
        let transcript = device_auth_transcript(
            host_id,
            manager.epoch,
            challenge.request_id,
            challenge.challenge_id,
            challenge.server_nonce,
            &challenge.device_id,
            public_key,
            invitation_id,
            secret_hash,
        );
        let signature = key.sign(&transcript);
        manager.authenticate_device(
            host_id,
            challenge.challenge_id,
            challenge.request_id,
            &challenge.device_id,
            public_key,
            signature.as_ref(),
            invitation_id,
            pairing_secret,
        )
    }

    fn signed_proof(
        manager: &SessionManager,
        host_id: &str,
        challenge: &DeviceAuthChallenge,
        key: &Ed25519KeyPair,
        invitation_id: Option<Uuid>,
        pairing_secret: Option<&str>,
    ) -> ([u8; 32], Vec<u8>) {
        let public_key: [u8; 32] = key.public_key().as_ref().try_into().expect("Ed25519 public key");
        let secret_hash = pairing_secret.map(|value| {
            let secret = URL_SAFE_NO_PAD.decode(value).expect("test secret encoding");
            Sha256::digest(secret).into()
        });
        let transcript = device_auth_transcript(
            host_id,
            manager.epoch,
            challenge.request_id,
            challenge.challenge_id,
            challenge.server_nonce,
            &challenge.device_id,
            public_key,
            invitation_id,
            secret_hash,
        );
        (public_key, key.sign(&transcript).as_ref().to_vec())
    }

    #[test]
    fn invitation_is_consumed_once_and_cannot_be_reused_after_revoke() {
        let path = manager_path();
        let manager = Arc::new(SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager"));
        let host_id = "test-host";
        let key = key_pair();
        let invitation_id = Uuid::new_v4();
        let (secret_bytes, secret) = secret();
        manager
            .create_invitation(invitation_id, Sha256::digest(secret_bytes).into())
            .expect("invitation");

        let first = challenge(&manager, host_id, &key, Some(invitation_id));
        let response =
            authenticate(&manager, host_id, &first, &key, Some(invitation_id), Some(&secret)).expect("first pair");
        assert_eq!(manager.trusted_terminals().len(), 1);
        manager.logout(&response.access_token).expect("end first session");
        manager.revoke_terminal(&first.device_id).expect("revoke device");

        let public_key: [u8; 32] = key.public_key().as_ref().try_into().expect("Terminal public key");
        let device_id = device_id_for_public_key(&public_key).expect("device id");
        assert!(matches!(
            manager.issue_device_challenge(host_id, Uuid::new_v4(), device_id, public_key, Some(invitation_id)),
            Err(AuthError::InvalidInvitation)
        ));
        assert!(manager.trusted_terminals().is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn trusted_terminal_can_reauthenticate_after_pairing_response_is_lost() {
        let path = manager_path();
        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager");
        let host_id = "test-host";
        let key = key_pair();
        let invitation_id = Uuid::new_v4();
        let (secret_bytes, secret) = secret();
        manager
            .create_invitation(invitation_id, Sha256::digest(secret_bytes).into())
            .expect("invitation");

        let first = challenge(&manager, host_id, &key, Some(invitation_id));
        let first_response = authenticate(&manager, host_id, &first, &key, Some(invitation_id), Some(&secret))
            .expect("first pair");
        let (session_id, generation) = manager.active_generation().expect("active first session");
        assert_eq!(session_id, first_response.session_id);

        let premature_retry = challenge(&manager, host_id, &key, None);
        assert!(matches!(
            authenticate(&manager, host_id, &premature_retry, &key, None, None),
            Err(AuthError::Busy)
        ));

        // The proof reached the Host, but the Terminal did not receive its
        // authentication result, so the transport loss is all it can observe.
        manager.mark_generation_lost(session_id, generation);

        let retry = challenge(&manager, host_id, &key, None);
        let retry_response = authenticate(&manager, host_id, &retry, &key, None, None)
            .expect("trusted-device retry after invitation consumption");
        assert_ne!(retry_response.session_id, first_response.session_id);
        assert_eq!(manager.trusted_terminals().len(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn trusted_terminal_survives_manager_restart_but_resume_ticket_does_not() {
        let path = manager_path();
        let first_epoch = Uuid::new_v4();
        let host_id = "test-host";
        let key = key_pair();
        let manager = SessionManager::load_from_path(first_epoch, path.clone()).expect("first manager");
        let invitation_id = Uuid::new_v4();
        let (secret_bytes, secret) = secret();
        manager
            .create_invitation(invitation_id, Sha256::digest(secret_bytes).into())
            .expect("invitation");
        let first = challenge(&manager, host_id, &key, Some(invitation_id));
        let response =
            authenticate(&manager, host_id, &first, &key, Some(invitation_id), Some(&secret)).expect("first pair");
        let session_id = response.session_id;
        let resume_ticket = response.resume_ticket.clone();
        drop(manager);

        let next_epoch = Uuid::new_v4();
        let restarted = SessionManager::load_from_path(next_epoch, path.clone()).expect("restarted manager");
        assert!(matches!(
            restarted.resume(session_id, &resume_ticket, Uuid::new_v4()),
            Err(AuthError::InvalidResume)
        ));
        let next = challenge(&restarted, host_id, &key, None);
        let response = authenticate(&restarted, host_id, &next, &key, None, None).expect("passwordless after restart");
        assert_ne!(response.server_epoch, first_epoch);
        assert_eq!(restarted.trusted_terminals().len(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn cancelled_or_expired_invitation_cannot_pair() {
        let path = manager_path();
        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager");
        let host_id = "test-host";
        let key = key_pair();
        let invitation_id = Uuid::new_v4();
        let (secret_bytes, _secret) = secret();
        manager
            .create_invitation(invitation_id, Sha256::digest(secret_bytes).into())
            .expect("invitation");
        manager.cancel_invitation(invitation_id).expect("cancel");
        let public_key: [u8; 32] = key.public_key().as_ref().try_into().expect("Terminal public key");
        let device_id = device_id_for_public_key(&public_key).expect("device id");
        assert!(matches!(
            manager.issue_device_challenge(host_id, Uuid::new_v4(), device_id, public_key, Some(invitation_id)),
            Err(AuthError::InvalidInvitation)
        ));
        assert!(manager.trusted_terminals().is_empty());

        let expired_id = Uuid::new_v4();
        manager
            .create_invitation(expired_id, Sha256::digest(secret_bytes).into())
            .expect("expired invitation");
        manager
            .pairing
            .lock()
            .expect("pairing state")
            .invitations
            .get_mut(&expired_id)
            .expect("stored invitation")
            .expires_at = 0;
        let public_key: [u8; 32] = key.public_key().as_ref().try_into().expect("Terminal public key");
        let device_id = device_id_for_public_key(&public_key).expect("device id");
        assert!(matches!(
            manager.issue_device_challenge(host_id, Uuid::new_v4(), device_id, public_key, Some(expired_id)),
            Err(AuthError::InvalidInvitation)
        ));
        assert_eq!(
            manager.invitation_history().iter().find(|entry| entry.id == expired_id).unwrap().status,
            super::InvitationStatus::Expired,
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn concurrent_proofs_consume_one_invitation_once() {
        let path = manager_path();
        let manager = Arc::new(SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager"));
        let host_id = "test-host";
        let key = key_pair();
        let invitation_id = Uuid::new_v4();
        let (secret_bytes, secret) = secret();
        manager
            .create_invitation(invitation_id, Sha256::digest(secret_bytes).into())
            .expect("invitation");
        let first = challenge(&manager, host_id, &key, Some(invitation_id));
        let second = challenge(&manager, host_id, &key, Some(invitation_id));
        let (public_key, first_signature) =
            signed_proof(&manager, host_id, &first, &key, Some(invitation_id), Some(&secret));
        let (_, second_signature) = signed_proof(&manager, host_id, &second, &key, Some(invitation_id), Some(&secret));
        let barrier = Arc::new(Barrier::new(3));
        let first_manager = Arc::clone(&manager);
        let first_barrier = Arc::clone(&barrier);
        let first_secret = secret.clone();
        let first_thread = std::thread::spawn(move || {
            first_barrier.wait();
            first_manager.authenticate_device(
                host_id,
                first.challenge_id,
                first.request_id,
                &first.device_id,
                public_key,
                &first_signature,
                Some(invitation_id),
                Some(&first_secret),
            )
        });
        let second_manager = Arc::clone(&manager);
        let second_barrier = Arc::clone(&barrier);
        let second_secret = secret.clone();
        let second_thread = std::thread::spawn(move || {
            second_barrier.wait();
            second_manager.authenticate_device(
                host_id,
                second.challenge_id,
                second.request_id,
                &second.device_id,
                public_key,
                &second_signature,
                Some(invitation_id),
                Some(&second_secret),
            )
        });
        barrier.wait();
        let results = [
            first_thread.join().expect("first proof"),
            second_thread.join().expect("second proof"),
        ];
        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "proof results: {:?}",
            results
                .iter()
                .map(|result| result.as_ref().map(|_| "ok").map_err(|error| format!("{error:?}")))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(AuthError::InvalidInvitation | AuthError::InvalidChallenge)))
                .count(),
            1,
            "proof results: {:?}",
            results
                .iter()
                .map(|result| result.as_ref().map(|_| "ok").map_err(|error| format!("{error:?}")))
                .collect::<Vec<_>>()
        );
        assert_eq!(manager.trusted_terminals().len(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn device_challenge_is_single_use() {
        let path = manager_path();
        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager");
        let host_id = "test-host";
        let key = key_pair();
        let invitation_id = Uuid::new_v4();
        let (secret_bytes, secret) = secret();
        manager
            .create_invitation(invitation_id, Sha256::digest(secret_bytes).into())
            .expect("invitation");
        let challenge = challenge(&manager, host_id, &key, Some(invitation_id));
        let response =
            authenticate(&manager, host_id, &challenge, &key, Some(invitation_id), Some(&secret)).expect("pair");
        assert!(matches!(
            authenticate(&manager, host_id, &challenge, &key, Some(invitation_id), Some(&secret)),
            Err(AuthError::InvalidChallenge)
        ));
        manager.logout(&response.access_token).expect("logout");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn concurrent_invitations_are_independent_and_single_use() {
        let path = manager_path();
        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager");
        let host_id = "test-host";
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let first_secret = [0x31_u8; 32];
        let second_secret = [0x52_u8; 32];
        let now = super::unix_time().expect("clock");

        manager.activate_invitation(
            first_id,
            Sha256::digest(first_secret).into(),
            now,
            now + super::INVITATION_TTL.as_secs(),
            Some("Laptop".to_owned()),
        ).expect("first invite");
        manager.activate_invitation(
            second_id,
            Sha256::digest(second_secret).into(),
            now,
            now + super::INVITATION_TTL.as_secs(),
            None,
        ).expect("second invite");

        let key = key_pair();
        let first = challenge(&manager, host_id, &key, Some(first_id));
        authenticate(&manager, host_id, &first, &key, Some(first_id), Some(&URL_SAFE_NO_PAD.encode(first_secret)))
            .expect("first invitation pairs");
        assert_eq!(manager.active_invitations().iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![second_id]);
        assert_eq!(manager.invitation_history().iter().find(|entry| entry.id == first_id).unwrap().status, super::InvitationStatus::Accepted);

        manager.kick();
        let second_key = key_pair();
        let second = challenge(&manager, host_id, &second_key, Some(second_id));
        authenticate(&manager, host_id, &second, &second_key, Some(second_id), Some(&URL_SAFE_NO_PAD.encode(second_secret)))
            .expect("second invitation remains usable");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn revoking_one_invitation_preserves_other_invites_and_records_history() {
        let path = manager_path();
        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager");
        let now = super::unix_time().expect("clock");
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        for id in [first_id, second_id] {
            manager.activate_invitation(id, [id.as_bytes()[0]; 32], now, now + 900, None).expect("invite");
        }

        manager.cancel_invitation(first_id).expect("revoke one");

        assert_eq!(manager.active_invitations().iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![second_id]);
        let history = manager.invitation_history();
        assert_eq!(history.iter().find(|entry| entry.id == first_id).unwrap().status, super::InvitationStatus::Revoked);
        assert_eq!(history.iter().find(|entry| entry.id == second_id).unwrap().status, super::InvitationStatus::Active);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn invitation_history_survives_restart_without_persisting_secrets() {
        let path = manager_path();
        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager");
        let host_id = "test-host";
        let invitation_id = Uuid::new_v4();
        let key = key_pair();
        let secret_bytes = [0x76_u8; 32];
        let encoded_secret = URL_SAFE_NO_PAD.encode(secret_bytes);
        let now = super::unix_time().expect("clock");
        manager.activate_invitation(
            invitation_id,
            Sha256::digest(secret_bytes).into(),
            now,
            now + 900,
            Some("Desk Terminal".to_owned()),
        ).expect("invite");
        let proof = challenge(&manager, host_id, &key, Some(invitation_id));
        let response = authenticate(&manager, host_id, &proof, &key, Some(invitation_id), Some(&encoded_secret)).expect("pair");
        let device_id = proof.device_id;
        manager.logout(&response.access_token).expect("logout");
        drop(manager);

        let stored = fs::read_to_string(&path).expect("persistent Host state");
        assert!(!stored.contains(&encoded_secret));
        assert!(!stored.contains("pairing_secret"));
        let restarted = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("restart");
        let entry = restarted.invitation_history().into_iter().find(|entry| entry.id == invitation_id).expect("history");
        assert_eq!(entry.status, super::InvitationStatus::Accepted);
        assert_eq!(entry.consumed_by_device_id.as_deref(), Some(device_id.as_str()));
        assert_eq!(entry.description.as_deref(), Some("Desk Terminal"));
        assert_eq!(restarted.trusted_terminals()[0].invitation_id, Some(invitation_id));
        restarted.set_terminal_name(&device_id, Some("Owner's laptop".to_owned())).expect("name Terminal");
        let renamed = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("load renamed Terminal");
        assert_eq!(renamed.trusted_terminals()[0].friendly_name.as_deref(), Some("Owner's laptop"));
        let retry = challenge(&renamed, host_id, &key, None);
        let response = authenticate(&renamed, host_id, &retry, &key, None, None).expect("reauthorize renamed Terminal");
        assert_eq!(renamed.trusted_terminals()[0].friendly_name.as_deref(), Some("Owner's laptop"));
        renamed.logout(&response.access_token).expect("logout after reauthorization");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn accepted_invitation_description_names_only_its_paired_terminal() {
        let path = manager_path();
        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager");
        let host_id = "test-host";
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let first_secret = [0x31_u8; 32];
        let second_secret = [0x52_u8; 32];
        let now = super::unix_time().expect("clock");
        manager.activate_invitation(
            first_id,
            Sha256::digest(first_secret).into(),
            now,
            now + super::INVITATION_TTL.as_secs(),
            Some("Desk Terminal".to_owned()),
        ).expect("first invite");
        manager.activate_invitation(
            second_id,
            Sha256::digest(second_secret).into(),
            now,
            now + super::INVITATION_TTL.as_secs(),
            Some("Travel Laptop".to_owned()),
        ).expect("second invite");

        let key = key_pair();
        let first = challenge(&manager, host_id, &key, Some(first_id));
        authenticate(&manager, host_id, &first, &key, Some(first_id), Some(&URL_SAFE_NO_PAD.encode(first_secret)))
            .expect("accepted invitation pairs");

        let terminal = manager.trusted_terminals().pop().expect("trusted terminal");
        assert_eq!(terminal.friendly_name.as_deref(), Some("Desk Terminal"));
        assert_eq!(terminal.invitation_id, Some(first_id));
        let history = manager.invitation_history();
        let accepted = history.iter().find(|entry| entry.id == first_id).expect("accepted history");
        assert_eq!(accepted.consumed_by_device_id.as_deref(), Some(terminal.device_id.as_str()));
        assert_eq!(accepted.status, super::InvitationStatus::Accepted);
        let other = history.iter().find(|entry| entry.id == second_id).expect("other invitation");
        assert_eq!(other.description.as_deref(), Some("Travel Laptop"));
        assert_eq!(other.status, super::InvitationStatus::Active);
        assert!(other.consumed_by_device_id.is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn empty_invitation_description_uses_neutral_terminal_name() {
        let path = manager_path();
        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager");
        let host_id = "test-host";
        let invitation_id = Uuid::new_v4();
        let (secret_bytes, secret) = secret();
        let now = super::unix_time().expect("clock");
        manager.activate_invitation(
            invitation_id,
            Sha256::digest(secret_bytes).into(),
            now,
            now + super::INVITATION_TTL.as_secs(),
            Some("   ".to_owned()),
        ).expect("invite with empty description");
        let key = key_pair();
        let proof = challenge(&manager, host_id, &key, Some(invitation_id));
        authenticate(&manager, host_id, &proof, &key, Some(invitation_id), Some(&secret)).expect("pair");
        assert_eq!(manager.trusted_terminals()[0].friendly_name.as_deref(), Some("Terminal"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn active_invitation_is_revoked_on_restart_and_legacy_trust_file_migrates() {
        let path = manager_path();
        let key = key_pair();
        let public_key: [u8; 32] = key.public_key().as_ref().try_into().expect("public key");
        let device_id = device_id_for_public_key(&public_key).expect("device id");
        let mut terminals = serde_json::Map::new();
        terminals.insert(device_id.clone(), serde_json::json!({
            "device_id": device_id,
            "public_key": URL_SAFE_NO_PAD.encode(public_key),
            "paired_at": 123
        }));
        let legacy = serde_json::json!({"version": 1, "terminals": terminals});
        fs::write(&path, serde_json::to_vec(&legacy).expect("legacy JSON")).expect("write legacy file");
        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("legacy migration");
        assert_eq!(manager.trusted_terminals().len(), 1);
        assert!(manager.trusted_terminals()[0].friendly_name.is_none());
        drop(manager);
        let migrated: serde_json::Value = serde_json::from_slice(&fs::read(&path).expect("migrated file")).expect("JSON");
        assert_eq!(migrated["version"], 2);

        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager");
        let now = super::unix_time().expect("clock");
        let invitation_id = Uuid::new_v4();
        manager.activate_invitation(invitation_id, [0x44; 32], now, now + 900, None).expect("invite");
        drop(manager);
        let restarted = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("restart");
        let invitation = restarted.invitation_history().into_iter().find(|entry| entry.id == invitation_id).expect("history");
        assert_eq!(invitation.status, super::InvitationStatus::Revoked);
        assert!(!restarted.active_invitations().iter().any(|entry| entry.id == invitation_id));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn active_invitation_count_is_bounded_at_twenty() {
        let path = manager_path();
        let manager = SessionManager::load_from_path(Uuid::new_v4(), path.clone()).expect("manager");
        let now = super::unix_time().expect("clock");
        for index in 0..super::MAX_ACTIVE_INVITATIONS {
            let id = Uuid::new_v4();
            manager.activate_invitation(id, [index as u8; 32], now, now + 900, None).expect("invite within bound");
        }
        let error = manager.activate_invitation(Uuid::new_v4(), [0x99; 32], now, now + 900, None).unwrap_err();
        assert!(matches!(error, AuthError::InvitationLimitReached));
        let _ = fs::remove_file(path);
    }
}

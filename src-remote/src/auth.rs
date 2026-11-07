use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::{Mutex, RwLock},
    time::{Duration, Instant},
};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD},
};
use hmac::{Hmac, Mac};
use rand::random;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;
const ACCESS_TTL: Duration = Duration::from_secs(900);
const HEARTBEAT_LOSS: Duration = Duration::from_secs(15);
const RESERVATION_TTL: Duration = Duration::from_secs(30);
const PENDING_ATTACH_TTL: Duration = Duration::from_secs(15);
const RESUME_REPLAY_TTL: Duration = Duration::from_secs(15);
const MAX_RESUME_REPLAYS: usize = 8;
const DUMMY_SALT: [u8; 16] = [0xa9; 16];
const DUMMY_VERIFIER: [u8; 32] = [0x5c; 32];

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
    #[error("credential input is invalid")]
    InvalidCredential,
    #[error("credential storage failed: {0}")]
    Storage(std::io::Error),
    #[error("credential hashing failed")]
    Hash,
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
    PendingAttach,
    Active,
    Reserved,
}

impl SessionPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Free => "FREE",
            Self::PendingAttach => "PENDING_ATTACH",
            Self::Active => "ACTIVE",
            Self::Reserved => "RESERVED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionStatus {
    pub phase: SessionPhase,
    pub transport_generation: Option<u64>,
    pub last_contact: Option<Instant>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredential {
    username: String,
    salt: String,
    verifier: String,
}

struct CredentialVerifier {
    username: String,
    salt: Zeroizing<Vec<u8>>,
    verifier: Zeroizing<Vec<u8>>,
}

impl CredentialVerifier {
    fn from_file() -> Result<Option<Self>, AuthError> {
        let path = credential_path()?;
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AuthError::Storage(error)),
        };
        let stored: StoredCredential = serde_json::from_str(&raw).map_err(|_| AuthError::InvalidCredential)?;
        let salt = STANDARD_NO_PAD
            .decode(stored.salt)
            .map_err(|_| AuthError::InvalidCredential)?;
        let verifier = STANDARD_NO_PAD
            .decode(stored.verifier)
            .map_err(|_| AuthError::InvalidCredential)?;
        if salt.len() != 16 || verifier.len() != 32 || stored.username.is_empty() || stored.username.len() > 128 {
            return Err(AuthError::InvalidCredential);
        }
        Ok(Some(Self {
            username: stored.username,
            salt: Zeroizing::new(salt),
            verifier: Zeroizing::new(verifier),
        }))
    }

    fn create(username: &str, password: &str) -> Result<Self, AuthError> {
        if username.is_empty()
            || username.chars().count() > 128
            || password.is_empty()
            || password.chars().count() > 1024
        {
            return Err(AuthError::InvalidCredential);
        }
        let salt = random::<[u8; 16]>();
        let mut verifier = Zeroizing::new(vec![0u8; 32]);
        derive_password(password, &salt, &mut verifier)?;
        Ok(Self {
            username: username.to_owned(),
            salt: Zeroizing::new(salt.to_vec()),
            verifier,
        })
    }

    fn verify(&self, username: &str, password: &str) -> Result<bool, AuthError> {
        let mut candidate = Zeroizing::new(vec![0u8; 32]);
        derive_password(password, &self.salt, &mut candidate)?;
        let username_matches: bool = self.username.as_bytes().ct_eq(username.as_bytes()).into();
        let password_matches: bool = self.verifier.as_slice().ct_eq(candidate.as_slice()).into();
        Ok(username_matches && password_matches)
    }

    fn save(&self) -> Result<(), AuthError> {
        let path = credential_path()?;
        let directory = path.parent().ok_or(AuthError::InvalidCredential)?;
        fs::create_dir_all(directory).map_err(AuthError::Storage)?;
        let stored = StoredCredential {
            username: self.username.clone(),
            salt: STANDARD_NO_PAD.encode(&self.salt),
            verifier: STANDARD_NO_PAD.encode(&self.verifier),
        };
        let serialized = serde_json::to_vec(&stored).map_err(|_| AuthError::InvalidCredential)?;
        fs::write(path, serialized).map_err(AuthError::Storage)
    }
}

fn derive_password(password: &str, salt: &[u8], output: &mut [u8]) -> Result<(), AuthError> {
    let params = Params::default();
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, output)
        .map_err(|_| AuthError::Hash)
}

fn credential_path() -> Result<PathBuf, AuthError> {
    #[cfg(windows)]
    {
        let app_data = std::env::var_os("APPDATA").ok_or(AuthError::InvalidCredential)?;
        Ok(PathBuf::from(app_data).join("ShockingVRC").join("remote_auth.json"))
    }
    #[cfg(not(windows))]
    {
        Ok(PathBuf::from("remote_auth.json"))
    }
}

struct Session {
    id: Uuid,
    phase: SessionPhase,
    access_hash: [u8; 32],
    previous_access_hash: Option<([u8; 32], Instant)>,
    resume_hash: [u8; 32],
    login_response: LoginResponse,
    wss_attached: bool,
    generation: u64,
    access_expires: Instant,
    last_contact: Instant,
    reserved_at: Option<Instant>,
    phase_started: Instant,
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
    credentials: RwLock<Option<CredentialVerifier>>,
    inner: Mutex<SessionInner>,
}

impl SessionManager {
    pub fn load(server_epoch: Uuid) -> Result<Self, AuthError> {
        Ok(Self {
            epoch: server_epoch,
            digest_key: Zeroizing::new(random()),
            credentials: RwLock::new(CredentialVerifier::from_file()?),
            inner: Mutex::new(SessionInner { session: None }),
        })
    }

    pub fn has_credentials(&self) -> bool {
        self.credentials
            .read()
            .map(|credentials| credentials.is_some())
            .unwrap_or(false)
    }

    pub fn set_credentials(&self, username: &str, password: &str) -> Result<(), AuthError> {
        let credentials = CredentialVerifier::create(username, password)?;
        credentials.save()?;
        *self.credentials.write().map_err(|_| AuthError::State)? = Some(credentials);
        self.kick();
        Ok(())
    }

    pub fn login(&self, username: &str, password: &str, pending_attach: bool) -> Result<LoginResponse, AuthError> {
        {
            let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
            self.expire_locked(&mut inner, Instant::now());
            if inner.session.is_some() {
                return Err(AuthError::Busy);
            }
        }
        let credentials = self.credentials.read().map_err(|_| AuthError::State)?;
        let verified = match credentials.as_ref() {
            Some(credentials) => credentials.verify(username, password)?,
            None => {
                let mut dummy = Zeroizing::new(vec![0u8; 32]);
                derive_password(password, &DUMMY_SALT, &mut dummy)?;
                let _: bool = DUMMY_VERIFIER.as_slice().ct_eq(dummy.as_slice()).into();
                false
            }
        };
        if !verified {
            return Err(AuthError::Failed);
        }
        drop(credentials);

        let now = Instant::now();
        let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
        self.expire_locked(&mut inner, now);
        if inner.session.is_some() {
            return Err(AuthError::Busy);
        }
        let response = self.new_login_response(Uuid::new_v4(), 1);
        let access_hash = self.digest(response.access_token.as_bytes());
        let resume_hash = self.digest(response.resume_ticket.as_bytes());
        inner.session = Some(Session {
            id: response.session_id,
            phase: if pending_attach {
                SessionPhase::PendingAttach
            } else {
                SessionPhase::Active
            },
            access_hash,
            previous_access_hash: None,
            resume_hash,
            login_response: response.clone(),
            wss_attached: !pending_attach,
            generation: 1,
            access_expires: now + ACCESS_TTL,
            last_contact: now,
            reserved_at: None,
            phase_started: now,
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
        session.login_response = response.clone();
        session.wss_attached = true;
        session.generation = next_generation;
        session.phase = SessionPhase::Active;
        session.last_contact = now;
        session.reserved_at = None;
        session.phase_started = now;
        session.access_expires = now + ACCESS_TTL;
        Ok(response)
    }

    pub fn attach(&self, token: &str) -> Result<LoginResponse, AuthError> {
        self.with_bearer(token, |session| {
            if session.phase == SessionPhase::Active && session.wss_attached {
                return Err(AuthError::Busy);
            }
            if !matches!(session.phase, SessionPhase::PendingAttach | SessionPhase::Active) {
                return Err(AuthError::Expired);
            }
            let response = session.login_response.clone();
            let now = Instant::now();
            session.wss_attached = true;
            session.phase = SessionPhase::Active;
            session.phase_started = now;
            session.last_contact = now;
            Ok(response)
        })
    }

    pub fn logout_generation(&self, session_id: Uuid, generation: u64) -> Result<(), AuthError> {
        let mut inner = self.inner.lock().map_err(|_| AuthError::State)?;
        self.expire_locked(&mut inner, Instant::now());
        let session = inner.session.as_ref().ok_or(AuthError::Unavailable)?;
        if session.id != session_id || session.generation != generation || session.phase != SessionPhase::Active {
            return Err(AuthError::Expired);
        }
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
        session.login_response.access_token.clone_from(&new_token);
        session.login_response.access_expires_in = ACCESS_TTL.as_secs() as u16;
        Ok(RefreshResponse {
            access_token: new_token,
            access_expires_in: ACCESS_TTL.as_secs() as u16,
        })
    }

    pub fn heartbeat(&self, token: &str) -> Result<(), AuthError> {
        self.with_bearer(token, |session| {
            if session.phase == SessionPhase::PendingAttach {
                session.phase = SessionPhase::Active;
            }
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
            session.login_response.access_token.clone_from(&new_token);
            session.login_response.access_expires_in = ACCESS_TTL.as_secs() as u16;
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
        inner.session = None;
        Ok(())
    }

    pub fn kick(&self) {
        if let Ok(mut inner) = self.inner.lock() {
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
                session.phase = SessionPhase::Reserved;
                session.wss_attached = false;
                session.reserved_at = Some(now);
                session.phase_started = now;
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
        if session.phase == SessionPhase::PendingAttach
            && now.duration_since(session.phase_started) >= PENDING_ATTACH_TTL
        {
            inner.session = None;
            return;
        }
        if session.phase == SessionPhase::Active && now.duration_since(session.last_contact) >= HEARTBEAT_LOSS {
            session.phase = SessionPhase::Reserved;
            session.reserved_at = Some(now);
        }
        if session.phase == SessionPhase::Reserved
            && session
                .reserved_at
                .is_some_and(|started| now.duration_since(started) >= RESERVATION_TTL)
        {
            inner.session = None;
        }
        if inner
            .session
            .as_ref()
            .is_some_and(|session| now >= session.access_expires && session.phase == SessionPhase::PendingAttach)
        {
            inner.session = None;
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

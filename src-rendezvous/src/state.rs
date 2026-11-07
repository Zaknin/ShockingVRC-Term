use std::{collections::HashMap, sync::Arc, time::{Duration, Instant}};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use crate::{
    config::LoadedConfig,
    protocol::{SIGNAL_PROTOCOL, SignalMessage},
    turn::{TurnError, issue_credentials},
};

pub const OUTBOUND_CAPACITY: usize = 32;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<LoadedConfig>,
    registry: Arc<Mutex<Registry>>,
}

#[derive(Default)]
struct Registry {
    hosts: HashMap<String, HostConnection>,
    rooms: HashMap<String, Room>,
    room_by_host: HashMap<String, String>,
}

#[derive(Clone)]
struct HostConnection {
    generation: Uuid,
    identity_public_key: String,
    outgoing: mpsc::Sender<Outbound>,
}

struct Room {
    host_id: String,
    host_generation: Uuid,
    terminal: mpsc::Sender<Outbound>,
    expires_at: Instant,
    candidates_by_peer: [usize; 2],
    ended_by_peer: [bool; 2],
    offer_seen: bool,
    answer_seen: bool,
}

#[derive(Clone, Debug)]
pub enum Outbound {
    Message(SignalMessage),
    Close,
    Ping(Vec<u8>),
    Pong(Vec<u8>),
}

#[derive(Clone, Debug)]
pub enum Peer {
    Host { host_id: String, generation: Uuid },
    Terminal { room_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateError {
    AuthRequired,
    HostNotFound,
    RoomBusy,
    RoomNotFound,
    RoomExpired,
    CandidateLimit,
    UnexpectedMessage,
    TurnUnconfigured,
    SignalBufferFull,
    RegistrationReplaced,
}

impl StateError {
    pub fn code(self) -> &'static str {
        match self {
            Self::AuthRequired => "auth_required",
            Self::HostNotFound => "host_not_found",
            Self::RoomBusy => "room_busy",
            Self::RoomNotFound => "room_not_found",
            Self::RoomExpired => "room_expired",
            Self::CandidateLimit => "candidate_limit",
            Self::UnexpectedMessage => "invalid_message",
            Self::TurnUnconfigured => "turn_unconfigured",
            Self::SignalBufferFull => "signal_buffer_full",
            Self::RegistrationReplaced => "registration_replaced",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::AuthRequired => "The supplied rendezvous authorization is not valid.",
            Self::HostNotFound => "The requested Host is not currently registered.",
            Self::RoomBusy => "This Host already has a pending signaling room.",
            Self::RoomNotFound => "The signaling room does not exist.",
            Self::RoomExpired => "The signaling room has expired.",
            Self::CandidateLimit => "The ICE candidate limit for this peer was reached.",
            Self::UnexpectedMessage => "This signaling message is not valid for the current peer or room state.",
            Self::TurnUnconfigured => "TURN credentials are not configured for this rendezvous instance.",
            Self::SignalBufferFull => "The peer signaling buffer is full; retry the exchange.",
            Self::RegistrationReplaced => "This Host registration was replaced by a newer connection.",
        }
    }
}

impl AppState {
    pub fn new(config: LoadedConfig) -> Self {
        Self {
            config: Arc::new(config),
            registry: Arc::new(Mutex::new(Registry::default())),
        }
    }

    pub async fn register_host(
        &self,
        host_id: &str,
        identity_public_key: String,
        outgoing: mpsc::Sender<Outbound>,
    ) -> (Uuid, Uuid) {
        let generation = Uuid::new_v4();
        let registration_epoch = Uuid::new_v4();
        let mut registry = self.registry.lock().await;
        if let Some(previous) = registry.hosts.remove(host_id) {
            let _ = previous.outgoing.try_send(Outbound::Close);
            remove_host_rooms(&mut registry, host_id, previous.generation, true, "Host registration replaced");
        }
        registry.hosts.insert(
            host_id.to_owned(),
            HostConnection {
                generation,
                identity_public_key,
                outgoing,
            },
        );
        log::info!("[rendezvous] Host registration accepted host={host_id} generation={generation}");
        (generation, registration_epoch)
    }

    pub async fn unregister_host(&self, host_id: &str, generation: Uuid) {
        let mut registry = self.registry.lock().await;
        let current = registry
            .hosts
            .get(host_id)
            .is_some_and(|host| host.generation == generation);
        if current {
            registry.hosts.remove(host_id);
            remove_host_rooms(&mut registry, host_id, generation, true, "Host signaling connection closed");
            log::info!("[rendezvous] Host registration closed host={host_id} generation={generation}");
        }
    }

    pub async fn authorize_terminal_join(&self, host_id: &str, authorization: &str) -> bool {
        let Some(policy) = self.config.hosts.get(host_id) else { return false; };
        let registered_key = {
            let registry = self.registry.lock().await;
            let Some(host) = registry.hosts.get(host_id) else { return false; };
            let Ok(public_key) = crate::pairing::verify_host_identity_key(host_id, &host.identity_public_key) else { return false; };
            public_key
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs()).unwrap_or_default();
        crate::pairing::verify_route_token(policy.registration_key.as_bytes(), host_id, authorization, now).is_ok()
            || crate::pairing::verify_device_grant(authorization, host_id, &registered_key).is_ok()
    }

    pub async fn create_room(
        &self,
        host_id: &str,
        request_id: Uuid,
        terminal: mpsc::Sender<Outbound>,
    ) -> Result<String, StateError> {
        let room_id = Uuid::new_v4().to_string();
        let nonce = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        let ttl = std::time::Duration::from_secs(self.config.rooms.ttl_seconds);
        let mut registry = self.registry.lock().await;
        let host = registry.hosts.get(host_id).cloned().ok_or(StateError::HostNotFound)?;
        if let Some(existing_room_id) = registry.room_by_host.get(host_id).cloned() {
            let is_expired = registry
                .rooms
                .get(&existing_room_id)
                .is_none_or(|room| room.expires_at <= Instant::now());
            if is_expired {
                remove_room(&mut registry, &existing_room_id, true, "room expired before a new join");
            } else {
                return Err(StateError::RoomBusy);
            }
        }
        let room = Room {
            host_id: host_id.to_owned(),
            host_generation: host.generation,
            terminal: terminal.clone(),
            expires_at: Instant::now() + ttl,
            candidates_by_peer: [0, 0],
            ended_by_peer: [false, false],
            offer_seen: false,
            answer_seen: false,
        };
        log::info!("[rendezvous] room opened id={room_id} host={host_id} request={request_id} ttl_seconds={}", ttl.as_secs());
        registry.room_by_host.insert(host_id.to_owned(), room_id.clone());
        registry.rooms.insert(room_id.clone(), room);
        let message = SignalMessage::JoinAccepted {
            protocol: SIGNAL_PROTOCOL,
            request_id,
            host_id: host_id.to_owned(),
            room_id: room_id.clone(),
            nonce,
        };
        if host.outgoing.try_send(Outbound::Message(message.clone())).is_err() {
            remove_room(&mut registry, &room_id, false, "Host signaling buffer full during room creation");
            return Err(StateError::SignalBufferFull);
        }
        if terminal.try_send(Outbound::Message(message)).is_err() {
            remove_room(&mut registry, &room_id, true, "Terminal signaling buffer full during room creation");
            return Err(StateError::SignalBufferFull);
        }
        Ok(room_id)
    }

    pub async fn unregister_terminal(&self, room_id: &str) {
        let mut registry = self.registry.lock().await;
        remove_room(&mut registry, room_id, true, "Terminal signaling connection closed");
    }

    /// Extends an active room while its signaling WebSocket is alive. The server sends
    /// periodic WebSocket pings, and a client's pong is the existing liveness signal.
    /// A missing room is a normal no-op for a registered Host with no Terminal.
    pub async fn renew_room_lease(&self, peer: &Peer) -> Result<bool, StateError> {
        let mut registry = self.registry.lock().await;
        let room_id = match peer {
            Peer::Host { host_id, generation } => {
                let Some(room_id) = registry.room_by_host.get(host_id).cloned() else {
                    return Ok(false);
                };
                let Some(room) = registry.rooms.get(&room_id) else {
                    return Ok(false);
                };
                if room.host_generation != *generation {
                    return Err(StateError::RegistrationReplaced);
                }
                room_id
            }
            Peer::Terminal { room_id } => room_id.clone(),
        };

        let now = Instant::now();
        let Some(room) = registry.rooms.get(&room_id) else {
            return Ok(false);
        };
        if room.expires_at <= now {
            remove_room(&mut registry, &room_id, true, "room TTL elapsed before signaling heartbeat");
            return Err(StateError::RoomExpired);
        }
        let authorized = match peer {
            Peer::Host { host_id, generation } => {
                &room.host_id == host_id && room.host_generation == *generation
            }
            Peer::Terminal { room_id: peer_room } => peer_room == &room_id,
        };
        if !authorized {
            return Err(StateError::RoomNotFound);
        }

        let expires_at = now + Duration::from_secs(self.config.rooms.ttl_seconds);
        if let Some(room) = registry.rooms.get_mut(&room_id) {
            room.expires_at = expires_at;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn forward(&self, peer: &Peer, message: SignalMessage) -> Result<(), StateError> {
        let room_id = match &message {
            SignalMessage::SignalOffer { room_id, .. }
            | SignalMessage::SignalAnswer { room_id, .. }
            | SignalMessage::SignalCandidate { room_id, .. }
            | SignalMessage::SignalEndOfCandidates { room_id, .. } => room_id,
            _ => return Err(StateError::UnexpectedMessage),
        };
        let signal_kind = match &message {
            SignalMessage::SignalOffer { .. } => "offer",
            SignalMessage::SignalAnswer { .. } => "answer",
            SignalMessage::SignalCandidate { .. } => "candidate",
            SignalMessage::SignalEndOfCandidates { .. } => "end_of_candidates",
            _ => unreachable!(),
        };
        let mut registry = self.registry.lock().await;
        let Some(room_snapshot) = registry.rooms.get(room_id) else {
            return Err(StateError::RoomNotFound);
        };
        if room_snapshot.expires_at <= Instant::now() {
            remove_room(&mut registry, room_id, true, "room TTL elapsed while forwarding signaling");
            return Err(StateError::RoomExpired);
        }
        let (source_index, destination) = match peer {
            Peer::Host { host_id, generation }
                if host_id == &room_snapshot.host_id && *generation == room_snapshot.host_generation =>
            {
                (1, room_snapshot.terminal.clone())
            }
            Peer::Terminal { room_id: current } if current == room_id => {
                let Some(host) = registry
                    .hosts
                    .get(&room_snapshot.host_id)
                    .filter(|host| host.generation == room_snapshot.host_generation)
                else {
                    return Err(StateError::HostNotFound);
                };
                (0, host.outgoing.clone())
            }
            _ => return Err(StateError::RoomNotFound),
        };
        let room = registry
            .rooms
            .get_mut(room_id)
            .expect("room remains locked in registry");
        match &message {
            SignalMessage::SignalOffer { .. } => {
                if source_index != 0 || room.offer_seen {
                    return Err(StateError::UnexpectedMessage);
                }
            }
            SignalMessage::SignalAnswer { .. } => {
                if source_index != 1 || !room.offer_seen || room.answer_seen {
                    return Err(StateError::UnexpectedMessage);
                }
            }
            SignalMessage::SignalCandidate { .. } => {
                if room.ended_by_peer[source_index] {
                    return Err(StateError::UnexpectedMessage);
                }
                if room.candidates_by_peer[source_index] >= self.config.rooms.max_candidates_per_peer {
                    return Err(StateError::CandidateLimit);
                }
            }
            SignalMessage::SignalEndOfCandidates { .. } => {
                if room.ended_by_peer[source_index] {
                    return Err(StateError::UnexpectedMessage);
                }
            }
            _ => unreachable!(),
        }
        if destination.try_send(Outbound::Message(message.clone())).is_err() {
            return Err(StateError::SignalBufferFull);
        }
        match message {
            SignalMessage::SignalOffer { .. } => room.offer_seen = true,
            SignalMessage::SignalAnswer { .. } => room.answer_seen = true,
            SignalMessage::SignalCandidate { .. } => room.candidates_by_peer[source_index] += 1,
            SignalMessage::SignalEndOfCandidates { .. } => room.ended_by_peer[source_index] = true,
            _ => unreachable!(),
        }
        if matches!(signal_kind, "offer" | "answer") {
            log::info!("[rendezvous] forwarded signaling message room={room_id} kind={signal_kind} source_peer={} host_candidates={} terminal_candidates={} offer_seen={} answer_seen={}",
                if source_index == 1 { "host" } else { "terminal" },
                room.candidates_by_peer[1], room.candidates_by_peer[0], room.offer_seen, room.answer_seen,
            );
        } else {
            log::debug!("[rendezvous] forwarded signaling message room={room_id} kind={signal_kind} source_peer={} host_candidates={} terminal_candidates={} offer_seen={} answer_seen={}",
                if source_index == 1 { "host" } else { "terminal" },
                room.candidates_by_peer[1], room.candidates_by_peer[0], room.offer_seen, room.answer_seen,
            );
        }
        Ok(())
    }

    pub async fn close_room(&self, peer: &Peer, request_id: Uuid, room_id: &str) -> Result<(), StateError> {
        let mut registry = self.registry.lock().await;
        let Some(room) = registry.rooms.get(room_id) else {
            return Err(StateError::RoomNotFound);
        };
        if room.expires_at <= Instant::now() {
            remove_room(&mut registry, room_id, true, "room TTL elapsed while closing");
            return Err(StateError::RoomExpired);
        }
        let (host_id, generation, terminal) = (room.host_id.clone(), room.host_generation, room.terminal.clone());
        let authorized = match peer {
            Peer::Host {
                host_id: peer_host_id,
                generation: peer_generation,
            } => peer_host_id == &host_id && *peer_generation == generation,
            Peer::Terminal { room_id: peer_room } => peer_room == room_id,
        };
        if !authorized {
            return Err(StateError::RoomNotFound);
        }
        let notify = match peer {
            Peer::Host { .. } => terminal,
            Peer::Terminal { .. } => registry
                .hosts
                .get(&host_id)
                .filter(|host| host.generation == generation)
                .map(|host| host.outgoing.clone())
                .ok_or(StateError::HostNotFound)?,
        };
        let close_reason = match peer {
            Peer::Host { .. } => "Host requested room close",
            Peer::Terminal { .. } => "Terminal requested room close",
        };
        remove_room(&mut registry, room_id, false, close_reason);
        let close = SignalMessage::RoomClose {
            protocol: SIGNAL_PROTOCOL,
            request_id,
            room_id: room_id.to_owned(),
        };
        let _ = notify.try_send(Outbound::Message(close));
        Ok(())
    }

    pub async fn turn_credentials(
        &self,
        peer: &Peer,
        request_id: Uuid,
        room_id: &str,
    ) -> Result<SignalMessage, StateError> {
        let mut registry = self.registry.lock().await;
        let room = registry.rooms.get(room_id).ok_or(StateError::RoomNotFound)?;
        if room.expires_at <= Instant::now() {
            remove_room(&mut registry, room_id, true, "room TTL elapsed while issuing TURN credentials");
            return Err(StateError::RoomExpired);
        }
        match peer {
            Peer::Host { host_id, generation } if host_id == &room.host_id && *generation == room.host_generation => {}
            Peer::Terminal { room_id: peer_room } if peer_room == room_id => {}
            _ => return Err(StateError::RoomNotFound),
        }
        let secret = self.config.turn_secret.as_deref().ok_or(StateError::TurnUnconfigured)?;
        let response = issue_credentials(
            secret,
            self.config.turn.credential_ttl_seconds,
            &self.config.turn.uris,
            room_id,
            request_id,
        )
        .map_err(|_: TurnError| StateError::TurnUnconfigured)?;
        if let SignalMessage::TurnCredentialsResult { expires_at, turn_uris, .. } = &response {
            log::info!("[rendezvous] TURN credentials issued room={room_id} request={request_id} uri_count={} valid_for_seconds={} expires_at={expires_at}", turn_uris.len(), self.config.turn.credential_ttl_seconds);
        } else {
            log::error!("[rendezvous] TURN credential generation returned an unexpected response room={room_id} request={request_id}");
        }
        Ok(response)
    }

    pub async fn expire_rooms(&self) -> usize {
        let mut registry = self.registry.lock().await;
        let expired: Vec<String> = registry
            .rooms
            .iter()
            .filter_map(|(room_id, room)| (room.expires_at <= Instant::now()).then_some(room_id.clone()))
            .collect();
        for room_id in &expired {
            remove_room(&mut registry, room_id, true, "room TTL elapsed");
        }
        if !expired.is_empty() {
            log::info!("expired {} signaling room(s)", expired.len());
        }
        expired.len()
    }

    pub async fn connection_counts(&self) -> (usize, usize) {
        let registry = self.registry.lock().await;
        (registry.hosts.len(), registry.rooms.len())
    }
}

fn remove_host_rooms(registry: &mut Registry, host_id: &str, generation: Uuid, notify: bool, reason: &str) {
    let room_ids: Vec<String> = registry
        .rooms
        .iter()
        .filter_map(|(id, room)| (room.host_id == host_id && room.host_generation == generation).then_some(id.clone()))
        .collect();
    for room_id in room_ids {
        remove_room(registry, &room_id, notify, reason);
    }
}

fn remove_room(registry: &mut Registry, room_id: &str, notify: bool, reason: &str) {
    let Some(room) = registry.rooms.remove(room_id) else {
        return;
    };
    if registry
        .room_by_host
        .get(&room.host_id)
        .is_some_and(|current| current == room_id)
    {
        registry.room_by_host.remove(&room.host_id);
    }
    log::info!(
        "[rendezvous] room closed id={room_id} host={} reason=\"{reason}\" host_candidates={} terminal_candidates={} offer_seen={} answer_seen={} notified={notify}",
        room.host_id, room.candidates_by_peer[1], room.candidates_by_peer[0], room.offer_seen, room.answer_seen,
    );
    if notify {
        let close = Outbound::Message(SignalMessage::room_close(room_id.to_owned()));
        let _ = room.terminal.try_send(close.clone());
        if let Some(host) = registry
            .hosts
            .get(&room.host_id)
            .filter(|host| host.generation == room.host_generation)
        {
            let _ = host.outgoing.try_send(close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HostAuthorization, RoomConfig, ServerConfig, TurnConfig};
    use std::path::PathBuf;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn loaded_config(room_ttl_seconds: u64, hosts: HashMap<String, HostAuthorization>) -> LoadedConfig {
        LoadedConfig {
            server: ServerConfig {
                bind: "127.0.0.1:0".into(),
                public_url: String::new(),
                health_path: "/healthz".into(),
                signal_path: "/api/signal/v1".into(),
                version_path: "/version".into(),
                tls: Default::default(),
            },
            rooms: RoomConfig {
                ttl_seconds: room_ttl_seconds,
                max_candidates_per_peer: 64,
                max_signal_frame_bytes: 65_536,
            },
            turn: TurnConfig {
                enabled: false,
                uris: Vec::new(),
                realm: String::new(),
                secret_file: String::new(),
                credential_ttl_seconds: 600,
            },
            hosts,
            turn_secret: None,
            config_dir: PathBuf::new(),
        }
    }

    fn test_state(room_ttl_seconds: u64) -> AppState {
        AppState::new(loaded_config(room_ttl_seconds, HashMap::from([("host".into(), HostAuthorization {
                registration_key: "registration".into(),
            })])))
    }

    #[tokio::test]
    async fn terminal_join_accepts_only_registered_host_route_or_signed_device_grant() {
        let host_key = Ed25519KeyPair::from_seed_unchecked(&[42; 32]).expect("test Host key");
        let host_public_key = URL_SAFE_NO_PAD.encode(host_key.public_key().as_ref());
        let host_id = crate::pairing::host_id_for_public_key(host_key.public_key().as_ref()).unwrap();
        let registration_key = "registration-key";
        let state = AppState::new(loaded_config(60, HashMap::from([(host_id.clone(), HostAuthorization {
            registration_key: registration_key.into(),
        })])));
        let (host_tx, _host_rx) = mpsc::channel(8);
        state.register_host(&host_id, host_public_key, host_tx).await;

        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let invitation_id = Uuid::new_v4();
        let route = crate::pairing::create_route_token(registration_key.as_bytes(), &host_id, invitation_id, now + 900);
        assert!(state.authorize_terminal_join(&host_id, &route).await);
        assert!(!state.authorize_terminal_join(&host_id, "legacy-static-token").await);
        assert!(!state.authorize_terminal_join("different-host", &route).await);

        let terminal_key = Ed25519KeyPair::from_seed_unchecked(&[43; 32]).expect("test Terminal key");
        let terminal_public_key = terminal_key.public_key().as_ref();
        let mut grant = crate::pairing::DeviceGrantV1 {
            version: 1,
            host_id: host_id.clone(),
            device_id: crate::pairing::device_id_for_public_key(terminal_public_key).unwrap(),
            terminal_public_key: URL_SAFE_NO_PAD.encode(terminal_public_key),
            signature: String::new(),
        };
        grant.signature = URL_SAFE_NO_PAD.encode(host_key.sign(&grant.signing_bytes()).as_ref());
        let grant = grant.to_authorization().unwrap();
        assert!(state.authorize_terminal_join(&host_id, &grant).await);
    }

    #[tokio::test]
    async fn signaling_heartbeat_extends_active_room_lease() {
        let state = test_state(60);
        let (host_tx, _host_rx) = mpsc::channel(8);
        let (generation, _) = state.register_host("host", "public-key".into(), host_tx).await;
        let (terminal_tx, _terminal_rx) = mpsc::channel(8);
        let room_id = state.create_room("host", Uuid::new_v4(), terminal_tx).await.unwrap();
        let original_expiry = state.registry.lock().await.rooms[&room_id].expires_at;

        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(state.renew_room_lease(&Peer::Terminal { room_id: room_id.clone() }).await.unwrap());
        let terminal_expiry = state.registry.lock().await.rooms[&room_id].expires_at;
        assert!(terminal_expiry > original_expiry);

        assert!(state.renew_room_lease(&Peer::Host { host_id: "host".into(), generation }).await.unwrap());
        let host_expiry = state.registry.lock().await.rooms[&room_id].expires_at;
        assert!(host_expiry > terminal_expiry);
    }

    #[tokio::test]
    async fn signaling_heartbeat_does_not_resurrect_or_create_room() {
        let state = test_state(60);
        assert!(!state.renew_room_lease(&Peer::Terminal { room_id: "missing".into() }).await.unwrap());
        let (host_tx, _host_rx) = mpsc::channel(8);
        let (generation, _) = state.register_host("host", "public-key".into(), host_tx).await;
        assert!(!state.renew_room_lease(&Peer::Host { host_id: "host".into(), generation }).await.unwrap());
    }
}

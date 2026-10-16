use std::{collections::HashMap, sync::Arc, time::Instant};

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
    _identity_public_key: String,
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
            remove_host_rooms(&mut registry, host_id, previous.generation, true);
        }
        registry.hosts.insert(
            host_id.to_owned(),
            HostConnection {
                generation,
                _identity_public_key: identity_public_key,
                outgoing,
            },
        );
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
            remove_host_rooms(&mut registry, host_id, generation, true);
        }
    }

    pub async fn create_room(
        &self,
        host_id: &str,
        request_id: Uuid,
        terminal: mpsc::Sender<Outbound>,
    ) -> Result<String, StateError> {
        let room_id = Uuid::new_v4().to_string();
        let nonce = Uuid::new_v4().to_string();
        let ttl = std::time::Duration::from_secs(self.config.rooms.ttl_seconds);
        let mut registry = self.registry.lock().await;
        let host = registry.hosts.get(host_id).cloned().ok_or(StateError::HostNotFound)?;
        if let Some(existing_room_id) = registry.room_by_host.get(host_id).cloned() {
            let is_expired = registry
                .rooms
                .get(&existing_room_id)
                .is_none_or(|room| room.expires_at <= Instant::now());
            if is_expired {
                remove_room(&mut registry, &existing_room_id, true);
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
            remove_room(&mut registry, &room_id, false);
            return Err(StateError::SignalBufferFull);
        }
        if terminal.try_send(Outbound::Message(message)).is_err() {
            remove_room(&mut registry, &room_id, true);
            return Err(StateError::SignalBufferFull);
        }
        Ok(room_id)
    }

    pub async fn unregister_terminal(&self, room_id: &str) {
        let mut registry = self.registry.lock().await;
        remove_room(&mut registry, room_id, true);
    }

    pub async fn forward(&self, peer: &Peer, message: SignalMessage) -> Result<(), StateError> {
        let room_id = match &message {
            SignalMessage::SignalOffer { room_id, .. }
            | SignalMessage::SignalAnswer { room_id, .. }
            | SignalMessage::SignalCandidate { room_id, .. }
            | SignalMessage::SignalEndOfCandidates { room_id, .. } => room_id,
            _ => return Err(StateError::UnexpectedMessage),
        };
        let mut registry = self.registry.lock().await;
        let Some(room_snapshot) = registry.rooms.get(room_id) else {
            return Err(StateError::RoomNotFound);
        };
        if room_snapshot.expires_at <= Instant::now() {
            remove_room(&mut registry, room_id, true);
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
        Ok(())
    }

    pub async fn close_room(&self, peer: &Peer, request_id: Uuid, room_id: &str) -> Result<(), StateError> {
        let mut registry = self.registry.lock().await;
        let Some(room) = registry.rooms.get(room_id) else {
            return Err(StateError::RoomNotFound);
        };
        if room.expires_at <= Instant::now() {
            remove_room(&mut registry, room_id, true);
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
        remove_room(&mut registry, room_id, false);
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
            remove_room(&mut registry, room_id, true);
            return Err(StateError::RoomExpired);
        }
        match peer {
            Peer::Host { host_id, generation } if host_id == &room.host_id && *generation == room.host_generation => {}
            Peer::Terminal { room_id: peer_room } if peer_room == room_id => {}
            _ => return Err(StateError::RoomNotFound),
        }
        let secret = self.config.turn_secret.as_deref().ok_or(StateError::TurnUnconfigured)?;
        issue_credentials(
            secret,
            self.config.turn.credential_ttl_seconds,
            &self.config.turn.uris,
            room_id,
            request_id,
        )
        .map_err(|_: TurnError| StateError::TurnUnconfigured)
    }

    pub async fn expire_rooms(&self) -> usize {
        let mut registry = self.registry.lock().await;
        let expired: Vec<String> = registry
            .rooms
            .iter()
            .filter_map(|(room_id, room)| (room.expires_at <= Instant::now()).then_some(room_id.clone()))
            .collect();
        for room_id in &expired {
            remove_room(&mut registry, room_id, true);
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

fn remove_host_rooms(registry: &mut Registry, host_id: &str, generation: Uuid, notify: bool) {
    let room_ids: Vec<String> = registry
        .rooms
        .iter()
        .filter_map(|(id, room)| (room.host_id == host_id && room.host_generation == generation).then_some(id.clone()))
        .collect();
    for room_id in room_ids {
        remove_room(registry, &room_id, notify);
    }
}

fn remove_room(registry: &mut Registry, room_id: &str, notify: bool) {
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

//! Room negotiation and ADR-033 DTLS/Host identity binding.

use std::{collections::{HashMap, VecDeque}, sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;
use tokio::time::timeout;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use zeroize::Zeroizing;

use shocking_vrc_rendezvous::protocol::{
    ICE_DISCOVERY_PROTOCOL, SIGNAL_PROTOCOL, SignalMessage, normalize_ice_server_uri,
};

use crate::{BindingError, BindingFields, DtlsFingerprints, IdentityProof, IceServerConfig, RtcApiChannels, RtcError, RtcPeer, SignalError, SignalSocket, binding_transcript, identity_fingerprint, join_message, register_message, server_id, verify_binding};

const ROOM_TIMEOUT: Duration = Duration::from_secs(70);
const MAX_PENDING_HOST_SIGNAL_MESSAGES: usize = 128;

pub trait HostIdentitySigner: Send + Sync {
    fn public_key(&self) -> &[u8];
    fn server_id(&self) -> &str;
    fn fingerprint(&self) -> &str;
    fn sign(&self, transcript: &[u8]) -> Result<Vec<u8>, String>;
}

#[derive(Clone)]
pub struct RegisteredHost {
    pub host_id: String,
    pub registration_epoch: Uuid,
    pub server_epoch: Uuid,
}

pub struct HostSignaling {
    pub socket: SignalSocket,
    pub registration: RegisteredHost,
    pending_messages: VecDeque<SignalMessage>,
}

impl HostSignaling {
    async fn receive_matching(
        &mut self,
        mut matches: impl FnMut(&SignalMessage) -> bool,
    ) -> Result<SignalMessage, SessionError> {
        let mut pending = std::mem::take(&mut self.pending_messages);
        let mut retained = VecDeque::with_capacity(pending.len());
        while let Some(message) = pending.pop_front() {
            if matches(&message) {
                retained.append(&mut pending);
                self.pending_messages = retained;
                return Ok(message);
            }
            if should_defer_host_message(&message) {
                retained.push_back(message);
            }
        }
        self.pending_messages = retained;

        loop {
            let message = self.socket.receive().await?;
            if matches(&message) {
                return Ok(message);
            }
            if should_defer_host_message(&message) {
                self.defer(message)?;
            }
        }
    }

    fn defer(&mut self, message: SignalMessage) -> Result<(), SessionError> {
        if self.pending_messages.len() >= MAX_PENDING_HOST_SIGNAL_MESSAGES {
            return Err(SessionError::SignalingQueueFull);
        }
        let kind = signal_kind(&message);
        self.pending_messages.push_back(message);
        log::debug!("[rtc] Host deferred unmatched signaling message kind={kind} queue_len={}", self.pending_messages.len());
        Ok(())
    }

    /// Waits for Rendezvous to close one specific room while preserving messages for
    /// subsequent rooms in the Host registration stream.
    pub async fn wait_for_room_close(&mut self, room_id: Uuid) -> Result<(), SessionError> {
        let room_text = room_id.to_string();
        match self.receive_matching(|message| {
            matches!(message, SignalMessage::RoomClose { room_id: closed_room, .. } if closed_room == &room_text)
        }).await? {
            SignalMessage::RoomClose { .. } => Ok(()),
            unexpected => Err(SessionError::UnexpectedSignal(signal_kind(&unexpected))),
        }
    }
}

pub struct EstablishedRtcSession {
    pub peer: Arc<RtcPeer>,
    pub channels: RtcApiChannels,
    pub signaling: Option<SignalingLease>,
    pub room_id: Uuid,
    pub request_id: Uuid,
    pub ice_candidate_pair: Option<String>,
    pub ice_servers_configured: bool,
    pub rendezvous_stun_received: bool,
    pub turn_credentials_received: bool,
    pub local_candidates_gathered: bool,
    pub turn_refresh_at: Option<tokio::time::Instant>,
}

#[derive(Clone)]
pub struct TurnCredentials {
    pub server: IceServerConfig,
    pub refresh_at: tokio::time::Instant,
}

#[derive(Clone, Default)]
pub struct IceServerDiscovery {
    pub stun_uris: Vec<String>,
    pub turn_credentials: Option<TurnCredentials>,
}

struct TurnRefreshRequest {
    request_id: Uuid,
    responder: oneshot::Sender<Result<SignalMessage, String>>,
}

/// Keeps the Terminal's room signaling socket alive for the WebRTC session lifetime.
/// Besides preserving the room, its receive task answers rendezvous WebSocket pings.
pub struct SignalingLease {
    cancel: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    turn_requests: mpsc::Sender<TurnRefreshRequest>,
    room_id: Uuid,
}

impl SignalingLease {
    pub async fn close(mut self) {
        log::info!("[rtc] Terminal closing signaling lease");
        if let Some(cancel) = self.cancel.take() { let _ = cancel.send(()); }
        if let Some(mut task) = self.task.take() {
            if timeout(Duration::from_secs(5), &mut task).await.is_err() {
                log::warn!("[rtc] Terminal signaling lease did not stop within 5 seconds; aborting its socket owner room={}", self.room_id);
                task.abort();
                let _ = task.await;
            }
        }
    }

    pub async fn refresh_turn_credentials(&self) -> Result<Option<TurnCredentials>, SessionError> {
        let request_id = Uuid::new_v4();
        let (response_tx, response_rx) = oneshot::channel();
        self.turn_requests.send(TurnRefreshRequest { request_id, responder: response_tx })
            .await.map_err(|_| SessionError::Closed)?;
        let response = timeout(Duration::from_secs(15), response_rx).await
            .map_err(|_| SessionError::Timeout("waiting for renewed TURN credentials"))?
            .map_err(|_| SessionError::Closed)?
            .map_err(SessionError::Rendezvous)?;
        parse_turn_credentials(response, request_id, &self.room_id.to_string())
            .map(|discovery| discovery.turn_credentials)
    }
}

impl Drop for SignalingLease {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() { let _ = cancel.send(()); }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub struct TerminalRtcConfig {
    pub rendezvous_url: String,
    pub host_id: String,
    pub rendezvous_authorization: Zeroizing<String>,
    pub expected_fingerprint: String,
    pub ice_servers: Vec<IceServerConfig>,
    pub udp_addrs: Vec<String>,
    pub force_relay: bool,
    pub resume_session_id: Option<Uuid>,
}

pub async fn register_host(
    rendezvous_url: &str,
    signer: &dyn HostIdentitySigner,
    registration_key: Zeroizing<String>,
    server_epoch: Uuid,
) -> Result<HostSignaling, SessionError> {
    log::info!("[rtc] opening Host signaling registration");
    let mut socket = SignalSocket::connect(rendezvous_url).await?;
    let request = register_message(
        signer.server_id().to_owned(),
        URL_SAFE_NO_PAD.encode(signer.public_key()),
        registration_key.as_str().to_owned(),
    );
    let request_id = request.request_id();
    socket.send(&request).await?;
    match socket.receive().await? {
        SignalMessage::Registered { request_id: response_id, host_id, registration_epoch, .. }
            if response_id == request_id && host_id == signer.server_id() => {
                log::info!("[rtc] rendezvous accepted Host registration");
                Ok(HostSignaling {
                    socket,
                    registration: RegisteredHost { host_id, registration_epoch, server_epoch },
                    pending_messages: VecDeque::new(),
                })
            }
        SignalMessage::Error { code, message, .. } => Err(SessionError::Rendezvous(format!("{code}: {message}"))),
        _ => Err(SessionError::Correlation),
    }
}

pub async fn accept_host_room(
    signaling: &mut HostSignaling,
    signer: &dyn HostIdentitySigner,
    ice_servers: &[IceServerConfig],
    udp_addrs: Vec<String>,
) -> Result<EstablishedRtcSession, SessionError> {
    let (request_id, room_id, room_nonce, force_relay) = loop {
        // A registered Host may wait indefinitely for the Terminal. The underlying
        // signaling receive handles rendezvous Ping/Pong and still times out if the
        // WebSocket goes quiet or disconnects; applying ROOM_TIMEOUT here closed a
        // healthy registration after 70 seconds even while heartbeats were arriving.
        match signaling.receive_matching(|message| matches!(
            message,
            SignalMessage::JoinAccepted { .. } | SignalMessage::RoomClose { .. } | SignalMessage::Error { .. }
        )).await? {
            SignalMessage::JoinAccepted { request_id, host_id, room_id, nonce, force_relay, .. }
                if host_id == signaling.registration.host_id => break (request_id, parse_uuid(&room_id)?, decode_nonce(&nonce)?, force_relay),
            SignalMessage::RoomClose { room_id, .. } => {
                log::debug!("[rtc] Host ignored stale room-close notification while awaiting a new room room={room_id}");
            }
            SignalMessage::Error { code, message, .. } => return Err(SessionError::Rendezvous(format!("{code}: {message}"))),
            unexpected => return Err(SessionError::UnexpectedSignal(signal_kind(&unexpected))),
        }
    };
    let room_text = room_id.to_string();
    log::info!("[rtc] Host accepted signaling room room={room_text} request={request_id}; preparing ICE");
    match establish_host_room(signaling, signer, ice_servers, udp_addrs, request_id, room_id, room_nonce, force_relay).await {
        Ok(session) => Ok(session),
        Err(error) => {
            log::warn!("[rtc] Host ICE room negotiation ended room={room_text}; closing room before accepting another: {error}");
            match close_host_room(signaling, request_id, room_id).await {
                Ok(()) => Err(SessionError::RoomSetup(error.to_string())),
                Err(close_error) => Err(SessionError::RoomCleanup(format!(
                    "{error}; signaling room cleanup failed: {close_error}"
                ))),
            }
        }
    }
}

async fn establish_host_room(
    signaling: &mut HostSignaling,
    signer: &dyn HostIdentitySigner,
    ice_servers: &[IceServerConfig],
    udp_addrs: Vec<String>,
    request_id: Uuid,
    room_id: Uuid,
    room_nonce: [u8; 32],
    force_relay: bool,
) -> Result<EstablishedRtcSession, SessionError> {
    let room_text = room_id.to_string();
    let discovery_and_offer = request_host_room_turn_credentials(
        signaling,
        &room_text,
        request_id,
    ).await?;
    let discovery = discovery_and_offer.0;
    let early_offer = discovery_and_offer.1;
    let turn_credentials_received = discovery.turn_credentials.is_some();
    let rendezvous_stun_received = !discovery.stun_uris.is_empty();
    let peer_ice_servers = merge_discovery_ice_servers(ice_servers, &discovery);
    let ice_servers_configured = !peer_ice_servers.is_empty();
    log::info!("[rtc] Host waiting for Terminal ICE offer room={room_text}");
    let offer_sdp = match early_offer {
        Some(offer) => offer,
        None => loop {
            match timeout(ROOM_TIMEOUT, signaling.receive_matching(|message| {
                is_room_signal_for(message, request_id, &room_text)
                    || matches!(message, SignalMessage::Error { request_id: response_id, .. } if *response_id == request_id)
            })).await
                .map_err(|_| SessionError::Timeout("waiting for an ICE offer"))?? {
                SignalMessage::SignalOffer { request_id: id, room_id: offered_room, sdp, .. }
                    if id == request_id && offered_room == room_text => break sdp,
                SignalMessage::SignalCandidate { request_id: id, room_id: candidate_room, .. }
                    if id == request_id && candidate_room == room_text => {},
                SignalMessage::SignalEndOfCandidates { request_id: id, room_id: candidate_room, .. }
                    if id == request_id && candidate_room == room_text => {},
                SignalMessage::RoomClose { room_id: closed, .. } if closed == room_text => return Err(SessionError::Closed),
                SignalMessage::Error { code, message, .. } => return Err(SessionError::Rendezvous(format!("{code}: {message}"))),
                unexpected => return Err(SessionError::UnexpectedSignal(signal_kind(&unexpected))),
            }
        },
    };
    log::info!("[rtc] Host received ICE offer room={room_text}");
    let mut peer = RtcPeer::new(&peer_ice_servers, udp_addrs, force_relay).await?;
    let answer_started = std::time::Instant::now();
    let setup_result = async {
        let answer_sdp = peer.accept_offer(&offer_sdp).await?;
        log::info!("[rtc] Host ICE gathering completed; sending answer room={room_text} elapsed_ms={}", answer_started.elapsed().as_millis());
        signaling.socket.send(&SignalMessage::SignalAnswer {
            protocol: SIGNAL_PROTOCOL, request_id, room_id: room_text.clone(), sdp: answer_sdp.clone(),
        }).await?;
        log::info!("[rtc] Host ICE answer sent to rendezvous room={room_text}");
        peer.wait_data_channel().await?;
        log::info!("[rtc] Host received Terminal DataChannel room={room_text}");
        peer.wait_connected().await?;
        peer.wait_open().await?;
        log::info!("[rtc] Host ICE/DTLS transport connected room={room_text}");
        log::info!("[rtc] Host verifying DTLS identity binding room={room_text}");
        prove_host_identity(&peer, signer, &signaling.registration, request_id, room_id, room_nonce, peer.fingerprints().await?).await?;
        log::info!("[rtc] Host DTLS identity binding verified room={room_text}");
        Ok::<(), SessionError>(())
    }.await;
    match setup_result {
        Ok(()) => {}
        Err(error) => {
            log::warn!("[rtc] Host failed ICE setup room={room_text} elapsed_ms={} error={error}", answer_started.elapsed().as_millis());
            peer.close().await;
            return Err(error);
        }
    }
    let peer = Arc::new(peer);
    let channels = peer.api_channels();
    let ice_candidate_pair = peer.selected_candidate_pair_label().await;
    let local_candidates_gathered = peer.local_candidate_count().await > 0;
    Ok(EstablishedRtcSession { peer, channels, signaling: None, room_id, request_id, ice_candidate_pair,
        ice_servers_configured, rendezvous_stun_received, turn_credentials_received, local_candidates_gathered,
        turn_refresh_at: discovery.turn_credentials.map(|credentials| credentials.refresh_at) })
}

/// Host TURN credential responses and the Terminal offer share one signaling socket.
/// The Terminal can finish its credential request and send its offer before the Host's
/// credential response is delivered, so retain a matching early offer while waiting for
/// the correlated TURN result instead of treating it as an unexpected protocol frame.
async fn request_host_room_turn_credentials(
    signaling: &mut HostSignaling,
    room_text: &str,
    join_request_id: Uuid,
) -> Result<(IceServerDiscovery, Option<String>), SessionError> {
    let turn_request_id = Uuid::new_v4();
    log::info!("[rtc] Host requesting Rendezvous ICE discovery room={room_text}");
    signaling.socket.send(&SignalMessage::TurnCredentialsRequest {
        protocol: ICE_DISCOVERY_PROTOCOL,
        request_id: turn_request_id,
        room_id: room_text.to_owned(),
    }).await?;

    let response = timeout(Duration::from_secs(20), async {
        let mut early_offer = None;
        let mut legacy_fallback_attempted = false;
        loop {
            let message = signaling.receive_matching(|message| {
                is_turn_response_for(message, turn_request_id)
                    || is_room_signal_for(message, join_request_id, room_text)
                    || matches!(message, SignalMessage::Error { request_id: response_id, .. } if *response_id == join_request_id)
            }).await?;
            match message {
                SignalMessage::SignalOffer { request_id, room_id, sdp, .. }
                    if request_id == join_request_id && room_id == room_text => {
                        if early_offer.is_none() {
                            log::debug!("[rtc] Host buffered Terminal ICE offer while awaiting TURN credentials room={room_text}");
                            early_offer = Some(sdp);
                        }
                    }
                SignalMessage::SignalCandidate { request_id, room_id, .. }
                | SignalMessage::SignalEndOfCandidates { request_id, room_id, .. }
                    if request_id == join_request_id && room_id == room_text => {}
                SignalMessage::RoomClose { room_id, .. } if room_id == room_text => return Err(SessionError::Closed),
                SignalMessage::Error { request_id, code, .. }
                    if request_id == turn_request_id && code == "unsupported_version" && !legacy_fallback_attempted => {
                        legacy_fallback_attempted = true;
                        log::debug!("[rtc] Rendezvous does not support extended ICE discovery; retrying TURN credentials with protocol v1");
                        signaling.socket.send(&SignalMessage::TurnCredentialsRequest {
                            protocol: SIGNAL_PROTOCOL,
                            request_id: turn_request_id,
                            room_id: room_text.to_owned(),
                        }).await?;
                    }
                message @ SignalMessage::TurnCredentialsResult { request_id, .. }
                    if request_id == turn_request_id => {
                        let discovery = parse_turn_credentials(message, turn_request_id, room_text)?;
                        return Ok((discovery, early_offer));
                    }
                message @ SignalMessage::Error { request_id, .. }
                    if request_id == turn_request_id => {
                        let discovery = parse_turn_credentials(message, turn_request_id, room_text)?;
                        return Ok((discovery, early_offer));
                    }
                SignalMessage::Error { request_id, code, message, .. }
                    if request_id == join_request_id => {
                        return Err(SessionError::Rendezvous(format!("{code}: {message}")));
                    }
                unexpected => signaling.defer(unexpected)?,
            }
        }
    }).await.map_err(|_| SessionError::Timeout("waiting for initial Host TURN credentials"))??;
    Ok(response)
}

pub async fn join_terminal_room(config: TerminalRtcConfig) -> Result<EstablishedRtcSession, SessionError> {
    log::info!("[rtc] Terminal opening rendezvous signaling; force_relay={}", config.force_relay);
    let mut socket = SignalSocket::connect(&config.rendezvous_url).await?;
    let request = join_message(
        config.host_id.clone(),
        config.rendezvous_authorization.as_str().to_owned(),
        config.force_relay,
    );
    let request_id = request.request_id();
    let accepted = async {
        socket.send(&request).await?;
        match timeout(ROOM_TIMEOUT, socket.receive()).await
            .map_err(|_| SessionError::Timeout("waiting for Host registration"))?? {
            SignalMessage::JoinAccepted { request_id: response_id, host_id, room_id, nonce, .. }
                if response_id == request_id && host_id == config.host_id => Ok((parse_uuid(&room_id)?, decode_nonce(&nonce)?)),
            SignalMessage::Error { code, message, .. } => Err(SessionError::Rendezvous(format!("{code}: {message}"))),
            unexpected => Err(SessionError::UnexpectedSignal(signal_kind(&unexpected))),
        }
    }.await;
    let (room_id, room_nonce) = match accepted {
        Ok(room) => room,
        Err(error) => {
            socket.close().await;
            return Err(error);
        }
    };
    let room_text = room_id.to_string();
    log::info!("[rtc] Terminal joined signaling room room={room_text} request={request_id}; preparing ICE");
    match establish_terminal_room(&mut socket, &config, request_id, room_id, room_nonce).await {
        Ok(mut session) => {
            session.signaling = Some(keep_terminal_signaling_alive(socket, room_id));
            Ok(session)
        }
        Err(error) => {
            log::warn!("[rtc] Terminal ICE room setup ended room={room_text}; cleaning up signaling room: {error}");
            if let Err(close_error) = close_terminal_room(&mut socket, request_id, room_id).await {
                log::warn!("[rtc] Terminal signaling room cleanup failed room={room_text}: {close_error}");
            }
            socket.close().await;
            Err(error)
        }
    }
}

async fn establish_terminal_room(
    socket: &mut SignalSocket,
    config: &TerminalRtcConfig,
    request_id: Uuid,
    room_id: Uuid,
    room_nonce: [u8; 32],
) -> Result<EstablishedRtcSession, SessionError> {
    let room_text = room_id.to_string();
    let discovery = request_ice_server_discovery(socket, &room_text).await?;
    let turn_credentials_received = discovery.turn_credentials.is_some();
    let rendezvous_stun_received = !discovery.stun_uris.is_empty();
    let peer_ice_servers = merge_discovery_ice_servers(&config.ice_servers, &discovery);
    let ice_servers_configured = !peer_ice_servers.is_empty();
    let mut peer = RtcPeer::new(&peer_ice_servers, config.udp_addrs.clone(), config.force_relay).await?;
    let setup_result = async {
        log::debug!("[rtc] Terminal creating ICE offer room={room_text}; force_relay={}", config.force_relay);
        let offer_sdp = peer.create_offer().await?;
        socket.send(&SignalMessage::SignalOffer {
            protocol: SIGNAL_PROTOCOL, request_id, room_id: room_text.clone(), sdp: offer_sdp,
        }).await?;
        log::info!("[rtc] Terminal ICE offer sent; waiting for Host answer room={room_text}");
        let answer_sdp = loop {
            let message = match timeout(ROOM_TIMEOUT, socket.receive()).await {
                Ok(result) => result?,
                Err(_) => {
                    log::error!("[rtc] Terminal timed out waiting for Host ICE answer room={room_text} timeout_seconds={}", ROOM_TIMEOUT.as_secs());
                    return Err(SessionError::Timeout("waiting for ICE answer"));
                }
            };
            match message {
                SignalMessage::SignalAnswer { request_id: id, room_id: answered_room, sdp, .. }
                    if id == request_id && answered_room == room_text => break sdp,
                SignalMessage::SignalCandidate { request_id: id, room_id: candidate_room, .. }
                    if id == request_id && candidate_room == room_text => {},
                SignalMessage::SignalEndOfCandidates { request_id: id, room_id: candidate_room, .. }
                    if id == request_id && candidate_room == room_text => {},
                SignalMessage::RoomClose { room_id: closed, .. } if closed == room_text => return Err(SessionError::Closed),
                SignalMessage::Error { code, message, .. } => return Err(SessionError::Rendezvous(format!("{code}: {message}"))),
                unexpected => return Err(SessionError::UnexpectedSignal(signal_kind(&unexpected))),
            }
        };
        log::info!("[rtc] Terminal received Host ICE answer room={room_text}");
        peer.set_answer(&answer_sdp).await?;
        peer.wait_connected().await?;
        peer.wait_open().await?;
        let ice_candidate_pair = peer.selected_candidate_pair_label().await;
        let local_candidates_gathered = peer.local_candidate_count().await > 0;
        log::info!("[rtc] Terminal ICE/DTLS transport connected room={room_text}");
        let fingerprints = peer.fingerprints().await?;
        log::info!("[rtc] Terminal verifying pinned DTLS identity binding room={room_text}");
        verify_terminal_identity(
            &peer,
            &config.expected_fingerprint,
            &config.host_id,
            request_id,
            room_id,
            room_nonce,
            config.resume_session_id,
            fingerprints,
        ).await?;
        Ok::<(Option<String>, bool), SessionError>((ice_candidate_pair, local_candidates_gathered))
    }.await;
    let (ice_candidate_pair, local_candidates_gathered) = match setup_result {
        Ok(result) => result,
        Err(error) => {
            peer.close().await;
            return Err(error);
        }
    };
    let peer = Arc::new(peer);
    let channels = peer.api_channels();
    log::info!("[rtc] Terminal DTLS identity binding verified room={room_text}");
    Ok(EstablishedRtcSession {
        peer,
        channels,
        signaling: None,
        room_id,
        request_id,
        ice_candidate_pair,
        ice_servers_configured,
        rendezvous_stun_received,
        turn_credentials_received,
        local_candidates_gathered,
        turn_refresh_at: discovery.turn_credentials.map(|credentials| credentials.refresh_at),
    })
}

async fn close_terminal_room(socket: &mut SignalSocket, request_id: Uuid, room_id: Uuid) -> Result<(), SessionError> {
    let room_text = room_id.to_string();
    timeout(Duration::from_secs(5), async {
        socket.send(&SignalMessage::RoomClose {
            protocol: SIGNAL_PROTOCOL,
            request_id,
            room_id: room_text.clone(),
        }).await?;
        loop {
            match socket.receive().await? {
                SignalMessage::RoomClose { request_id: response_id, room_id: response_room, .. }
                    if response_id == request_id && response_room == room_text => return Ok(()),
                SignalMessage::RoomClose { room_id: closed_room, .. } if closed_room == room_text => continue,
                SignalMessage::Error { request_id: response_id, code, .. }
                    if response_id == request_id && matches!(code.as_str(), "room_not_found" | "room_expired") => return Ok(()),
                SignalMessage::Error { request_id: response_id, code, message, .. } if response_id == request_id => {
                    return Err(SessionError::Rendezvous(format!("{code}: {message}")));
                }
                _ => continue,
            }
        }
    }).await.map_err(|_| SessionError::Timeout("closing the Terminal signaling room"))?
}

fn keep_terminal_signaling_alive(mut socket: SignalSocket, room_id: Uuid) -> SignalingLease {
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
    let (turn_request_tx, mut turn_request_rx) = mpsc::channel::<TurnRefreshRequest>(2);
    let task = tokio::spawn(async move {
        let expected_room = room_id.to_string();
        log::debug!("[rtc] Terminal signaling lease active room={expected_room}");
        let mut pending_turn_requests: HashMap<Uuid, oneshot::Sender<Result<SignalMessage, String>>> = HashMap::new();
        loop {
            pending_turn_requests.retain(|_, responder| !responder.is_closed());
            tokio::select! {
                _ = &mut cancel_rx => { log::info!("[rtc] Terminal signaling lease closed by local cancellation room={expected_room}"); break },
                request = turn_request_rx.recv() => match request {
                    Some(request) => {
                        let message = SignalMessage::TurnCredentialsRequest {
                            protocol: SIGNAL_PROTOCOL,
                            request_id: request.request_id,
                            room_id: expected_room.clone(),
                        };
                        if let Err(error) = socket.send(&message).await {
                            let _ = request.responder.send(Err(error.to_string()));
                            log::warn!("[rtc] Terminal could not request renewed TURN credentials room={expected_room}: {error}");
                            break;
                        }
                        pending_turn_requests.insert(request.request_id, request.responder);
                    }
                    None => break,
                },
                message = socket.receive() => match message {
                    Ok(message @ SignalMessage::TurnCredentialsResult { request_id, .. }) => {
                        if let Some(responder) = pending_turn_requests.remove(&request_id) {
                            let _ = responder.send(Ok(message));
                        }
                    }
                    Ok(SignalMessage::Error { request_id, code, .. })
                        if pending_turn_requests.contains_key(&request_id) && code == "unsupported_version" => {
                            let legacy = SignalMessage::TurnCredentialsRequest {
                                protocol: SIGNAL_PROTOCOL,
                                request_id,
                                room_id: expected_room.clone(),
                            };
                            if let Err(error) = socket.send(&legacy).await {
                                if let Some(responder) = pending_turn_requests.remove(&request_id) {
                                    let _ = responder.send(Err(error.to_string()));
                                }
                            }
                        }
                    Ok(message @ SignalMessage::Error { request_id, .. }) if pending_turn_requests.contains_key(&request_id) => {
                        if let Some(responder) = pending_turn_requests.remove(&request_id) {
                            let _ = responder.send(Ok(message));
                        }
                    }
                    Ok(SignalMessage::RoomClose { room_id, .. }) if room_id == expected_room => { log::warn!("[rtc] Terminal received rendezvous room close room={expected_room}"); break },
                    Ok(SignalMessage::Error { code, .. }) => { log::warn!("[rtc] Terminal signaling lease ended by rendezvous error room={expected_room} code={code}"); break },
                    Ok(_) => continue,
                    Err(SignalError::Timeout(_)) => continue,
                    Err(error) => { log::warn!("[rtc] Terminal signaling lease ended room={expected_room}: {error}"); break },
                }
            }
        }
        for (_, responder) in pending_turn_requests {
            let _ = responder.send(Err("signaling lease ended".to_owned()));
        }
        socket.close().await;
    });
    SignalingLease { cancel: Some(cancel_tx), task: Some(task), turn_requests: turn_request_tx, room_id }
}

/// Closes the current room and drains the matching rendezvous response from the Host's persistent socket.
/// A Terminal may already have closed the room as it disconnected; that is also a successful close.
pub async fn close_host_room(
    signaling: &mut HostSignaling,
    request_id: Uuid,
    room_id: Uuid,
) -> Result<(), SessionError> {
    let room_text = room_id.to_string();
    timeout(Duration::from_secs(5), async {
        signaling.socket.send(&SignalMessage::RoomClose {
            protocol: SIGNAL_PROTOCOL,
            request_id,
            room_id: room_text.clone(),
        }).await?;
        loop {
            match signaling.receive_matching(|message| match message {
                SignalMessage::RoomClose { room_id: response_room, .. } => response_room == &room_text,
                SignalMessage::Error { request_id: response_id, .. } => *response_id == request_id,
                _ => false,
            }).await? {
                SignalMessage::RoomClose { request_id: response_id, room_id: response_room, .. }
                    if response_id == request_id && response_room == room_text => return Ok(()),
                SignalMessage::RoomClose { room_id: notified_room, .. } if notified_room == room_text => continue,
                SignalMessage::Error { request_id: response_id, code, .. }
                    if response_id == request_id && matches!(code.as_str(), "room_not_found" | "room_expired") => return Ok(()),
                SignalMessage::Error { request_id: response_id, code, message, .. } if response_id == request_id => {
                    return Err(SessionError::Rendezvous(format!("{code}: {message}")));
                }
                unexpected => return Err(SessionError::UnexpectedSignal(signal_kind(&unexpected))),
            }
        }
    }).await.map_err(|_| SessionError::Timeout("closing the signaling room"))?
}

pub async fn request_ice_server_discovery(
    socket: &mut SignalSocket,
    room_text: &str,
) -> Result<IceServerDiscovery, SessionError> {
    let request_id = Uuid::new_v4();
    log::info!("[rtc] requesting Rendezvous ICE discovery room={room_text}");
    socket.send(&SignalMessage::TurnCredentialsRequest {
        protocol: ICE_DISCOVERY_PROTOCOL,
        request_id,
        room_id: room_text.to_owned(),
    }).await?;
    let response = socket.receive().await?;
    let response = if matches!(&response, SignalMessage::Error { request_id: response_id, code, .. }
        if *response_id == request_id && code == "unsupported_version") {
        log::debug!("[rtc] Rendezvous does not support extended ICE discovery; retrying TURN credentials with protocol v1");
        socket.send(&SignalMessage::TurnCredentialsRequest {
            protocol: SIGNAL_PROTOCOL,
            request_id,
            room_id: room_text.to_owned(),
        }).await?;
        socket.receive().await?
    } else {
        response
    };
    parse_turn_credentials(response, request_id, room_text)
}

/// Retains the existing TURN refresh API while the initial negotiation also receives STUN recommendations.
pub async fn request_turn_credentials(
    signaling: &mut HostSignaling,
    room_text: &str,
) -> Result<Option<TurnCredentials>, SessionError> {
    let request_id = Uuid::new_v4();
    log::info!("[rtc] requesting TURN credential renewal room={room_text}");
    signaling.socket.send(&SignalMessage::TurnCredentialsRequest {
        protocol: SIGNAL_PROTOCOL,
        request_id,
        room_id: room_text.to_owned(),
    }).await?;
    let response = signaling.receive_matching(|message| {
        is_turn_response_for(message, request_id)
            || matches!(message, SignalMessage::RoomClose { room_id, .. } if room_id == room_text)
    }).await?;
    match response {
        SignalMessage::RoomClose { .. } => Err(SessionError::Closed),
        SignalMessage::Error { request_id: response_id, code, .. }
            if response_id == request_id && matches!(code.as_str(), "room_not_found" | "room_expired") => {
                Err(SessionError::Closed)
            }
        response => parse_turn_credentials(response, request_id, room_text)
            .map(|discovery| discovery.turn_credentials),
    }
}

fn is_turn_response_for(message: &SignalMessage, request_id: Uuid) -> bool {
    matches!(message,
        SignalMessage::TurnCredentialsResult { request_id: response_id, .. }
        | SignalMessage::Error { request_id: response_id, .. }
        if *response_id == request_id
    )
}

fn is_room_signal_for(message: &SignalMessage, request_id: Uuid, room_text: &str) -> bool {
    match message {
        SignalMessage::SignalOffer { request_id: response_id, room_id, .. }
        | SignalMessage::SignalCandidate { request_id: response_id, room_id, .. }
        | SignalMessage::SignalEndOfCandidates { request_id: response_id, room_id, .. } => {
            *response_id == request_id && room_id == room_text
        }
        SignalMessage::RoomClose { room_id, .. } => room_id == room_text,
        _ => false,
    }
}

fn should_defer_host_message(message: &SignalMessage) -> bool {
    matches!(
        message,
        SignalMessage::JoinAccepted { .. }
            | SignalMessage::SignalOffer { .. }
            | SignalMessage::SignalCandidate { .. }
            | SignalMessage::SignalEndOfCandidates { .. }
            | SignalMessage::RoomClose { .. }
    )
}

fn parse_turn_credentials(
    response: SignalMessage,
    request_id: Uuid,
    room_text: &str,
) -> Result<IceServerDiscovery, SessionError> {
    match response {
        SignalMessage::TurnCredentialsResult {
            request_id: response_id,
            room_id: response_room,
            protocol,
            turn_uris,
            stun_uris,
            username,
            credential,
            expires_at,
            ..
        } if response_id == request_id && response_room == room_text => {
            let stun_uris = if protocol == ICE_DISCOVERY_PROTOCOL { stun_uris } else { Vec::new() };
            if turn_uris.is_empty() {
                if !username.is_empty() || !credential.is_empty() || expires_at != 0 {
                    return Err(SessionError::Rendezvous("rendezvous returned incomplete TURN credentials".into()));
                }
                log::info!("[rtc] Rendezvous ICE discovery returned no TURN credentials; recommended_stun_uri_count={}", stun_uris.len());
                return Ok(IceServerDiscovery { stun_uris, turn_credentials: None });
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs();
            let Some(remaining_seconds) = remaining_turn_credential_lifetime(expires_at, now) else {
                log::error!("[rtc] rendezvous returned expired TURN credentials room={room_text} expires_at={expires_at} now={now}");
                return Err(SessionError::Rendezvous("rendezvous returned expired or incomplete TURN credentials".into()));
            };
            if username.is_empty() || credential.is_empty() {
                log::error!("[rtc] rendezvous returned incomplete TURN credentials room={room_text}");
                return Err(SessionError::Rendezvous("rendezvous returned expired or incomplete TURN credentials".into()));
            }
            let Some(refresh_after) = turn_credential_refresh_delay(remaining_seconds) else {
                log::error!("[rtc] rendezvous TURN credentials expire too soon for a new ICE attempt room={room_text} valid_for_seconds={remaining_seconds}");
                return Err(SessionError::Rendezvous("rendezvous TURN credentials expire too soon for ICE negotiation".into()));
            };
            log::info!("[rtc] fresh TURN credentials received room={room_text} uri_count={} expires_at={expires_at} valid_for_seconds={remaining_seconds}", turn_uris.len());
            Ok(IceServerDiscovery {
                stun_uris,
                turn_credentials: Some(TurnCredentials {
                    server: IceServerConfig { urls: turn_uris, username: Some(username), credential: Some(credential) },
                    refresh_at: tokio::time::Instant::now() + refresh_after,
                }),
            })
        }
        SignalMessage::Error { request_id: response_id, code, .. }
            if response_id == request_id && code == "turn_unconfigured" => {
                log::info!("[rtc] rendezvous has no TURN service configured for room={room_text}");
                Ok(IceServerDiscovery::default())
            }
        SignalMessage::Error { request_id: response_id, code, message, .. } if response_id == request_id => {
            Err(SessionError::Rendezvous(format!("{code}: {message}")))
        }
        unexpected => Err(SessionError::UnexpectedSignal(signal_kind(&unexpected))),
    }
}

fn remaining_turn_credential_lifetime(expires_at: u64, now: u64) -> Option<u64> {
    expires_at.checked_sub(now).filter(|remaining| *remaining > 0)
}

fn turn_credential_refresh_delay(remaining_seconds: u64) -> Option<Duration> {
    const MIN_REMAINING_SECONDS: u64 = 180;
    if remaining_seconds < MIN_REMAINING_SECONDS {
        return None;
    }
    let refresh_lead_seconds = 240.min(90.max(remaining_seconds / 3));
    Some(Duration::from_secs(remaining_seconds - refresh_lead_seconds))
}

fn merge_discovery_ice_servers(
    configured: &[IceServerConfig],
    discovery: &IceServerDiscovery,
) -> Vec<IceServerConfig> {
    let mut merged: Vec<IceServerConfig> = Vec::new();
    let mut add = |server: &IceServerConfig| {
        let has_credentials = server.username.as_deref().is_some_and(|value| !value.is_empty())
            && server.credential.as_deref().is_some_and(|value| !value.is_empty());
        for url in &server.urls {
            let canonical = normalize_ice_server_uri(url)
                .unwrap_or_else(|_| url.trim().to_ascii_lowercase());
            let existing = merged.iter().position(|entry| {
                entry.urls.first().is_some_and(|existing_url| {
                    normalize_ice_server_uri(existing_url)
                        .unwrap_or_else(|_| existing_url.trim().to_ascii_lowercase()) == canonical
                })
            });
            let single = IceServerConfig {
                urls: vec![url.clone()],
                username: server.username.clone(),
                credential: server.credential.clone(),
            };
            match existing {
                Some(index) => {
                    let old = &merged[index];
                    let old_has_credentials = old.username.as_deref().is_some_and(|value| !value.is_empty())
                        && old.credential.as_deref().is_some_and(|value| !value.is_empty());
                    if has_credentials && !old_has_credentials {
                        merged[index] = single;
                    }
                }
                None => merged.push(single),
            }
        }
    };
    for server in configured {
        add(server);
    }
    for url in &discovery.stun_uris {
        add(&IceServerConfig { urls: vec![url.clone()], username: None, credential: None });
    }
    if let Some(credentials) = &discovery.turn_credentials {
        add(&credentials.server);
    }
    merged
}

#[cfg(test)]
mod turn_credential_tests {
    use super::{
        IceServerDiscovery, TurnCredentials, merge_discovery_ice_servers,
        remaining_turn_credential_lifetime, turn_credential_refresh_delay,
    };
    use crate::IceServerConfig;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    #[test]
    fn new_ice_attempt_rejects_expired_turn_credentials() {
        assert_eq!(remaining_turn_credential_lifetime(999, 1_000), None);
        assert_eq!(remaining_turn_credential_lifetime(1_000, 1_000), None);
    }

    #[test]
    fn new_ice_attempt_accepts_fresh_turn_credentials_and_reports_remaining_ttl() {
        assert_eq!(remaining_turn_credential_lifetime(1_600, 1_000), Some(600));
    }

    #[test]
    fn refresh_is_scheduled_before_credentials_expire() {
        assert_eq!(turn_credential_refresh_delay(600), Some(std::time::Duration::from_secs(400)));
        assert_eq!(turn_credential_refresh_delay(180), Some(std::time::Duration::from_secs(90)));
        assert_eq!(turn_credential_refresh_delay(179), None);
    }

    #[test]
    fn rendezvous_and_custom_ice_servers_merge_without_duplicate_uris_and_keep_credentials() {
        let configured = [
            IceServerConfig { urls: vec!["STUN:stun.example.test".to_owned()], username: None, credential: None },
            IceServerConfig { urls: vec!["turn:relay.example.test?transport=udp".to_owned()], username: None, credential: None },
        ];
        let discovery = IceServerDiscovery {
            stun_uris: vec![
                "stun:stun.example.test:3478".to_owned(),
                "stun:[2001:db8::1]:3478".to_owned(),
            ],
            turn_credentials: Some(TurnCredentials {
                server: IceServerConfig {
                    urls: vec!["turn:relay.example.test:3478?transport=udp".to_owned()],
                    username: Some("temporary-user".to_owned()),
                    credential: Some("temporary-password".to_owned()),
                },
                refresh_at: tokio::time::Instant::now(),
            }),
        };

        let merged = merge_discovery_ice_servers(&configured, &discovery);

        assert_eq!(merged.len(), 3);
        let relay = merged.iter().find(|server| server.urls[0].starts_with("turn:")).unwrap();
        assert_eq!(relay.username.as_deref(), Some("temporary-user"));
        assert_eq!(relay.credential.as_deref(), Some("temporary-password"));
        assert!(merged.iter().any(|server| server.urls[0] == "stun:[2001:db8::1]:3478"));
    }

    #[tokio::test]
    async fn extended_discovery_falls_back_to_legacy_turn_credentials_response() {
        use super::request_ice_server_discovery;
        use crate::SignalSocket;
        use shocking_vrc_rendezvous::protocol::{ICE_DISCOVERY_PROTOCOL, SIGNAL_PROTOCOL, SignalMessage};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let rendezvous = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let first: SignalMessage = loop {
                if let Some(Ok(Message::Text(text))) = socket.next().await {
                    break serde_json::from_str(&text).unwrap();
                }
            };
            let request_id = first.request_id();
            assert!(matches!(first, SignalMessage::TurnCredentialsRequest { protocol: ICE_DISCOVERY_PROTOCOL, .. }));
            let unsupported = SignalMessage::Error {
                protocol: SIGNAL_PROTOCOL,
                request_id,
                code: "unsupported_version".to_owned(),
                message: "protocol v2 is not supported".to_owned(),
            };
            socket.send(Message::Text(serde_json::to_string(&unsupported).unwrap().into())).await.unwrap();

            let fallback: SignalMessage = loop {
                if let Some(Ok(Message::Text(text))) = socket.next().await {
                    break serde_json::from_str(&text).unwrap();
                }
            };
            assert!(matches!(fallback, SignalMessage::TurnCredentialsRequest { protocol: SIGNAL_PROTOCOL, .. }));
            let legacy = SignalMessage::TurnCredentialsResult {
                protocol: SIGNAL_PROTOCOL,
                request_id,
                room_id: "legacy-room".to_owned(),
                turn_uris: Vec::new(),
                stun_uris: Vec::new(),
                username: String::new(),
                credential: String::new(),
                expires_at: 0,
            };
            socket.send(Message::Text(serde_json::to_string(&legacy).unwrap().into())).await.unwrap();
        });

        let mut socket = SignalSocket::connect(&format!("ws://{address}/ws")).await.unwrap();
        let discovery = request_ice_server_discovery(&mut socket, "legacy-room").await.unwrap();
        assert!(discovery.stun_uris.is_empty());
        assert!(discovery.turn_credentials.is_none());
        socket.close().await;
        rendezvous.await.unwrap();
    }
}

#[cfg(test)]
mod host_room_close_queue_tests {
    use std::collections::VecDeque;
    use super::{HostSignaling, RegisteredHost, close_host_room};
    use base64::Engine;
    use crate::SignalSocket;
    use futures_util::{SinkExt, StreamExt};
    use shocking_vrc_rendezvous::protocol::{SIGNAL_PROTOCOL, SignalMessage};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};
    use uuid::Uuid;

    #[tokio::test]
    async fn permanent_room_not_found_renewal_is_reported_as_a_closed_room() {
        use super::{request_turn_credentials, SessionError};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let room_id = Uuid::new_v4();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let request: SignalMessage = loop {
                if let Some(Ok(Message::Text(text))) = socket.next().await {
                    break serde_json::from_str(&text).unwrap();
                }
            };
            let request_id = match request {
                SignalMessage::TurnCredentialsRequest { request_id, room_id: requested_room, .. } => {
                    assert_eq!(requested_room, room_id.to_string());
                    request_id
                }
                other => panic!("expected TURN renewal request, received {other:?}"),
            };
            let missing = SignalMessage::Error {
                protocol: SIGNAL_PROTOCOL,
                request_id,
                code: "room_not_found".to_owned(),
                message: "room no longer exists".to_owned(),
            };
            socket.send(Message::Text(serde_json::to_string(&missing).unwrap().into())).await.unwrap();
        });

        let mut signaling = HostSignaling {
            socket: SignalSocket::connect(&format!("ws://{address}/ws")).await.unwrap(),
            registration: RegisteredHost {
                host_id: "host-local".to_owned(),
                registration_epoch: Uuid::new_v4(),
                server_epoch: Uuid::new_v4(),
            },
            pending_messages: VecDeque::new(),
        };

        let result = request_turn_credentials(&mut signaling, &room_id.to_string()).await;

        assert!(matches!(result, Err(SessionError::Closed)), "dead-room renewal must stop the active room");
        signaling.socket.close().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn transient_turn_renewal_error_does_not_mark_room_closed() {
        use super::{request_turn_credentials, SessionError};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let room_id = Uuid::new_v4();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let request: SignalMessage = loop {
                if let Some(Ok(Message::Text(text))) = socket.next().await {
                    break serde_json::from_str(&text).unwrap();
                }
            };
            let request_id = match request {
                SignalMessage::TurnCredentialsRequest { request_id, room_id: requested_room, .. } => {
                    assert_eq!(requested_room, room_id.to_string());
                    request_id
                }
                other => panic!("expected TURN renewal request, received {other:?}"),
            };
            let transient = SignalMessage::Error {
                protocol: SIGNAL_PROTOCOL,
                request_id,
                code: "temporarily_unavailable".to_owned(),
                message: "temporary test failure".to_owned(),
            };
            socket.send(Message::Text(serde_json::to_string(&transient).unwrap().into())).await.unwrap();
        });

        let mut signaling = HostSignaling {
            socket: SignalSocket::connect(&format!("ws://{address}/ws")).await.unwrap(),
            registration: RegisteredHost {
                host_id: "host-local".to_owned(),
                registration_epoch: Uuid::new_v4(),
                server_epoch: Uuid::new_v4(),
            },
            pending_messages: VecDeque::new(),
        };

        let result = request_turn_credentials(&mut signaling, &room_id.to_string()).await;

        assert!(matches!(result, Err(SessionError::Rendezvous(_))), "a transient renewal error must leave the room active");
        signaling.socket.close().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn active_room_close_is_room_scoped_and_preserves_the_next_join_and_offer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let closing_room = Uuid::new_v4();
        let next_request = Uuid::new_v4();
        let next_room = Uuid::new_v4();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            for message in [
                SignalMessage::RoomClose {
                    protocol: SIGNAL_PROTOCOL,
                    request_id: Uuid::new_v4(),
                    room_id: closing_room.to_string(),
                },
                SignalMessage::JoinAccepted {
                    protocol: SIGNAL_PROTOCOL,
                    request_id: next_request,
                    host_id: "host-local".to_owned(),
                    room_id: next_room.to_string(),
                    nonce: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([8_u8; 32]),
                    force_relay: false,
                },
                SignalMessage::SignalOffer {
                    protocol: SIGNAL_PROTOCOL,
                    request_id: next_request,
                    room_id: next_room.to_string(),
                    sdp: "next-room offer".to_owned(),
                },
            ] {
                socket.send(Message::Text(serde_json::to_string(&message).unwrap().into())).await.unwrap();
            }
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) { break; }
            }
        });

        let mut signaling = HostSignaling {
            socket: SignalSocket::connect(&format!("ws://{address}/ws")).await.unwrap(),
            registration: RegisteredHost {
                host_id: "host-local".to_owned(),
                registration_epoch: Uuid::new_v4(),
                server_epoch: Uuid::new_v4(),
            },
            pending_messages: VecDeque::new(),
        };

        signaling.wait_for_room_close(closing_room).await.unwrap();
        let join = signaling.receive_matching(|message| matches!(message, SignalMessage::JoinAccepted { .. })).await.unwrap();
        assert!(matches!(join, SignalMessage::JoinAccepted { request_id, room_id, .. }
            if request_id == next_request && room_id == next_room.to_string()));
        let offer = signaling.receive_matching(|message| matches!(message, SignalMessage::SignalOffer { .. })).await.unwrap();
        assert!(matches!(offer, SignalMessage::SignalOffer { request_id, room_id, sdp, .. }
            if request_id == next_request && room_id == next_room.to_string() && sdp == "next-room offer"));

        signaling.socket.close().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn closing_a_room_does_not_consume_the_next_join_or_offer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let close_request = Uuid::new_v4();
        let closing_room = Uuid::new_v4();
        let next_request = Uuid::new_v4();
        let next_room = Uuid::new_v4();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let request: SignalMessage = loop {
                if let Some(Ok(Message::Text(text))) = socket.next().await {
                    break serde_json::from_str(&text).unwrap();
                }
            };
            assert!(matches!(request, SignalMessage::RoomClose { request_id, room_id, .. }
                if request_id == close_request && room_id == closing_room.to_string()));

            for message in [
                SignalMessage::JoinAccepted {
                    protocol: SIGNAL_PROTOCOL,
                    request_id: next_request,
                    host_id: "host-local".to_owned(),
                    room_id: next_room.to_string(),
                    nonce: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7_u8; 32]),
                    force_relay: false,
                },
                SignalMessage::SignalOffer {
                    protocol: SIGNAL_PROTOCOL,
                    request_id: next_request,
                    room_id: next_room.to_string(),
                    sdp: "early offer".to_owned(),
                },
                SignalMessage::RoomClose {
                    protocol: SIGNAL_PROTOCOL,
                    request_id: close_request,
                    room_id: closing_room.to_string(),
                },
            ] {
                socket.send(Message::Text(serde_json::to_string(&message).unwrap().into())).await.unwrap();
            }
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) { break; }
            }
        });

        let mut signaling = HostSignaling {
            socket: SignalSocket::connect(&format!("ws://{address}/ws")).await.unwrap(),
            registration: RegisteredHost {
                host_id: "host-local".to_owned(),
                registration_epoch: Uuid::new_v4(),
                server_epoch: Uuid::new_v4(),
            },
            pending_messages: VecDeque::new(),
        };
        close_host_room(&mut signaling, close_request, closing_room).await.unwrap();

        let next_message = tokio::time::timeout(std::time::Duration::from_millis(100), signaling.receive_matching(|_| true))
            .await
            .expect("the Host must retain the next room message")
            .unwrap();
        assert!(matches!(next_message, SignalMessage::JoinAccepted { request_id, room_id, .. }
            if request_id == next_request && room_id == next_room.to_string()));
        let early_offer = signaling.receive_matching(|_| true).await.unwrap();
        assert!(matches!(early_offer, SignalMessage::SignalOffer { request_id, room_id, .. }
            if request_id == next_request && room_id == next_room.to_string()));

        signaling.socket.close().await;
        server.await.unwrap();
    }
}

async fn prove_host_identity(
    peer: &RtcPeer, signer: &dyn HostIdentitySigner, registration: &RegisteredHost,
    request_id: Uuid, room_id: Uuid, room_nonce: [u8; 32], fingerprints: DtlsFingerprints,
) -> Result<(), SessionError> {
    let challenge_frame = timeout(Duration::from_secs(10), peer.recv_json()).await
        .map_err(|_| SessionError::Timeout("waiting for the Terminal identity challenge"))??;
    let challenge: Value = serde_json::from_slice(&challenge_frame).map_err(|e| SessionError::Identity(e.to_string()))?;
    if challenge.get("type").and_then(Value::as_str) != Some("rtc.identity.challenge")
        || challenge.get("protocol").and_then(Value::as_u64) != Some(1)
        || challenge.get("request_id").and_then(Value::as_str) != Some(request_id.to_string().as_str())
        || challenge.get("room_id").and_then(Value::as_str) != Some(room_id.to_string().as_str())
    { return Err(SessionError::Correlation); }
    let client_nonce = decode_nonce(challenge.get("client_nonce").and_then(Value::as_str).ok_or(SessionError::Correlation)?)?;
    let resume_session_id = challenge.get("resume_session_id").and_then(Value::as_str).map(parse_uuid).transpose()?;
    let mut server_nonce = [0_u8; 32];
    SystemRandom::new().fill(&mut server_nonce).map_err(|_| SessionError::Identity("cryptographic random generator unavailable".into()))?;
    let fields = BindingFields {
        host_id: registration.host_id.clone(), server_epoch: registration.server_epoch,
        registration_epoch: registration.registration_epoch, request_id, room_id, room_nonce,
        client_nonce, server_nonce, host_dtls_fingerprint: fingerprints.local,
        client_dtls_fingerprint: fingerprints.remote, resume_session_id,
    };
    let signature = signer.sign(&binding_transcript(&fields)?).map_err(SessionError::Identity)?;
    let proof = IdentityProof::new(fields, signer.public_key().to_vec(), signature);
    peer.send_json(&serde_json::to_vec(&proof).map_err(|e| SessionError::Identity(e.to_string()))?).await?;
    let response_frame = timeout(Duration::from_secs(10), peer.recv_json()).await
        .map_err(|_| SessionError::Timeout("waiting for the Terminal identity acknowledgement"))??;
    let response: Value = serde_json::from_slice(&response_frame).map_err(|e| SessionError::Identity(e.to_string()))?;
    if response.get("type").and_then(Value::as_str) != Some("rtc.identity.verified")
        || response.get("request_id").and_then(Value::as_str) != Some(request_id.to_string().as_str())
        || response.get("room_id").and_then(Value::as_str) != Some(room_id.to_string().as_str())
    { return Err(SessionError::Identity("Terminal did not acknowledge the pinned Host identity".into())); }
    log::info!("[rtc] Host DTLS peer identity acknowledgement verified");
    Ok(())
}

async fn verify_terminal_identity(
    peer: &RtcPeer, pin: &str, host_id: &str, request_id: Uuid, room_id: Uuid,
    room_nonce: [u8; 32], resume_session_id: Option<Uuid>, fingerprints: DtlsFingerprints,
) -> Result<(), SessionError> {
    let mut client_nonce = [0_u8; 32];
    SystemRandom::new().fill(&mut client_nonce).map_err(|_| SessionError::Identity("cryptographic random generator unavailable".into()))?;
    let challenge = serde_json::json!({
        "type":"rtc.identity.challenge", "protocol":1, "request_id":request_id,
        "room_id":room_id, "client_nonce":URL_SAFE_NO_PAD.encode(client_nonce), "resume_session_id":resume_session_id,
    });
    peer.send_json(&serde_json::to_vec(&challenge).map_err(|e| SessionError::Identity(e.to_string()))?).await?;
    let proof_frame = timeout(Duration::from_secs(10), peer.recv_json()).await
        .map_err(|_| SessionError::Timeout("waiting for the signed Host identity proof"))??;
    let proof: IdentityProof = serde_json::from_slice(&proof_frame).map_err(|e| SessionError::Identity(e.to_string()))?;
    if proof.fields.host_id != host_id || proof.fields.request_id != request_id || proof.fields.room_id != room_id
        || proof.fields.room_nonce != room_nonce || proof.fields.client_nonce != client_nonce
        || proof.fields.resume_session_id != resume_session_id
        || proof.fields.host_dtls_fingerprint != fingerprints.remote
        || proof.fields.client_dtls_fingerprint != fingerprints.local
    { return Err(SessionError::Identity("signed identity proof did not match this room and negotiated DTLS peers".into())); }
    verify_binding(&proof, &proof.fields, pin)?;
    log::info!("[rtc] pinned Host DTLS identity proof verified");
    let verified = serde_json::json!({"type":"rtc.identity.verified","protocol":1,"request_id":request_id,"room_id":room_id});
    peer.send_json(&serde_json::to_vec(&verified).map_err(|e| SessionError::Identity(e.to_string()))?).await?;
    Ok(())
}

fn decode_nonce(value: &str) -> Result<[u8; 32], SessionError> {
    URL_SAFE_NO_PAD.decode(value).map_err(|_| SessionError::Correlation)?
        .try_into().map_err(|_| SessionError::Correlation)
}

fn signal_kind(message: &SignalMessage) -> &'static str {
    match message {
        SignalMessage::Register { .. } => "register",
        SignalMessage::Registered { .. } => "registered",
        SignalMessage::Join { .. } => "join",
        SignalMessage::JoinAccepted { .. } => "join.accepted",
        SignalMessage::SignalOffer { .. } => "signal.offer",
        SignalMessage::SignalAnswer { .. } => "signal.answer",
        SignalMessage::SignalCandidate { .. } => "signal.candidate",
        SignalMessage::SignalEndOfCandidates { .. } => "signal.end_of_candidates",
        SignalMessage::TurnCredentialsRequest { .. } => "turn.credentials.request",
        SignalMessage::TurnCredentialsResult { .. } => "turn.credentials.result",
        SignalMessage::RoomClose { .. } => "room.close",
        SignalMessage::Error { .. } => "error",
    }
}

fn parse_uuid(value: &str) -> Result<Uuid, SessionError> { Uuid::parse_str(value).map_err(|_| SessionError::Correlation) }

pub fn ephemeral_identity() -> Result<(String, String, ring::signature::Ed25519KeyPair), SessionError> {
    use ring::signature::{Ed25519KeyPair, KeyPair};
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|e| SessionError::Identity(e.to_string()))?;
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|e| SessionError::Identity(e.to_string()))?;
    let public = key.public_key().as_ref();
    Ok((server_id(public), identity_fingerprint(public), key))
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)] Signal(#[from] SignalError),
    #[error(transparent)] Peer(#[from] RtcError),
    #[error(transparent)] Binding(#[from] BindingError),
    #[error("rendezvous rejected the room: {0}")] Rendezvous(String),
    #[error("signaling request/room correlation did not match")] Correlation,
    #[error("unexpected rendezvous message during negotiation: {0}")] UnexpectedSignal(&'static str),
    #[error("room closed during negotiation")] Closed,
    #[error("Host room negotiation failed and the room was closed: {0}")] RoomSetup(String),
    #[error("Host room negotiation failed and cleanup could not be confirmed: {0}")] RoomCleanup(String),
    #[error("timed out while {0}")] Timeout(&'static str),
    #[error("Host signaling inbox exceeded its bounded capacity")] SignalingQueueFull,
    #[error("Host identity proof failed: {0}")] Identity(String),
}

impl SessionError {
    /// A room-local negotiation failure is safe to retry on the current Host registration.
    pub fn is_room_local(&self) -> bool {
        matches!(self, Self::RoomSetup(_) | Self::Closed)
    }
}

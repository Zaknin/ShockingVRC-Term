//! Room negotiation and ADR-033 DTLS/Host identity binding.

use std::{sync::Arc, time::Duration};
use std::collections::HashMap;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;
use tokio::time::timeout;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use zeroize::Zeroizing;

use shocking_vrc_rendezvous::protocol::{SIGNAL_PROTOCOL, SignalMessage};

use crate::{BindingError, BindingFields, DtlsFingerprints, IdentityProof, IceServerConfig, RtcApiChannels, RtcError, RtcPeer, SignalError, SignalSocket, binding_transcript, identity_fingerprint, join_message, register_message, server_id, verify_binding};

const ROOM_TIMEOUT: Duration = Duration::from_secs(70);

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
}

pub struct EstablishedRtcSession {
    pub peer: Arc<RtcPeer>,
    pub channels: RtcApiChannels,
    pub signaling: Option<SignalingLease>,
    pub room_id: Uuid,
    pub request_id: Uuid,
    pub ice_candidate_pair: Option<String>,
    pub turn_refresh_at: Option<tokio::time::Instant>,
}

#[derive(Clone)]
pub struct TurnCredentials {
    pub server: IceServerConfig,
    pub refresh_at: tokio::time::Instant,
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
        if let Some(task) = self.task.take() { let _ = task.await; }
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
    }
}

impl Drop for SignalingLease {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() { let _ = cancel.send(()); }
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
    let (request_id, room_id, room_nonce) = loop {
        // A registered Host may wait indefinitely for the Terminal. The underlying
        // signaling receive handles rendezvous Ping/Pong and still times out if the
        // WebSocket goes quiet or disconnects; applying ROOM_TIMEOUT here closed a
        // healthy registration after 70 seconds even while heartbeats were arriving.
        match signaling.socket.receive().await? {
            SignalMessage::JoinAccepted { request_id, host_id, room_id, nonce, .. }
                if host_id == signaling.registration.host_id => break (request_id, parse_uuid(&room_id)?, decode_nonce(&nonce)?),
            SignalMessage::RoomClose { .. } => return Err(SessionError::Closed),
            SignalMessage::Error { code, message, .. } => return Err(SessionError::Rendezvous(format!("{code}: {message}"))),
            unexpected => return Err(SessionError::UnexpectedSignal(signal_kind(&unexpected))),
        }
    };
    let room_text = room_id.to_string();
    log::info!("[rtc] Host accepted signaling room room={room_text} request={request_id}; preparing ICE");
    let mut peer_ice_servers = ice_servers.to_vec();
    let turn_credentials = request_turn_credentials(&mut signaling.socket, &room_text).await?;
    if let Some(credentials) = &turn_credentials {
        peer_ice_servers.push(credentials.server.clone());
    }
    log::info!("[rtc] Host waiting for Terminal ICE offer room={room_text}");
    let offer_sdp = loop {
        match timeout(ROOM_TIMEOUT, signaling.socket.receive()).await
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
    };
    log::info!("[rtc] Host received ICE offer room={room_text}");
    let mut peer = RtcPeer::new(&peer_ice_servers, udp_addrs, false).await?;
    let answer_started = std::time::Instant::now();
    let answer_sdp = match peer.accept_offer(&offer_sdp).await {
        Ok(answer) => answer,
        Err(error) => {
            log::error!("[rtc] Host failed to prepare ICE answer room={room_text} elapsed_ms={} error={error}", answer_started.elapsed().as_millis());
            return Err(error.into());
        }
    };
    log::info!("[rtc] Host ICE gathering completed; sending answer room={room_text} elapsed_ms={}", answer_started.elapsed().as_millis());
    if let Err(error) = signaling.socket.send(&SignalMessage::SignalAnswer {
        protocol: SIGNAL_PROTOCOL, request_id, room_id: room_text.clone(), sdp: answer_sdp,
    }).await {
        log::error!("[rtc] Host failed to send ICE answer to rendezvous room={room_text}: {error}");
        return Err(error.into());
    }
    log::info!("[rtc] Host ICE answer sent to rendezvous room={room_text}");
    peer.wait_data_channel().await?;
    log::info!("[rtc] Host received Terminal DataChannel room={room_text}");
    peer.wait_connected().await?;
    peer.wait_open().await?;
    log::info!("[rtc] Host ICE/DTLS transport connected room={room_text}");
    log::info!("[rtc] Host verifying DTLS identity binding room={room_text}");
    prove_host_identity(&peer, signer, &signaling.registration, request_id, room_id, room_nonce, peer.fingerprints().await?).await?;
    log::info!("[rtc] Host DTLS identity binding verified room={room_text}");
    let peer = Arc::new(peer);
    let channels = peer.api_channels();
    let ice_candidate_pair = peer.selected_candidate_pair_label().await;
    Ok(EstablishedRtcSession { peer, channels, signaling: None, room_id, request_id, ice_candidate_pair,
        turn_refresh_at: turn_credentials.map(|credentials| credentials.refresh_at) })
}

pub async fn join_terminal_room(config: TerminalRtcConfig) -> Result<EstablishedRtcSession, SessionError> {
    log::info!("[rtc] Terminal opening rendezvous signaling; force_relay={}", config.force_relay);
    let mut socket = SignalSocket::connect(&config.rendezvous_url).await?;
    let request = join_message(config.host_id.clone(), config.rendezvous_authorization.as_str().to_owned());
    let request_id = request.request_id();
    socket.send(&request).await?;
    let (room_id, room_nonce) = match timeout(ROOM_TIMEOUT, socket.receive()).await
        .map_err(|_| SessionError::Timeout("waiting for Host registration"))?? {
        SignalMessage::JoinAccepted { request_id: response_id, host_id, room_id, nonce, .. }
            if response_id == request_id && host_id == config.host_id => (parse_uuid(&room_id)?, decode_nonce(&nonce)?),
        SignalMessage::Error { code, message, .. } => return Err(SessionError::Rendezvous(format!("{code}: {message}"))),
        unexpected => return Err(SessionError::UnexpectedSignal(signal_kind(&unexpected))),
    };
    let room_text = room_id.to_string();
    log::info!("[rtc] Terminal joined signaling room room={room_text} request={request_id}; preparing ICE");
    let mut peer_ice_servers = config.ice_servers.clone();
    let turn_credentials = request_turn_credentials(&mut socket, &room_text).await?;
    if let Some(credentials) = &turn_credentials {
        peer_ice_servers.push(credentials.server.clone());
    }
    let mut peer = RtcPeer::new(&peer_ice_servers, config.udp_addrs, config.force_relay).await?;
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
    log::info!("[rtc] Terminal ICE/DTLS transport connected room={room_text}");
    let fingerprints = peer.fingerprints().await?;
    log::info!("[rtc] Terminal verifying pinned DTLS identity binding room={room_text}");
    if let Err(error) = verify_terminal_identity(
        &peer,
        &config.expected_fingerprint,
        &config.host_id,
        request_id,
        room_id,
        room_nonce,
        config.resume_session_id,
        fingerprints,
    ).await {
        peer.close().await;
        return Err(error);
    }
    let peer = Arc::new(peer);
    let channels = peer.api_channels();
    let signaling = Some(keep_terminal_signaling_alive(socket, room_id));
    log::info!("[rtc] Terminal DTLS identity binding verified room={room_text}");
    Ok(EstablishedRtcSession { peer, channels, signaling, room_id, request_id, ice_candidate_pair,
        turn_refresh_at: turn_credentials.map(|credentials| credentials.refresh_at) })
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
    signaling.socket.send(&SignalMessage::RoomClose {
        protocol: SIGNAL_PROTOCOL,
        request_id,
        room_id: room_text.clone(),
    }).await?;
    loop {
        let message = timeout(Duration::from_secs(5), signaling.socket.receive()).await
            .map_err(|_| SessionError::Timeout("closing the signaling room"))??;
        match message {
            SignalMessage::RoomClose { request_id: response_id, room_id: response_room, .. }
                if response_id == request_id && response_room == room_text => return Ok(()),
            SignalMessage::RoomClose { room_id: notified_room, .. } if notified_room == room_text => continue,
            SignalMessage::Error { request_id: response_id, code, .. }
                if response_id == request_id && matches!(code.as_str(), "room_not_found" | "room_expired") => return Ok(()),
            SignalMessage::Error { request_id: response_id, code, message, .. } if response_id == request_id => {
                return Err(SessionError::Rendezvous(format!("{code}: {message}")));
            }
            _ => continue,
        }
    }
}

pub async fn request_turn_credentials(
    socket: &mut SignalSocket,
    room_text: &str,
) -> Result<Option<TurnCredentials>, SessionError> {
    let request_id = Uuid::new_v4();
    log::info!("[rtc] requesting fresh TURN credentials room={room_text}");
    socket.send(&SignalMessage::TurnCredentialsRequest {
        protocol: SIGNAL_PROTOCOL,
        request_id,
        room_id: room_text.to_owned(),
    }).await?;
    parse_turn_credentials(socket.receive().await?, request_id, room_text)
}

fn parse_turn_credentials(
    response: SignalMessage,
    request_id: Uuid,
    room_text: &str,
) -> Result<Option<TurnCredentials>, SessionError> {
    match response {
        SignalMessage::TurnCredentialsResult {
            request_id: response_id,
            room_id: response_room,
            turn_uris,
            username,
            credential,
            expires_at,
            ..
        } if response_id == request_id && response_room == room_text && !turn_uris.is_empty() => {
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
            Ok(Some(TurnCredentials {
                server: IceServerConfig { urls: turn_uris, username: Some(username), credential: Some(credential) },
                refresh_at: tokio::time::Instant::now() + refresh_after,
            }))
        }
        SignalMessage::Error { request_id: response_id, code, .. }
            if response_id == request_id && code == "turn_unconfigured" => {
                log::info!("[rtc] rendezvous has no TURN service configured for room={room_text}");
                Ok(None)
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

#[cfg(test)]
mod turn_credential_tests {
    use super::{remaining_turn_credential_lifetime, turn_credential_refresh_delay};

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
    #[error("timed out while {0}")] Timeout(&'static str),
    #[error("Host identity proof failed: {0}")] Identity(String),
}

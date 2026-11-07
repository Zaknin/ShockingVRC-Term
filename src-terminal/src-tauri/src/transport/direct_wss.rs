use std::pin::Pin;
use std::{collections::HashMap, sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use data_encoding::BASE32_NOPAD;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use ring::{
    rand::{SecureRandom, SystemRandom},
    signature::{ED25519, UnparsedPublicKey},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpStream,
    sync::{mpsc, oneshot},
    time::{Instant, interval, sleep, timeout},
};
use tokio_tungstenite::{
    Connector, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    protocol::{
        CapabilitiesDto, CatalogDto, CommandAckDto, ConnectionStateDto, HealthDto, HostIdentityDto, LogsDto,
        MAX_FRAME_BYTES, PROTOCOL_VERSION, ProfileDto, SnapshotDto, UiEvent,
    },
    transport::tls_pin::{PinnedTls, pinned_tls_config},
};

type DirectSocket = WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

enum SessionSocket {
    Direct(DirectSocket),
    Rtc {
        inbound: mpsc::Receiver<Vec<u8>>,
        outbound: Option<mpsc::Sender<Vec<u8>>>,
        peer: Option<Arc<shocking_vrc_rtc::RtcPeer>>,
        channels: Option<shocking_vrc_rtc::RtcApiChannels>,
        signaling: Option<shocking_vrc_rtc::SignalingLease>,
        ice_candidate_pair: Option<String>,
        room_id: uuid::Uuid,
        turn_refresh_at: Option<tokio::time::Instant>,
    },
}

impl SessionSocket {
    fn from_rtc(mut session: shocking_vrc_rtc::EstablishedRtcSession) -> Result<Self, String> {
        let (inbound, outbound) = session.channels.take().map_err(|error| error.to_string())?;
        let room_id = session.room_id;
        Ok(Self::Rtc {
            inbound,
            outbound: Some(outbound),
            peer: Some(session.peer),
            channels: Some(session.channels),
            signaling: session.signaling.take(),
            ice_candidate_pair: session.ice_candidate_pair,
            room_id,
            turn_refresh_at: session.turn_refresh_at,
        })
    }

    async fn send_json(&mut self, value: &Value) -> Result<(), String> {
        let text = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        if text.len() > MAX_FRAME_BYTES {
            return Err("Remote API frame exceeds the v1 size limit".to_owned());
        }
        match self {
            Self::Direct(socket) => timeout(
                CONNECT_TIMEOUT,
                socket.send(Message::Text(
                    String::from_utf8(text).map_err(|error| error.to_string())?.into(),
                )),
            )
            .await
            .map_err(|_| "Timed out sending to Host".to_owned())?
            .map_err(|error| error.to_string()),
            Self::Rtc {
                outbound: Some(outbound),
                ..
            } => timeout(CONNECT_TIMEOUT, outbound.send(text))
                .await
                .map_err(|_| "Timed out sending to Host DataChannel".to_owned())?
                .map_err(|_| "Host DataChannel closed".to_owned()),
            Self::Rtc { outbound: None, .. } => Err("Host DataChannel closed".to_owned()),
        }
    }

    async fn send_message(&mut self, message: Message) -> Result<(), String> {
        match self {
            Self::Direct(socket) => socket.send(message).await.map_err(|error| error.to_string()),
            Self::Rtc { .. } if matches!(message, Message::Pong(_) | Message::Ping(_)) => Ok(()),
            Self::Rtc { .. } => Err("WebRTC DataChannel accepts Remote API JSON frames only".to_owned()),
        }
    }

    async fn next(&mut self) -> Option<Result<Message, String>> {
        match self {
            Self::Direct(socket) => socket
                .next()
                .await
                .map(|frame| frame.map_err(|error| error.to_string())),
            Self::Rtc { inbound, .. } => inbound.recv().await.map(|bytes| {
                String::from_utf8(bytes)
                    .map(|text| Message::Text(text.into()))
                    .map_err(|_| "Host sent a non-UTF-8 DataChannel frame".to_owned())
            }),
        }
    }

    fn transport_name(&self) -> &'static str {
        match self {
            Self::Direct(_) => "direct_wss",
            Self::Rtc { .. } => "webrtc_datachannel",
        }
    }

    fn room_id(&self) -> Option<uuid::Uuid> {
        match self {
            Self::Rtc { room_id, .. } => Some(*room_id),
            Self::Direct(_) => None,
        }
    }

    fn ice_candidate_pair(&self) -> Option<String> {
        match self {
            Self::Rtc { ice_candidate_pair, .. } => ice_candidate_pair.clone(),
            Self::Direct(_) => None,
        }
    }

    fn turn_refresh_delay(&self) -> Option<Duration> {
        match self {
            Self::Rtc { turn_refresh_at, .. } => {
                turn_refresh_at.map(|at| at.saturating_duration_since(tokio::time::Instant::now()))
            }
            Self::Direct(_) => None,
        }
    }

    async fn refresh_turn_credentials(&mut self) -> Result<Option<Duration>, String> {
        let Self::Rtc {
            peer: Some(peer),
            signaling: Some(signaling),
            room_id,
            turn_refresh_at,
            ..
        } = self
        else {
            return Ok(None);
        };
        log::info!("[terminal] requesting TURN credential renewal room={room_id}");
        match signaling
            .refresh_turn_credentials()
            .await
            .map_err(|error| error.to_string())?
        {
            Some(credentials) => {
                peer.update_turn_credentials(credentials.server)
                    .await
                    .map_err(|error| error.to_string())?;
                let delay = credentials
                    .refresh_at
                    .saturating_duration_since(tokio::time::Instant::now());
                *turn_refresh_at = Some(credentials.refresh_at);
                log::info!(
                    "[terminal] renewed TURN credentials submitted to WebRTC; next_refresh_in_seconds={}",
                    delay.as_secs()
                );
                Ok(Some(delay))
            }
            None => {
                *turn_refresh_at = None;
                Ok(None)
            }
        }
    }

    async fn close(&mut self) {
        match self {
            Self::Direct(socket) => {
                let _ = socket.close(None).await;
            }
            Self::Rtc {
                outbound,
                peer,
                channels,
                signaling,
                ..
            } => {
                outbound.take();
                if let (Some(channels), Some(peer)) = (channels.take(), peer.take()) {
                    channels.close(&peer).await;
                }
                if let Some(signaling) = signaling.take() {
                    signaling.close().await;
                }
            }
        }
    }
}

impl Drop for SessionSocket {
    fn drop(&mut self) {
        if let Self::Rtc {
            outbound,
            peer,
            channels,
            signaling,
            ..
        } = self
        {
            outbound.take();
            let rtc_close = if let (Some(channels), Some(peer)) = (channels.take(), peer.take()) {
                Some((channels, peer))
            } else {
                None
            };
            let signal_close = signaling.take();
            if rtc_close.is_some() || signal_close.is_some() {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        if let Some((channels, peer)) = rtc_close {
                            channels.close(&peer).await;
                        }
                        if let Some(signaling) = signal_close {
                            signaling.close().await;
                        }
                    });
                }
            }
        }
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const HEARTBEAT_PERIOD: Duration = Duration::from_secs(5);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);
const RESUME_WINDOW: Duration = Duration::from_secs(60);
const MAX_RESUME_RETRY_DELAY: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeRetryDecision {
    RetryAfter(Duration),
    FailFast,
}

fn resume_retry_decision(error: &str, retry_index: u32) -> ResumeRetryDecision {
    let error = error.to_ascii_uppercase();
    if error.contains("SESSION_REVOKED") || error.contains("RESUME_TICKET_INVALID") {
        return ResumeRetryDecision::FailFast;
    }

    let exponent = retry_index.min(2);
    let seconds = 1_u64 << exponent;
    ResumeRetryDecision::RetryAfter(Duration::from_secs(seconds).min(MAX_RESUME_RETRY_DELAY))
}

fn is_room_busy(error: &str) -> bool {
    error.to_ascii_lowercase().contains("room_busy")
}

async fn wait_for_turn_refresh(timer: &mut Option<Pin<Box<tokio::time::Sleep>>>) {
    match timer {
        Some(timer) => timer.as_mut().await,
        None => std::future::pending().await,
    }
}

pub struct PendingConnection {
    profile: ProfileDto,
    socket: SessionSocket,
    identity: HelloIdentity,
}

pub struct AuthenticatedConnection {
    profile: ProfileDto,
    socket: SessionSocket,
    identity: HelloIdentity,
    secrets: SessionSecrets,
    pub initial_snapshot: SnapshotDto,
    pub capabilities: CapabilitiesDto,
    pub device_grant: Zeroizing<String>,
}

struct HelloIdentity {
    server_id: String,
    server_epoch: String,
    fingerprint: String,
    client_nonce: String,
}

struct SessionSecrets {
    session_id: String,
    resume_ticket: Zeroizing<String>,
    access_token: Zeroizing<String>,
    access_expires_at: Instant,
    generation: u64,
    resume_request_id: Uuid,
}

pub enum SessionAction {
    Disconnect,
    FetchState,
    FetchCapabilities,
    FetchCatalog(String),
    FetchLogs(Option<String>, u16),
    Command {
        request_id: Uuid,
        frame: Value,
        response: oneshot::Sender<Result<CommandAckDto, String>>,
    },
}

impl AuthenticatedConnection {
    pub fn transport_name(&self) -> &'static str {
        self.socket.transport_name()
    }
}

pub async fn begin_connection(
    profile: ProfileDto,
    rendezvous_authorization: Option<Zeroizing<String>>,
    resume_session_id: Option<Uuid>,
) -> Result<(PendingConnection, HostIdentityDto), String> {
    log::info!(
        "[terminal] connection attempt mode={} force_relay={}",
        profile.connection_mode,
        profile.force_relay
    );
    let direct_error = if profile.connection_mode != "traversal_only" {
        let direct = async {
            let pinned_tls = pinned_tls_config(&profile.fingerprint)?;
            check_https_health(&profile, &pinned_tls.config).await?;
            open_pinned_wss(profile.clone(), pinned_tls).await
        }
        .await;
        match direct {
            Ok(pending) => {
                log::info!("[terminal] Direct HTTPS/WSS connection verified");
                let identity = host_identity_dto(&pending);
                return Ok((pending, identity));
            }
            Err(error) if profile.connection_mode == "direct_only" => {
                log::warn!("[terminal] Direct HTTPS/WSS connection failed: {error}");
                return Err(error);
            }
            Err(error) => {
                log::warn!("[terminal] Direct HTTPS/WSS attempt failed; continuing to ICE: {error}");
                Some(error)
            }
        }
    } else {
        None
    };

    if profile.connection_mode == "direct_only" {
        return Err(direct_error.unwrap_or_else(|| "Direct WSS is unavailable".to_owned()));
    }
    if profile.rendezvous_url.trim().is_empty() {
        return Err(match direct_error {
            Some(error) => format!("Direct WSS failed ({error}); configure a rendezvous URL to use ICE traversal"),
            None => "This profile requires a rendezvous URL for ICE traversal".to_owned(),
        });
    }
    if profile.server_id.trim().is_empty() {
        return Err("Enter the Host ID shown in Host Setup before using ICE traversal".to_owned());
    }
    let rendezvous_authorization = rendezvous_authorization.ok_or_else(|| {
        "Pair this Terminal with a Host before using ICE traversal".to_owned()
    })?;
    let ice_servers = profile
        .ice_server_urls
        .iter()
        .map(|url| shocking_vrc_rtc::IceServerConfig {
            urls: vec![url.clone()],
            username: None,
            credential: None,
        })
        .collect();
    let established = match shocking_vrc_rtc::join_terminal_room(shocking_vrc_rtc::TerminalRtcConfig {
        rendezvous_url: profile.rendezvous_url.clone(),
        host_id: profile.server_id.clone(),
        rendezvous_authorization,
        expected_fingerprint: profile.fingerprint.clone(),
        ice_servers,
        udp_addrs: vec!["0.0.0.0:0".to_owned()],
        force_relay: profile.force_relay,
        resume_session_id,
    })
    .await
    {
        Ok(established) => established,
        Err(error) => {
            log::error!("[terminal] ICE room setup failed: {error}");
            return Err(match direct_error {
                Some(direct_error) => format!("Direct WSS failed ({direct_error}); ICE traversal failed ({error})"),
                None => error.to_string(),
            });
        }
    };
    log::info!(
        "[terminal] ICE DataChannel connected; selected_pair={}",
        established.ice_candidate_pair.as_deref().unwrap_or("unavailable")
    );
    let mut socket = SessionSocket::from_rtc(established)?;
    let room_label = socket
        .room_id()
        .map_or_else(|| "direct".to_owned(), |room_id| room_id.to_string());
    let (client_nonce, client_nonce_bytes) = random_nonce()?;
    let hello_request_id = Uuid::new_v4();
    log::info!(
        "[terminal] sending Host API hello over {} room={room_label}",
        socket.transport_name()
    );
    send_json(
        &mut socket,
        &json!({
            "type":"hello", "protocol":PROTOCOL_VERSION, "request_id":hello_request_id,
            "client":"ShockingVRC Terminal", "client_version":env!("CARGO_PKG_VERSION"),
            "client_nonce":client_nonce, "transport":"webrtc-datachannel"
        }),
    )
    .await?;
    let hello = match receive_json(&mut socket).await {
        Ok(hello) => hello,
        Err(error) => {
            log::error!("[terminal] failed waiting for Host API hello response room={room_label}: {error}");
            return Err(error);
        }
    };
    if frame_type(&hello) != Some("hello.ok") {
        return Err(remote_frame_error(&hello));
    }
    log::info!("[terminal] Host API hello response received over DataChannel room={room_label}");
    let identity = verify_rtc_hello(&hello, &profile, &client_nonce, client_nonce_bytes, hello_request_id)?;
    let pending = PendingConnection {
        profile,
        socket,
        identity,
    };
    let identity = HostIdentityDto {
        profile_id: pending.profile.id.clone(),
        host_label: pending.profile.label.clone(),
        server_id: pending.identity.server_id.clone(),
        fingerprint: pending.identity.fingerprint.clone(),
        protocol: PROTOCOL_VERSION,
    };
    Ok((pending, identity))
}

fn host_identity_dto(pending: &PendingConnection) -> HostIdentityDto {
    HostIdentityDto {
        profile_id: pending.profile.id.clone(),
        host_label: pending.profile.label.clone(),
        server_id: pending.identity.server_id.clone(),
        fingerprint: pending.identity.fingerprint.clone(),
        protocol: PROTOCOL_VERSION,
    }
}

fn verify_rtc_hello(
    hello: &Value,
    profile: &ProfileDto,
    client_nonce: &str,
    client_nonce_bytes: [u8; 32],
    request_id: Uuid,
) -> Result<HelloIdentity, String> {
    if hello.get("protocol").and_then(Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
        || hello.get("request_id").and_then(Value::as_str) != Some(request_id.to_string().as_str())
    {
        return Err("Host returned an incompatible DataChannel hello response".to_owned());
    }
    let server_id = string_field(hello, "server_id")?;
    if !profile.server_id.is_empty() && server_id != profile.server_id {
        return Err("Rendezvous Host ID did not match the pinned Host identity".to_owned());
    }
    let server_epoch = string_field(hello, "server_epoch")?;
    let fingerprint = string_field(hello, "fingerprint")?.to_ascii_lowercase();
    if fingerprint != profile.fingerprint {
        return Err("Pinned Host identity changed".to_owned());
    }
    let public_key = URL_SAFE_NO_PAD
        .decode(string_field(hello, "server_key")?)
        .map_err(|_| "Host returned an invalid identity key".to_owned())?;
    if shocking_vrc_rtc::identity_fingerprint(&public_key) != profile.fingerprint
        || shocking_vrc_rtc::server_id(&public_key) != server_id
    {
        return Err("Host Ed25519 identity did not match the saved SPKI pin and Host ID".to_owned());
    }
    let server_nonce = URL_SAFE_NO_PAD
        .decode(string_field(hello, "server_nonce")?)
        .map_err(|_| "Host returned an invalid identity nonce".to_owned())?;
    if server_nonce.len() != 32 {
        return Err("Host returned an invalid identity nonce".to_owned());
    }
    let proof = URL_SAFE_NO_PAD
        .decode(string_field(hello, "identity_proof")?)
        .map_err(|_| "Host returned an invalid identity proof".to_owned())?;
    let transcript = identity_transcript(&server_epoch, &client_nonce_bytes, &server_nonce, server_id)?;
    UnparsedPublicKey::new(&ED25519, &public_key)
        .verify(&transcript, &proof)
        .map_err(|_| "Host API identity proof did not match its pinned key".to_owned())?;
    Ok(HelloIdentity {
        server_id: server_id.to_owned(),
        server_epoch: server_epoch.to_owned(),
        fingerprint,
        client_nonce: client_nonce.to_owned(),
    })
}

async fn check_https_health(profile: &ProfileDto, tls: &rustls::ClientConfig) -> Result<(), String> {
    let client = Client::builder()
        .use_preconfigured_tls(tls.clone())
        .timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| format!("Could not prepare pinned HTTPS transport: {error}"))?;
    let url = format!("{}api/v1/health", profile.http_base_url());
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| pin_or_network_error(&profile.fingerprint, error.to_string()))?;
    if !response.status().is_success() {
        return Err(format!("Host HTTPS health endpoint returned {}", response.status()));
    }
    let health: HealthDto = response
        .json()
        .await
        .map_err(|_| "Host returned an invalid HTTPS health response".to_owned())?;
    if health.protocol_min > PROTOCOL_VERSION || health.protocol_max < PROTOCOL_VERSION {
        return Err("Host does not support Remote API protocol v1".to_owned());
    }
    Ok(())
}

async fn open_pinned_wss(profile: ProfileDto, pinned_tls: PinnedTls) -> Result<PendingConnection, String> {
    let url = format!("{}api/v1/ws", profile.wss_base_url());
    let request = url
        .into_client_request()
        .map_err(|error| format!("Invalid Host WebSocket address: {error}"))?;
    let mut ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    ws_config.max_message_size = Some(MAX_FRAME_BYTES);
    ws_config.max_frame_size = Some(MAX_FRAME_BYTES);
    let connected = timeout(
        CONNECT_TIMEOUT,
        connect_async_tls_with_config(
            request,
            Some(ws_config),
            false,
            Some(Connector::Rustls(Arc::clone(&pinned_tls.config))),
        ),
    )
    .await
    .map_err(|_| "Timed out connecting to the Host WebSocket".to_owned())?
    .map_err(|error| pin_or_network_error(&profile.fingerprint, error.to_string()))?;
    let mut socket = SessionSocket::Direct(connected.0);
    let (client_nonce, client_nonce_bytes) = random_nonce()?;
    let hello_request_id = Uuid::new_v4();
    send_json(
        &mut socket,
        &json!({
            "type": "hello",
            "protocol": PROTOCOL_VERSION,
            "request_id": hello_request_id,
            "client": "ShockingVRC Terminal",
            "client_version": env!("CARGO_PKG_VERSION"),
            "client_nonce": client_nonce,
            "transport": "wss"
        }),
    )
    .await?;
    let hello = receive_json(&mut socket).await?;
    if frame_type(&hello) != Some("hello.ok") {
        return Err(remote_frame_error(&hello));
    }
    let identity = verify_hello(
        &hello,
        &profile,
        &pinned_tls,
        &client_nonce,
        client_nonce_bytes,
        hello_request_id,
    )?;
    Ok(PendingConnection {
        profile,
        socket,
        identity,
    })
}

fn verify_hello(
    hello: &Value,
    profile: &ProfileDto,
    pinned_tls: &PinnedTls,
    client_nonce: &str,
    client_nonce_bytes: [u8; 32],
    request_id: Uuid,
) -> Result<HelloIdentity, String> {
    if hello.get("protocol").and_then(Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
        || hello.get("request_id").and_then(Value::as_str) != Some(request_id.to_string().as_str())
    {
        return Err("Host returned an incompatible hello response".to_owned());
    }
    let server_id = string_field(hello, "server_id")?;
    let server_epoch = string_field(hello, "server_epoch")?;
    let fingerprint = string_field(hello, "fingerprint")?.to_ascii_lowercase();
    if fingerprint != profile.fingerprint {
        return Err("TLS identity pin mismatch: the Host fingerprint changed".to_owned());
    }
    let pinned_public_key = pinned_tls.observed_public_key()?;
    let advertised_public_key = URL_SAFE_NO_PAD
        .decode(string_field(hello, "server_key")?)
        .map_err(|_| "Host returned an invalid identity key".to_owned())?;
    if advertised_public_key.as_slice() != pinned_public_key {
        return Err("Host WSS identity key does not match the pinned TLS certificate".to_owned());
    }
    let public_key_hash = Sha256::digest(pinned_public_key);
    let expected_server_id = BASE32_NOPAD.encode(&public_key_hash).to_ascii_lowercase();
    if server_id != expected_server_id {
        return Err("Host server ID does not match its pinned Ed25519 key".to_owned());
    }
    let server_nonce = URL_SAFE_NO_PAD
        .decode(string_field(hello, "server_nonce")?)
        .map_err(|_| "Host returned an invalid identity nonce".to_owned())?;
    if server_nonce.len() != 32 {
        return Err("Host returned an invalid identity nonce".to_owned());
    }
    let proof = URL_SAFE_NO_PAD
        .decode(string_field(hello, "identity_proof")?)
        .map_err(|_| "Host returned an invalid identity proof".to_owned())?;
    let transcript = identity_transcript(&server_epoch, &client_nonce_bytes, &server_nonce, &server_id)?;
    UnparsedPublicKey::new(&ED25519, pinned_public_key)
        .verify(&transcript, &proof)
        .map_err(|_| "Host identity proof does not match the pinned key".to_owned())?;
    Ok(HelloIdentity {
        server_id: server_id.to_owned(),
        server_epoch: server_epoch.to_owned(),
        fingerprint,
        client_nonce: client_nonce.to_owned(),
    })
}

fn identity_transcript(
    epoch: &str,
    client_nonce: &[u8; 32],
    server_nonce: &[u8],
    server_id: &str,
) -> Result<Vec<u8>, String> {
    let epoch = Uuid::parse_str(epoch).map_err(|_| "Host returned an invalid server epoch".to_owned())?;
    let server_id_len = u16::try_from(server_id.len()).map_err(|_| "Host server ID is too long".to_owned())?;
    let mut transcript = Vec::with_capacity(128 + server_id.len());
    transcript.extend_from_slice(b"ShockingVRC-Term/direct-wss/v1\0");
    transcript.extend_from_slice(epoch.as_bytes());
    transcript.extend_from_slice(&32_u16.to_be_bytes());
    transcript.extend_from_slice(client_nonce);
    transcript.extend_from_slice(server_nonce);
    transcript.extend_from_slice(&server_id_len.to_be_bytes());
    transcript.extend_from_slice(server_id.as_bytes());
    Ok(transcript)
}

pub async fn finish_device_auth(
    mut pending: PendingConnection,
    terminal_identity: &crate::storage::credentials::TerminalDeviceIdentity,
    invitation: Option<&shocking_vrc_rendezvous::pairing::PairingInvitationV1>,
) -> Result<AuthenticatedConnection, String> {
    if let Some(invitation) = invitation {
        if invitation.host_id != pending.identity.server_id
            || invitation.host_fingerprint != pending.identity.fingerprint
        {
            return Err("The invitation Host identity does not match the verified transport".to_owned());
        }
    }
    let request_id = Uuid::new_v4();
    let room_label = pending
        .socket
        .room_id()
        .map_or_else(|| "direct".to_owned(), |room_id| room_id.to_string());
    log::info!(
        "[terminal] sending passwordless device proof over {} room={room_label}",
        pending.socket.transport_name()
    );
    let public_key = URL_SAFE_NO_PAD.encode(terminal_identity.public_key);
    send_json(
        &mut pending.socket,
        &json!({
            "type": "auth.device.begin",
            "protocol": PROTOCOL_VERSION,
            "request_id": request_id,
            "device_id": terminal_identity.device_id,
            "public_key": public_key.clone(),
            "invitation_id": invitation.map(|value| value.invitation_id),
        }),
    )
    .await?;
    let challenge = wait_for_type(&mut pending.socket, "auth.device.challenge").await?;
    if challenge.get("request_id").and_then(Value::as_str) != Some(request_id.to_string().as_str()) {
        return Err("Host returned a device challenge for a different request".to_owned());
    }
    let challenge_id = challenge.get("challenge_id").and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok()).ok_or_else(|| "Host returned an invalid device challenge".to_owned())?;
    if challenge.get("device_id").and_then(Value::as_str) != Some(terminal_identity.device_id.as_str()) {
        return Err("Host challenge is bound to a different Terminal device".to_owned());
    }
    let nonce_bytes = challenge.get("server_nonce").and_then(Value::as_str)
        .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok()).ok_or_else(|| "Host returned an invalid device nonce".to_owned())?;
    let server_nonce: [u8; 32] = nonce_bytes.try_into().map_err(|_| "Host returned an invalid device nonce".to_owned())?;
    let invitation_id = invitation.map(|value| value.invitation_id);
    let pairing_secret = if let Some(invitation) = invitation {
        Some(Zeroizing::new(invitation.pairing_secret.clone()))
    } else { None };
    let raw_secret = pairing_secret.as_ref().map(|secret| {
        URL_SAFE_NO_PAD.decode(secret.as_str()).map(Zeroizing::new)
            .map_err(|_| "Invitation pairing secret is invalid".to_owned())
    }).transpose()?;
    let secret_hash = raw_secret.as_ref().map(|raw| Sha256::digest(raw.as_slice()).into());
    let transcript = shocking_vrc_rendezvous::pairing::device_auth_transcript(
        &pending.identity.server_id,
        Uuid::parse_str(&pending.identity.server_epoch).map_err(|_| "Host server epoch is invalid".to_owned())?,
        request_id,
        challenge_id,
        server_nonce,
        &terminal_identity.device_id,
        terminal_identity.public_key,
        invitation_id,
        secret_hash,
    );
    let signature = URL_SAFE_NO_PAD.encode(terminal_identity.sign(&transcript)?);
    send_json(
        &mut pending.socket,
        &json!({
            "type": "auth.device.prove",
            "protocol": PROTOCOL_VERSION,
            "request_id": request_id,
            "challenge_id": challenge_id,
            "device_id": terminal_identity.device_id,
            "public_key": public_key,
            "signature": signature,
            "invitation_id": invitation_id,
            "pairing_secret": pairing_secret.as_deref(),
        }),
    ).await?;
    let auth = wait_for_type(&mut pending.socket, "auth.result").await?;
    if auth.get("request_id").and_then(Value::as_str) != Some(request_id.to_string().as_str()) {
        return Err("Host returned a login response for a different request".to_owned());
    }
    if auth.get("server_epoch").and_then(Value::as_str) != Some(pending.identity.server_epoch.as_str()) {
        return Err("Host login response came from a different server epoch".to_owned());
    }
    if auth.get("ok").and_then(Value::as_bool) != Some(true)
        || auth.get("session_state").and_then(Value::as_str) != Some("ACTIVE")
    {
        log::warn!("[terminal] Host did not accept device authentication room={room_label}");
        return Err("Host did not activate the passwordless Remote Session".to_owned());
    }
    let device_grant = string_field(&auth, "device_grant")?.to_owned();
    log::info!("[terminal] Host device authentication accepted; waiting for initial Snapshot room={room_label}");
    let mut secrets = parse_session_secrets(&auth)?;
    let snapshot_value = wait_for_type(&mut pending.socket, "snapshot").await?;
    let snapshot = snapshot_from_frame(&snapshot_value)?;
    log::info!("[terminal] initial Host Snapshot received over DataChannel room={room_label}");
    let capabilities_request_id = Uuid::new_v4();
    send_json(
        &mut pending.socket,
        &json!({
            "type": "capabilities.get",
            "protocol": PROTOCOL_VERSION,
            "request_id": capabilities_request_id
        }),
    )
    .await?;
    let capabilities_value = wait_for_type(&mut pending.socket, "capabilities").await?;
    if capabilities_value.get("request_id").and_then(Value::as_str)
        != Some(capabilities_request_id.to_string().as_str())
    {
        return Err("Host returned capabilities for a different request".to_owned());
    }
    let capabilities: CapabilitiesDto = serde_json::from_value(
        capabilities_value
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| capabilities_value.clone()),
    )
    .map_err(|_| "Host returned invalid capabilities".to_owned())?;
    secrets.resume_request_id = Uuid::new_v4();
    Ok(AuthenticatedConnection {
        profile: pending.profile,
        socket: pending.socket,
        identity: pending.identity,
        secrets,
        initial_snapshot: snapshot,
        capabilities,
        device_grant: Zeroizing::new(device_grant),
    })
}

fn parse_session_secrets(auth: &Value) -> Result<SessionSecrets, String> {
    let session_id = string_field(auth, "session_id")?.to_owned();
    let resume_ticket = string_field(auth, "resume_ticket")?.to_owned();
    let access_token = string_field(auth, "access_token")?.to_owned();
    let access_expires_in = auth
        .get("access_expires_in")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Host returned an invalid token lifetime".to_owned())?;
    if auth.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("Host login was not accepted".to_owned());
    }
    let generation = auth
        .get("transport_generation")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Host returned an invalid transport generation".to_owned())?;
    Ok(SessionSecrets {
        session_id,
        resume_ticket: Zeroizing::new(resume_ticket),
        access_token: Zeroizing::new(access_token),
        access_expires_at: Instant::now() + Duration::from_secs(access_expires_in),
        generation,
        resume_request_id: Uuid::new_v4(),
    })
}

pub async fn run_session(
    mut connection: AuthenticatedConnection,
    mut actions: mpsc::Receiver<SessionAction>,
    updates: Arc<crate::app_state::UiBridge>,
    cancel: CancellationToken,
) {
    log::info!(
        "[terminal] authenticated session loop started transport={}",
        connection.transport_name()
    );
    publish_connected(&updates, &connection, "online", None).await;
    updates
        .publish(UiEvent::Snapshot(connection.initial_snapshot.clone()))
        .await;
    updates
        .publish(UiEvent::Capabilities(connection.capabilities.clone()))
        .await;

    let mut heartbeat = interval(HEARTBEAT_PERIOD);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_pong = Instant::now();
    let mut ping_nonce: Option<String> = None;
    let mut socket_dead = false;
    let mut refresh_at = connection.secrets.access_expires_at - Duration::from_secs(60);
    let mut refresh_timer = Box::pin(sleep(refresh_at.saturating_duration_since(Instant::now())));
    let mut refresh_request_id: Option<Uuid> = None;
    let mut pending_commands: HashMap<Uuid, oneshot::Sender<Result<CommandAckDto, String>>> = HashMap::new();
    let mut turn_refresh_timer = connection
        .socket
        .turn_refresh_delay()
        .map(|delay| Box::pin(sleep(delay)));

    loop {
        pending_commands.retain(|_, response| !response.is_closed());
        tokio::select! {
            _ = cancel.cancelled() => {
                fail_pending_commands(&mut pending_commands, "Host session ended before command acknowledgement; outcome is unknown.");
                let _ = send_logout(&mut connection.socket).await;
                break;
            }
            action = actions.recv() => {
                match action {
                    Some(SessionAction::Disconnect) => {
                        fail_pending_commands(&mut pending_commands, "Disconnected before Host command acknowledgement; outcome is unknown.");
                        let _ = send_logout(&mut connection.socket).await;
                        break;
                    }
                    Some(SessionAction::FetchState) => {
                        let _ = send_read_request(&mut connection.socket, "state.get", None).await;
                    }
                    Some(SessionAction::FetchCapabilities) => {
                        let _ = send_read_request(&mut connection.socket, "capabilities.get", None).await;
                    }
                    Some(SessionAction::FetchCatalog(kind)) if matches!(kind.as_str(), "presets" | "zone_presets") => {
                        let _ = send_read_request(&mut connection.socket, "catalogs.get", Some(json!({"catalog": kind}))).await;
                    }
                    Some(SessionAction::FetchLogs(cursor, limit)) => {
                        let _ = send_read_request(&mut connection.socket, "logs.get", Some(json!({"cursor": cursor, "limit": limit.clamp(1, 200)}))).await;
                    }
                    Some(SessionAction::Command { request_id, frame, response }) => {
                        if let Err(error) = send_json(&mut connection.socket, &frame).await {
                            let _ = response.send(Err(format!("Host command acknowledgement was not received; outcome is unknown: {error}")));
                        } else {
                            pending_commands.insert(request_id, response);
                        }
                    }
                    _ => {}
                }
            }
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > HEARTBEAT_TIMEOUT {
                    log::warn!("[terminal] Host heartbeat timed out; attempting same-session resume");
                    fail_pending_commands(&mut pending_commands, "Connection interrupted before Host command acknowledgement; outcome is unknown. The command was not replayed.");
                    fail_queued_command_actions(&mut actions, "Connection interrupted before Host command acknowledgement; outcome is unknown. The command was not replayed.");
                    publish_connected(&updates, &connection, "reconnecting", Some("Host heartbeat was missed; trying to resume the same session".to_owned())).await;
                    match timeout(Duration::from_secs(3), connection.socket.close()).await {
                        Ok(()) => log::info!("[terminal] closed stale transport before same-session resume"),
                        Err(_) => log::warn!("[terminal] stale transport close timed out; continuing same-session resume"),
                    }
                    let resumed = tokio::select! {
                        _ = cancel.cancelled() => Err("Disconnected by user".to_owned()),
                        result = timeout(RESUME_WINDOW, reconnect_and_resume(&connection, &cancel)) => {
                            match result {
                                Ok(result) => result,
                                Err(_) => Err(format!("Host session resume window elapsed after {} seconds; sign in again", RESUME_WINDOW.as_secs())),
                            }
                        },
                    };
                    match resumed {
                        Ok((socket, identity, secrets, snapshot, capabilities)) => {
                            log::info!("[terminal] same-session resume succeeded transport={}", socket.transport_name());
                            connection.socket = socket;
                            connection.identity = identity;
                            connection.secrets = secrets;
                            connection.initial_snapshot = snapshot.clone();
                            connection.capabilities = capabilities.clone();
                            last_pong = Instant::now();
                            ping_nonce = None;
                            socket_dead = false;
                            updates.publish(UiEvent::Snapshot(snapshot)).await;
                            updates.publish(UiEvent::Capabilities(capabilities)).await;
                            fail_queued_command_actions(&mut actions, "Command was not sent while the Host session was reconnecting; submit it again after reviewing fresh state.");
                            publish_connected(&updates, &connection, "online", Some("Session resumed with fresh Host state".to_owned())).await;
                            refresh_at = connection.secrets.access_expires_at - Duration::from_secs(60);
                            refresh_timer.as_mut().reset(tokio::time::Instant::now() + refresh_at.saturating_duration_since(Instant::now()));
                            turn_refresh_timer = connection.socket.turn_refresh_delay().map(|delay| Box::pin(sleep(delay)));
                        }
                        Err(error) => {
                            log::error!("[terminal] same-session resume failed: {error}");
                            if error.contains("SESSION_REVOKED") || error.contains("RESUME_TICKET_INVALID") {
                                publish_connected(&updates, &connection, "revoked", Some("Host revoked this session. Reconnect and sign in again.".to_owned())).await;
                            } else {
                                publish_connected(&updates, &connection, "disconnected", Some(error)).await;
                            }
                            return;
                        }
                    }
                }
                let nonce = Uuid::new_v4().to_string();
                let request_id = Uuid::new_v4();
                if send_json(&mut connection.socket, &json!({
                    "type": "ping", "protocol": PROTOCOL_VERSION,
                    "request_id": request_id, "nonce": nonce
                })).await.is_err() {
                    last_pong = Instant::now() - HEARTBEAT_TIMEOUT;
                    socket_dead = true;
                    fail_pending_commands(&mut pending_commands, "Connection interrupted before Host command acknowledgement; outcome is unknown. The command was not replayed.");
                } else {
                    ping_nonce = Some(nonce);
                }
            }
            _ = &mut refresh_timer, if refresh_request_id.is_none() => {
                let request_id = Uuid::new_v4();
                if send_json(&mut connection.socket, &json!({
                    "type": "auth.token.refresh", "protocol": PROTOCOL_VERSION,
                    "request_id": request_id
                })).await.is_ok() {
                    log::debug!("[terminal] requested access-token refresh; token value omitted");
                    refresh_request_id = Some(request_id);
                } else {
                    log::warn!("[terminal] access-token refresh request could not be sent");
                }
                refresh_timer.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(20));
            }
            _ = wait_for_turn_refresh(&mut turn_refresh_timer), if turn_refresh_timer.is_some() => {
                match connection.socket.refresh_turn_credentials().await {
                    Ok(Some(delay)) => {
                        turn_refresh_timer = Some(Box::pin(sleep(delay)));
                    }
                    Ok(None) => {
                        log::warn!("[terminal] TURN renewal returned no TURN service; periodic renewal stopped");
                        turn_refresh_timer = None;
                    }
                    Err(error) => {
                        log::error!("[terminal] TURN credential renewal failed: {error}; retrying shortly");
                        turn_refresh_timer = Some(Box::pin(sleep(Duration::from_secs(15))));
                    }
                }
            }
            frame = connection.socket.next(), if !socket_dead => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<Value>(text.as_str()) {
                            Ok(value) => {
                                match frame_type(&value) {
                                    Some("snapshot") => if let Ok(snapshot) = snapshot_from_frame(&value) {
                                        updates.publish(UiEvent::Snapshot(snapshot)).await;
                                    },
                                    Some("capabilities") => if let Ok(capabilities) = parse_capabilities(&value) {
                                        updates.publish(UiEvent::Capabilities(capabilities)).await;
                                    },
                                    Some("catalogs") => if let Ok(catalog) = parse_catalog(&value) {
                                        updates.publish(UiEvent::Catalog(catalog)).await;
                                    },
                                    Some("logs") => if let Ok(logs) = serde_json::from_value::<LogsDto>(value.clone()) {
                                        updates.publish(UiEvent::Logs(logs)).await;
                                    },
                                    Some("command.accepted") => {
                                        let request_id = value.get("request_id").and_then(Value::as_str)
                                            .and_then(|raw| Uuid::parse_str(raw).ok());
                                        if let Some(request_id) = request_id {
                                            if let Some(response) = pending_commands.remove(&request_id) {
                                                if value.get("protocol").and_then(Value::as_u64) != Some(u64::from(PROTOCOL_VERSION)) {
                                                    let _ = response.send(Err("Host returned an incompatible command acknowledgement".to_owned()));
                                                } else if value.get("queued").and_then(Value::as_bool) == Some(true) {
                                                    let _ = response.send(Ok(CommandAckDto { request_id: request_id.to_string(), queued: true }));
                                                } else {
                                                    let _ = response.send(Err("Host did not confirm that the command was queued".to_owned()));
                                                }
                                            }
                                        }
                                    }
                                    Some("command.error") => {
                                        let request_id = value.get("request_id").and_then(Value::as_str)
                                            .and_then(|raw| Uuid::parse_str(raw).ok());
                                        if let Some(request_id) = request_id {
                                            if let Some(response) = pending_commands.remove(&request_id) {
                                                if value.get("protocol").and_then(Value::as_u64) != Some(u64::from(PROTOCOL_VERSION)) {
                                                    let _ = response.send(Err("Host returned an incompatible command error".to_owned()));
                                                } else {
                                                    let code = value.get("code").and_then(Value::as_str).unwrap_or("HOST_ERROR");
                                                    let message = value.get("message").and_then(Value::as_str).unwrap_or("Host rejected the command");
                                                    let _ = response.send(Err(format!("{code}: {message}")));
                                                }
                                            }
                                        }
                                    }
                                    Some("pong") if value.get("nonce").and_then(Value::as_str) == ping_nonce.as_deref() => {
                                        last_pong = Instant::now();
                                        ping_nonce = None;
                                        publish_connected(&updates, &connection, "online", None).await;
                                    }
                                    Some("auth.token") if value.get("request_id").and_then(Value::as_str) == refresh_request_id.map(|id| id.to_string()).as_deref() => {
                                        if let (Some(token), Some(ttl)) = (value.get("access_token").and_then(Value::as_str), value.get("access_expires_in").and_then(Value::as_u64)) {
                                            connection.secrets.access_token = Zeroizing::new(token.to_owned());
                                            connection.secrets.access_expires_at = Instant::now() + Duration::from_secs(ttl);
                                            log::info!("[terminal] access-token refresh succeeded; valid_for_seconds={ttl}; token value omitted");
                                            refresh_at = connection.secrets.access_expires_at - Duration::from_secs(60);
                                            refresh_timer.as_mut().reset(tokio::time::Instant::now() + refresh_at.saturating_duration_since(Instant::now()));
                                        }
                                        refresh_request_id = None;
                                    }
                                    Some("session.revoked") => {
                                        connection.secrets.resume_ticket.zeroize();
                                        connection.secrets.access_token.zeroize();
                                        publish_connected(&updates, &connection, "revoked", Some("Host revoked this session. Reconnect and sign in again.".to_owned())).await;
                                        return;
                                    }
                                    Some("error") => {
                                        if let Some(code) = value.get("code").and_then(Value::as_str) {
                                            let response_request_id = value.get("request_id").and_then(Value::as_str);
                                            if refresh_request_id.map(|id| id.to_string()).as_deref() == response_request_id {
                                                log::warn!("[terminal] access-token refresh rejected code={code}");
                                                refresh_request_id = None;
                                                if code == "AUTH_REQUIRED" {
                                                    last_pong = Instant::now() - HEARTBEAT_TIMEOUT;
                                                    socket_dead = true;
                                                } else {
                                                    refresh_timer.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(30));
                                                }
                                            }
                                            if matches!(code, "SESSION_REVOKED" | "RESUME_TICKET_INVALID") {
                                                fail_pending_commands(&mut pending_commands, "Host revoked the session before command acknowledgement; outcome is unknown.");
                                                connection.secrets.resume_ticket.zeroize();
                                                connection.secrets.access_token.zeroize();
                                                publish_connected(&updates, &connection, "revoked", Some(code.to_owned())).await;
                                                return;
                                            }
                                            updates.publish(UiEvent::Error(code.to_owned())).await;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Err(_) => {
                                updates.publish(UiEvent::Error("Host sent an invalid protocol frame".to_owned())).await;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if connection.socket.send_message(Message::Pong(payload)).await.is_err() {
                            last_pong = Instant::now() - HEARTBEAT_TIMEOUT;
                            socket_dead = true;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        last_pong = Instant::now() - HEARTBEAT_TIMEOUT;
                        socket_dead = true;
                        fail_pending_commands(&mut pending_commands, "Connection interrupted before Host command acknowledgement; outcome is unknown. The command was not replayed.");
                    }
                    Some(Ok(Message::Binary(_))) => {
                        updates.publish(UiEvent::Error("Host sent a non-JSON WebSocket frame".to_owned())).await;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Frame(_))) => {}
                }
            }
        }
    }

    fail_pending_commands(
        &mut pending_commands,
        "Host session ended before command acknowledgement; outcome is unknown.",
    );
    publish_connected(&updates, &connection, "disconnected", None).await;
    connection.socket.close().await;
}

fn fail_pending_commands(pending: &mut HashMap<Uuid, oneshot::Sender<Result<CommandAckDto, String>>>, message: &str) {
    for (_, response) in pending.drain() {
        let _ = response.send(Err(message.to_owned()));
    }
}

fn fail_queued_command_actions(actions: &mut mpsc::Receiver<SessionAction>, message: &str) {
    while let Ok(action) = actions.try_recv() {
        if let SessionAction::Command { response, .. } = action {
            let _ = response.send(Err(message.to_owned()));
        }
    }
}

async fn reconnect_and_resume(
    connection: &AuthenticatedConnection,
    cancel: &CancellationToken,
) -> Result<(SessionSocket, HelloIdentity, SessionSecrets, SnapshotDto, CapabilitiesDto), String> {
    let request_id = connection.secrets.resume_request_id;
    let mut retry_index = 0_u32;
    loop {
        if cancel.is_cancelled() {
            return Err("Disconnected by user".to_owned());
        }
        let delay = match resume_retry_decision("temporary signaling error", retry_index) {
            ResumeRetryDecision::RetryAfter(delay) => delay,
            ResumeRetryDecision::FailFast => unreachable!("temporary error is retryable"),
        };
        tokio::select! {
            _ = cancel.cancelled() => return Err("Disconnected by user".to_owned()),
            _ = sleep(delay) => {}
        }
        if retry_index > 0 {
            log::info!(
                "[terminal] retrying same-session resume attempt={} delay_seconds={}",
                retry_index.saturating_add(1),
                delay.as_secs()
            );
        }
            let rendezvous_authorization =
                crate::storage::credentials::read_device_grant(&connection.profile.id, &connection.profile.fingerprint)?;
        let resume_session_id = Uuid::parse_str(&connection.secrets.session_id)
            .map_err(|_| "Stored Host session ID is invalid".to_owned())?;
        let pending = tokio::select! {
            _ = cancel.cancelled() => return Err("Disconnected by user".to_owned()),
            result = timeout(Duration::from_secs(25), begin_connection(connection.profile.clone(), rendezvous_authorization, Some(resume_session_id))) => {
                match result {
                    Ok(result) => result,
                    Err(_) => Err("Timed out re-establishing the pinned Host transport".to_owned()),
                }
            },
        };
        let (mut pending, _) = match pending {
            Ok(pending) => pending,
            Err(error) => {
                if resume_retry_decision(&error, retry_index) == ResumeRetryDecision::FailFast {
                    return Err(error);
                }
                if is_room_busy(&error) {
                    log::warn!(
                        "[terminal] rendezvous room busy during same-session resume; retry_index={}",
                        retry_index
                    );
                }
                retry_index = retry_index.saturating_add(1);
                continue;
            }
        };
        if pending.identity.server_epoch != connection.identity.server_epoch {
            log::info!("[terminal] Host epoch changed; authenticating the paired device on the new Host process");
            let terminal_identity = crate::storage::credentials::TerminalDeviceIdentity::load_or_create()?;
            let fresh = finish_device_auth(pending, &terminal_identity, None).await?;
            crate::storage::credentials::save_device_grant(
                &fresh.profile.id, &fresh.profile.fingerprint, &fresh.device_grant,
            )?;
            return Ok((fresh.socket, fresh.identity, fresh.secrets, fresh.initial_snapshot, fresh.capabilities));
        }
        let resume = json!({
            "type": "auth.resume",
            "protocol": PROTOCOL_VERSION,
            "request_id": request_id,
            "session_id": connection.secrets.session_id,
            "resume_ticket": &*connection.secrets.resume_ticket,
            "client_nonce": pending.identity.client_nonce
        });
        if let Err(error) = send_json(&mut pending.socket, &resume).await {
            if resume_retry_decision(&error, retry_index) == ResumeRetryDecision::FailFast {
                return Err(error);
            }
            retry_index = retry_index.saturating_add(1);
            continue;
        }
        let auth = match timeout(CONNECT_TIMEOUT, wait_for_type(&mut pending.socket, "auth.result")).await {
            Ok(Ok(auth)) => auth,
            Ok(Err(error)) => {
                if error.to_ascii_uppercase().contains("RESUME_TICKET_INVALID") {
                    log::info!("[terminal] Host session ticket expired; authenticating the saved device key");
                    let terminal_identity = crate::storage::credentials::TerminalDeviceIdentity::load_or_create()?;
                    let fresh = finish_device_auth(pending, &terminal_identity, None).await?;
                    crate::storage::credentials::save_device_grant(
                        &fresh.profile.id, &fresh.profile.fingerprint, &fresh.device_grant,
                    )?;
                    return Ok((fresh.socket, fresh.identity, fresh.secrets, fresh.initial_snapshot, fresh.capabilities));
                }
                if resume_retry_decision(&error, retry_index) == ResumeRetryDecision::FailFast {
                    return Err(error);
                }
                retry_index = retry_index.saturating_add(1);
                continue;
            }
            Err(_) => {
                retry_index = retry_index.saturating_add(1);
                continue;
            }
        };
        if auth.get("request_id").and_then(Value::as_str) != Some(request_id.to_string().as_str())
            || auth.get("session_state").and_then(Value::as_str) != Some("ACTIVE")
        {
            return Err("Host returned an invalid session resume response".to_owned());
        }
        if auth.get("session_id").and_then(Value::as_str) != Some(connection.secrets.session_id.as_str())
            || auth.get("server_epoch").and_then(Value::as_str) != Some(connection.identity.server_epoch.as_str())
        {
            return Err("Host restarted; sign in again to create a new session".to_owned());
        }
        let secrets = parse_session_secrets(&auth)?;
        if secrets.generation <= connection.secrets.generation {
            return Err("Host did not fence the previous transport generation during resume".to_owned());
        }
        let snapshot = match wait_for_type(&mut pending.socket, "snapshot")
            .await
            .and_then(|value| snapshot_from_frame(&value))
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if resume_retry_decision(&error, retry_index) == ResumeRetryDecision::FailFast {
                    return Err(error);
                }
                retry_index = retry_index.saturating_add(1);
                continue;
            }
        };
        let capabilities_request_id = send_read_request(&mut pending.socket, "capabilities.get", None).await?;
        let capabilities_frame = wait_for_type(&mut pending.socket, "capabilities").await?;
        if capabilities_frame.get("request_id").and_then(Value::as_str)
            != Some(capabilities_request_id.to_string().as_str())
        {
            return Err("Host returned capabilities for a different request".to_owned());
        }
        let capabilities = parse_capabilities(&capabilities_frame)?;
        let identity = pending.identity;
        return Ok((pending.socket, identity, secrets, snapshot, capabilities));
    }
}

async fn send_logout(socket: &mut SessionSocket) -> Result<(), String> {
    let request_id = Uuid::new_v4();
    send_json(
        socket,
        &json!({
            "type": "auth.logout", "protocol": PROTOCOL_VERSION, "request_id": request_id
        }),
    )
    .await?;
    let _ = timeout(Duration::from_secs(2), wait_for_type(socket, "auth.logged_out")).await;
    socket.close().await;
    Ok(())
}

async fn send_read_request(
    socket: &mut SessionSocket,
    message_type: &str,
    fields: Option<Value>,
) -> Result<Uuid, String> {
    let request_id = Uuid::new_v4();
    let mut frame = json!({ "type": message_type, "protocol": PROTOCOL_VERSION, "request_id": request_id });
    if let (Some(object), Some(fields)) = (frame.as_object_mut(), fields) {
        if let Some(fields) = fields.as_object() {
            object.extend(fields.clone());
        }
    }
    send_json(socket, &frame).await?;
    Ok(request_id)
}

async fn send_json(socket: &mut SessionSocket, value: &Value) -> Result<(), String> {
    socket.send_json(value).await
}

async fn receive_json(socket: &mut SessionSocket) -> Result<Value, String> {
    let message = timeout(CONNECT_TIMEOUT, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => return Ok(text),
                Some(Ok(Message::Ping(payload))) => socket.send_message(Message::Pong(payload)).await?,
                Some(Ok(Message::Pong(_))) | Some(Ok(Message::Frame(_))) => continue,
                Some(Ok(Message::Close(_))) | None => return Err("Host closed the transport".to_owned()),
                Some(Ok(Message::Binary(_))) => return Err("Host sent a non-JSON transport frame".to_owned()),
                Some(Err(error)) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| "Timed out waiting for a Host response".to_owned())??;
    serde_json::from_str(message.as_str()).map_err(|_| "Host returned invalid JSON".to_owned())
}

async fn wait_for_type(socket: &mut SessionSocket, expected: &str) -> Result<Value, String> {
    loop {
        let frame = receive_json(socket).await?;
        match frame_type(&frame) {
            Some(actual) if actual == expected => return Ok(frame),
            Some("error") => return Err(remote_frame_error(&frame)),
            Some("snapshot") if expected == "capabilities" => continue,
            Some("server.notice") => continue,
            _ => return Err(format!("Host response did not match the expected {expected} message")),
        }
    }
}

fn frame_type(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

fn remote_frame_error(value: &Value) -> String {
    let code = value.get("code").and_then(Value::as_str).unwrap_or("HOST_ERROR");
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Host rejected the request");
    format!("{code}: {message}")
}

fn string_field<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Host response omitted {name}"))
}

fn snapshot_from_frame(value: &Value) -> Result<SnapshotDto, String> {
    let state = value
        .get("state")
        .cloned()
        .ok_or_else(|| "Host snapshot did not contain state".to_owned())?;
    serde_json::from_value(state).map_err(|_| "Host returned a snapshot that does not match Remote API v1".to_owned())
}

fn parse_capabilities(value: &Value) -> Result<CapabilitiesDto, String> {
    serde_json::from_value(value.get("capabilities").cloned().unwrap_or_else(|| value.clone()))
        .map_err(|_| "Host returned invalid capabilities".to_owned())
}

fn parse_catalog(value: &Value) -> Result<CatalogDto, String> {
    serde_json::from_value(value.clone()).map_err(|_| "Host returned an invalid catalog".to_owned())
}

async fn publish_connected(
    updates: &crate::app_state::UiBridge,
    connection: &AuthenticatedConnection,
    phase: &str,
    message: Option<String>,
) {
    let (transport, ice_candidate_pair) = reported_transport(
        phase,
        connection.socket.transport_name(),
        connection.socket.ice_candidate_pair(),
    );
    updates
        .publish(UiEvent::Connection(ConnectionStateDto {
            phase: phase.to_owned(),
            transport,
            ice_candidate_pair,
            profile_id: Some(connection.profile.id.clone()),
            host_label: Some(connection.profile.label.clone()),
            fingerprint: Some(connection.identity.fingerprint.clone()),
            server_id: Some(connection.identity.server_id.clone()),
            last_heartbeat: Some(format!(
                "{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            )),
            message,
        }))
        .await
}

fn reported_transport(
    phase: &str,
    transport: &str,
    ice_candidate_pair: Option<String>,
) -> (Option<String>, Option<String>) {
    if phase == "online" {
        (Some(transport.to_owned()), ice_candidate_pair)
    } else {
        (None, None)
    }
}

fn random_nonce() -> Result<(String, [u8; 32]), String> {
    let mut bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "Could not create Host identity nonce".to_owned())?;
    Ok((URL_SAFE_NO_PAD.encode(bytes), bytes))
}

fn pin_or_network_error(fingerprint: &str, message: String) -> String {
    let message_lower = message.to_ascii_lowercase();
    if message_lower.contains("certificate") || message_lower.contains("tls") || message_lower.contains("pin") {
        format!(
            "TLS identity pin verification failed for Host {fingerprint}. Confirm the saved fingerprint against the Host display."
        )
    } else {
        format!("Could not reach the Host over Direct HTTPS/WSS: {message}")
    }
}

#[cfg(test)]
mod resume_retry_tests {
    use super::*;

    #[test]
    fn rendezvous_room_busy_is_retryable_but_invalid_resume_is_terminal() {
        assert_eq!(
            resume_retry_decision("rendezvous rejected the room: room_busy", 0),
            ResumeRetryDecision::RetryAfter(Duration::from_secs(1))
        );
        assert!(is_room_busy("rendezvous rejected the room: room_busy"));
        assert!(!is_room_busy("RESUME_TICKET_INVALID"));
        assert_eq!(resume_retry_decision("RESUME_TICKET_INVALID", 0), ResumeRetryDecision::FailFast);
        assert_eq!(resume_retry_decision("SESSION_REVOKED", 0), ResumeRetryDecision::FailFast);
    }

    #[test]
    fn resume_retry_backoff_is_capped() {
        assert_eq!(
            resume_retry_decision("temporary signaling error", 1),
            ResumeRetryDecision::RetryAfter(Duration::from_secs(2))
        );
        assert_eq!(
            resume_retry_decision("temporary signaling error", 2),
            ResumeRetryDecision::RetryAfter(Duration::from_secs(4))
        );
        assert_eq!(
            resume_retry_decision("temporary signaling error", 20),
            ResumeRetryDecision::RetryAfter(Duration::from_secs(4))
        );
    }

    #[test]
    fn stale_transport_pair_is_cleared_outside_online_phase() {
        let selected_pair = Some("LOCAL RELAY/UDP ↔ REMOTE RELAY/UDP".to_owned());

        assert_eq!(
            reported_transport("reconnecting", "webrtc_datachannel", selected_pair.clone()),
            (None, None)
        );
        assert_eq!(
            reported_transport("disconnected", "webrtc_datachannel", selected_pair),
            (None, None)
        );
        assert_eq!(
            reported_transport("online", "webrtc_datachannel", Some("pair".to_owned())),
            (Some("webrtc_datachannel".to_owned()), Some("pair".to_owned()))
        );
    }
}

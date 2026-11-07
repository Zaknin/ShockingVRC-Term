use std::pin::Pin;
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    sync::Arc,
    time::Duration,
};

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
    sync::{mpsc, oneshot, watch},
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
        MAX_FRAME_BYTES, PROTOCOL_VERSION, ProfileDto, SnapshotContextDto, SnapshotDto, SnapshotEventDto, UiEvent,
    },
    transport::tls_pin::{PinnedTls, pinned_tls_config},
};

type DirectSocket = WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

#[derive(Debug, PartialEq, Eq)]
enum TurnRefreshOutcome {
    Rescheduled(Duration),
    Reconnect(ReconnectCause),
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReconnectCause {
    HeartbeatTimeout,
    SignalingLeaseLost,
    PermanentTurnRenewalFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SignalingLossPolicy {
    PreserveDirectDataChannel,
    ResumeRelayOrUnknown,
}

fn signaling_loss_policy(selected_pair_uses_relay: Option<bool>) -> SignalingLossPolicy {
    if selected_pair_uses_relay == Some(false) {
        SignalingLossPolicy::PreserveDirectDataChannel
    } else {
        SignalingLossPolicy::ResumeRelayOrUnknown
    }
}

fn reconnect_status_message(cause: ReconnectCause) -> &'static str {
    match cause {
        ReconnectCause::SignalingLeaseLost => "Rendezvous signaling ended; reconnecting the same authenticated relay session",
        ReconnectCause::PermanentTurnRenewalFailure => "TURN renewal needs a new signaling room; reconnecting the same authenticated session",
        ReconnectCause::HeartbeatTimeout => "Host heartbeat was missed; trying to resume the same session",
    }
}

fn permanent_turn_refresh_outcome(
    error: &shocking_vrc_rtc::SessionError,
    disposition: shocking_vrc_rtc::TurnRefreshFailureDisposition,
) -> Option<TurnRefreshOutcome> {
    match error {
        shocking_vrc_rtc::SessionError::Closed => Some(
            if disposition == shocking_vrc_rtc::TurnRefreshFailureDisposition::ReconnectRelaySession {
                TurnRefreshOutcome::Reconnect(ReconnectCause::SignalingLeaseLost)
            } else {
                TurnRefreshOutcome::Disabled
            },
        ),
        shocking_vrc_rtc::SessionError::Rendezvous(_) | shocking_vrc_rtc::SessionError::UnexpectedSignal(_) => Some(
            if disposition == shocking_vrc_rtc::TurnRefreshFailureDisposition::ReconnectRelaySession {
                TurnRefreshOutcome::Reconnect(ReconnectCause::PermanentTurnRenewalFailure)
            } else {
                TurnRefreshOutcome::Disabled
            },
        ),
        _ => None,
    }
}

enum SessionSocket {
    Direct(DirectSocket),
    Rtc {
        inbound: mpsc::Receiver<Vec<u8>>,
        outbound: Option<mpsc::Sender<Vec<u8>>>,
        peer: Option<Arc<shocking_vrc_rtc::RtcPeer>>,
        channels: Option<shocking_vrc_rtc::RtcApiChannels>,
        signaling: Option<shocking_vrc_rtc::SignalingLease>,
        signaling_status: Option<watch::Receiver<shocking_vrc_rtc::SignalingLeaseStatus>>,
        ice_candidate_pair: Option<String>,
        ice_servers_configured: bool,
        rendezvous_stun_received: bool,
        turn_credentials_received: bool,
        local_candidates_gathered: bool,
        room_id: uuid::Uuid,
        turn_refresh_at: Option<tokio::time::Instant>,
    },
}

impl SessionSocket {
    fn from_rtc(mut session: shocking_vrc_rtc::EstablishedRtcSession) -> Result<Self, String> {
        let (inbound, outbound) = session.channels.take().map_err(|error| error.to_string())?;
        let room_id = session.room_id;
        let ice_servers_configured = session.ice_servers_configured;
        let rendezvous_stun_received = session.rendezvous_stun_received;
        let turn_credentials_received = session.turn_credentials_received;
        let local_candidates_gathered = session.local_candidates_gathered;
        let signaling_status = session.signaling.as_ref().map(shocking_vrc_rtc::SignalingLease::subscribe_status);
        Ok(Self::Rtc {
            inbound,
            outbound: Some(outbound),
            peer: Some(session.peer),
            channels: Some(session.channels),
            signaling: session.signaling.take(),
            signaling_status,
            ice_candidate_pair: session.ice_candidate_pair,
            ice_servers_configured,
            rendezvous_stun_received,
            turn_credentials_received,
            local_candidates_gathered,
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

    fn received_turn_credentials(&self) -> bool {
        matches!(
            self,
            Self::Rtc {
                turn_credentials_received: true,
                ..
            }
        )
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

    fn ice_status(&self) -> (Option<bool>, Option<bool>, Option<bool>, Option<bool>) {
        match self {
            Self::Rtc {
                ice_servers_configured,
                rendezvous_stun_received,
                turn_credentials_received,
                local_candidates_gathered,
                ..
            } => (
                Some(*ice_servers_configured),
                Some(*rendezvous_stun_received),
                Some(*turn_credentials_received),
                Some(*local_candidates_gathered),
            ),
            Self::Direct(_) => (None, None, None, None),
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

    fn signaling_status_receiver(&self) -> Option<watch::Receiver<shocking_vrc_rtc::SignalingLeaseStatus>> {
        match self {
            Self::Rtc { signaling_status, .. } => signaling_status.clone(),
            Self::Direct(_) => None,
        }
    }

    async fn selected_pair_uses_relay(&self) -> Option<bool> {
        match self {
            Self::Rtc { peer: Some(peer), .. } => peer.selected_pair_uses_relay().await,
            _ => None,
        }
    }

    async fn disable_signaling(&mut self) {
        if let Self::Rtc { signaling, turn_refresh_at, .. } = self {
            *turn_refresh_at = None;
            if let Some(signaling) = signaling.take() {
                signaling.close().await;
            }
        }
    }

    async fn refresh_turn_credentials(&mut self) -> Result<TurnRefreshOutcome, String> {
        let Self::Rtc {
            peer: Some(peer),
            signaling: Some(signaling),
            room_id,
            turn_refresh_at,
            ..
        } = self
        else {
            return Ok(TurnRefreshOutcome::Disabled);
        };
        let disposition = shocking_vrc_rtc::turn_refresh_failure_disposition(peer.selected_pair_uses_relay().await);
        log::info!("[terminal] requesting TURN credential renewal room={room_id}");
        let credentials = match signaling.refresh_turn_credentials().await {
            Ok(credentials) => credentials,
            Err(error) => {
                if let Some(outcome) = permanent_turn_refresh_outcome(&error, disposition) {
                    *turn_refresh_at = None;
                    match outcome {
                        TurnRefreshOutcome::Reconnect(ReconnectCause::SignalingLeaseLost) => log::info!(
                            "[terminal] Rendezvous closed the signaling lease during scheduled TURN renewal; resuming the authenticated relay session in a new room"
                        ),
                        TurnRefreshOutcome::Reconnect(_) => log::error!(
                            "[terminal] permanent TURN refresh response on the relay path; resuming the authenticated session in a new room"
                        ),
                        TurnRefreshOutcome::Disabled => log::warn!(
                            "[terminal] permanent TURN refresh response on the direct path; preserving the active DataChannel and stopping renewal"
                        ),
                        TurnRefreshOutcome::Rescheduled(_) => unreachable!("permanent errors are never rescheduled"),
                    }
                    return Ok(outcome);
                }
                return Err(error.to_string());
            }
        };
        match credentials {
            Some(credentials) => {
                let delay = credentials
                    .refresh_at
                    .saturating_duration_since(tokio::time::Instant::now());
                if let Err(error) = peer.update_turn_credentials(credentials.server).await {
                    if matches!(error, shocking_vrc_rtc::RtcError::Configuration(_)) {
                        *turn_refresh_at = None;
                        if disposition == shocking_vrc_rtc::TurnRefreshFailureDisposition::ReconnectRelaySession {
                            log::error!(
                                "[terminal] TURN endpoint configuration changed for the active relay; resuming the authenticated session to apply the new configuration: {error}"
                            );
                            return Ok(TurnRefreshOutcome::Reconnect(ReconnectCause::PermanentTurnRenewalFailure));
                        }
                        log::error!(
                            "[terminal] TURN endpoint configuration changed while the selected path is direct; renewal stopped until the next ICE room: {error}"
                        );
                        return Ok(TurnRefreshOutcome::Disabled);
                    }
                    return Err(error.to_string());
                }
                *turn_refresh_at = Some(credentials.refresh_at);
                if disposition == shocking_vrc_rtc::TurnRefreshFailureDisposition::ReconnectRelaySession {
                    log::info!(
                        "[terminal] staged fresh TURN credentials for a future ICE gathering; active relay allocation remains bound to its original username and was not reauthenticated; monotonic_ms={} next_refresh_in_seconds={}",
                        shocking_vrc_rtc::monotonic_ms(),
                        delay.as_secs()
                    );
                } else {
                    log::info!(
                        "[terminal] staged fresh TURN credentials for a future ICE gathering while preserving the active direct path; monotonic_ms={} next_refresh_in_seconds={}",
                        shocking_vrc_rtc::monotonic_ms(),
                        delay.as_secs()
                    );
                }
                Ok(TurnRefreshOutcome::Rescheduled(delay))
            }
            None => {
                *turn_refresh_at = None;
                if disposition == shocking_vrc_rtc::TurnRefreshFailureDisposition::ReconnectRelaySession {
                    log::error!(
                        "[terminal] Rendezvous no longer provides TURN for the active relay; resuming the authenticated session to confirm service state"
                    );
                    Ok(TurnRefreshOutcome::Reconnect(ReconnectCause::PermanentTurnRenewalFailure))
                } else {
                    Ok(TurnRefreshOutcome::Disabled)
                }
            }
        }
    }

    async fn close(&mut self) {
        self.close_with_reason(shocking_vrc_rtc::PeerCloseReason::ManualDisconnect).await;
    }

    async fn close_with_reason(&mut self, reason: shocking_vrc_rtc::PeerCloseReason) {
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
                if let Some(signaling) = signaling.take() {
                    // Notify Rendezvous first so Host can release this room even if
                    // local DataChannel shutdown is delayed.
                    signaling.close().await;
                }
                if let (Some(channels), Some(peer)) = (channels.take(), peer.take()) {
                    channels.close_with_reason(&peer, reason).await;
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
                        if let Some(signaling) = signal_close {
                            signaling.close().await;
                        }
                        if let Some((channels, peer)) = rtc_close {
                            channels.close(&peer).await;
                        }
                    });
                }
            }
        }
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const HEARTBEAT_PERIOD: Duration = Duration::from_secs(3);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT_PROBE_LEAD: Duration = Duration::from_millis(500);
const RESUME_WINDOW: Duration = Duration::from_secs(25);
const RESUME_CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(8);
const RESUME_TURN_CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(12);
const STALE_TRANSPORT_CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_PENDING_PING_NONCES: usize = 8;
const MAX_RESUME_RETRY_DELAY: Duration = Duration::from_secs(4);
const ROOM_BUSY_RETRY_DELAY: Duration = Duration::from_millis(250);
const MAX_ROOM_BUSY_RETRIES: usize = 8;
const SESSION_END_ACK_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteSessionEndReason {
    HostKick,
    SessionRevoked,
    HostUnavailable,
}

fn parse_session_end(
    value: &Value,
    identity: &HelloIdentity,
    secrets: &SessionSecrets,
) -> Option<RemoteSessionEndReason> {
    if value.get("protocol").and_then(Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
        || value.get("server_epoch").and_then(Value::as_str) != Some(identity.server_epoch.as_str())
    {
        return None;
    }

    let message_type = value.get("type").and_then(Value::as_str)?;
    let is_bound = value.get("session_id").and_then(Value::as_str) == Some(secrets.session_id.as_str())
        && value.get("transport_generation").and_then(Value::as_u64) == Some(secrets.generation);
    match message_type {
        "session.ended" | "session.revoked" if !is_bound => return None,
        "session.ended" | "session.revoked" => {}
        _ => return None,
    }

    match value.get("reason").and_then(Value::as_str)? {
        "host_kick" => Some(RemoteSessionEndReason::HostKick),
        "session_revoked" => Some(RemoteSessionEndReason::SessionRevoked),
        "host_unavailable" => Some(RemoteSessionEndReason::HostUnavailable),
        _ => None,
    }
}

async fn acknowledge_session_end(connection: &mut AuthenticatedConnection) -> bool {
    let acknowledgement = json!({
        "type": "session.ended.ack",
        "protocol": PROTOCOL_VERSION,
        "server_epoch": connection.identity.server_epoch,
        "session_id": connection.secrets.session_id,
        "transport_generation": connection.secrets.generation,
    });
    matches!(
        timeout(SESSION_END_ACK_TIMEOUT, connection.socket.send_json(&acknowledgement)).await,
        Ok(Ok(()))
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeRetryDecision {
    RetryAfter(Duration),
    FailFast,
}

#[derive(Debug)]
enum ResumeError {
    Transport(String),
    Ended(RemoteSessionEndReason),
    HostUnavailable(String),
}

type ResumeOutput = (SessionSocket, HelloIdentity, SessionSecrets, SnapshotDto, u64, CapabilitiesDto);
type ResumeAttempt<'a> = Pin<Box<dyn Future<Output = Result<ResumeOutput, ResumeError>> + Send + 'a>>;
type ResumeOperation =
    dyn for<'a> FnMut(&'a AuthenticatedConnection, &'a CancellationToken) -> ResumeAttempt<'a> + Send;
type PendingConnectOutput = Result<(PendingConnection, HostIdentityDto), String>;
type PendingConnectAttempt<'a> = Pin<Box<dyn Future<Output = PendingConnectOutput> + Send + 'a>>;
type PendingConnector = dyn for<'a> FnMut(&'a ProfileDto, Uuid) -> PendingConnectAttempt<'a> + Send;

impl std::fmt::Display for ResumeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => formatter.write_str(error),
            Self::Ended(reason) => write!(formatter, "Host ended the session: {reason:?}"),
            Self::HostUnavailable(message) => formatter.write_str(message),
        }
    }
}

impl From<String> for ResumeError {
    fn from(error: String) -> Self {
        Self::Transport(error)
    }
}

enum ResumeAuthResponse {
    Authenticated(Value),
    Ended(Value),
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

fn session_end_reason_from_protocol_error(error: &str) -> Option<RemoteSessionEndReason> {
    let code = error.split_once(':').map_or(error, |(code, _)| code).trim();
    if code == "SESSION_REVOKED" {
        Some(RemoteSessionEndReason::SessionRevoked)
    } else {
        None
    }
}

fn is_room_busy(error: &str) -> bool {
    error.to_ascii_lowercase().contains("room_busy")
}

async fn retry_room_busy<T, E, Attempt, AttemptFuture>(mut attempt: Attempt) -> Result<T, E>
where
    E: std::fmt::Display,
    Attempt: FnMut() -> AttemptFuture,
    AttemptFuture: std::future::Future<Output = Result<T, E>>,
{
    for retry_index in 0..=MAX_ROOM_BUSY_RETRIES {
        match attempt().await {
            Err(error) if is_room_busy(&error.to_string()) && retry_index < MAX_ROOM_BUSY_RETRIES => {
                log::warn!(
                    "[terminal] Rendezvous room is still closing; retrying ICE join attempt={}",
                    retry_index + 2
                );
                sleep(ROOM_BUSY_RETRY_DELAY).await;
            }
            result => return result,
        }
    }
    unreachable!("the bounded room-busy loop returns on its final attempt")
}

async fn resume_within_window<T>(
    resume: impl std::future::Future<Output = Result<T, ResumeError>>,
) -> Result<T, ResumeError> {
    let started = tokio::time::Instant::now();
    log::info!(
        "[terminal] same-session resume deadline started monotonic_ms={} deadline_ms={}",
        shocking_vrc_rtc::monotonic_ms(),
        RESUME_WINDOW.as_millis()
    );
    match timeout(RESUME_WINDOW, resume).await {
        Ok(result) => {
            log::info!(
                "[terminal] same-session resume deadline ended monotonic_ms={} elapsed_ms={} remaining_ms={} succeeded={}",
                shocking_vrc_rtc::monotonic_ms(),
                started.elapsed().as_millis(),
                RESUME_WINDOW.saturating_sub(started.elapsed()).as_millis(),
                result.is_ok()
            );
            result
        }
        Err(_) => {
            log::warn!(
                "[terminal] same-session resume deadline expired monotonic_ms={} elapsed_ms={} remaining_ms=0",
                shocking_vrc_rtc::monotonic_ms(),
                started.elapsed().as_millis()
            );
            Err(ResumeError::HostUnavailable(format!(
                "Host session resume window elapsed after {} seconds; sign in again",
                RESUME_WINDOW.as_secs()
            )))
        }
    }
}

fn resume_attempt_timeout(connection: &AuthenticatedConnection) -> Duration {
    if connection.profile.force_relay || connection.socket.received_turn_credentials() {
        RESUME_TURN_CONNECT_ATTEMPT_TIMEOUT
    } else {
        RESUME_CONNECT_ATTEMPT_TIMEOUT
    }
}

async fn resume_connect_attempt<T>(
    connect: impl Future<Output = Result<T, String>>,
    attempt_timeout: Duration,
) -> Result<T, String> {
    timeout(attempt_timeout, connect)
        .await
        .unwrap_or_else(|_| Err("Timed out re-establishing the pinned Host transport".to_owned()))
}

async fn wait_for_turn_refresh(timer: &mut Option<Pin<Box<tokio::time::Sleep>>>) {
    match timer {
        Some(timer) => timer.as_mut().await,
        None => std::future::pending().await,
    }
}

async fn wait_before_liveness_deadline<T>(
    deadline: &mut Pin<Box<tokio::time::Sleep>>,
    operation: impl Future<Output = T>,
) -> Result<T, ()> {
    tokio::select! {
        biased;
        _ = deadline.as_mut() => Err(()),
        result = operation => Ok(result),
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
    pub initial_snapshot_seq: u64,
    pub capabilities: CapabilitiesDto,
    pub device_grant: Zeroizing<String>,
    pub session_generation: u64,
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
    if profile.force_relay && profile.connection_mode == "direct_only" {
        return Err("Relay-only mode requires an ICE traversal profile; Direct-only mode cannot use TURN".to_owned());
    }
    let direct_error = if direct_attempt_allowed(
        &profile.connection_mode,
        profile.force_relay,
        profile.host.is_some(),
        profile.port.is_some(),
    ) {
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
    } else if profile.connection_mode == "direct_only" {
        return Err("Direct-only profile has no Direct Host endpoint".to_owned());
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
    let rendezvous_authorization = rendezvous_authorization
        .ok_or_else(|| "Pair this Terminal with a Host before using ICE traversal".to_owned())?;
    let ice_servers: Vec<shocking_vrc_rtc::IceServerConfig> = profile
        .ice_server_urls
        .iter()
        .map(|url| shocking_vrc_rtc::IceServerConfig {
            urls: vec![url.clone()],
            username: None,
            credential: None,
        })
        .collect();
    let established = match retry_room_busy(|| {
        let config = shocking_vrc_rtc::TerminalRtcConfig {
            rendezvous_url: profile.rendezvous_url.clone(),
            host_id: profile.server_id.clone(),
            rendezvous_authorization: Zeroizing::new(rendezvous_authorization.to_string()),
            expected_fingerprint: profile.fingerprint.clone(),
            ice_servers: ice_servers.clone(),
            udp_addrs: vec!["0.0.0.0:0".to_owned()],
            force_relay: profile.force_relay,
            resume_session_id,
        };
        async move { shocking_vrc_rtc::join_terminal_room(config).await }
    })
    .await
    {
        Ok(established) => established,
        Err(error) => {
            let error = rendezvous_join_error(&error.to_string());
            log::error!("[terminal] ICE room setup failed: {error}");
            return Err(match direct_error {
                Some(direct_error) => format!("Direct WSS failed ({direct_error}); ICE traversal failed ({error})"),
                None => error,
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

fn direct_attempt_allowed(connection_mode: &str, force_relay: bool, has_host: bool, has_port: bool) -> bool {
    !force_relay && connection_mode != "traversal_only" && has_host && has_port
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
    let base_url = profile
        .http_base_url()
        .ok_or_else(|| "Direct Host endpoint is not configured".to_owned())?;
    let url = format!("{base_url}api/v1/health");
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
    let base_url = profile
        .wss_base_url()
        .ok_or_else(|| "Direct Host endpoint is not configured".to_owned())?;
    let url = format!("{base_url}api/v1/ws");
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
    let challenge_id = challenge
        .get("challenge_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| "Host returned an invalid device challenge".to_owned())?;
    if challenge.get("device_id").and_then(Value::as_str) != Some(terminal_identity.device_id.as_str()) {
        return Err("Host challenge is bound to a different Terminal device".to_owned());
    }
    let nonce_bytes = challenge
        .get("server_nonce")
        .and_then(Value::as_str)
        .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
        .ok_or_else(|| "Host returned an invalid device nonce".to_owned())?;
    let server_nonce: [u8; 32] = nonce_bytes
        .try_into()
        .map_err(|_| "Host returned an invalid device nonce".to_owned())?;
    let invitation_id = invitation.map(|value| value.invitation_id);
    let pairing_secret = if let Some(invitation) = invitation {
        Some(Zeroizing::new(invitation.pairing_secret.clone()))
    } else {
        None
    };
    let raw_secret = pairing_secret
        .as_ref()
        .map(|secret| {
            URL_SAFE_NO_PAD
                .decode(secret.as_str())
                .map(Zeroizing::new)
                .map_err(|_| "Invitation pairing secret is invalid".to_owned())
        })
        .transpose()?;
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
    )
    .await?;
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
    let (snapshot, snapshot_seq) = snapshot_from_frame(&snapshot_value, &pending.identity.server_epoch)?;
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
        initial_snapshot_seq: snapshot_seq,
        capabilities,
        device_grant: Zeroizing::new(device_grant),
        session_generation: 0,
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
    connection: AuthenticatedConnection,
    actions: mpsc::Receiver<SessionAction>,
    updates: Arc<crate::app_state::UiBridge>,
    cancel: CancellationToken,
) {
    run_session_with_resume(
        connection,
        actions,
        updates,
        cancel,
        Box::new(|connection, cancel| Box::pin(reconnect_and_resume(connection, cancel))),
    )
    .await;
}

async fn run_session_with_resume(
    mut connection: AuthenticatedConnection,
    mut actions: mpsc::Receiver<SessionAction>,
    updates: Arc<crate::app_state::UiBridge>,
    cancel: CancellationToken,
    mut resume: Box<ResumeOperation>,
) {
    log::info!(
        "[terminal] authenticated session loop started transport={}",
        connection.transport_name()
    );
    let room_id = connection.socket.room_id();
    let candidate_pair = connection.socket.ice_candidate_pair();
    updates.diagnostics.record(
        crate::diagnostics::DiagnosticKind::ConnectionEstablished,
        room_id, None, Some(connection.secrets.generation),
        Some(connection.transport_name()), candidate_pair.as_deref(),
    );
    updates.diagnostics.record(
        crate::diagnostics::DiagnosticKind::SessionStarted,
        room_id, None, Some(connection.secrets.generation),
        Some(connection.transport_name()), candidate_pair.as_deref(),
    );
    publish_connected(&updates, &connection, "online", None).await;
    updates.diagnostics.record(
        crate::diagnostics::DiagnosticKind::SnapshotResponse,
        room_id, None, Some(connection.secrets.generation),
        Some(connection.transport_name()), candidate_pair.as_deref(),
    );
    updates
        .publish(UiEvent::Snapshot(snapshot_event(
            connection.initial_snapshot.clone(),
            connection.initial_snapshot_seq,
            &connection,
        )))
        .await;
    updates
        .publish(UiEvent::Capabilities(connection.capabilities.clone()))
        .await;

    let mut heartbeat = interval(HEARTBEAT_PERIOD);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut liveness_deadline = Box::pin(sleep(HEARTBEAT_TIMEOUT));
    let mut liveness_probe = Box::pin(sleep(HEARTBEAT_TIMEOUT - HEARTBEAT_PROBE_LEAD));
    let mut pending_ping_nonces = VecDeque::<(String, tokio::time::Instant)>::with_capacity(MAX_PENDING_PING_NONCES);
    let mut socket_dead = false;
    let mut reconnect_cause = None;
    let mut refresh_at = connection.secrets.access_expires_at - Duration::from_secs(60);
    let mut refresh_timer = Box::pin(sleep(refresh_at.saturating_duration_since(Instant::now())));
    let mut refresh_request_id: Option<Uuid> = None;
    let mut pending_commands: HashMap<Uuid, oneshot::Sender<Result<CommandAckDto, String>>> = HashMap::new();
    let mut latest_snapshot_seq = connection.initial_snapshot_seq;
    let mut turn_refresh_timer = connection
        .socket
        .turn_refresh_delay()
        .map(|delay| Box::pin(sleep(delay)));
    let mut signaling_status = connection.socket.signaling_status_receiver();

    loop {
        pending_commands.retain(|_, response| !response.is_closed());
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                fail_pending_commands(&mut pending_commands, "Host session ended before command acknowledgement; outcome is unknown.");
                let _ = send_logout(&mut connection.socket).await;
                break;
            }
            action = actions.recv(), if !liveness_deadline.as_ref().is_elapsed() => {
                match action {
                    Some(SessionAction::Disconnect) => {
                        fail_pending_commands(&mut pending_commands, "Disconnected before Host command acknowledgement; outcome is unknown.");
                        let _ = send_logout(&mut connection.socket).await;
                        break;
                    }
                    Some(SessionAction::FetchState) => {
                        match wait_before_liveness_deadline(&mut liveness_deadline, send_read_request(&mut connection.socket, "state.get", None)).await {
                            Ok(Ok(request_id)) => updates.diagnostics.record(
                                crate::diagnostics::DiagnosticKind::SnapshotRequest,
                                connection.socket.room_id(), Some(request_id),
                                Some(connection.secrets.generation), Some(connection.transport_name()),
                                connection.socket.ice_candidate_pair().as_deref(),
                            ),
                            _ => continue,
                        }
                    }
                    Some(SessionAction::FetchCapabilities) => {
                        if wait_before_liveness_deadline(&mut liveness_deadline, send_read_request(&mut connection.socket, "capabilities.get", None)).await.is_err() { continue; }
                    }
                    Some(SessionAction::FetchCatalog(kind)) if matches!(kind.as_str(), "presets" | "zone_presets") => {
                        let request = send_read_request(&mut connection.socket, "catalogs.get", Some(json!({"catalog": kind})));
                        if wait_before_liveness_deadline(&mut liveness_deadline, request).await.is_err() { continue; }
                    }
                    Some(SessionAction::FetchLogs(cursor, limit)) => {
                        let request = send_read_request(&mut connection.socket, "logs.get", Some(json!({"cursor": cursor, "limit": limit.clamp(1, 200)})));
                        if wait_before_liveness_deadline(&mut liveness_deadline, request).await.is_err() { continue; }
                    }
                    Some(SessionAction::Command { request_id, frame, response }) => {
                        match wait_before_liveness_deadline(&mut liveness_deadline, send_json(&mut connection.socket, &frame)).await {
                            Err(()) => {
                                let _ = response.send(Err("Connection interrupted before Host command acknowledgement; outcome is unknown. The command was not replayed.".to_owned()));
                                continue;
                            }
                            Ok(Err(error)) => {
                                let _ = response.send(Err(format!("Host command acknowledgement was not received; outcome is unknown: {error}")));
                            }
                            Ok(Ok(())) => { pending_commands.insert(request_id, response); }
                        }
                    }
                    _ => {}
                }
            }
            _ = &mut liveness_deadline => {
                let reason = reconnect_cause.take().unwrap_or(ReconnectCause::HeartbeatTimeout);
                if reason == ReconnectCause::HeartbeatTimeout {
                    updates.diagnostics.record(
                        crate::diagnostics::DiagnosticKind::HeartbeatTimeout,
                        connection.socket.room_id(), None, Some(connection.secrets.generation),
                        Some(connection.transport_name()), connection.socket.ice_candidate_pair().as_deref(),
                    );
                }
                updates.diagnostics.record(
                    crate::diagnostics::DiagnosticKind::ResumeStarted,
                    connection.socket.room_id(), None, Some(connection.secrets.generation),
                    Some(connection.transport_name()), connection.socket.ice_candidate_pair().as_deref(),
                );
                let reconnect_message = reconnect_status_message(reason);
                match reason {
                    ReconnectCause::SignalingLeaseLost => log::info!("[terminal] relay signaling lease ended; attempting bounded same-session resume"),
                    ReconnectCause::PermanentTurnRenewalFailure => log::error!("[terminal] permanent TURN renewal failure requires a new signaling room; attempting same-session resume"),
                    ReconnectCause::HeartbeatTimeout => log::warn!("[terminal] Host heartbeat deadline elapsed; attempting same-session resume"),
                }
                let peer_close_reason = match reason {
                    ReconnectCause::PermanentTurnRenewalFailure => shocking_vrc_rtc::PeerCloseReason::TurnRotation,
                    ReconnectCause::SignalingLeaseLost => shocking_vrc_rtc::PeerCloseReason::RoomClosed,
                    ReconnectCause::HeartbeatTimeout => shocking_vrc_rtc::PeerCloseReason::ApiSessionEnded,
                };
                fail_pending_commands(&mut pending_commands, "Connection interrupted before Host command acknowledgement; outcome is unknown. The command was not replayed.");
                fail_queued_command_actions(&mut actions, "Connection interrupted before Host command acknowledgement; outcome is unknown. The command was not replayed.");
                publish_connected(&updates, &connection, "reconnecting", Some(reconnect_message.to_owned())).await;
                let (cancelled, close_result) = tokio::select! {
                    _ = cancel.cancelled() => (true, None),
                    result = timeout(STALE_TRANSPORT_CLOSE_TIMEOUT, connection.socket.close_with_reason(peer_close_reason)) => (false, Some(result)),
                };
                if cancelled {
                    publish_connected(&updates, &connection, "user_disconnected", Some("Disconnected from Host. Reconnect when ready.".to_owned())).await;
                    return;
                }
                match close_result.expect("a non-cancelled close attempt has a result") {
                    Ok(()) => log::info!("[terminal] closed stale transport before same-session resume"),
                    Err(_) => log::warn!("[terminal] stale transport close timed out; continuing same-session resume"),
                }
                let resumed = tokio::select! {
                    _ = cancel.cancelled() => Err(ResumeError::Transport("Disconnected by user".to_owned())),
                    result = resume_within_window(resume(&connection, &cancel)) => result,
                };
                match resumed {
                    Ok((socket, identity, secrets, snapshot, snapshot_seq, capabilities)) => {
                        log::info!("[terminal] same-session resume succeeded transport={}", socket.transport_name());
                        connection.socket = socket;
                        connection.identity = identity;
                        connection.secrets = secrets;
                        updates.diagnostics.record(
                            crate::diagnostics::DiagnosticKind::ResumeSucceeded,
                            connection.socket.room_id(), None, Some(connection.secrets.generation),
                            Some(connection.transport_name()), connection.socket.ice_candidate_pair().as_deref(),
                        );
                        connection.initial_snapshot = snapshot.clone();
                        connection.initial_snapshot_seq = snapshot_seq;
                        latest_snapshot_seq = snapshot_seq;
                        connection.capabilities = capabilities.clone();
                        liveness_deadline.as_mut().reset(tokio::time::Instant::now() + HEARTBEAT_TIMEOUT);
                        liveness_probe.as_mut().reset(tokio::time::Instant::now() + HEARTBEAT_TIMEOUT - HEARTBEAT_PROBE_LEAD);
                        pending_ping_nonces.clear();
                        socket_dead = false;
                        publish_connected(&updates, &connection, "reconnecting", Some("Session resumed; synchronizing the fresh Host snapshot".to_owned())).await;
                        updates.publish(UiEvent::Snapshot(snapshot_event(snapshot, snapshot_seq, &connection))).await;
                        updates.publish(UiEvent::Capabilities(capabilities)).await;
                        fail_queued_command_actions(&mut actions, "Command was not sent while the Host session was reconnecting; submit it again after reviewing fresh state.");
                        publish_connected(&updates, &connection, "online", Some("Session resumed with fresh Host state".to_owned())).await;
                        refresh_at = connection.secrets.access_expires_at - Duration::from_secs(60);
                        refresh_timer.as_mut().reset(tokio::time::Instant::now() + refresh_at.saturating_duration_since(Instant::now()));
                        turn_refresh_timer = connection.socket.turn_refresh_delay().map(|delay| Box::pin(sleep(delay)));
                        reconnect_cause = None;
                        signaling_status = connection.socket.signaling_status_receiver();
                    }
                    Err(error) => {
                        updates.diagnostics.record(
                            crate::diagnostics::DiagnosticKind::ResumeFailed,
                            connection.socket.room_id(), None, Some(connection.secrets.generation),
                            Some(connection.transport_name()), connection.socket.ice_candidate_pair().as_deref(),
                        );
                        log::error!("[terminal] same-session resume failed: {error}");
                        updates.diagnostics.record(
                            crate::diagnostics::DiagnosticKind::SessionEnded,
                            connection.socket.room_id(), None, Some(connection.secrets.generation),
                            Some(connection.transport_name()), connection.socket.ice_candidate_pair().as_deref(),
                        );
                        match error {
                            ResumeError::Ended(reason) => publish_session_end(&mut connection, &updates, reason).await,
                            ResumeError::HostUnavailable(message) => {
                                publish_connected(&updates, &connection, "host_unavailable", Some(format!(
                                    "The Host did not return during the bounded resume window; reconnect manually when available. {message}"
                                ))).await;
                            }
                            ResumeError::Transport(message) if message == "Disconnected by user" => {
                                publish_connected(&updates, &connection, "user_disconnected", Some("Disconnected from Host. Reconnect when ready.".to_owned())).await;
                            }
                            ResumeError::Transport(message) if session_end_reason_from_protocol_error(&message).is_some() => {
                                publish_session_end(&mut connection, &updates, RemoteSessionEndReason::SessionRevoked).await;
                            }
                            ResumeError::Transport(message) if message.contains("RESUME_TICKET_INVALID") || message.contains("Host restarted") => {
                                publish_connected(&updates, &connection, "host_unavailable", Some("The Host no longer recognizes this session. Reconnect manually when it is available; the saved profile and pairing were left unchanged.".to_owned())).await;
                            }
                            ResumeError::Transport(message) => {
                                publish_connected(&updates, &connection, "transport_lost", Some(format!("The Host transport could not be resumed: {message}"))).await;
                            }
                        }
                        return;
                    }
                }
            }
            _ = heartbeat.tick() => {
                let nonce = Uuid::new_v4().to_string();
                let request_id = Uuid::new_v4();
                let ping_send = wait_before_liveness_deadline(&mut liveness_deadline, send_json(&mut connection.socket, &json!({
                    "type": "ping", "protocol": PROTOCOL_VERSION,
                    "request_id": request_id, "nonce": nonce
                }))).await;
                if ping_send.is_err() { continue; }
                if ping_send.expect("handled liveness timeout above").is_err() {
                    liveness_deadline.as_mut().reset(tokio::time::Instant::now());
                    socket_dead = true;
                    fail_pending_commands(&mut pending_commands, "Connection interrupted before Host command acknowledgement; outcome is unknown. The command was not replayed.");
                } else {
                    updates.diagnostics.record(
                        crate::diagnostics::DiagnosticKind::HeartbeatTx,
                        connection.socket.room_id(), Some(request_id), Some(connection.secrets.generation),
                        Some(connection.transport_name()), connection.socket.ice_candidate_pair().as_deref(),
                    );
                    log::debug!("[terminal] API heartbeat tx monotonic_ms={} probe=false", shocking_vrc_rtc::monotonic_ms());
                    pending_ping_nonces.push_back((nonce, tokio::time::Instant::now()));
                    if pending_ping_nonces.len() > MAX_PENDING_PING_NONCES {
                        pending_ping_nonces.pop_front();
                    }
                }
            }
            _ = &mut liveness_probe => {
                let nonce = Uuid::new_v4().to_string();
                let request_id = Uuid::new_v4();
                let probe_send = wait_before_liveness_deadline(&mut liveness_deadline, send_json(&mut connection.socket, &json!({
                    "type": "ping", "protocol": PROTOCOL_VERSION,
                    "request_id": request_id, "nonce": nonce
                }))).await;
                if probe_send.is_err() { continue; }
                if probe_send.expect("handled liveness timeout above").is_err() {
                    liveness_deadline.as_mut().reset(tokio::time::Instant::now());
                    socket_dead = true;
                    fail_pending_commands(&mut pending_commands, "Connection interrupted before Host command acknowledgement; outcome is unknown. The command was not replayed.");
                } else {
                    updates.diagnostics.record(
                        crate::diagnostics::DiagnosticKind::HeartbeatTx,
                        connection.socket.room_id(), Some(request_id), Some(connection.secrets.generation),
                        Some(connection.transport_name()), connection.socket.ice_candidate_pair().as_deref(),
                    );
                    log::debug!("[terminal] API heartbeat tx monotonic_ms={} probe=true", shocking_vrc_rtc::monotonic_ms());
                    pending_ping_nonces.push_back((nonce, tokio::time::Instant::now()));
                    if pending_ping_nonces.len() > MAX_PENDING_PING_NONCES {
                        pending_ping_nonces.pop_front();
                    }
                    liveness_probe.as_mut().reset(tokio::time::Instant::now() + HEARTBEAT_TIMEOUT);
                }
            }
            _ = &mut refresh_timer, if refresh_request_id.is_none() => {
                let request_id = Uuid::new_v4();
                let refresh_send = wait_before_liveness_deadline(&mut liveness_deadline, send_json(&mut connection.socket, &json!({
                    "type": "auth.token.refresh", "protocol": PROTOCOL_VERSION,
                    "request_id": request_id
                }))).await;
                if refresh_send.is_err() { continue; }
                if refresh_send.expect("handled liveness timeout above").is_ok() {
                    log::debug!("[terminal] requested access-token refresh; token value omitted");
                    refresh_request_id = Some(request_id);
                } else {
                    log::warn!("[terminal] access-token refresh request could not be sent");
                }
                refresh_timer.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(20));
            }
            _ = wait_for_turn_refresh(&mut turn_refresh_timer), if turn_refresh_timer.is_some() => {
                let refreshed = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        fail_pending_commands(&mut pending_commands, "Host session ended before command acknowledgement; outcome is unknown.");
                        let _ = send_logout(&mut connection.socket).await;
                        break;
                    }
                    result = wait_before_liveness_deadline(&mut liveness_deadline, connection.socket.refresh_turn_credentials()) => result,
                };
                if refreshed.is_err() { continue; }
                match refreshed.expect("handled liveness timeout above") {
                    Ok(TurnRefreshOutcome::Rescheduled(delay)) => {
                        turn_refresh_timer = Some(Box::pin(sleep(delay)));
                    }
                    Ok(TurnRefreshOutcome::Reconnect(cause)) => {
                        turn_refresh_timer = None;
                        reconnect_cause = Some(cause);
                        liveness_deadline.as_mut().reset(tokio::time::Instant::now());
                        continue;
                    }
                    Ok(TurnRefreshOutcome::Disabled) => {
                        log::warn!("[terminal] TURN renewal is unavailable or no longer matches the direct ICE configuration; periodic renewal stopped");
                        turn_refresh_timer = None;
                    }
                    Err(error) => {
                        log::error!("[terminal] TURN credential renewal failed: {error}; retrying shortly");
                        turn_refresh_timer = Some(Box::pin(sleep(Duration::from_secs(15))));
                    }
                }
            }
            _status = async {
                match signaling_status.as_mut() {
                    Some(status) => wait_for_signaling_lease_end(status).await,
                    None => std::future::pending().await,
                }
            } => {
                signaling_status = None;
                turn_refresh_timer = None;
                updates.diagnostics.record(
                    crate::diagnostics::DiagnosticKind::SignalingLeaseLost,
                    connection.socket.room_id(), None, Some(connection.secrets.generation),
                    Some(connection.transport_name()), connection.socket.ice_candidate_pair().as_deref(),
                );
                if signaling_loss_policy(connection.socket.selected_pair_uses_relay().await)
                    == SignalingLossPolicy::PreserveDirectDataChannel
                {
                    log::warn!("[terminal] Rendezvous signaling lease ended on a direct P2P path; preserving the authenticated DataChannel and disabling signaling-dependent renewal");
                    connection.socket.disable_signaling().await;
                    publish_connected(
                        &updates,
                        &connection,
                        "online",
                        Some("Rendezvous signaling is unavailable. The authenticated direct DataChannel remains active; reconnect to restore signaling and TURN renewal.".to_owned()),
                    )
                    .await;
                } else {
                    log::info!("[terminal] Rendezvous signaling lease ended on a relay or unknown path; beginning bounded same-session recovery");
                    reconnect_cause = Some(ReconnectCause::SignalingLeaseLost);
                    liveness_deadline.as_mut().reset(tokio::time::Instant::now());
                }
            }
            frame = connection.socket.next(), if !socket_dead => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<Value>(text.as_str()) {
                            Ok(value) => {
                                match frame_type(&value) {
                                    Some("snapshot") => if let Ok((snapshot, seq)) = snapshot_from_frame(&value, &connection.identity.server_epoch)
                                        && seq > latest_snapshot_seq
                                    {
                                        latest_snapshot_seq = seq;
                                        updates.diagnostics.record(
                                            crate::diagnostics::DiagnosticKind::SnapshotResponse,
                                            connection.socket.room_id(),
                                            value.get("request_id").and_then(Value::as_str).and_then(|raw| Uuid::parse_str(raw).ok()),
                                            Some(connection.secrets.generation), Some(connection.transport_name()),
                                            connection.socket.ice_candidate_pair().as_deref(),
                                        );
                                        updates.publish(UiEvent::Snapshot(snapshot_event(snapshot, seq, &connection))).await;
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
                                                    let _ = response.send(Ok(CommandAckDto {
                                                        request_id: request_id.to_string(),
                                                        queued: true,
                                                        session_generation: connection.session_generation,
                                                        server_epoch: connection.identity.server_epoch.clone(),
                                                        transport_generation: connection.secrets.generation,
                                                        snapshot_seq: latest_snapshot_seq,
                                                    }));
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
                                    Some("pong") => {
                                        if let Some(nonce) = value.get("nonce").and_then(Value::as_str)
                                            && let Some(index) = pending_ping_nonces.iter().position(|(sent, _)| sent == nonce)
                                        {
                                            let acknowledged_at = pending_ping_nonces[index].1;
                                            pending_ping_nonces.drain(..=index);
                                            liveness_deadline.as_mut().reset(tokio::time::Instant::now() + HEARTBEAT_TIMEOUT);
                                            liveness_probe.as_mut().reset(tokio::time::Instant::now() + HEARTBEAT_TIMEOUT - HEARTBEAT_PROBE_LEAD);
                                            updates.diagnostics.record(
                                                crate::diagnostics::DiagnosticKind::HeartbeatAck,
                                                connection.socket.room_id(),
                                                value.get("request_id").and_then(Value::as_str).and_then(|raw| Uuid::parse_str(raw).ok()),
                                                Some(connection.secrets.generation), Some(connection.transport_name()),
                                                connection.socket.ice_candidate_pair().as_deref(),
                                            );
                                            log::debug!("[terminal] API heartbeat rx and acknowledged monotonic_ms={} ack_latency_ms={}", shocking_vrc_rtc::monotonic_ms(), acknowledged_at.elapsed().as_millis());
                                            publish_connected(&updates, &connection, "online", None).await;
                                        }
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
                                    Some("session.ended" | "session.revoked") => {
                                        if let Some(reason) = parse_session_end(&value, &connection.identity, &connection.secrets) {
                                            if frame_type(&value) == Some("session.ended")
                                                && !acknowledge_session_end(&mut connection).await
                                            {
                                                log::warn!("[terminal] could not acknowledge the authenticated Host session end before transport close");
                                            }
                                            updates.diagnostics.record(
                                                crate::diagnostics::DiagnosticKind::SessionEnded,
                                                connection.socket.room_id(),
                                                value.get("request_id").and_then(Value::as_str).and_then(|raw| Uuid::parse_str(raw).ok()),
                                                Some(connection.secrets.generation), Some(connection.transport_name()),
                                                connection.socket.ice_candidate_pair().as_deref(),
                                            );
                                            publish_session_end(&mut connection, &updates, reason).await;
                                            return;
                                        }
                                    }
                                    Some("error") => {
                                        if let Some(code) = value.get("code").and_then(Value::as_str) {
                                            let response_request_id = value.get("request_id").and_then(Value::as_str);
                                            if refresh_request_id.map(|id| id.to_string()).as_deref() == response_request_id {
                                                log::warn!("[terminal] access-token refresh rejected code={code}");
                                                refresh_request_id = None;
                                                if code == "AUTH_REQUIRED" {
                                                    liveness_deadline.as_mut().reset(tokio::time::Instant::now());
                                                    socket_dead = true;
                                                } else {
                                                    refresh_timer.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(30));
                                                }
                                            }
                                            if session_end_reason_from_protocol_error(code).is_some() {
                                                fail_pending_commands(&mut pending_commands, "Host revoked the session before command acknowledgement; outcome is unknown.");
                                                updates.diagnostics.record(
                                                    crate::diagnostics::DiagnosticKind::SessionEnded,
                                                    connection.socket.room_id(),
                                                    value.get("request_id").and_then(Value::as_str).and_then(|raw| Uuid::parse_str(raw).ok()),
                                                    Some(connection.secrets.generation), Some(connection.transport_name()),
                                                    connection.socket.ice_candidate_pair().as_deref(),
                                                );
                                                publish_session_end(&mut connection, &updates, RemoteSessionEndReason::SessionRevoked).await;
                                                return;
                                            }
                                            if code == "RESUME_TICKET_INVALID" {
                                                fail_pending_commands(&mut pending_commands, "Host no longer accepts this session before command acknowledgement; outcome is unknown.");
                                                connection.secrets.resume_ticket.zeroize();
                                                connection.secrets.access_token.zeroize();
                                                updates.diagnostics.record(
                                                    crate::diagnostics::DiagnosticKind::SessionEnded,
                                                    connection.socket.room_id(),
                                                    value.get("request_id").and_then(Value::as_str).and_then(|raw| Uuid::parse_str(raw).ok()),
                                                    Some(connection.secrets.generation), Some(connection.transport_name()),
                                                    connection.socket.ice_candidate_pair().as_deref(),
                                                );
                                                publish_connected(&updates, &connection, "host_unavailable", Some("The Host no longer recognizes this session. Reconnect manually; the saved profile and pairing were left unchanged.".to_owned())).await;
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
                                        let pong_sent = wait_before_liveness_deadline(&mut liveness_deadline, connection.socket.send_message(Message::Pong(payload))).await;
                                        if pong_sent.is_err() { continue; }
                                        if pong_sent.expect("handled liveness timeout above").is_err() {
                            liveness_deadline.as_mut().reset(tokio::time::Instant::now());
                            socket_dead = true;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        liveness_deadline.as_mut().reset(tokio::time::Instant::now());
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
    updates.diagnostics.record(
        crate::diagnostics::DiagnosticKind::SessionEnded,
        connection.socket.room_id(), None, Some(connection.secrets.generation),
        Some(connection.transport_name()), connection.socket.ice_candidate_pair().as_deref(),
    );
    publish_connected(
        &updates,
        &connection,
        "user_disconnected",
        Some("Disconnected from Host. Reconnect when ready.".to_owned()),
    )
    .await;
    let _ = timeout(Duration::from_secs(3), connection.socket.close()).await;
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
) -> Result<ResumeOutput, ResumeError> {
    let connect: Box<PendingConnector> = Box::new(|profile, resume_session_id| {
        Box::pin(async move {
            let rendezvous_authorization =
                crate::storage::credentials::read_device_grant(&profile.id, &profile.fingerprint)?;
            begin_connection(profile.clone(), rendezvous_authorization, Some(resume_session_id)).await
        })
    });
    reconnect_and_resume_with(connection, cancel, connect).await
}

async fn reconnect_and_resume_with(
    connection: &AuthenticatedConnection,
    cancel: &CancellationToken,
    mut connect: Box<PendingConnector>,
) -> Result<ResumeOutput, ResumeError> {
    let request_id = connection.secrets.resume_request_id;
    let resume_session_id =
        Uuid::parse_str(&connection.secrets.session_id).map_err(|_| "Stored Host session ID is invalid".to_owned())?;
    let connect_attempt_timeout = resume_attempt_timeout(connection);
    let mut retry_index = 0_u32;
    let resume_started = tokio::time::Instant::now();
    loop {
        if cancel.is_cancelled() {
            return Err(ResumeError::Transport("Disconnected by user".to_owned()));
        }
        if retry_index > 0 {
            let delay = match resume_retry_decision("temporary signaling error", retry_index - 1) {
                ResumeRetryDecision::RetryAfter(delay) => delay,
                ResumeRetryDecision::FailFast => unreachable!("temporary error is retryable"),
            };
            log::info!(
                "[terminal] retrying same-session resume monotonic_ms={} attempt={} delay_seconds={} remaining_window_ms={}",
                shocking_vrc_rtc::monotonic_ms(),
                retry_index.saturating_add(1),
                delay.as_secs(),
                RESUME_WINDOW.saturating_sub(resume_started.elapsed()).as_millis()
            );
            tokio::select! {
                _ = cancel.cancelled() => return Err(ResumeError::Transport("Disconnected by user".to_owned())),
                _ = sleep(delay) => {}
            }
        }
        log::info!(
            "[terminal] same-session resume attempt started monotonic_ms={} attempt={} join_timeout_ms={} remaining_window_ms={}",
            shocking_vrc_rtc::monotonic_ms(),
            retry_index.saturating_add(1),
            connect_attempt_timeout.as_millis(),
            RESUME_WINDOW.saturating_sub(resume_started.elapsed()).as_millis()
        );
        let pending = tokio::select! {
            _ = cancel.cancelled() => return Err(ResumeError::Transport("Disconnected by user".to_owned())),
            result = resume_connect_attempt(
                connect(&connection.profile, resume_session_id),
                connect_attempt_timeout,
            ) => result,
        };
        let (mut pending, _) = match pending {
            Ok(pending) => pending,
            Err(error) => {
                if resume_retry_decision(&error, retry_index) == ResumeRetryDecision::FailFast {
                    return Err(error.into());
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
        log::info!(
            "[terminal] resume transport and API hello completed monotonic_ms={} attempt={} host_identity_verified=true remaining_window_ms={}",
            shocking_vrc_rtc::monotonic_ms(),
            retry_index.saturating_add(1),
            RESUME_WINDOW.saturating_sub(resume_started.elapsed()).as_millis()
        );
        if pending.identity.server_epoch != connection.identity.server_epoch {
            log::info!("[terminal] Host epoch changed; authenticating the paired device on the new Host process");
            let terminal_identity = crate::storage::credentials::TerminalDeviceIdentity::load_or_create()?;
            let mut fresh = finish_device_auth(pending, &terminal_identity, None).await?;
            fresh.session_generation = connection.session_generation;
            crate::storage::credentials::save_device_grant(
                &fresh.profile.id,
                &fresh.profile.fingerprint,
                &fresh.device_grant,
            )?;
            return Ok((
                fresh.socket,
                fresh.identity,
                fresh.secrets,
                fresh.initial_snapshot,
                fresh.initial_snapshot_seq,
                fresh.capabilities,
            ));
        }
        let resume = json!({
            "type": "auth.resume",
            "protocol": PROTOCOL_VERSION,
            "request_id": request_id,
            "session_id": connection.secrets.session_id,
            "resume_ticket": &*connection.secrets.resume_ticket,
            "client_nonce": pending.identity.client_nonce
        });
        log::info!("[terminal] auth.resume tx monotonic_ms={} attempt={} remaining_window_ms={}", shocking_vrc_rtc::monotonic_ms(), retry_index.saturating_add(1), RESUME_WINDOW.saturating_sub(resume_started.elapsed()).as_millis());
        if let Err(error) = send_json(&mut pending.socket, &resume).await {
            if resume_retry_decision(&error, retry_index) == ResumeRetryDecision::FailFast {
                return Err(error.into());
            }
            retry_index = retry_index.saturating_add(1);
            continue;
        }
        let auth = match timeout(CONNECT_TIMEOUT, wait_for_resume_auth(&mut pending.socket)).await {
            Ok(Ok(ResumeAuthResponse::Authenticated(auth))) => auth,
            Ok(Ok(ResumeAuthResponse::Ended(frame))) => {
                return match parse_session_end(&frame, &connection.identity, &connection.secrets) {
                    Some(reason) => Err(ResumeError::Ended(reason)),
                    None => Err(ResumeError::Transport("Host sent an unbound session-end notice".to_owned())),
                };
            }
            Ok(Err(error)) => {
                if resume_retry_decision(&error, retry_index) == ResumeRetryDecision::FailFast {
                    return Err(error.into());
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
            return Err(ResumeError::Transport(
                "Host returned an invalid session resume response".to_owned(),
            ));
        }
        if auth.get("session_id").and_then(Value::as_str) != Some(connection.secrets.session_id.as_str())
            || auth.get("server_epoch").and_then(Value::as_str) != Some(connection.identity.server_epoch.as_str())
        {
            return Err(ResumeError::Transport(
                "Host restarted; sign in again to create a new session".to_owned(),
            ));
        }
        let secrets = parse_session_secrets(&auth)?;
        if secrets.generation <= connection.secrets.generation {
            return Err(ResumeError::Transport(
                "Host did not fence the previous transport generation during resume".to_owned(),
            ));
        }
        log::info!(
            "[terminal] auth.resume acknowledged monotonic_ms={} generation={} remaining_window_ms={}",
            shocking_vrc_rtc::monotonic_ms(),
            secrets.generation,
            RESUME_WINDOW.saturating_sub(resume_started.elapsed()).as_millis()
        );
        let (snapshot, snapshot_seq) = match wait_for_type(&mut pending.socket, "snapshot")
            .await
            .and_then(|value| snapshot_from_frame(&value, &pending.identity.server_epoch))
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if resume_retry_decision(&error, retry_index) == ResumeRetryDecision::FailFast {
                    return Err(error.into());
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
            return Err(ResumeError::Transport(
                "Host returned capabilities for a different request".to_owned(),
            ));
        }
        let capabilities = parse_capabilities(&capabilities_frame)?;
        let identity = pending.identity;
        return Ok((pending.socket, identity, secrets, snapshot, snapshot_seq, capabilities));
    }
}

async fn send_logout(socket: &mut SessionSocket) -> Result<(), String> {
    let request_id = Uuid::new_v4();
    let frame = json!({
        "type": "auth.logout", "protocol": PROTOCOL_VERSION, "request_id": request_id
    });
    let _ = timeout(Duration::from_secs(2), send_json(socket, &frame)).await;
    let _ = timeout(Duration::from_secs(1), wait_for_type(socket, "auth.logged_out")).await;
    let _ = timeout(Duration::from_secs(2), socket.close()).await;
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

async fn wait_for_resume_auth(socket: &mut SessionSocket) -> Result<ResumeAuthResponse, String> {
    loop {
        let frame = receive_json(socket).await?;
        match frame_type(&frame) {
            Some("auth.result") => return Ok(ResumeAuthResponse::Authenticated(frame)),
            Some("session.ended" | "session.revoked") => return Ok(ResumeAuthResponse::Ended(frame)),
            Some("error") => return Err(remote_frame_error(&frame)),
            Some("server.notice") => continue,
            _ => return Err("Host returned an unexpected session-resume response".to_owned()),
        }
    }
}

fn frame_type(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

fn rendezvous_join_error(error: &str) -> String {
    if error.contains("auth_required:") {
        return "Rendezvous could not authorize the room request. Confirm the Host is online and registered; if it is, verify the saved pairing.".to_owned();
    }
    error.to_owned()
}

fn remote_frame_error(value: &Value) -> String {
    let code = value.get("code").and_then(Value::as_str).unwrap_or("HOST_ERROR");
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Host rejected the request");
    format!("{code}: {message}")
}

#[cfg(test)]
mod rendezvous_error_tests {
    use super::rendezvous_join_error;

    #[test]
    fn auth_required_does_not_claim_saved_pairing_is_invalid() {
        let error = rendezvous_join_error(
            "rendezvous rejected the room: auth_required: The supplied rendezvous authorization is not valid.",
        );
        assert!(error.contains("Host is online and registered"));
        assert!(error.contains("verify the saved pairing"));
        assert!(!error.contains("authorization is not valid"));
        assert_eq!(rendezvous_join_error("room_busy: busy"), "room_busy: busy");
    }
}

fn string_field<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Host response omitted {name}"))
}

fn snapshot_from_frame(value: &Value, expected_server_epoch: &str) -> Result<(SnapshotDto, u64), String> {
    if value.get("server_epoch").and_then(Value::as_str) != Some(expected_server_epoch) {
        return Err("Host snapshot belongs to a different server epoch".to_owned());
    }
    let seq = value.get("seq").and_then(Value::as_u64)
        .ok_or_else(|| "Host snapshot omitted its sequence number".to_owned())?;
    let state = value
        .get("state")
        .cloned()
        .ok_or_else(|| "Host snapshot did not contain state".to_owned())?;
    let snapshot = serde_json::from_value(state)
        .map_err(|_| "Host returned a snapshot that does not match Remote API v1".to_owned())?;
    Ok((snapshot, seq))
}

fn snapshot_event(snapshot: SnapshotDto, seq: u64, connection: &AuthenticatedConnection) -> SnapshotEventDto {
    SnapshotEventDto {
        state: snapshot,
        context: SnapshotContextDto {
            seq,
            server_epoch: connection.identity.server_epoch.clone(),
            transport_generation: connection.secrets.generation,
            session_generation: connection.session_generation,
        },
    }
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
    let (ice_servers_configured, rendezvous_stun_received, turn_credentials_received, local_candidates_gathered) =
        connection.socket.ice_status();
    updates
        .publish(UiEvent::Connection(ConnectionStateDto {
            phase: phase.to_owned(),
            transport,
            ice_candidate_pair,
            ice_servers_configured,
            rendezvous_stun_received,
            turn_credentials_received,
            local_candidates_gathered,
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
            session_generation: Some(connection.session_generation),
            server_epoch: Some(connection.identity.server_epoch.clone()),
            transport_generation: Some(connection.secrets.generation),
        }))
        .await
}

async fn wait_for_signaling_lease_end(
    status: &mut watch::Receiver<shocking_vrc_rtc::SignalingLeaseStatus>,
) -> shocking_vrc_rtc::SignalingLeaseStatus {
    loop {
        let current = *status.borrow_and_update();
        if current != shocking_vrc_rtc::SignalingLeaseStatus::Active {
            return current;
        }
        if status.changed().await.is_err() {
            return shocking_vrc_rtc::SignalingLeaseStatus::TransportLost;
        }
    }
}

async fn publish_session_end(
    connection: &mut AuthenticatedConnection,
    updates: &crate::app_state::UiBridge,
    reason: RemoteSessionEndReason,
) {
    connection.secrets.resume_ticket.zeroize();
    connection.secrets.access_token.zeroize();
    connection.device_grant.zeroize();
    let (phase, message) = match reason {
        RemoteSessionEndReason::HostKick => (
            "host_kicked",
            "The Host ended this session. Your profile and pairing are saved; reconnect when ready.".to_owned(),
        ),
        RemoteSessionEndReason::SessionRevoked => {
            let message = match crate::storage::credentials::delete(&connection.profile.id) {
                Ok(()) => "The Host revoked this Terminal's access. Its saved device grant was deleted; pair again with a new Host invitation.".to_owned(),
                Err(error) => {
                    log::error!("[terminal] Host revoked the active pairing but the saved grant could not be deleted: {error}");
                    format!("The Host revoked this Terminal's access, but Windows Credential Manager could not delete its saved grant: {error}")
                }
            };
            ("session_revoked", message)
        }
        RemoteSessionEndReason::HostUnavailable => (
            "host_unavailable",
            "The Host is unavailable. Reconnect manually when remote access is available.".to_owned(),
        ),
    };
    publish_connected(updates, connection, phase, Some(message)).await;
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

#[cfg(test)]
mod two_process_acceptance {
    use super::*;
    use crate::{app_state::UiBridge, storage::credentials::TerminalDeviceIdentity};
    use base64::Engine;
    use shocking_vrc_remote::{HostIdentity, api::test_support, auth::SessionManager};
    use shocking_vrc_rendezvous::pairing::PairingInvitationV1;
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        net::{SocketAddr, TcpListener},
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::{Arc, Mutex, atomic::{AtomicU8, Ordering}},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    use tokio_util::sync::CancellationToken;
    use zeroize::Zeroizing;
    use shocking_vrc_remote::{BackendError, RemoteBackend, RemoteCommand};

    const TEST_NAME: &str =
        "transport::direct_wss::two_process_acceptance::production_api_direct_wss_process_loss_matrix";
    const CONFIG_TEST_NAME: &str =
        "transport::direct_wss::two_process_acceptance::production_api_two_process_authoritative_configuration_sequence";
    const ROLE_ENV: &str = "SHOCKINGVRC_DIRECT_WSS_PROCESS_ROLE";
    const CONFIG_SYNC_ENV: &str = "SHOCKINGVRC_DIRECT_WSS_CONFIG_SYNC";

    fn monotonic_ms() -> u128 {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed().as_millis()
    }

    struct ConfigurationBackend {
        snapshots: test_support::SnapshotBackend,
        channel_a_max: AtomicU8,
        commands: Mutex<Vec<(Uuid, u8)>>,
    }

    impl Default for ConfigurationBackend {
        fn default() -> Self {
            Self {
                snapshots: test_support::SnapshotBackend::default(),
                channel_a_max: AtomicU8::new(30),
                commands: Mutex::new(Vec::new()),
            }
        }
    }

    impl ConfigurationBackend {
        fn commands_since(&self, offset: usize) -> Vec<(usize, Uuid, u8)> {
            self.commands.lock().expect("configuration history lock")
                .iter().enumerate().skip(offset)
                .map(|(index, (request_id, value))| (index, *request_id, *value))
                .collect()
        }
    }

    impl RemoteBackend for ConfigurationBackend {
        fn snapshot(&self) -> Result<shocking_vrc_remote::SnapshotDto, BackendError> {
            let mut snapshot = self.snapshots.snapshot()?;
            snapshot.config.channel_a.limits.max = self.channel_a_max.load(Ordering::SeqCst);
            Ok(snapshot)
        }

        fn presets(&self) -> Result<shocking_vrc_remote::protocol::PresetCatalogDto, BackendError> {
            self.snapshots.presets()
        }

        fn zone_presets(&self) -> Result<shocking_vrc_remote::protocol::ZonePresetCatalogDto, BackendError> {
            self.snapshots.zone_presets()
        }

        fn logs(&self, cursor: Option<u64>, limit: u16) -> Result<shocking_vrc_remote::protocol::LogPageDto, BackendError> {
            self.snapshots.logs(cursor, limit)
        }

        fn enqueue(&self, command: RemoteCommand) -> Result<(), BackendError> {
            if command.op == "channels.set_limit_max" && command.args.get("channel").and_then(Value::as_str) == Some("A") {
                let value = command.args.get("value").and_then(Value::as_u64)
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or_else(|| BackendError::new("INVALID_ARGUMENT", "synthetic maximum must fit in u8"))?;
                self.channel_a_max.store(value, Ordering::SeqCst);
                self.commands.lock().expect("configuration history lock").push((command.request_id, value));
                Ok(())
            } else {
                self.snapshots.enqueue(command)
            }
        }
    }

    fn write_event(path: &Path, mut value: serde_json::Value) {
        static EVENT_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = EVENT_WRITE_LOCK.lock().expect("serialize process event appends");
        value["pid"] = json!(std::process::id());
        value["monotonic_ms"] = json!(monotonic_ms());
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open process event log");
        let mut line = serde_json::to_vec(&value).expect("encode process event JSON");
        line.push(b'\n');
        file.write_all(&line).expect("append complete process event row");
        file.flush().expect("flush process event");
    }

    fn read_events(path: &Path) -> Vec<serde_json::Value> {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    async fn wait_for_event(
        path: &Path,
        timeout_after: Duration,
        mut predicate: impl FnMut(&serde_json::Value) -> bool,
        description: &'static str,
    ) -> serde_json::Value {
        tokio::time::timeout(timeout_after, async {
            loop {
                if let Some(event) = read_events(path).into_iter().find(|event| predicate(event)) {
                    return event;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {description}"))
    }

    async fn run_host_process() {
        let root = PathBuf::from(std::env::var("SHOCKINGVRC_DIRECT_WSS_PROCESS_ROOT").unwrap());
        let ready_path = PathBuf::from(std::env::var("SHOCKINGVRC_DIRECT_WSS_HOST_READY").unwrap());
        let stop_path = PathBuf::from(std::env::var("SHOCKINGVRC_DIRECT_WSS_HOST_STOP").unwrap());
        let event_path = PathBuf::from(std::env::var("SHOCKINGVRC_DIRECT_WSS_HOST_EVENTS").unwrap());
        let configuration_mode = std::env::var_os(CONFIG_SYNC_ENV).is_some();
        let identity = Arc::new(HostIdentity::ephemeral_test_host());
        let epoch = Uuid::new_v4();
        let trust_path = root.join("isolated-host-trust.json");
        let sessions =
            Arc::new(SessionManager::load_from_path(epoch, trust_path.clone()).expect("isolated Host trust store"));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind isolated Direct Host API");
        listener.set_nonblocking(true).expect("make Host listener nonblocking");
        let address = listener.local_addr().expect("Host API address");

        let invitation_id = Uuid::new_v4();
        let pairing_secret = [0x67_u8; 32];
        sessions
            .create_invitation(invitation_id, Sha256::digest(pairing_secret).into())
            .expect("create one-time isolated pairing invitation");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_secs();
        let mut invitation = PairingInvitationV1 {
            version: 1,
            host_id: identity.server_id().to_owned(),
            host_public_key: URL_SAFE_NO_PAD.encode(identity.public_key()),
            host_spki_der: URL_SAFE_NO_PAD.encode(identity.spki_der()),
            host_fingerprint: identity.fingerprint().to_owned(),
            direct_host: Some("127.0.0.1".into()),
            direct_port: Some(address.port()),
            rendezvous_url: None,
            ice_server_urls: Vec::new(),
            invitation_id,
            pairing_secret: URL_SAFE_NO_PAD.encode(pairing_secret),
            route_token: None,
            issued_at: now,
            expires_at: now + 900,
            signature: String::new(),
        };
        invitation.signature = URL_SAFE_NO_PAD.encode(
            identity
                .sign(&invitation.signing_bytes())
                .expect("sign test invitation"),
        );
        let invitation_uri = invitation.to_uri(now).expect("serialize signed Direct WSS invitation");

        let configuration_backend = Arc::new(ConfigurationBackend::default());
        let backend: Arc<dyn RemoteBackend> = if configuration_mode {
            configuration_backend.clone()
        } else {
            Arc::new(test_support::SnapshotBackend::default())
        };
        let router = test_support::production_api_router(
            backend,
            Arc::clone(&sessions),
            Arc::clone(&identity),
            epoch,
        );
        let tls = shocking_vrc_remote::tls::server_config(&identity).expect("ephemeral Host TLS certificate");
        let server = axum_server::from_tcp_rustls(listener, tls).expect("configure isolated TLS server");
        let handle = axum_server::Handle::new();
        let server_handle = handle.clone();
        let mut server_task = tokio::spawn(
            server
                .handle(server_handle)
                .serve(router.into_make_service_with_connect_info::<SocketAddr>()),
        );
        fs::write(
            &ready_path,
            serde_json::to_vec(&json!({
                "host_id": identity.server_id(),
                "fingerprint": identity.fingerprint(),
                "port": address.port(),
                "invitation": invitation_uri,
                "server_epoch": epoch,
            }))
            .expect("serialize Host fixture metadata"),
        )
        .expect("publish isolated Host metadata");
        write_event(
            &event_path,
            json!({"kind":"host_ready","port":address.port(),"server_epoch":epoch}),
        );

        let mut previous_phase = String::new();
        let mut previous_contact = None;
        let mut reported_configuration_commands = 0;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(25)) => {
                    let status = sessions.status();
                    let phase = status.phase.as_str().to_owned();
                    if phase != previous_phase {
                        write_event(&event_path, json!({
                            "kind":"host_phase", "phase":phase,
                            "generation":status.transport_generation,
                        }));
                        previous_phase = phase;
                    }
                    if status.last_contact != previous_contact {
                        if status.last_contact.is_some() {
                            write_event(&event_path, json!({"kind":"host_contact"}));
                        }
                        previous_contact = status.last_contact;
                    }
                    if configuration_mode {
                        let commands = configuration_backend.commands_since(reported_configuration_commands);
                        for (index, request_id, value) in &commands {
                            write_event(&event_path, json!({
                                "kind":"host_configuration_commit",
                                "ordinal":index,
                                "request_id":request_id,
                                "target":value,
                                "committed_max":value,
                            }));
                        }
                        reported_configuration_commands += commands.len();
                    }
                }
                _ = async { while !stop_path.exists() { tokio::time::sleep(Duration::from_millis(25)).await; } } => break,
                result = &mut server_task => panic!("production Host API server exited early: {result:?}"),
            }
        }
        handle.graceful_shutdown(Some(Duration::from_secs(2)));
        server_task
            .await
            .expect("join production Host API listener")
            .expect("graceful Host API shutdown");
        write_event(&event_path, json!({"kind":"host_stopped"}));
        drop(sessions);
        let _ = fs::remove_file(trust_path);
    }

    async fn run_terminal_process() {
        let host_ready = PathBuf::from(std::env::var("SHOCKINGVRC_DIRECT_WSS_HOST_READY").unwrap());
        let stop_path = PathBuf::from(std::env::var("SHOCKINGVRC_DIRECT_WSS_TERMINAL_STOP").unwrap());
        let event_path = PathBuf::from(std::env::var("SHOCKINGVRC_DIRECT_WSS_TERMINAL_EVENTS").unwrap());
        let configuration_mode = std::env::var_os(CONFIG_SYNC_ENV).is_some();
        let host: serde_json::Value =
            serde_json::from_slice(&fs::read(host_ready).expect("Host ready metadata")).expect("parse Host metadata");
        let port = host["port"].as_u64().expect("Host API port") as u16;
        let profile = ProfileDto {
            id: Uuid::new_v4().to_string(),
            label: "Isolated process Host".into(),
            label_is_custom: true,
            host_display_name: Some("Isolated process Host".into()),
            host: Some("127.0.0.1".into()),
            port: Some(port),
            fingerprint: host["fingerprint"].as_str().unwrap().to_owned(),
            server_id: host["host_id"].as_str().unwrap().to_owned(),
            connection_mode: "direct_only".into(),
            rendezvous_url: String::new(),
            ice_server_urls: Vec::new(),
            force_relay: false,
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_secs();
        let invitation = PairingInvitationV1::parse_uri(host["invitation"].as_str().unwrap(), now)
            .expect("parse valid isolated pairing invitation");
        let terminal_identity = TerminalDeviceIdentity::ephemeral_for_test();
        let (pending, _) = begin_connection(profile, None, None)
            .await
            .expect("production Direct WSS identity-pinned connection");
        let connection = finish_device_auth(pending, &terminal_identity, Some(&invitation))
            .await
            .expect("production passwordless device auth and initial Host snapshot");
        assert_eq!(connection.transport_name(), "direct_wss");
        let session_id = connection.secrets.session_id.clone();
        let server_epoch = connection.identity.server_epoch.clone();
        let grant = connection.device_grant.to_string();
        let initial_revision = connection.initial_snapshot.bridge_revision;
        write_event(
            &event_path,
            json!({
                "kind":"authenticated", "transport":"direct_wss", "session_id":session_id,
                "server_epoch":server_epoch, "generation":connection.secrets.generation,
                "snapshot_revision":initial_revision,
            }),
        );

        let updates = Arc::new(UiBridge::default());
        let (actions, action_receiver) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let resume_events = event_path.clone();
        let resume_grant = grant.clone();
        let resume: Box<ResumeOperation> = Box::new(move |current, cancel| {
            let resume_events = resume_events.clone();
            let resume_grant = resume_grant.clone();
            let expected_session = current.secrets.session_id.clone();
            let expected_epoch = current.identity.server_epoch.clone();
            Box::pin(async move {
                let connect: Box<PendingConnector> = Box::new(move |profile, session_id| {
                    let device_grant = Zeroizing::new(resume_grant.clone());
                    Box::pin(async move {
                        let (pending, identity) =
                            begin_connection(profile.clone(), Some(device_grant), Some(session_id)).await?;
                        Ok((pending, identity))
                    })
                });
                let result = reconnect_and_resume_with(current, cancel, connect).await;
                if let Ok((_, identity, secrets, snapshot, _snapshot_seq, _capabilities)) = &result {
                    write_event(
                        &resume_events,
                        json!({
                            "kind":"resume_success", "session_id":secrets.session_id,
                            "same_session":secrets.session_id == expected_session,
                            "server_epoch":identity.server_epoch,
                            "same_epoch":identity.server_epoch == expected_epoch,
                            "generation":secrets.generation,
                            "snapshot_revision":snapshot.bridge_revision,
                        }),
                    );
                }
                result
            })
        });
        let mut session_task = tokio::spawn(run_session_with_resume(
            connection,
            action_receiver,
            Arc::clone(&updates),
            cancel.clone(),
            resume,
        ));
        if configuration_mode {
            let initial = wait_for_process_limit_snapshot(&updates, 30, 0, &event_path, "initial Host maximum").await;
            write_event(&event_path, json!({
                "kind":"terminal_configuration_initial",
                "host_value":initial.state.config.channel_a.limits.max,
                "seq":initial.context.seq,
                "bridge_revision":initial.state.bridge_revision,
            }));

            let mut final_ack = None;
            for target in [60_u8, 90, 40, 70] {
                final_ack = Some(queue_configuration_limit(&actions, &event_path, target).await);
            }
            let rapid_ack = final_ack.expect("four rapid commands were queued");
            let rapid_snapshot = wait_for_process_limit_snapshot(
                &updates, 70, rapid_ack.snapshot_seq, &event_path, "post-ack rapid-batch maximum",
            ).await;
            write_event(&event_path, json!({
                "kind":"terminal_configuration_rapid_final",
                "host_value":rapid_snapshot.state.config.channel_a.limits.max,
                "seq":rapid_snapshot.context.seq,
                "ack_snapshot_seq":rapid_ack.snapshot_seq,
                "bridge_revision":rapid_snapshot.state.bridge_revision,
            }));

            for target in [60_u8, 90, 40, 70] {
                let ack = queue_configuration_limit(&actions, &event_path, target).await;
                let observed = wait_for_process_limit_snapshot(
                    &updates, target, ack.snapshot_seq, &event_path, "post-ack per-step Host maximum",
                ).await;
                write_event(&event_path, json!({
                    "kind":"terminal_configuration_step_confirmed",
                    "request_id":ack.request_id,
                    "target":target,
                    "host_value":observed.state.config.channel_a.limits.max,
                    "seq":observed.context.seq,
                    "ack_snapshot_seq":ack.snapshot_seq,
                    "bridge_revision":observed.state.bridge_revision,
                }));
            }
            write_event(&event_path, json!({"kind":"terminal_configuration_sequence_complete"}));
        }
        let mut last_phase = String::new();
        let mut last_snapshot_revision = initial_revision;
        let mut last_heartbeat = None;
        loop {
            if let Some(connection) = updates.latest_connection_for_test().await
                && connection.phase != last_phase
            {
                write_event(&event_path, json!({"kind":"terminal_phase","phase":connection.phase}));
                last_phase = connection.phase;
            }
            if let Some(connection) = updates.latest_connection_for_test().await
                && connection.last_heartbeat != last_heartbeat
            {
                if connection.last_heartbeat.is_some() {
                    write_event(&event_path, json!({"kind":"terminal_heartbeat"}));
                }
                last_heartbeat = connection.last_heartbeat;
            }
            if let Some(snapshot) = updates.latest_snapshot_for_test().await
                && snapshot.state.bridge_revision != last_snapshot_revision
            {
                write_event(&event_path, json!({"kind":"snapshot","revision":snapshot.state.bridge_revision}));
                last_snapshot_revision = snapshot.state.bridge_revision;
            }
            if stop_path.exists() {
                cancel.cancel();
                break;
            }
            if session_task.is_finished() {
                write_event(&event_path, json!({"kind":"session_task_finished"}));
                while !stop_path.exists() {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(4), &mut session_task).await;
        write_event(&event_path, json!({"kind":"terminal_stopped"}));
    }

    async fn queue_configuration_limit(
        actions: &mpsc::Sender<SessionAction>,
        event_path: &Path,
        target: u8,
    ) -> CommandAckDto {
        let request_id = Uuid::new_v4();
        let frame = json!({
            "type":"command",
            "protocol":PROTOCOL_VERSION,
            "request_id":request_id,
            "op":"channels.set_limit_max",
            "args":{"channel":"A","value":target},
        });
        let (response, receiver) = oneshot::channel();
        actions.send(SessionAction::Command { request_id, frame, response }).await
            .expect("submit configuration command to Terminal session task");
        let ack = tokio::time::timeout(Duration::from_secs(5), receiver).await
            .expect("Host queued configuration command")
            .expect("Terminal transport returned the command result")
            .expect("Host accepted the configuration command");
        write_event(event_path, json!({
            "kind":"terminal_configuration_queued",
            "request_id":ack.request_id,
            "target":target,
            "ack_snapshot_seq":ack.snapshot_seq,
            "session_generation":ack.session_generation,
            "server_epoch":ack.server_epoch,
            "transport_generation":ack.transport_generation,
        }));
        ack
    }

    async fn wait_for_process_limit_snapshot(
        updates: &UiBridge,
        target: u8,
        after_seq: u64,
        event_path: &Path,
        description: &'static str,
    ) -> SnapshotEventDto {
        let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(snapshot) = updates.latest_snapshot_for_test().await
                    && snapshot.context.seq > after_seq
                    && snapshot.state.config.channel_a.limits.max == target
                {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap_or_else(|_| panic!("timed out waiting for {description}"));
        write_event(event_path, json!({
            "kind":"terminal_configuration_snapshot_observed",
            "target":target,
            "host_value":snapshot.state.config.channel_a.limits.max,
            "seq":snapshot.context.seq,
            "bridge_revision":snapshot.state.bridge_revision,
        }));
        snapshot
    }

    struct ProcessPair {
        host: ChildGuard,
        terminal: ChildGuard,
        root: PathBuf,
        host_ready: PathBuf,
        host_stop: PathBuf,
        terminal_stop: PathBuf,
        host_events: PathBuf,
        terminal_events: PathBuf,
        _ready_cleanup: RemoveFileOnDrop,
    }

    impl ProcessPair {
        async fn start(root: &Path, label: &str) -> Self {
            Self::start_with_configuration_mode(root, label, false).await
        }

        async fn start_configuration(root: &Path, label: &str) -> Self {
            Self::start_with_configuration_mode(root, label, true).await
        }

        async fn start_with_configuration_mode(root: &Path, label: &str, configuration_mode: bool) -> Self {
            let pair_root = root.join(label);
            fs::create_dir_all(&pair_root).expect("create isolated process test folder");
            let host_ready = pair_root.join("host-ready.json");
            let host_stop = pair_root.join("host-stop");
            let terminal_stop = pair_root.join("terminal-stop");
            let host_events = pair_root.join("host-events.jsonl");
            let terminal_events = pair_root.join("terminal-events.jsonl");
            let host_log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(pair_root.join("host.log"))
                .unwrap();
            let terminal_log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(pair_root.join("terminal.log"))
                .unwrap();
            let executable = std::env::current_exe().expect("current Terminal unit test process");
            let common = |command: &mut Command| {
                command
                    .args(["--exact", if configuration_mode { CONFIG_TEST_NAME } else { TEST_NAME }, "--nocapture"])
                    .env("SHOCKINGVRC_DIRECT_WSS_PROCESS_ROOT", &pair_root)
                    .env("SHOCKINGVRC_DIRECT_WSS_HOST_READY", &host_ready)
                    .env("SHOCKINGVRC_DIRECT_WSS_HOST_EVENTS", &host_events);
                if configuration_mode { command.env(CONFIG_SYNC_ENV, "1"); }
            };
            let mut host_command = Command::new(&executable);
            common(&mut host_command);
            let mut host = ChildGuard(
                host_command
                    .env(ROLE_ENV, "host")
                    .env("SHOCKINGVRC_DIRECT_WSS_HOST_STOP", &host_stop)
                    .stdout(Stdio::from(host_log))
                    .stderr(Stdio::from(
                        OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(pair_root.join("host.log"))
                            .unwrap(),
                    ))
                    .spawn()
                    .expect("spawn independent production Host API process"),
                Some(pair_root.join("isolated-host-trust.json")),
            );
            let ready_cleanup = RemoveFileOnDrop(host_ready.clone());

            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if host_ready.exists() {
                        break;
                    }
                    if let Some(status) = host.0.try_wait().expect("poll independent Host process") {
                        panic!("Host process exited before API startup: {status}");
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("independent Host API ready");

            let mut terminal_command = Command::new(&executable);
            common(&mut terminal_command);
            let terminal = ChildGuard(
                terminal_command
                    .env(ROLE_ENV, "terminal")
                    .env("SHOCKINGVRC_DIRECT_WSS_TERMINAL_STOP", &terminal_stop)
                    .env("SHOCKINGVRC_DIRECT_WSS_TERMINAL_EVENTS", &terminal_events)
                    .stdout(Stdio::from(terminal_log))
                    .stderr(Stdio::from(
                        OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(pair_root.join("terminal.log"))
                            .unwrap(),
                    ))
                    .spawn()
                    .expect("spawn independent production Terminal transport process"),
                None,
            );
            let pair = Self {
                host,
                terminal,
                root: pair_root,
                host_ready,
                host_stop,
                terminal_stop,
                host_events,
                terminal_events,
                _ready_cleanup: ready_cleanup,
            };
            let connected = wait_for_event(
                &pair.terminal_events,
                Duration::from_secs(20),
                |event| event["kind"] == "authenticated",
                "Terminal passwordless Direct WSS authentication",
            )
            .await;
            assert_eq!(connected["transport"], "direct_wss");
            fs::remove_file(&pair.host_ready).expect("remove temporary invitation material after pairing");
            wait_for_event(
                &pair.terminal_events,
                Duration::from_secs(5),
                |event| event["kind"] == "terminal_phase" && event["phase"] == "online",
                "Terminal online state",
            )
            .await;
            wait_for_event(
                &pair.host_events,
                Duration::from_secs(5),
                |event| event["kind"] == "host_phase" && event["phase"] == "ACTIVE",
                "Host active session",
            )
            .await;
            pair
        }

        async fn stop(&mut self) {
            let _ = fs::write(&self.terminal_stop, "stop");
            let _ = fs::write(&self.host_stop, "stop");
            wait_child(&mut self.terminal, Duration::from_secs(8), "Terminal").await;
            wait_child(&mut self.host, Duration::from_secs(8), "Host").await;
            let _ = fs::remove_file(&self.terminal_stop);
            let _ = fs::remove_file(&self.host_stop);
            let _ = fs::remove_file(&self.host_ready);
        }

        fn terminal_pid(&self) -> u32 {
            self.terminal.0.id()
        }
        fn host_pid(&self) -> u32 {
            self.host.0.id()
        }
    }

    impl Drop for ProcessPair {
        fn drop(&mut self) {
            let _ = fs::write(&self.terminal_stop, "stop");
            let _ = fs::write(&self.host_stop, "stop");
            let _ = self.terminal.0.kill();
            let _ = self.host.0.kill();
            let _ = self.terminal.0.wait();
            let _ = self.host.0.wait();
            let _ = fs::remove_file(&self.terminal_stop);
            let _ = fs::remove_file(&self.host_stop);
            let _ = fs::remove_file(&self.host_ready);
            let _ = &self.root;
        }
    }

    struct ChildGuard(Child, Option<PathBuf>);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
            if let Some(path) = self.1.as_ref() {
                let _ = fs::remove_file(path);
            }
        }
    }

    struct RemoveFileOnDrop(PathBuf);

    impl Drop for RemoveFileOnDrop {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    async fn wait_child(child: &mut ChildGuard, limit: Duration, label: &'static str) {
        tokio::time::timeout(limit, async {
            loop {
                if let Some(status) = child.0.try_wait().expect("poll child shutdown") {
                    assert!(status.success(), "{label} process did not stop cleanly: {status}");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out stopping {label} child process"));
    }

    fn set_loopback_packet_loss(enabled: bool) {
        let current = Command::new("tc")
            .args(["qdisc", "show", "dev", "lo"])
            .output()
            .expect("inspect isolated loopback qdisc");
        assert!(
            current.status.success(),
            "tc state read failed: {}",
            String::from_utf8_lossy(&current.stderr)
        );
        let active = String::from_utf8_lossy(&current.stdout)
            .lines()
            .any(|line| line.starts_with("qdisc netem "));
        if enabled {
            assert!(
                !active,
                "an unexpected netem rule already exists on the isolated test container loopback"
            );
        } else if !active {
            return;
        }
        let args = if enabled {
            vec!["qdisc", "add", "dev", "lo", "root", "netem", "loss", "100%"]
        } else {
            vec!["qdisc", "del", "dev", "lo", "root"]
        };
        let result = Command::new("tc")
            .args(args)
            .output()
            .expect("change isolated loopback packet loss");
        assert!(
            result.status.success(),
            "tc transition failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    fn dropped_loopback_packets() -> usize {
        let output = Command::new("tc")
            .args(["-s", "qdisc", "show", "dev", "lo"])
            .output()
            .expect("read loopback loss counters");
        assert!(
            output.status.success(),
            "tc counters failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .find_map(|pair| {
                (pair[0].trim_start_matches('(') == "dropped").then(|| {
                    pair[1]
                        .trim_end_matches(',')
                        .parse::<usize>()
                        .expect("numeric drop count")
                })
            })
            .unwrap_or(0)
    }

    async fn inject_outage(pair: &ProcessPair, duration_seconds: u64, expect_resume: bool) -> bool {
        let initial = wait_for_event(
            &pair.terminal_events,
            Duration::from_secs(5),
            |event| event["kind"] == "authenticated",
            "initial authenticated Terminal session metadata",
        )
        .await;
        let initial_snapshot_revision = initial["snapshot_revision"]
            .as_u64()
            .expect("initial authoritative snapshot revision");
        let last_heartbeat_ms = read_events(&pair.terminal_events)
            .iter()
            .filter(|event| event["kind"] == "terminal_heartbeat")
            .filter_map(|event| event["monotonic_ms"].as_u64())
            .max()
            .unwrap_or_default();
        let fresh_heartbeat = wait_for_event(
            &pair.terminal_events,
            Duration::from_secs(5),
            |event| {
                event["kind"] == "terminal_heartbeat"
                    && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 > last_heartbeat_ms as u128
            },
            "fresh authenticated Terminal heartbeat before packet loss",
        )
        .await;
        let last_authenticated_activity_ms = fresh_heartbeat["monotonic_ms"]
            .as_u64()
            .expect("timestamp of the last authenticated heartbeat")
            as u128;
        let terminal_before = read_events(&pair.terminal_events);
        let resume_count_before = terminal_before
            .iter()
            .filter(|event| event["kind"] == "resume_success")
            .count();
        let reconnect_count_before = terminal_before
            .iter()
            .filter(|event| event["kind"] == "terminal_phase" && event["phase"] == "reconnecting")
            .count();
        let before_packets = dropped_loopback_packets();
        set_loopback_packet_loss(true);
        let drop_started = monotonic_ms();
        let packet_sample = tokio::task::spawn_blocking(move || {
            std::thread::sleep(Duration::from_secs(duration_seconds.saturating_sub(2)));
            dropped_loopback_packets()
        });
        println!(
            "fault start duration_s={duration_seconds} mono_ms={drop_started} host_pid={} terminal_pid={}",
            pair.host_pid(),
            pair.terminal_pid()
        );
        if duration_seconds >= 10 {
            let terminal_loss = wait_for_event(
                &pair.terminal_events,
                Duration::from_secs(12),
                |event| {
                    event["kind"] == "terminal_phase"
                        && event["phase"] == "reconnecting"
                        && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= drop_started
                },
                "Terminal heartbeat-loss detection while qdisc drops packets",
            )
            .await;
            let detected_ms = terminal_loss["monotonic_ms"].as_u64().unwrap() as u128 - drop_started;
            println!("terminal reconnecting duration_s={duration_seconds} detected_ms={detected_ms}");
            wait_for_event(
                &pair.host_events,
                Duration::from_secs(3),
                |event| {
                    event["kind"] == "host_phase"
                        && event["phase"] == "RESERVED"
                        && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= drop_started
                },
                "Host ACTIVE to RESERVED transition during packet loss",
            )
            .await;
        }
        tokio::time::sleep(
            Duration::from_secs(duration_seconds)
                .saturating_sub(Duration::from_millis((monotonic_ms() - drop_started) as u64)),
        )
        .await;
        let restore_command_started = monotonic_ms();
        set_loopback_packet_loss(false);
        let drop_ended = monotonic_ms();
        let packets = packet_sample.await.expect("join in-window qdisc packet sampler");
        assert!(
            packets > before_packets,
            "qdisc counter did not prove real dropped packets during {duration_seconds}s outage"
        );
        println!(
            "fault restored duration_s={duration_seconds} actual_ms={} restore_command_ms={} sampled_drops={}",
            drop_ended - drop_started,
            drop_ended - restore_command_started,
            packets - before_packets
        );

        if duration_seconds == 9 {
            let boundary_outcome = tokio::time::timeout(Duration::from_secs(35), async {
                loop {
                    let events = read_events(&pair.terminal_events);
                    if let Some(resumed) = events.iter().rev().find(|event| {
                        event["kind"] == "resume_success"
                            && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= drop_started
                    }) {
                        break json!({"outcome":"resumed","event":resumed});
                    }

                    let reconnecting_at = events
                        .iter()
                        .filter(|event| {
                            (event["kind"] == "resume_attempt"
                                || (event["kind"] == "terminal_phase" && event["phase"] == "reconnecting"))
                                && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= drop_started
                        })
                        .filter_map(|event| event["monotonic_ms"].as_u64())
                        .min();
                    if let Some(activity) = events.iter().find(|event| {
                        event["kind"] == "terminal_heartbeat"
                            && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= drop_ended
                            && reconnecting_at.is_none_or(|reconnecting| {
                                event["monotonic_ms"].as_u64().unwrap_or_default() < reconnecting
                            })
                    }) {
                        let activity_ms = activity["monotonic_ms"].as_u64().unwrap_or_default();
                        tokio::time::sleep(Duration::from_secs(4)).await;
                        let confirmed_events = read_events(&pair.terminal_events);
                        if let Some(resumed) = confirmed_events.iter().rev().find(|event| {
                            event["kind"] == "resume_success"
                                && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= drop_started
                        }) {
                            break json!({"outcome":"resumed","event":resumed});
                        }
                        if confirmed_events.iter().any(|event| {
                            (event["kind"] == "resume_attempt"
                                || (event["kind"] == "terminal_phase" && event["phase"] == "reconnecting"))
                                && event["monotonic_ms"].as_u64().unwrap_or_default() >= activity_ms
                        }) {
                            continue;
                        }
                        break json!({"outcome":"survived","event":activity});
                    }
                    if events.iter().any(|event| {
                        event["kind"] == "terminal_phase"
                            && event["phase"] == "host_unavailable"
                            && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= drop_started
                    }) {
                        break json!({"outcome":"host_unavailable"});
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("9-second boundary outcome after liveness deadline");

            if boundary_outcome["outcome"] == "resumed" {
                let resumed = boundary_outcome["event"].clone();
                let reconnecting_at = read_events(&pair.terminal_events)
                    .iter()
                    .filter(|event| {
                        (event["kind"] == "resume_attempt"
                            || (event["kind"] == "terminal_phase" && event["phase"] == "reconnecting"))
                            && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= drop_started
                    })
                    .filter_map(|event| event["monotonic_ms"].as_u64())
                    .min()
                    .expect("liveness detection before boundary resume");
                let detected_after_activity_ms = reconnecting_at.saturating_sub(last_authenticated_activity_ms as u64);
                assert!(
                    (9_500..=11_500).contains(&detected_after_activity_ms),
                    "9-second boundary detection must be measured from last authenticated activity; got {detected_after_activity_ms} ms"
                );
                assert_eq!(resumed["same_session"], true, "boundary resume must retain the session");
                assert_eq!(resumed["same_epoch"], true, "boundary resume must retain Host identity epoch");
                assert!(resumed["generation"].as_u64().unwrap() > initial["generation"].as_u64().unwrap());
                let resumed_snapshot_revision = resumed["snapshot_revision"].as_u64().unwrap();
                assert!(
                    resumed_snapshot_revision > initial_snapshot_revision,
                    "boundary resume must publish a fresh authoritative snapshot"
                );
                wait_for_event(
                    &pair.terminal_events,
                    Duration::from_secs(5),
                    |event| {
                        event["kind"] == "snapshot"
                            && event["revision"]
                                .as_u64()
                                .is_some_and(|revision| revision >= resumed_snapshot_revision)
                            && event["monotonic_ms"].as_u64().unwrap_or_default()
                                >= resumed["monotonic_ms"].as_u64().unwrap_or_default()
                    },
                    "fresh authoritative Snapshot after boundary resume",
                )
                .await;
                wait_for_event(
                    &pair.terminal_events,
                    Duration::from_secs(5),
                    |event| {
                        event["kind"] == "terminal_phase"
                            && event["phase"] == "online"
                            && event["monotonic_ms"].as_u64().unwrap_or_default()
                                >= resumed["monotonic_ms"].as_u64().unwrap_or_default()
                    },
                    "Terminal ONLINE after fresh boundary snapshot",
                )
                .await;
                println!(
                    "boundary outcome=same_session_resume duration_s=9 detected_after_activity_ms={detected_after_activity_ms} resume_after_restore_ms={} generation={} snapshot_revision={}",
                    resumed["monotonic_ms"].as_u64().unwrap() as u128 - drop_ended,
                    resumed["generation"],
                    resumed["snapshot_revision"]
                );
                return true;
            }

            assert_eq!(
                boundary_outcome["outcome"], "survived",
                "9-second loss must either deliver authenticated traffic before liveness expiry or recover the same session"
            );
            println!("boundary outcome=survived_without_reconnect duration_s=9");
            return false;
        }

        if duration_seconds == 5 {
            tokio::time::sleep(Duration::from_secs(4)).await;
            let events = read_events(&pair.terminal_events);
            let resume_events = events
                .iter()
                .filter(|event| event["kind"] == "resume_success")
                .collect::<Vec<_>>();
            let resumed = resume_events.len() > resume_count_before;
            if resumed {
                panic!("the 5-second packet loss must not trigger a resume");
            } else {
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event["kind"] == "terminal_phase" && event["phase"] == "reconnecting")
                        .count(),
                    reconnect_count_before,
                    "short packet loss must not mark the Terminal reconnecting"
                );
                println!("short outage outcome=survived duration_s={duration_seconds} reconnects=0");
            }
            assert!(
                duration_seconds != 5 || !resumed,
                "the 5-second packet loss must not trigger a resume"
            );
            return resumed;
        }

        if expect_resume {
            let resumed = wait_for_event(
                &pair.terminal_events,
                Duration::from_secs(20),
                |event| {
                    event["kind"] == "resume_success"
                        && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= drop_ended
                },
                "authenticated same-session Direct WSS resume",
            )
            .await;
            assert_eq!(resumed["same_session"], true);
            assert_eq!(resumed["same_epoch"], true);
            assert!(resumed["generation"].as_u64().unwrap() >= 2);
            assert!(
                resumed["snapshot_revision"].as_u64().unwrap() > initial_snapshot_revision,
                "resume must publish a fresh authoritative Host snapshot"
            );
            wait_for_event(
                &pair.terminal_events,
                Duration::from_secs(5),
                |event| {
                    event["kind"] == "terminal_phase"
                        && event["phase"] == "online"
                        && event["monotonic_ms"].as_u64().unwrap_or_default() as u128
                            >= resumed["monotonic_ms"].as_u64().unwrap() as u128
                },
                "Terminal ONLINE after fresh snapshot",
            )
            .await;
            println!(
                "resume success duration_s={duration_seconds} generation={} snapshot_revision={}",
                resumed["generation"], resumed["snapshot_revision"]
            );
            true
        } else {
            let unavailable = wait_for_event(
                &pair.terminal_events,
                Duration::from_secs(5),
                |event| event["kind"] == "terminal_phase" && event["phase"] == "host_unavailable",
                "bounded resume window expiration",
            )
            .await;
            assert!(
                unavailable["monotonic_ms"].as_u64().unwrap() as u128 <= drop_ended,
                "40-second impairment should exhaust Terminal recovery before restoration"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
            let after_restore = read_events(&pair.terminal_events);
            assert_eq!(
                after_restore
                    .iter()
                    .filter(|event| event["kind"] == "resume_success")
                    .count(),
                resume_count_before,
                "restoration after the bounded recovery window must not silently log in again"
            );
            println!("bounded failure duration_s={duration_seconds} phase=host_unavailable no_relogin=true");
            false
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn production_api_two_process_authoritative_configuration_sequence() {
        if let Ok(role) = std::env::var(ROLE_ENV) {
            match role.as_str() {
                "host" => run_host_process().await,
                "terminal" => run_terminal_process().await,
                _ => panic!("unknown Direct WSS test process role"),
            }
            return;
        }

        let evidence_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("configuration-sync-evidence")
            .join(format!("run-{}", Uuid::new_v4()));
        fs::create_dir_all(&evidence_root).expect("create local two-process configuration evidence folder");
        let mut pair = ProcessPair::start_configuration(&evidence_root, "host-terminal").await;

        let commits = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let events = read_events(&pair.host_events);
                let commits: Vec<_> = events.into_iter().filter(|event| event["kind"] == "host_configuration_commit").collect();
                if commits.len() >= 8 { return commits; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("Host process recorded eight authoritative commits");
        let expected = [60_u64, 90, 40, 70, 60, 90, 40, 70];
        assert_eq!(commits.iter().take(8).map(|event| event["target"].as_u64().unwrap()).collect::<Vec<_>>(), expected);
        for (ordinal, event) in commits.iter().take(8).enumerate() {
            assert_eq!(event["ordinal"].as_u64(), Some(ordinal as u64));
            assert_eq!(event["committed_max"], event["target"], "Host fixture exposes its committed value");
        }

        let initial = wait_for_event(
            &pair.terminal_events,
            Duration::from_secs(5),
            |event| event["kind"] == "terminal_configuration_initial",
            "Terminal initial maximum snapshot",
        ).await;
        assert_eq!(initial["host_value"], 30);
        let rapid = wait_for_event(
            &pair.terminal_events,
            Duration::from_secs(5),
            |event| event["kind"] == "terminal_configuration_rapid_final",
            "post-ack snapshot after rapid command burst",
        ).await;
        assert_eq!(rapid["host_value"], 70);
        assert!(rapid["seq"].as_u64().unwrap() > rapid["ack_snapshot_seq"].as_u64().unwrap());
        wait_for_event(
            &pair.terminal_events,
            Duration::from_secs(5),
            |event| event["kind"] == "terminal_configuration_sequence_complete",
            "all four per-step Terminal snapshots",
        ).await;
        let terminal_steps: Vec<_> = read_events(&pair.terminal_events).into_iter()
            .filter(|event| event["kind"] == "terminal_configuration_step_confirmed")
            .collect();
        assert_eq!(terminal_steps.iter().map(|event| event["host_value"].as_u64().unwrap()).collect::<Vec<_>>(), [60, 90, 40, 70]);
        for step in &terminal_steps {
            assert!(step["seq"].as_u64().unwrap() > step["ack_snapshot_seq"].as_u64().unwrap());
            assert_eq!(step["target"], step["host_value"]);
        }

        let host_pid = pair.host_pid();
        let terminal_pid = pair.terminal_pid();
        pair.stop().await;
        println!("AUTHORITATIVE_CONFIGURATION_TWO_PROCESS_PASS host_pid={host_pid} terminal_pid={terminal_pid} initial=30 rapid=[60,90,40,70] confirmed=[60,90,40,70] evidence={}", evidence_root.display());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn production_api_direct_wss_process_loss_matrix() {
        if let Ok(role) = std::env::var(ROLE_ENV) {
            match role.as_str() {
                "host" => run_host_process().await,
                "terminal" => run_terminal_process().await,
                _ => panic!("unknown Direct WSS test process role"),
            }
            return;
        }
        if std::env::var_os("SHOCKINGVRC_TEST_TC_NETEM").is_none() || !cfg!(target_os = "linux") {
            eprintln!("skipping process packet-loss acceptance without isolated Linux tc-netem mode");
            return;
        }

        let evidence_path = PathBuf::from(std::env::var("SHOCKINGVRC_DIRECT_WSS_PROCESS_EVIDENCE").unwrap());
        let root = evidence_path.join(format!("run-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create unique isolated process fixture root");
        let mut matrix = ProcessPair::start(&root, "matrix").await;
        let mut nine_second_resume = None;
        for duration in [5, 9, 15, 25] {
            let resumed = inject_outage(&matrix, duration, true).await;
            if duration == 9 {
                nine_second_resume = Some(resumed);
            }
        }
        let forty_second_resumed = inject_outage(&matrix, 40, false).await;
        assert!(
            !forty_second_resumed,
            "40-second bounded outage must not resume after restoration"
        );
        matrix.stop().await;

        let mut cycles = ProcessPair::start(&root, "cycles").await;
        let host_pid = cycles.host_pid();
        let terminal_pid = cycles.terminal_pid();
        let first_resume_count = read_events(&cycles.terminal_events)
            .iter()
            .filter(|event| event["kind"] == "resume_success")
            .count();
        for cycle in 1..=10 {
            inject_outage(&cycles, 15, true).await;
            assert_eq!(
                cycles.host_pid(),
                host_pid,
                "Host must remain the same process through repeated cycle {cycle}"
            );
            assert_eq!(
                cycles.terminal_pid(),
                terminal_pid,
                "Terminal must remain the same process through repeated cycle {cycle}"
            );
        }
        let final_resume_count = read_events(&cycles.terminal_events)
            .iter()
            .filter(|event| event["kind"] == "resume_success")
            .count();
        assert_eq!(
            final_resume_count - first_resume_count,
            10,
            "same Host and Terminal processes must complete ten real loss/resume cycles"
        );
        cycles.stop().await;
        println!(
            "DIRECT_WSS_TWO_PROCESS_PACKET_LOSS_MATRIX_COMPLETE host_pid={host_pid} terminal_pid={terminal_pid} cycles=10 nine_second_resume={nine_second_resume:?}"
        );
        assert!(
            nine_second_resume.is_some(),
            "the 9-second liveness-boundary case must complete safely"
        );

        println!("process evidence preserved at {}", root.display());
    }
}

#[cfg(test)]
#[path = "rtc_process_loss.rs"]
mod rtc_process_loss;

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
    use crate::app_state::UiBridge;

    #[test]
    fn permanent_turn_refresh_failure_reconnects_only_relay_sessions() {
        let relay = shocking_vrc_rtc::TurnRefreshFailureDisposition::ReconnectRelaySession;
        let direct = shocking_vrc_rtc::TurnRefreshFailureDisposition::KeepDirectSession;
        assert_eq!(
            permanent_turn_refresh_outcome(&shocking_vrc_rtc::SessionError::Closed, relay),
            Some(TurnRefreshOutcome::Reconnect(ReconnectCause::SignalingLeaseLost)),
        );
        assert_eq!(
            permanent_turn_refresh_outcome(&shocking_vrc_rtc::SessionError::Closed, direct),
            Some(TurnRefreshOutcome::Disabled),
        );
        assert_eq!(
            permanent_turn_refresh_outcome(
                &shocking_vrc_rtc::SessionError::Rendezvous("permanent response".to_owned()),
                relay,
            ),
            Some(TurnRefreshOutcome::Reconnect(ReconnectCause::PermanentTurnRenewalFailure)),
        );
        assert_eq!(
            permanent_turn_refresh_outcome(&shocking_vrc_rtc::SessionError::Timeout("test"), relay),
            None,
        );
        assert_eq!(
            permanent_turn_refresh_outcome(
                &shocking_vrc_rtc::SessionError::TransientRendezvous("temporarily_unavailable".to_owned()),
                relay,
            ),
            None,
            "temporary Rendezvous failures must retry without tearing down a healthy relay session",
        );
    }

    #[test]
    fn permanent_turn_renewal_failure_is_distinct_from_a_heartbeat_timeout() {
        assert_ne!(ReconnectCause::PermanentTurnRenewalFailure, ReconnectCause::HeartbeatTimeout);
        assert!(reconnect_status_message(ReconnectCause::PermanentTurnRenewalFailure).contains("TURN"));
        assert!(reconnect_status_message(ReconnectCause::HeartbeatTimeout).contains("heartbeat"));
    }

    #[test]
    fn signaling_loss_preserves_only_a_confirmed_direct_path() {
        assert_eq!(
            signaling_loss_policy(Some(false)),
            SignalingLossPolicy::PreserveDirectDataChannel,
        );
        assert_eq!(
            signaling_loss_policy(Some(true)),
            SignalingLossPolicy::ResumeRelayOrUnknown,
        );
        assert_eq!(
            signaling_loss_policy(None),
            SignalingLossPolicy::ResumeRelayOrUnknown,
        );
    }

    #[tokio::test]
    async fn terminal_signaling_loss_status_is_observed_without_consuming_datachannel_frames() {
        let (sender, mut status) = watch::channel(shocking_vrc_rtc::SignalingLeaseStatus::Active);
        sender.send_replace(shocking_vrc_rtc::SignalingLeaseStatus::RoomClosed);
        assert_eq!(
            wait_for_signaling_lease_end(&mut status).await,
            shocking_vrc_rtc::SignalingLeaseStatus::RoomClosed,
        );
        assert_eq!(
            signaling_loss_policy(Some(false)),
            SignalingLossPolicy::PreserveDirectDataChannel,
        );
    }

    #[cfg(windows)]
    static CREDENTIAL_STORE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn test_snapshot() -> SnapshotDto {
        serde_json::from_value(json!({
            "bridge_revision": 0,
            "active_device": 0,
            "devices": [],
            "vrchat": { "connected": false, "osc_port": 0, "oscquery_port": null },
            "avatar_zones": [],
            "config": {
                "channel_a": {
                    "zones": [], "frequency": [0, 0, 0, 0], "intensity": [0, 0, 0, 0],
                    "limits": { "min": 0, "max": 200 }, "aggregation": "mean",
                    "freq_modulation": [null, null, null, null],
                    "intensity_modulation": [null, null, null, null]
                },
                "channel_b": {
                    "zones": [], "frequency": [0, 0, 0, 0], "intensity": [0, 0, 0, 0],
                    "limits": { "min": 0, "max": 200 }, "aggregation": "mean",
                    "freq_modulation": [null, null, null, null],
                    "intensity_modulation": [null, null, null, null]
                },
                "ukf": {}, "norms": {}
            },
            "status": {},
            "alarm": {},
            "remote_policy": {
                "remote_strength": {
                    "enabled": false,
                    "max_by_channel": { "A": null, "B": null },
                    "scope": "power_limits"
                },
                "max_remote_sessions": 1
            }
        }))
        .expect("valid test snapshot")
    }

    fn test_authenticated_connection() -> (AuthenticatedConnection, mpsc::Sender<Vec<u8>>, mpsc::Receiver<Vec<u8>>) {
        let server_epoch = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let (inbound_tx, inbound) = mpsc::channel(8);
        let (outbound, outbound_rx) = mpsc::channel(8);
        let profile = ProfileDto {
            id: "profile-test".to_owned(),
            label: "Host test".to_owned(),
            label_is_custom: false,
            host_display_name: None,
            host: None,
            port: None,
            fingerprint: "test-fingerprint".to_owned(),
            server_id: "test-host".to_owned(),
            connection_mode: "traversal_only".to_owned(),
            rendezvous_url: String::new(),
            ice_server_urls: Vec::new(),
            force_relay: false,
        };
        let socket = SessionSocket::Rtc {
            inbound,
            outbound: Some(outbound),
            peer: None,
            channels: None,
            signaling: None,
            signaling_status: None,
            ice_candidate_pair: None,
            ice_servers_configured: false,
            rendezvous_stun_received: false,
            turn_credentials_received: false,
            local_candidates_gathered: false,
            room_id: Uuid::new_v4(),
            turn_refresh_at: None,
        };
        (
            AuthenticatedConnection {
                profile,
                socket,
                identity: identity_for_session_end_test(&server_epoch.to_string()),
                secrets: SessionSecrets {
                    session_id: session_id.to_string(),
                    resume_ticket: Zeroizing::new("test-ticket".to_owned()),
                    access_token: Zeroizing::new("test-token".to_owned()),
                    access_expires_at: Instant::now() + Duration::from_secs(3_600),
                    generation: 1,
                    resume_request_id: Uuid::new_v4(),
                },
                initial_snapshot: test_snapshot(),
                initial_snapshot_seq: 1,
                capabilities: CapabilitiesDto {
                    operations: Vec::new(),
                    features: Vec::new(),
                },
                device_grant: Zeroizing::new("test-device-grant".to_owned()),
                session_generation: 1,
            },
            inbound_tx,
            outbound_rx,
        )
    }

    async fn assert_terminal_end_stays_offline_for_30_seconds(reason: Option<&str>, expected_phase: &str) {
        let (connection, inbound, _outbound) = test_authenticated_connection();
        if let Some(reason) = reason {
            inbound
                .send(
                    serde_json::to_vec(&json!({
                        "type": "session.ended",
                        "protocol": PROTOCOL_VERSION,
                        "server_epoch": connection.identity.server_epoch,
                        "session_id": connection.secrets.session_id,
                        "transport_generation": connection.secrets.generation,
                        "reason": reason,
                    }))
                    .unwrap(),
                )
                .await
                .expect("queue authenticated session end");
        }
        let updates = Arc::new(UiBridge::default());
        let (actions, action_receiver) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        if reason.is_none() {
            cancel.cancel();
        }
        let session = run_session(connection, action_receiver, Arc::clone(&updates), cancel);
        tokio::time::timeout(Duration::from_secs(2), session)
            .await
            .expect("ended session exited without waiting for heartbeat timeout");

        assert_eq!(
            updates
                .latest_connection_for_test()
                .await
                .expect("terminal connection state")
                .phase,
            expected_phase
        );
        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(
            updates.connection_phases_for_test().await,
            vec!["online", expected_phase],
            "no delayed resume or automatic login may follow an explicit end"
        );
        drop(actions);
    }

    #[tokio::test(start_paused = true)]
    async fn host_kick_does_not_resume_automatically_after_30_seconds() {
        assert_terminal_end_stays_offline_for_30_seconds(Some("host_kick"), "host_kicked").await;
    }

    #[tokio::test(start_paused = true)]
    async fn manual_disconnect_does_not_resume_automatically_after_30_seconds() {
        assert_terminal_end_stays_offline_for_30_seconds(None, "user_disconnected").await;
    }

    #[tokio::test]
    async fn direct_websocket_session_honors_the_bound_host_kick_notice() {
        use tokio::net::TcpListener;
        use tokio_tungstenite::{accept_async, tungstenite::Message};

        let (mut connection, _inbound, _outbound) = test_authenticated_connection();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback WebSocket listener");
        let address = listener.local_addr().expect("loopback listener address");
        let expected_epoch = connection.identity.server_epoch.clone();
        let expected_session_id = connection.secrets.session_id.clone();
        let expected_generation = connection.secrets.generation;
        let frame = json!({
            "type": "session.ended",
            "protocol": PROTOCOL_VERSION,
            "server_epoch": connection.identity.server_epoch,
            "session_id": connection.secrets.session_id,
            "transport_generation": connection.secrets.generation,
            "reason": "host_kick",
        });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("Terminal WSS peer");
            let mut socket = accept_async(stream).await.expect("WebSocket handshake");
            socket
                .send(Message::Text(frame.to_string().into()))
                .await
                .expect("send bound Host Kick");
            let mut acknowledgement = None;
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Text(text) => {
                        let value: Value = serde_json::from_str(text.as_str()).expect("Terminal acknowledgement JSON");
                        if value["type"] == "session.ended.ack" {
                            acknowledgement = Some(value);
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            acknowledgement
        });
        let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/remote"))
            .await
            .expect("Terminal loopback WebSocket");
        connection.socket = SessionSocket::Direct(socket);
        let updates = Arc::new(UiBridge::default());
        let (_actions, action_receiver) = mpsc::channel(8);

        tokio::time::timeout(
            Duration::from_secs(2),
            run_session(connection, action_receiver, Arc::clone(&updates), CancellationToken::new()),
        )
        .await
        .expect("Direct WebSocket session ended without heartbeat-based resume");
        assert_eq!(updates.latest_connection_for_test().await.unwrap().phase, "host_kicked");
        let acknowledgement = server
            .await
            .expect("WebSocket server cleanup")
            .expect("Terminal acknowledged the bound Host end before closing");
        assert_eq!(acknowledgement["server_epoch"], expected_epoch);
        assert_eq!(acknowledgement["session_id"], expected_session_id);
        assert_eq!(acknowledgement["transport_generation"], expected_generation);
    }

    #[tokio::test(start_paused = true)]
    async fn same_session_resume_ends_at_the_finite_host_unavailable_deadline() {
        let task = tokio::spawn(resume_within_window(std::future::pending::<Result<(), ResumeError>>()));
        tokio::task::yield_now().await;
        tokio::time::advance(RESUME_WINDOW).await;

        assert!(matches!(
            task.await.expect("bounded resume task completed"),
            Err(ResumeError::HostUnavailable(message)) if message.contains("resume window elapsed after 25 seconds")
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_resume_transport_attempt_is_canceled_before_the_retry_window() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let dropped = Arc::new(AtomicBool::new(false));
        let attempt_dropped = Arc::clone(&dropped);
        let attempt = async move {
            struct MarkDropped(Arc<AtomicBool>);
            impl Drop for MarkDropped {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _mark_dropped = MarkDropped(attempt_dropped);
            std::future::pending::<Result<(), String>>().await
        };
        let task = tokio::spawn(resume_connect_attempt(attempt, RESUME_CONNECT_ATTEMPT_TIMEOUT));
        tokio::task::yield_now().await;
        tokio::time::advance(RESUME_CONNECT_ATTEMPT_TIMEOUT).await;

        let result = task.await.expect("bounded transport attempt completed");
        assert!(
            result
                .unwrap_err()
                .contains("Timed out re-establishing the pinned Host transport")
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "timed-out connector must release its session resources"
        );
    }

    #[test]
    fn relay_resume_allows_turn_allocation_time_without_changing_liveness_deadline() {
        let (mut connection, _old_inbound, _old_outbound) = test_authenticated_connection();
        assert_eq!(resume_attempt_timeout(&connection), RESUME_CONNECT_ATTEMPT_TIMEOUT);
        assert_eq!(HEARTBEAT_TIMEOUT, Duration::from_secs(10));

        connection.profile.force_relay = true;
        assert_eq!(resume_attempt_timeout(&connection), RESUME_TURN_CONNECT_ATTEMPT_TIMEOUT);

        connection.profile.force_relay = false;
        if let SessionSocket::Rtc {
            turn_credentials_received,
            ..
        } = &mut connection.socket
        {
            *turn_credentials_received = true;
        }
        assert_eq!(resume_attempt_timeout(&connection), RESUME_TURN_CONNECT_ATTEMPT_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn first_same_session_resume_attempt_starts_without_backoff() {
        let (connection, _old_inbound, _old_outbound) = test_authenticated_connection();
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let connect: Box<PendingConnector> = Box::new(move |_, _| {
            let _ = started_tx.send(());
            Box::pin(async { Err("temporary test failure".to_owned()) })
        });
        let cancel = CancellationToken::new();
        let mut attempt = Box::pin(reconnect_and_resume_with(&connection, &cancel, connect));
        let started_immediately = tokio::select! {
            _ = &mut attempt => false,
            started = tokio::time::timeout(Duration::from_millis(1), started_rx.recv()) => started.is_ok(),
        };
        cancel.cancel();
        drop(attempt);

        assert!(
            started_immediately,
            "the first auth.resume transport attempt must not sleep for backoff"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_loss_is_detected_at_ten_seconds_independent_of_ping_ticks() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        assert_eq!(HEARTBEAT_PERIOD, Duration::from_secs(3));
        assert_eq!(HEARTBEAT_TIMEOUT, Duration::from_secs(10));
        let (connection, _old_inbound, _old_outbound) = test_authenticated_connection();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let resume: Box<ResumeOperation> = Box::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        });
        let updates = Arc::new(UiBridge::default());
        let (_actions, action_receiver) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let session = tokio::spawn(run_session_with_resume(
            connection,
            action_receiver,
            updates,
            cancel.clone(),
            resume,
        ));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(9)).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 0, "do not reconnect before the loss deadline");

        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "the independent deadline starts resume at 10 seconds"
        );

        cancel.cancel();
        session.await.expect("Terminal session stopped after test cancellation");
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_deadline_preempts_a_blocked_outbound_operation() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (connection, _inbound, _outbound_rx) = test_authenticated_connection();
        let outbound = match &connection.socket {
            SessionSocket::Rtc {
                outbound: Some(outbound),
                ..
            } => outbound.clone(),
            _ => unreachable!("test connection uses the RTC channel adapter"),
        };
        for _ in 0..8 {
            outbound.try_send(Vec::new()).expect("fill the outbound queue");
        }

        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let resume: Box<ResumeOperation> = Box::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        });
        let updates = Arc::new(UiBridge::default());
        let (_actions, action_receiver) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let session = tokio::spawn(run_session_with_resume(
            connection,
            action_receiver,
            updates,
            cancel.clone(),
            resume,
        ));

        tokio::task::yield_now().await;
        tokio::time::advance(HEARTBEAT_TIMEOUT).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a blocked heartbeat send must not hold the independent loss deadline past 10 seconds",
        );

        cancel.cancel();
        session.await.expect("Terminal session stopped after test cancellation");
    }

    #[tokio::test(start_paused = true)]
    async fn pending_mutating_command_is_not_replayed_after_same_session_resume() {
        let (connection, _old_inbound, mut old_outbound) = test_authenticated_connection();
        let session_id = connection.secrets.session_id.clone();
        let server_epoch = connection.identity.server_epoch.clone();
        let (replacement_inbound_tx, replacement_inbound) = mpsc::channel(8);
        let (replacement_outbound_tx, mut replacement_outbound_rx) = mpsc::channel(16);
        let replacement_socket = SessionSocket::Rtc {
            inbound: replacement_inbound,
            outbound: Some(replacement_outbound_tx),
            peer: None,
            channels: None,
            signaling: None,
            signaling_status: None,
            ice_candidate_pair: None,
            ice_servers_configured: false,
            rendezvous_stun_received: false,
            turn_credentials_received: false,
            local_candidates_gathered: false,
            room_id: Uuid::new_v4(),
            turn_refresh_at: None,
        };
        let mut snapshot_json = serde_json::to_value(test_snapshot()).unwrap();
        snapshot_json["bridge_revision"] = json!(42);
        let fresh_snapshot = serde_json::from_value(snapshot_json).unwrap();
        let fresh_capabilities = CapabilitiesDto {
            operations: Vec::new(),
            features: vec!["resumed-session".to_owned()],
        };
        let mut replacement = Some((
            replacement_socket,
            HelloIdentity {
                server_id: "test-host".to_owned(),
                server_epoch: server_epoch.clone(),
                fingerprint: "test-fingerprint".to_owned(),
                client_nonce: "replacement-nonce".to_owned(),
            },
            SessionSecrets {
                session_id: session_id.clone(),
                resume_ticket: Zeroizing::new("rotated-test-ticket".to_owned()),
                access_token: Zeroizing::new("rotated-test-token".to_owned()),
                access_expires_at: Instant::now() + Duration::from_secs(3_600),
                generation: 2,
                resume_request_id: Uuid::new_v4(),
            },
            fresh_snapshot,
            2,
            fresh_capabilities,
        ));
        let resume: Box<ResumeOperation> = Box::new(move |current, _cancel| {
            assert_eq!(current.secrets.session_id, session_id);
            assert_eq!(current.identity.server_epoch, server_epoch);
            let resumed = replacement.take().expect("one replacement transport");
            Box::pin(async move { Ok(resumed) })
        });

        let updates = Arc::new(UiBridge::default());
        let (actions, action_receiver) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let session = tokio::spawn(run_session_with_resume(
            connection,
            action_receiver,
            Arc::clone(&updates),
            cancel.clone(),
            resume,
        ));

        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        while old_outbound.try_recv().is_ok() {}

        let request_id = Uuid::new_v4();
        let (response, response_receiver) = oneshot::channel();
        actions
            .send(SessionAction::Command {
                request_id,
                frame: json!({
                    "type": "command",
                    "protocol": PROTOCOL_VERSION,
                    "request_id": request_id,
                    "operation": "modulation.clear_segment",
                    "params": {"segment": "test-only"},
                }),
                response,
            })
            .await
            .expect("queue one test-only mutating command");

        let mut observed_once = false;
        for _ in 0..32 {
            tokio::task::yield_now().await;
            while let Ok(bytes) = old_outbound.try_recv() {
                let frame: Value = serde_json::from_slice(&bytes).expect("old transport JSON frame");
                if frame["request_id"] == request_id.to_string() {
                    assert_eq!(frame["operation"], "modulation.clear_segment");
                    observed_once = true;
                }
            }
            if observed_once {
                break;
            }
        }
        assert!(observed_once, "send the mutation once on the original transport");

        tokio::time::advance(HEARTBEAT_TIMEOUT).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        let response = tokio::time::timeout(Duration::from_secs(1), response_receiver)
            .await
            .expect("pending command resolves when the old session loses liveness")
            .expect("command result is returned to its caller");
        let error = response.expect_err("the unacknowledged command outcome is unknown");
        assert!(error.contains("outcome is unknown"));
        assert!(error.contains("not replayed"));

        let state = updates.latest_connection_for_test().await.unwrap();
        assert_eq!(state.phase, "online");
        assert_eq!(state.message.as_deref(), Some("Session resumed with fresh Host state"));
        assert_eq!(updates.latest_snapshot_for_test().await.unwrap().state.bridge_revision, 42);

        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        while let Ok(bytes) = replacement_outbound_rx.try_recv() {
            let frame: Value = serde_json::from_slice(&bytes).expect("replacement transport JSON frame");
            assert_ne!(
                frame["request_id"],
                json!(request_id.to_string()),
                "the original mutating command must not be replayed after resume"
            );
        }

        drop(replacement_inbound_tx);
        cancel.cancel();
        session.await.expect("Terminal session stopped after test cancellation");
    }

    #[tokio::test(start_paused = true)]
    async fn snapshot_frames_are_processed_while_the_heartbeat_deadline_runs() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (connection, inbound, _outbound_rx) = test_authenticated_connection();
        let server_epoch = connection.identity.server_epoch.clone();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let resume: Box<ResumeOperation> = Box::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        });
        let updates = Arc::new(UiBridge::default());
        let (_actions, action_receiver) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let session = tokio::spawn(run_session_with_resume(
            connection,
            action_receiver,
            Arc::clone(&updates),
            cancel.clone(),
            resume,
        ));

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(9)).await;
        let mut snapshot = serde_json::to_value(test_snapshot()).expect("serialize Host snapshot");
        snapshot["bridge_revision"] = json!(9);
        inbound
            .send(
                json!({ "type": "snapshot", "server_epoch": server_epoch, "seq": 2, "state": snapshot })
                    .to_string()
                    .into_bytes(),
            )
            .await
            .expect("send Host snapshot during the liveness deadline");
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(updates.latest_snapshot_for_test().await.unwrap().state.bridge_revision, 9);

        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "one-way snapshot traffic does not mask missing round-trip heartbeat liveness"
        );

        cancel.cancel();
        session.await.expect("Terminal session stopped after test cancellation");
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_pong_for_an_earlier_outstanding_ping_refreshes_liveness() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (connection, inbound, mut outbound) = test_authenticated_connection();
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let resume: Box<ResumeOperation> = Box::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        });
        let updates = Arc::new(UiBridge::default());
        let (_actions, action_receiver) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let session = tokio::spawn(run_session_with_resume(
            connection,
            action_receiver,
            updates,
            cancel.clone(),
            resume,
        ));
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        let first_ping: Value = serde_json::from_slice(&outbound.try_recv().expect("immediate heartbeat")).unwrap();
        assert_eq!(first_ping["type"], "ping");

        tokio::time::advance(HEARTBEAT_PERIOD).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        let second_ping: Value = serde_json::from_slice(&outbound.try_recv().expect("second heartbeat")).unwrap();
        assert_ne!(first_ping["nonce"], second_ping["nonce"]);
        inbound
            .send(
                json!({ "type": "pong", "nonce": first_ping["nonce"] })
                    .to_string()
                    .into_bytes(),
            )
            .await
            .expect("send delayed pong for first ping");
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }

        tokio::time::advance(Duration::from_secs(9)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            0,
            "a delayed valid pong refreshes the independent deadline"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        cancel.cancel();
        session.await.expect("Terminal session stopped after test cancellation");
    }

    #[tokio::test(start_paused = true)]
    async fn transient_transport_loss_resumes_the_same_session_with_fresh_state() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (connection, old_inbound, old_outbound) = test_authenticated_connection();
        drop(old_inbound);
        drop(old_outbound);

        let expected_session_id = connection.secrets.session_id.clone();
        let expected_epoch = connection.identity.server_epoch.clone();
        let mut fresh_snapshot_json = serde_json::to_value(test_snapshot()).unwrap();
        fresh_snapshot_json["bridge_revision"] = json!(42);
        let fresh_snapshot = serde_json::from_value(fresh_snapshot_json).unwrap();
        let fresh_capabilities = CapabilitiesDto {
            operations: Vec::new(),
            features: vec!["resumed-session".to_owned()],
        };
        let (new_inbound_tx, new_inbound) = mpsc::channel(8);
        let (new_outbound, _new_outbound_rx) = mpsc::channel(8);
        let replacement_socket = SessionSocket::Rtc {
            inbound: new_inbound,
            outbound: Some(new_outbound),
            peer: None,
            channels: None,
            signaling: None,
            signaling_status: None,
            ice_candidate_pair: None,
            ice_servers_configured: false,
            rendezvous_stun_received: false,
            turn_credentials_received: false,
            local_candidates_gathered: false,
            room_id: Uuid::new_v4(),
            turn_refresh_at: None,
        };
        let mut replacement = Some((
            replacement_socket,
            SessionSecrets {
                session_id: expected_session_id.clone(),
                resume_ticket: Zeroizing::new("rotated-test-ticket".to_owned()),
                access_token: Zeroizing::new("rotated-test-token".to_owned()),
                access_expires_at: Instant::now() + Duration::from_secs(3_600),
                generation: 2,
                resume_request_id: Uuid::new_v4(),
            },
            fresh_snapshot,
            2,
            fresh_capabilities,
        ));
        let resume_attempts = Arc::new(AtomicUsize::new(0));
        let attempt_counter = Arc::clone(&resume_attempts);
        let resume: Box<ResumeOperation> = Box::new(move |current, _cancel| {
            attempt_counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(current.secrets.session_id, expected_session_id);
            assert_eq!(current.identity.server_epoch, expected_epoch);
            assert_eq!(current.secrets.generation, 1);
            let (socket, secrets, snapshot, snapshot_seq, capabilities) = replacement.take().expect("one successful resume result");
            let identity = HelloIdentity {
                server_id: current.identity.server_id.clone(),
                server_epoch: current.identity.server_epoch.clone(),
                fingerprint: current.identity.fingerprint.clone(),
                client_nonce: current.identity.client_nonce.clone(),
            };
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok((socket, identity, secrets, snapshot, snapshot_seq, capabilities))
            })
        });

        let updates = Arc::new(UiBridge::default());
        let (_actions, action_receiver) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let session = tokio::spawn(run_session_with_resume(
            connection,
            action_receiver,
            Arc::clone(&updates),
            cancel.clone(),
            resume,
        ));

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
            if updates.latest_connection_for_test().await.is_some_and(|state| {
                state.phase == "online" && state.message.as_deref() == Some("Session resumed with fresh Host state")
            }) {
                break;
            }
        }

        let state = updates.latest_connection_for_test().await.unwrap();
        assert_eq!(state.phase, "online");
        assert_eq!(state.message.as_deref(), Some("Session resumed with fresh Host state"));
        assert_eq!(updates.latest_snapshot_for_test().await.unwrap().state.bridge_revision, 42);
        assert_eq!(
            updates.connection_phases_for_test().await,
            vec!["online", "reconnecting", "reconnecting", "online"]
        );
        assert_eq!(resume_attempts.load(Ordering::SeqCst), 1);

        drop(new_inbound_tx);
        cancel.cancel();
        session.await.expect("session exits after test cleanup");
    }

    #[tokio::test]
    async fn loopback_transport_loss_retries_a_failed_resume_and_restores_fresh_state() {
        use futures_util::{SinkExt, StreamExt};
        use tokio::net::TcpListener;
        use tokio_tungstenite::{accept_async, tungstenite::Message};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("ws://{address}/api/v1/ws");
        let (mut connection, _, _) = test_authenticated_connection();
        let server_epoch = connection.identity.server_epoch.clone();
        let session_id = connection.secrets.session_id.clone();
        let server_session_id = session_id.clone();
        let resume_request_id = connection.secrets.resume_request_id;
        let server_session = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut original = accept_async(stream).await.unwrap();
            original.send(Message::Close(None)).await.unwrap();

            let (stream, _) = listener.accept().await.unwrap();
            let mut failed_resume = accept_async(stream).await.unwrap();
            failed_resume.send(Message::Close(None)).await.unwrap();

            let (stream, _) = listener.accept().await.unwrap();
            let mut resumed = accept_async(stream).await.unwrap();
            let resume: Value = loop {
                match resumed.next().await {
                    Some(Ok(Message::Text(text))) => break serde_json::from_str(&text).unwrap(),
                    Some(Ok(Message::Ping(payload))) => resumed.send(Message::Pong(payload)).await.unwrap(),
                    other => panic!("expected auth.resume on the replacement transport, got {other:?}"),
                }
            };
            assert_eq!(resume["type"], "auth.resume");
            assert_eq!(resume["session_id"], server_session_id);
            assert_eq!(resume["resume_ticket"], "test-ticket");
            assert_eq!(resume["request_id"], resume_request_id.to_string());

            let auth = json!({
                "type": "auth.result",
                "protocol": PROTOCOL_VERSION,
                "request_id": resume_request_id,
                "ok": true,
                "session_state": "ACTIVE",
                "session_id": server_session_id,
                "server_epoch": server_epoch,
                "access_token": "rotated-test-token",
                "resume_ticket": "rotated-test-ticket",
                "access_expires_in": 3600,
                "transport_generation": 2
            });
            resumed.send(Message::Text(auth.to_string().into())).await.unwrap();

            let mut fresh_snapshot = serde_json::to_value(test_snapshot()).unwrap();
            fresh_snapshot["bridge_revision"] = json!(42);
            resumed
                .send(Message::Text(
                    json!({
                        "type": "snapshot", "protocol": PROTOCOL_VERSION,
                        "server_epoch": server_epoch, "seq": 1,
                        "state": fresh_snapshot
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();

            let capabilities_request: Value = loop {
                match resumed.next().await {
                    Some(Ok(Message::Text(text))) => break serde_json::from_str(&text).unwrap(),
                    Some(Ok(Message::Ping(payload))) => resumed.send(Message::Pong(payload)).await.unwrap(),
                    other => panic!("expected capabilities request, got {other:?}"),
                }
            };
            assert_eq!(capabilities_request["type"], "capabilities.get");
            let capabilities = json!({
                "type": "capabilities", "protocol": PROTOCOL_VERSION,
                "request_id": capabilities_request["request_id"],
                "capabilities": { "operations": [], "features": ["resumed-session"] }
            });
            resumed
                .send(Message::Text(capabilities.to_string().into()))
                .await
                .unwrap();
        });

        let (mut initial_socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        assert!(matches!(initial_socket.next().await, Some(Ok(Message::Close(_)))));
        connection.socket = SessionSocket::Direct(initial_socket);

        let expected_profile_id = connection.profile.id.clone();
        let expected_session_uuid = Uuid::parse_str(&session_id).unwrap();
        let identity = HelloIdentity {
            server_id: connection.identity.server_id.clone(),
            server_epoch: connection.identity.server_epoch.clone(),
            fingerprint: connection.identity.fingerprint.clone(),
            client_nonce: connection.identity.client_nonce.clone(),
        };
        let connect_url = url.clone();
        let connect: Box<PendingConnector> = Box::new(move |profile, resume_session_id| {
            let connect_url = connect_url.clone();
            let expected_profile_id = expected_profile_id.clone();
            let identity = HelloIdentity {
                server_id: identity.server_id.clone(),
                server_epoch: identity.server_epoch.clone(),
                fingerprint: identity.fingerprint.clone(),
                client_nonce: identity.client_nonce.clone(),
            };
            Box::pin(async move {
                assert_eq!(profile.id, expected_profile_id);
                assert_eq!(resume_session_id, expected_session_uuid);
                let (socket, _) = tokio_tungstenite::connect_async(connect_url)
                    .await
                    .map_err(|error| error.to_string())?;
                let pending = PendingConnection {
                    profile: profile.clone(),
                    socket: SessionSocket::Direct(socket),
                    identity,
                };
                let host_identity = host_identity_dto(&pending);
                Ok((pending, host_identity))
            })
        });

        let cancel = CancellationToken::new();
        let (socket, resumed_identity, secrets, snapshot, _snapshot_seq, capabilities) = tokio::time::timeout(
            Duration::from_secs(10),
            reconnect_and_resume_with(&connection, &cancel, connect),
        )
        .await
        .expect("bounded local resume completed")
        .expect("one failed transport attempt was retried");

        assert_eq!(socket.transport_name(), "direct_wss");
        assert_eq!(resumed_identity.server_epoch, connection.identity.server_epoch);
        assert_eq!(secrets.session_id, connection.secrets.session_id);
        assert_eq!(secrets.generation, 2);
        assert_eq!(secrets.resume_ticket.as_str(), "rotated-test-ticket");
        assert_eq!(snapshot.bridge_revision, 42);
        assert_eq!(capabilities.features, vec!["resumed-session"]);
        server_session
            .await
            .expect("local WebSocket peer completed the resume exchange");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn loopback_tcp_proxy_loss_matrix_preserves_resume_window_and_fresh_state() {
        use futures_util::{SinkExt, StreamExt};
        use std::process::Command;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::{
                TcpListener, TcpStream,
                tcp::{OwnedReadHalf, OwnedWriteHalf},
            },
        };
        use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

        let use_tc_netem = std::env::var_os("SHOCKINGVRC_TEST_TC_NETEM").is_some();
        if use_tc_netem {
            assert!(cfg!(target_os = "linux"), "tc netem mode is Linux-only");
        }

        fn set_tc_netem_loss(enabled: bool) {
            let current = Command::new("tc")
                .args(["qdisc", "show", "dev", "lo"])
                .output()
                .expect("read tc loopback qdisc state");
            assert!(
                current.status.success(),
                "tc qdisc state read failed: {}",
                String::from_utf8_lossy(&current.stderr)
            );
            let netem_active = String::from_utf8_lossy(&current.stdout)
                .lines()
                .any(|line| line.starts_with("qdisc netem "));
            if !enabled && !netem_active {
                return;
            }

            let mut command = Command::new("tc");
            if enabled {
                assert!(!netem_active, "loopback netem must not already be active before a new outage");
                command.args(["qdisc", "add", "dev", "lo", "root", "netem", "loss", "100%"]);
            } else {
                command.args(["qdisc", "del", "dev", "lo", "root"]);
            }
            let output = command.output().expect("run tc to control loopback packet loss");
            assert!(
                output.status.success(),
                "tc packet-loss transition failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        fn parse_tc_dropped_packets(text: &str) -> usize {
            text.split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .find_map(|pair| {
                    (pair[0].trim_start_matches('(') == "dropped").then(|| {
                        pair[1]
                            .trim_end_matches(',')
                            .parse::<usize>()
                            .expect("numeric tc drop counter")
                    })
                })
                .unwrap_or(0)
        }

        fn tc_dropped_packets() -> usize {
            let output = Command::new("tc")
                .args(["-s", "qdisc", "show", "dev", "lo"])
                .output()
                .expect("read tc loopback packet counters");
            assert!(
                output.status.success(),
                "tc packet counter read failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            parse_tc_dropped_packets(&String::from_utf8_lossy(&output.stdout))
        }

        assert_eq!(
            parse_tc_dropped_packets("Sent 64 bytes 1 pkt (dropped 5, overlimits 0 requeues 0)"),
            5,
            "the Linux tc counter parser must accept the standard parenthesized stats format"
        );

        fn unix_epoch_millis() -> u128 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_millis()
        }

        #[derive(Default)]
        struct FakeHostSession {
            generation: u64,
            ticket: String,
            last_contact: Option<Instant>,
        }

        async fn copy_with_optional_drop(
            mut reader: OwnedReadHalf,
            mut writer: OwnedWriteHalf,
            drop_frames: Arc<AtomicBool>,
            dropped_bytes: Arc<AtomicUsize>,
            forwarded_bytes: Arc<AtomicUsize>,
        ) {
            let mut buffer = [0_u8; 8192];
            loop {
                let read = match reader.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                if drop_frames.load(Ordering::SeqCst) {
                    dropped_bytes.fetch_add(read, Ordering::SeqCst);
                } else if writer.write_all(&buffer[..read]).await.is_ok() {
                    forwarded_bytes.fetch_add(read, Ordering::SeqCst);
                } else {
                    break;
                }
            }
        }

        struct TcpLossProxy {
            url: String,
            drop_frames: Arc<AtomicBool>,
            dropped_bytes: Arc<AtomicUsize>,
            forwarded_bytes: Arc<AtomicUsize>,
            cancel: CancellationToken,
            accept_task: Option<tokio::task::JoinHandle<()>>,
        }

        impl Drop for TcpLossProxy {
            fn drop(&mut self) {
                self.cancel.cancel();
                if let Some(task) = self.accept_task.take() {
                    task.abort();
                }
            }
        }

        async fn start_tcp_loss_proxy(target: std::net::SocketAddr) -> TcpLossProxy {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind loopback TCP fault proxy");
            let address = listener.local_addr().expect("fault proxy address");
            let drop_frames = Arc::new(AtomicBool::new(false));
            let dropped_bytes = Arc::new(AtomicUsize::new(0));
            let forwarded_bytes = Arc::new(AtomicUsize::new(0));
            let cancel = CancellationToken::new();
            let task_cancel = cancel.clone();
            let task_drop = Arc::clone(&drop_frames);
            let task_dropped = Arc::clone(&dropped_bytes);
            let task_forwarded = Arc::clone(&forwarded_bytes);
            let accept_task = tokio::spawn(async move {
                loop {
                    let accepted = tokio::select! {
                        _ = task_cancel.cancelled() => break,
                        accepted = listener.accept() => accepted,
                    };
                    let Ok((client, _)) = accepted else {
                        continue;
                    };
                    let cancel = task_cancel.clone();
                    let drop_frames = Arc::clone(&task_drop);
                    let dropped_bytes = Arc::clone(&task_dropped);
                    let forwarded_bytes = Arc::clone(&task_forwarded);
                    tokio::spawn(async move {
                        let Ok(server) = TcpStream::connect(target).await else {
                            return;
                        };
                        let (client_read, client_write) = client.into_split();
                        let (server_read, server_write) = server.into_split();
                        let mut client_to_server = tokio::spawn(copy_with_optional_drop(
                            client_read,
                            server_write,
                            Arc::clone(&drop_frames),
                            Arc::clone(&dropped_bytes),
                            Arc::clone(&forwarded_bytes),
                        ));
                        let mut server_to_client = tokio::spawn(copy_with_optional_drop(
                            server_read,
                            client_write,
                            Arc::clone(&drop_frames),
                            Arc::clone(&dropped_bytes),
                            Arc::clone(&forwarded_bytes),
                        ));
                        tokio::select! {
                            _ = cancel.cancelled() => {},
                            _ = &mut client_to_server => {},
                            _ = &mut server_to_client => {},
                        }
                        client_to_server.abort();
                        server_to_client.abort();
                    });
                }
            });
            TcpLossProxy {
                url: format!("ws://{address}/api/v1/ws"),
                drop_frames,
                dropped_bytes,
                forwarded_bytes,
                cancel,
                accept_task: Some(accept_task),
            }
        }

        async fn serve_mock_authenticated_socket(
            stream: TcpStream,
            session_id: String,
            server_epoch: String,
            state: Arc<std::sync::Mutex<FakeHostSession>>,
            pings: Arc<AtomicUsize>,
            resumes: Arc<AtomicUsize>,
        ) {
            let Ok(mut socket) = accept_async(stream).await else {
                return;
            };
            let mut connection_generation = 1_u64;
            while let Some(Ok(message)) = socket.next().await {
                let Message::Text(text) = message else {
                    if let Message::Ping(payload) = message {
                        let _ = socket.send(Message::Pong(payload)).await;
                    }
                    continue;
                };
                let Ok(frame) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match frame.get("type").and_then(Value::as_str) {
                    Some("ping") => {
                        let active_generation = {
                            let mut state = state.lock().expect("mock Host session lock");
                            if state.generation == connection_generation {
                                state.last_contact = Some(Instant::now());
                                true
                            } else {
                                false
                            }
                        };
                        if active_generation {
                            pings.fetch_add(1, Ordering::SeqCst);
                            let pong = json!({ "type": "pong", "nonce": frame["nonce"] });
                            if socket.send(Message::Text(pong.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some("auth.resume") => {
                        let request_id = frame.get("request_id").and_then(Value::as_str).unwrap_or_default();
                        let resumed = {
                            let mut state = state.lock().expect("mock Host session lock");
                            let elapsed = state
                                .last_contact
                                .map(|at| Instant::now().duration_since(at))
                                .unwrap_or(Duration::MAX);
                            let expired = elapsed >= HEARTBEAT_TIMEOUT + Duration::from_secs(30);
                            let valid = !expired
                                && frame.get("session_id").and_then(Value::as_str) == Some(session_id.as_str())
                                && frame.get("resume_ticket").and_then(Value::as_str) == Some(state.ticket.as_str());
                            if valid {
                                state.generation = state.generation.saturating_add(1);
                                state.ticket = format!("test-ticket-{}", state.generation);
                                state.last_contact = Some(Instant::now());
                                Some((state.generation, state.ticket.clone()))
                            } else {
                                None
                            }
                        };
                        let Some((generation, ticket)) = resumed else {
                            let error = json!({
                                "type": "error", "protocol": PROTOCOL_VERSION,
                                "request_id": request_id, "code": "RESUME_TICKET_INVALID",
                                "message": "The Host resume reservation expired",
                            });
                            let _ = socket.send(Message::Text(error.to_string().into())).await;
                            break;
                        };
                        connection_generation = generation;
                        resumes.fetch_add(1, Ordering::SeqCst);

                        let auth = json!({
                            "type": "auth.result", "protocol": PROTOCOL_VERSION,
                            "request_id": request_id, "ok": true, "session_state": "ACTIVE",
                            "session_id": session_id, "server_epoch": server_epoch,
                            "access_token": format!("rotated-token-{generation}"),
                            "resume_ticket": ticket, "access_expires_in": 3600,
                            "transport_generation": generation,
                        });
                        let mut snapshot = serde_json::to_value(test_snapshot()).expect("serialize fresh snapshot");
                        snapshot["bridge_revision"] = json!(42 + generation);
                        if socket.send(Message::Text(auth.to_string().into())).await.is_err()
                            || socket
                                .send(Message::Text(
                                    json!({
                                        "type": "snapshot", "protocol": PROTOCOL_VERSION,
                                        "server_epoch": server_epoch, "seq": 1, "state": snapshot
                                    })
                                    .to_string()
                                    .into(),
                                ))
                                .await
                                .is_err()
                        {
                            break;
                        }
                        loop {
                            let Some(Ok(Message::Text(text))) = socket.next().await else {
                                break;
                            };
                            let Ok(request) = serde_json::from_str::<Value>(&text) else {
                                continue;
                            };
                            if request.get("type").and_then(Value::as_str) == Some("capabilities.get") {
                                let capabilities = json!({
                                    "type": "capabilities", "protocol": PROTOCOL_VERSION,
                                    "request_id": request["request_id"],
                                    "capabilities": { "operations": [], "features": ["packet-loss-resume"] },
                                });
                                if socket
                                    .send(Message::Text(capabilities.to_string().into()))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                break;
                            }
                        }
                    }
                    Some("auth.logout") => break,
                    _ => {}
                }
            }
        }

        async fn wait_until<T>(limit: Duration, mut predicate: impl FnMut() -> Option<T>, what: &'static str) -> T {
            tokio::time::timeout(limit, async {
                loop {
                    if let Some(value) = predicate() {
                        return value;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        }

        async fn wait_for_connection_state(
            updates: &UiBridge,
            phase: &str,
            message: Option<&str>,
            limit: Duration,
        ) -> ConnectionStateDto {
            tokio::time::timeout(limit, async {
                loop {
                    if let Some(state) = updates.latest_connection_for_test().await
                        && state.phase == phase
                        && message.is_none_or(|expected| state.message.as_deref() == Some(expected))
                    {
                        return state;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for Terminal state {phase}"))
        }

        let server_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind local fake Host");
        let server_address = server_listener.local_addr().expect("fake Host address");
        let (mut connection, _old_inbound, _old_outbound) = test_authenticated_connection();
        let session_id = connection.secrets.session_id.clone();
        let server_epoch = connection.identity.server_epoch.clone();
        let state = Arc::new(std::sync::Mutex::new(FakeHostSession {
            generation: 1,
            ticket: "test-ticket".to_owned(),
            last_contact: Some(Instant::now()),
        }));
        let pings = Arc::new(AtomicUsize::new(0));
        let resumes = Arc::new(AtomicUsize::new(0));
        let accept_state = Arc::clone(&state);
        let accept_pings = Arc::clone(&pings);
        let accept_resumes = Arc::clone(&resumes);
        let server_session_id = session_id.clone();
        let server_epoch_copy = server_epoch.clone();
        let server_task = tokio::spawn(async move {
            loop {
                let accepted = server_listener.accept().await;
                let Ok((stream, _)) = accepted else {
                    break;
                };
                tokio::spawn(serve_mock_authenticated_socket(
                    stream,
                    server_session_id.clone(),
                    server_epoch_copy.clone(),
                    Arc::clone(&accept_state),
                    Arc::clone(&accept_pings),
                    Arc::clone(&accept_resumes),
                ));
            }
        });

        let proxy = start_tcp_loss_proxy(server_address).await;
        let (initial_socket, _) = tokio::time::timeout(Duration::from_secs(5), connect_async(&proxy.url))
            .await
            .expect("initial WebSocket handshake completed")
            .expect("connect to fake Host through the TCP proxy");
        connection.socket = SessionSocket::Direct(initial_socket);
        let updates = Arc::new(UiBridge::default());
        let (_actions, action_receiver) = mpsc::channel(8);
        let cancel = CancellationToken::new();

        let resume_url = proxy.url.clone();
        let resume: Box<ResumeOperation> = Box::new(move |current, cancel| {
            let resume_url = resume_url.clone();
            let identity_template = HelloIdentity {
                server_id: current.identity.server_id.clone(),
                server_epoch: current.identity.server_epoch.clone(),
                fingerprint: current.identity.fingerprint.clone(),
                client_nonce: current.identity.client_nonce.clone(),
            };
            Box::pin(async move {
                let connect: Box<PendingConnector> = Box::new(move |profile, _resume_session_id| {
                    let url = resume_url.clone();
                    let identity = HelloIdentity {
                        server_id: identity_template.server_id.clone(),
                        server_epoch: identity_template.server_epoch.clone(),
                        fingerprint: identity_template.fingerprint.clone(),
                        client_nonce: identity_template.client_nonce.clone(),
                    };
                    Box::pin(async move {
                        let (socket, _) = timeout(Duration::from_secs(1), connect_async(url))
                            .await
                            .map_err(|_| "local loss proxy withheld the WebSocket handshake".to_owned())?
                            .map_err(|error| error.to_string())?;
                        let pending = PendingConnection {
                            profile: profile.clone(),
                            socket: SessionSocket::Direct(socket),
                            identity,
                        };
                        let host_identity = host_identity_dto(&pending);
                        Ok((pending, host_identity))
                    })
                });
                reconnect_and_resume_with(current, cancel, connect).await
            })
        });

        let session = tokio::spawn(run_session_with_resume(
            connection,
            action_receiver,
            Arc::clone(&updates),
            cancel.clone(),
            resume,
        ));
        let mut observed_pings = wait_until(
            Duration::from_secs(5),
            || {
                let count = pings.load(Ordering::SeqCst);
                (count > 0).then_some(count)
            },
            "first authenticated heartbeat",
        )
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        for outage in [5_u64, 9, 15, 25, 40] {
            observed_pings = wait_until(
                Duration::from_secs(6),
                || {
                    let count = pings.load(Ordering::SeqCst);
                    (count > observed_pings).then_some(count)
                },
                "a live heartbeat before the next packet drop",
            )
            .await;
            tokio::time::sleep(Duration::from_millis(100)).await;

            let resume_count_before = resumes.load(Ordering::SeqCst);
            let reconnect_count_before = updates
                .connection_phases_for_test()
                .await
                .iter()
                .filter(|phase| **phase == "reconnecting")
                .count();
            println!("TCP fault case start outage_seconds={outage}");
            if use_tc_netem {
                set_tc_netem_loss(true);
            } else {
                proxy.drop_frames.store(true, Ordering::SeqCst);
            }
            let outage_started = Instant::now();
            let fault_start_unix_ms = unix_epoch_millis();
            println!(
                "TCP fault active pid={} start_unix_ms={} injector={}",
                std::process::id(),
                fault_start_unix_ms,
                if use_tc_netem {
                    "tc-netem-loopback"
                } else {
                    "tcp-proxy-payload"
                }
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
            let forwarding_during_outage = proxy.forwarded_bytes.load(Ordering::SeqCst);
            let dropped_before = if use_tc_netem {
                tc_dropped_packets()
            } else {
                proxy.dropped_bytes.load(Ordering::SeqCst)
            };
            let detection = if outage >= 10 {
                let updates = Arc::clone(&updates);
                let loss_started = outage_started;
                Some(tokio::spawn(async move {
                    wait_for_connection_state(&updates, "reconnecting", None, Duration::from_secs(12)).await;
                    loss_started.elapsed()
                }))
            } else {
                None
            };
            tokio::time::sleep(Duration::from_secs(outage).saturating_sub(outage_started.elapsed())).await;
            let elapsed = outage_started.elapsed();
            assert!(elapsed >= Duration::from_secs(outage));
            let dropped_after = if use_tc_netem {
                tc_dropped_packets()
            } else {
                proxy.dropped_bytes.load(Ordering::SeqCst)
            };
            assert!(
                dropped_after > dropped_before,
                "the loss injector must prove packets/bytes were dropped during the {outage}s outage"
            );
            if !use_tc_netem {
                assert_eq!(
                    proxy.forwarded_bytes.load(Ordering::SeqCst),
                    forwarding_during_outage,
                    "the TCP proxy must forward no bytes while the {outage}s loss switch is enabled"
                );
            }

            let phases = updates.connection_phases_for_test().await;
            let reconnect_count = phases.iter().filter(|phase| **phase == "reconnecting").count();
            if outage < 10 {
                assert_eq!(
                    reconnect_count, reconnect_count_before,
                    "a {outage}s outage must not start recovery before the 10s deadline"
                );
                if use_tc_netem {
                    set_tc_netem_loss(false);
                } else {
                    proxy.drop_frames.store(false, Ordering::SeqCst);
                }
                println!("TCP fault restored outage_seconds={outage} end_unix_ms={}", unix_epoch_millis());
                observed_pings = wait_until(
                    Duration::from_secs(6),
                    || {
                        let count = pings.load(Ordering::SeqCst);
                        (count > observed_pings).then_some(count)
                    },
                    "heartbeat traffic after restoring a short outage",
                )
                .await;
                assert_eq!(
                    resumes.load(Ordering::SeqCst),
                    resume_count_before,
                    "short loss must keep the current session online"
                );
                let forwarded_after_restore = proxy.forwarded_bytes.load(Ordering::SeqCst);
                assert!(
                    forwarded_after_restore > forwarding_during_outage,
                    "proxy counters show traffic resumed after a short outage"
                );
                println!(
                    "TCP fault case recovered without session resume outage_seconds={outage} dropped_packets_or_bytes={} forwarded_after_restore_bytes={}",
                    dropped_after - dropped_before,
                    forwarded_after_restore - forwarding_during_outage
                );
            } else if outage < 40 {
                let detection_after = detection
                    .expect("loss observer started")
                    .await
                    .expect("join loss observer");
                assert!(
                    detection_after <= Duration::from_millis(10_500),
                    "Terminal loss detection exceeded its approximately 10-second bound: {detection_after:?}"
                );
                if use_tc_netem {
                    set_tc_netem_loss(false);
                } else {
                    proxy.drop_frames.store(false, Ordering::SeqCst);
                }
                println!("TCP fault restored outage_seconds={outage} end_unix_ms={}", unix_epoch_millis());
                let recovery_started = Instant::now();
                let resumed_count = wait_until(
                    Duration::from_secs(15),
                    || {
                        let count = resumes.load(Ordering::SeqCst);
                        (count > resume_count_before).then_some(count)
                    },
                    "same-session resume after restoring the TCP proxy",
                )
                .await;
                wait_for_connection_state(&updates, "online", None, Duration::from_secs(5)).await;
                let recovered_after = recovery_started.elapsed();
                let forwarded_after_restore = proxy.forwarded_bytes.load(Ordering::SeqCst);
                assert!(
                    forwarded_after_restore > forwarding_during_outage,
                    "proxy counters show traffic resumed after recovery"
                );
                assert_eq!(
                    updates.latest_snapshot_for_test().await.unwrap().state.bridge_revision,
                    42 + resumed_count as u64 + 1
                );
                println!(
                    "TCP fault case resumed outage_seconds={outage} detected_after_ms={} restored_after_ms={} dropped_packets_or_bytes={} forwarded_after_restore_bytes={}",
                    detection_after.as_millis(),
                    recovered_after.as_millis(),
                    dropped_after - dropped_before,
                    forwarded_after_restore - forwarding_during_outage
                );
                observed_pings = wait_until(
                    Duration::from_secs(6),
                    || {
                        let count = pings.load(Ordering::SeqCst);
                        (count > observed_pings).then_some(count)
                    },
                    "heartbeat on the resumed generation",
                )
                .await;
            } else {
                let detection_after = detection
                    .expect("loss observer started")
                    .await
                    .expect("join loss observer");
                assert!(
                    detection_after <= Duration::from_millis(10_500),
                    "Terminal loss detection exceeded its approximately 10-second bound: {detection_after:?}"
                );
                wait_for_connection_state(&updates, "host_unavailable", None, Duration::from_secs(2)).await;
                if use_tc_netem {
                    set_tc_netem_loss(false);
                } else {
                    proxy.drop_frames.store(false, Ordering::SeqCst);
                }
                println!("TCP fault restored outage_seconds={outage} end_unix_ms={}", unix_epoch_millis());
                assert_eq!(
                    resumes.load(Ordering::SeqCst),
                    resume_count_before,
                    "the 40s outage outlives the Terminal's bounded same-session retry window"
                );
                tokio::time::timeout(Duration::from_secs(1), async {
                    while !session.is_finished() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("Terminal stops after its bounded resume window");
                tokio::time::sleep(Duration::from_secs(1)).await;
                assert_eq!(
                    resumes.load(Ordering::SeqCst),
                    resume_count_before,
                    "restoring the route after the bounded window must not trigger automatic re-login"
                );
                println!(
                    "TCP fault case bounded_outage outage_seconds={outage} detected_after_ms={} dropped_packets_or_bytes={} forwarded_after_restore_bytes={}",
                    detection_after.as_millis(),
                    dropped_after - dropped_before,
                    proxy.forwarded_bytes.load(Ordering::SeqCst) - forwarding_during_outage
                );
            }
        }

        if use_tc_netem {
            set_tc_netem_loss(false);
        } else {
            proxy.drop_frames.store(false, Ordering::SeqCst);
        }
        cancel.cancel();
        let _ = timeout(Duration::from_secs(3), session).await;
        server_task.abort();
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn rtc_datachannel_session_honors_the_bound_host_kick_notice() {
        let (connection, inbound, mut outbound) = test_authenticated_connection();
        let expected_epoch = connection.identity.server_epoch.clone();
        let expected_session_id = connection.secrets.session_id.clone();
        let expected_generation = connection.secrets.generation;
        inbound
            .send(
                serde_json::to_vec(&json!({
                    "type": "session.ended",
                    "protocol": PROTOCOL_VERSION,
                    "server_epoch": expected_epoch,
                    "session_id": expected_session_id,
                    "transport_generation": expected_generation,
                    "reason": "host_kick",
                }))
                .expect("serialize Host Kick frame"),
            )
            .await
            .expect("queue Host Kick frame over the RTC DataChannel adapter");

        let updates = Arc::new(UiBridge::default());
        let (_actions, action_receiver) = mpsc::channel(8);
        tokio::time::timeout(
            Duration::from_secs(2),
            run_session(connection, action_receiver, Arc::clone(&updates), CancellationToken::new()),
        )
        .await
        .expect("RTC DataChannel session ended without waiting for a transport timeout");

        let mut acknowledgement = None;
        while let Some(frame) = outbound.recv().await {
            let value: Value = serde_json::from_slice(&frame).expect("RTC acknowledgement JSON");
            if value["type"] == "session.ended.ack" {
                acknowledgement = Some(value);
                break;
            }
        }
        let acknowledgement = acknowledgement.expect("Terminal acknowledged the bound Host end over RTC");
        assert_eq!(acknowledgement["server_epoch"], expected_epoch);
        assert_eq!(acknowledgement["session_id"], expected_session_id);
        assert_eq!(acknowledgement["transport_generation"], expected_generation);
        assert_eq!(updates.latest_connection_for_test().await.unwrap().phase, "host_kicked");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn explicit_session_revocation_error_deletes_the_saved_device_grant() {
        struct CredentialGrantCleanup(String);

        impl Drop for CredentialGrantCleanup {
            fn drop(&mut self) {
                let _ = crate::storage::credentials::delete(&self.0);
            }
        }

        let _credential_store_guard = CREDENTIAL_STORE_TEST_LOCK.lock().await;
        let (mut connection, inbound, _outbound) = test_authenticated_connection();
        let profile_id = Uuid::new_v4().to_string();
        let fingerprint = "ab".repeat(32);
        connection.profile.id = profile_id.clone();
        connection.profile.fingerprint = fingerprint.clone();
        let _cleanup = CredentialGrantCleanup(profile_id.clone());
        crate::storage::credentials::save_device_grant(&profile_id, &fingerprint, "dg1.test-grant")
            .expect("write an isolated test grant");
        assert!(
            crate::storage::credentials::read_device_grant(&profile_id, &fingerprint)
                .expect("read isolated test grant")
                .is_some()
        );
        inbound
            .send(
                serde_json::to_vec(&json!({
                    "type": "error",
                    "protocol": PROTOCOL_VERSION,
                    "code": "SESSION_REVOKED",
                    "message": "This Terminal was revoked by the Host",
                }))
                .expect("serialize revocation response"),
            )
            .await
            .expect("queue explicit session revocation");

        let updates = Arc::new(UiBridge::default());
        let (_actions, action_receiver) = mpsc::channel(8);
        tokio::time::timeout(
            Duration::from_secs(2),
            run_session(connection, action_receiver, Arc::clone(&updates), CancellationToken::new()),
        )
        .await
        .expect("revocation stops the active session");

        assert_eq!(updates.latest_connection_for_test().await.unwrap().phase, "session_revoked");
        assert!(
            crate::storage::credentials::read_device_grant(&profile_id, &fingerprint)
                .expect("check deleted test grant")
                .is_none()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn host_kick_preserves_the_saved_device_grant_for_manual_reconnect() {
        struct CredentialGrantCleanup(String);

        impl Drop for CredentialGrantCleanup {
            fn drop(&mut self) {
                let _ = crate::storage::credentials::delete(&self.0);
            }
        }

        let _credential_store_guard = CREDENTIAL_STORE_TEST_LOCK.lock().await;
        let (connection, inbound, _outbound) = test_authenticated_connection();
        let profile_id = Uuid::new_v4().to_string();
        let fingerprint = "cd".repeat(32);
        let mut connection = connection;
        connection.profile.id = profile_id.clone();
        connection.profile.fingerprint = fingerprint.clone();
        let _cleanup = CredentialGrantCleanup(profile_id.clone());
        crate::storage::credentials::save_device_grant(&profile_id, &fingerprint, "dg1.test-grant")
            .expect("write isolated test grant");
        inbound
            .send(
                serde_json::to_vec(&json!({
                    "type": "session.ended",
                    "protocol": PROTOCOL_VERSION,
                    "server_epoch": connection.identity.server_epoch,
                    "session_id": connection.secrets.session_id,
                    "transport_generation": connection.secrets.generation,
                    "reason": "host_kick",
                }))
                .expect("serialize Host Kick frame"),
            )
            .await
            .expect("queue Host Kick frame");

        let updates = Arc::new(UiBridge::default());
        let (_actions, action_receiver) = mpsc::channel(8);
        run_session(connection, action_receiver, updates, CancellationToken::new()).await;

        let saved_grant = crate::storage::credentials::read_device_grant(&profile_id, &fingerprint)
            .expect("read saved grant after Host Kick");
        assert!(
            saved_grant
                .as_ref()
                .is_some_and(|grant| grant.as_str() == "dg1.test-grant"),
            "Host Kick must preserve the pairing grant for a later manual Connect"
        );
    }

    fn identity_for_session_end_test(server_epoch: &str) -> HelloIdentity {
        HelloIdentity {
            server_id: "test-host".to_owned(),
            server_epoch: server_epoch.to_owned(),
            fingerprint: "test-fingerprint".to_owned(),
            client_nonce: "test-nonce".to_owned(),
        }
    }

    fn secrets_for_session_end_test(session_id: Uuid, generation: u64) -> SessionSecrets {
        SessionSecrets {
            session_id: session_id.to_string(),
            resume_ticket: Zeroizing::new(String::new()),
            access_token: Zeroizing::new(String::new()),
            access_expires_at: Instant::now(),
            generation,
            resume_request_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn remote_end_notice_requires_current_identity_session_and_generation() {
        let server_epoch = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let identity = identity_for_session_end_test(&server_epoch.to_string());
        let secrets = secrets_for_session_end_test(session_id, 4);
        let frame = json!({
            "type": "session.ended",
            "protocol": PROTOCOL_VERSION,
            "server_epoch": server_epoch,
            "session_id": session_id,
            "transport_generation": 4,
            "reason": "host_kick",
        });

        assert_eq!(
            parse_session_end(&frame, &identity, &secrets),
            Some(RemoteSessionEndReason::HostKick)
        );
        let mut session_revoked = frame.clone();
        session_revoked["reason"] = json!("session_revoked");
        assert_eq!(
            parse_session_end(&session_revoked, &identity, &secrets),
            Some(RemoteSessionEndReason::SessionRevoked)
        );
        let mut host_unavailable = frame.clone();
        host_unavailable["reason"] = json!("host_unavailable");
        assert_eq!(
            parse_session_end(&host_unavailable, &identity, &secrets),
            Some(RemoteSessionEndReason::HostUnavailable)
        );
        let mut stale_generation = frame.clone();
        stale_generation["transport_generation"] = json!(3);
        assert_eq!(parse_session_end(&stale_generation, &identity, &secrets), None);
        let mut wrong_epoch = frame.clone();
        wrong_epoch["server_epoch"] = json!(Uuid::new_v4());
        assert_eq!(parse_session_end(&wrong_epoch, &identity, &secrets), None);

        let unbound_legacy_kick = json!({
            "type": "session.revoked",
            "protocol": PROTOCOL_VERSION,
            "server_epoch": server_epoch,
            "reason": "host_kick",
        });
        assert_eq!(parse_session_end(&unbound_legacy_kick, &identity, &secrets), None);
        let mut bound_legacy_kick = frame;
        bound_legacy_kick["type"] = json!("session.revoked");
        assert_eq!(
            parse_session_end(&bound_legacy_kick, &identity, &secrets),
            Some(RemoteSessionEndReason::HostKick)
        );
        let ambiguous_legacy_revoke = json!({
            "type": "session.revoked",
            "protocol": PROTOCOL_VERSION,
            "server_epoch": server_epoch,
            "reason": "session_revoked",
        });
        assert_eq!(parse_session_end(&ambiguous_legacy_revoke, &identity, &secrets), None);
    }

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

    #[tokio::test(start_paused = true)]
    async fn manual_reconnect_retries_room_busy_until_the_previous_room_closes() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempt_counter = Arc::clone(&attempts);
        let task = tokio::spawn(retry_room_busy(move || {
            let attempt = attempt_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            async move {
                if attempt < 3 {
                    Err("Rendezvous rejected the room: room_busy".to_owned())
                } else {
                    Ok("joined")
                }
            }
        }));

        tokio::task::yield_now().await;
        tokio::time::advance(ROOM_BUSY_RETRY_DELAY).await;
        tokio::task::yield_now().await;
        tokio::time::advance(ROOM_BUSY_RETRY_DELAY).await;
        assert_eq!(task.await.expect("manual connect completed").unwrap(), "joined");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn manual_reconnect_does_not_retry_permanent_join_errors() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempt_counter = Arc::clone(&attempts);
        let result = retry_room_busy(move || {
            attempt_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Err::<(), _>("Host identity pin mismatch".to_owned()) }
        })
        .await;

        assert_eq!(result.unwrap_err(), "Host identity pin mismatch");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn manual_reconnect_stops_after_the_room_busy_retry_limit() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempt_counter = Arc::clone(&attempts);
        let task = tokio::spawn(retry_room_busy(move || {
            attempt_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Err::<(), _>("Rendezvous rejected the room: room_busy".to_owned()) }
        }));

        tokio::task::yield_now().await;
        for _ in 0..MAX_ROOM_BUSY_RETRIES {
            tokio::time::advance(ROOM_BUSY_RETRY_DELAY).await;
            tokio::task::yield_now().await;
        }
        assert!(
            task.await
                .expect("bounded manual connect completed")
                .unwrap_err()
                .contains("room_busy")
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), MAX_ROOM_BUSY_RETRIES + 1);
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

    #[test]
    fn relay_only_profiles_never_attempt_direct_wss() {
        assert!(!direct_attempt_allowed("auto", true, true, true));
        assert!(!direct_attempt_allowed("direct_only", true, true, true));
        assert!(direct_attempt_allowed("auto", false, true, true));
        assert!(!direct_attempt_allowed("traversal_only", false, true, true));
    }
}

use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        ConnectInfo, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{
        StatusCode,
        header::{AUTHORIZATION, CACHE_CONTROL, HeaderMap, HeaderValue},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shocking_vrc_rtc::{RtcApiOutbound, monotonic_ms};
use subtle::ConstantTimeEq;
use tokio::{
    sync::Semaphore,
    time::{interval_at, timeout},
};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    BackendError, HostIdentity, RemoteBackend,
    auth::{AuthError, LoginResponse, SessionEndReason, SessionManager, SessionPhase},
    operations::REMOTE_OPERATIONS,
    protocol::{
        ApiErrorDto, CapabilitiesDto, CommandAcceptedDto, CommandRequest, HealthDto, MAX_FRAME_BYTES, PROTOCOL_VERSION,
        ProtocolError, RemoteCommand, SnapshotDto, parse_rest_command, parse_unique_json,
    },
    settings::{RemoteAccessSettings, RemoteChannel, RemoteLimitOperation, StrengthPolicyError},
};

const SESSION_END_ACK_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_STATE_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(2);
const MIN_STATE_RECONCILIATION_INTERVAL: Duration = Duration::from_millis(500);
const MAX_STATE_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatePublication {
    FullSnapshot,
    Telemetry,
    Suppressed,
}

#[derive(Debug, Clone, PartialEq)]
struct SnapshotProjection {
    authoritative: Value,
    telemetry: Value,
}

/// Build semantic projections without using the monotonically increasing bridge revision.
/// Device connectivity remains authoritative state; only replaceable live status and contact
/// samples are telemetry.
fn snapshot_projection(snapshot: &SnapshotDto) -> Result<SnapshotProjection, ()> {
    let mut value = serde_json::to_value(snapshot).map_err(|_| ())?;
    let object = value.as_object_mut().ok_or(())?;
    object.remove("bridge_revision");
    let avatar_zones = object.remove("avatar_zones").ok_or(())?;
    let status = object.get_mut("status").and_then(Value::as_object_mut).ok_or(())?;
    let device_connected = status.get("device_connected").cloned().ok_or(())?;
    let channel_a = status.remove("channel_a").ok_or(())?;
    let channel_b = status.remove("channel_b").ok_or(())?;
    let telemetry = json!({
        "avatar_zones": avatar_zones,
        "status": { "channel_a": channel_a, "channel_b": channel_b }
    });
    status.insert("device_connected".into(), device_connected);
    Ok(SnapshotProjection {
        authoritative: value,
        telemetry,
    })
}

struct SnapshotPublisher {
    session: Option<(Uuid, u64)>,
    previous: Option<SnapshotProjection>,
    last_full_at: Option<Instant>,
    full_snapshots: u64,
    telemetry_updates: u64,
    suppressed_duplicates: u64,
    full_payload_bytes: u64,
    telemetry_payload_bytes: u64,
    started_at: Instant,
    host_age_samples: u64,
    host_age_total_ms: u64,
    host_age_max_ms: u64,
}

impl Default for SnapshotPublisher {
    fn default() -> Self {
        Self {
            session: None,
            previous: None,
            last_full_at: None,
            full_snapshots: 0,
            telemetry_updates: 0,
            suppressed_duplicates: 0,
            full_payload_bytes: 0,
            telemetry_payload_bytes: 0,
            started_at: Instant::now(),
            host_age_samples: 0,
            host_age_total_ms: 0,
            host_age_max_ms: 0,
        }
    }
}

impl SnapshotPublisher {
    fn seed(&mut self, session: WsSession, snapshot: &SnapshotDto, now: Instant) -> Result<(), ()> {
        self.session = Some((session.id, session.generation));
        self.previous = Some(snapshot_projection(snapshot)?);
        self.last_full_at = Some(now);
        Ok(())
    }

    fn observe(
        &mut self,
        session: WsSession,
        snapshot: &SnapshotDto,
        telemetry_negotiated: bool,
        now: Instant,
        reconciliation: Duration,
    ) -> Result<StatePublication, ()> {
        let projection = snapshot_projection(snapshot)?;
        let changed_session = self.session != Some((session.id, session.generation));
        let Some(previous) = self.previous.as_ref() else {
            self.session = Some((session.id, session.generation));
            self.previous = Some(projection);
            self.last_full_at = Some(now);
            return Ok(StatePublication::FullSnapshot);
        };
        if changed_session {
            self.session = Some((session.id, session.generation));
            self.previous = Some(projection);
            self.last_full_at = Some(now);
            return Ok(StatePublication::FullSnapshot);
        }
        let authoritative_changed = previous.authoritative != projection.authoritative;
        let telemetry_changed = previous.telemetry != projection.telemetry;
        self.previous = Some(projection);
        if authoritative_changed || (telemetry_changed && !telemetry_negotiated) {
            self.last_full_at = Some(now);
            return Ok(StatePublication::FullSnapshot);
        }
        if telemetry_changed {
            return Ok(StatePublication::Telemetry);
        }
        if self
            .last_full_at
            .is_none_or(|last| now.duration_since(last) >= reconciliation)
        {
            self.last_full_at = Some(now);
            return Ok(StatePublication::FullSnapshot);
        }
        self.suppressed_duplicates = self.suppressed_duplicates.saturating_add(1);
        Ok(StatePublication::Suppressed)
    }

    fn record_sent(&mut self, publication: StatePublication, bytes: usize, host_snapshot_age_ms: Option<u64>) {
        match publication {
            StatePublication::FullSnapshot => {
                self.full_snapshots = self.full_snapshots.saturating_add(1);
                self.full_payload_bytes = self.full_payload_bytes.saturating_add(bytes as u64);
            }
            StatePublication::Telemetry => {
                self.telemetry_updates = self.telemetry_updates.saturating_add(1);
                self.telemetry_payload_bytes = self.telemetry_payload_bytes.saturating_add(bytes as u64);
            }
            StatePublication::Suppressed => {}
        }
        if let Some(age_ms) = host_snapshot_age_ms {
            self.host_age_samples = self.host_age_samples.saturating_add(1);
            self.host_age_total_ms = self.host_age_total_ms.saturating_add(age_ms);
            self.host_age_max_ms = self.host_age_max_ms.max(age_ms);
        }
    }

    fn log_summary(&self, transport: &str, sender: &ApiSender) {
        let elapsed_secs = self.started_at.elapsed().as_secs_f64().max(0.001);
        let host_age_average =
            (self.host_age_samples > 0).then(|| self.host_age_total_ms as f64 / self.host_age_samples as f64);
        let queue_metrics = sender.outbound_queue_metrics();
        log::info!(
            "[remote] state delivery metrics monotonic_ms={} transport={} observation_secs={:.1} full_snapshots={} full_per_sec={:.3} telemetry_updates={} telemetry_per_sec={:.3} duplicate_samples_suppressed={} suppressed_per_sec={:.3} full_payload_bytes={} full_bytes_per_sec={:.1} telemetry_payload_bytes={} telemetry_bytes_per_sec={:.1} host_snapshot_age_avg_ms={} host_snapshot_age_max_ms={} control_queue_depth={} replaceable_queue_depth={} coalesced_state_frames={} datachannel_congested={}",
            monotonic_ms(),
            transport,
            elapsed_secs,
            self.full_snapshots,
            self.full_snapshots as f64 / elapsed_secs,
            self.telemetry_updates,
            self.telemetry_updates as f64 / elapsed_secs,
            self.suppressed_duplicates,
            self.suppressed_duplicates as f64 / elapsed_secs,
            self.full_payload_bytes,
            self.full_payload_bytes as f64 / elapsed_secs,
            self.telemetry_payload_bytes,
            self.telemetry_payload_bytes as f64 / elapsed_secs,
            host_age_average.map_or_else(|| "unavailable".to_owned(), |value| format!("{value:.1}")),
            self.host_age_max_ms,
            queue_metrics.map_or(0, |metrics| metrics.0),
            queue_metrics.map_or(0, |metrics| metrics.1),
            queue_metrics.map_or(0, |metrics| metrics.2),
            queue_metrics.map_or(false, |metrics| metrics.3),
        );
    }
}

fn state_reconciliation_interval() -> Duration {
    let configured = std::env::var("SHOCKINGVRC_STATE_RECONCILIATION_MS").ok();
    bounded_state_reconciliation_interval(configured.as_deref())
}

fn bounded_state_reconciliation_interval(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .map(|value| value.clamp(MIN_STATE_RECONCILIATION_INTERVAL, MAX_STATE_RECONCILIATION_INTERVAL))
        .unwrap_or(DEFAULT_STATE_RECONCILIATION_INTERVAL)
}

#[derive(Clone)]
pub struct ApiState {
    backend: Arc<dyn RemoteBackend>,
    sessions: Arc<SessionManager>,
    identity: Arc<HostIdentity>,
    settings: Arc<RwLock<RemoteAccessSettings>>,
    server_epoch: Uuid,
    login_throttle: Arc<Mutex<LoginThrottle>>,
    mutation_rate: Arc<Mutex<HashMap<Uuid, VecDeque<Instant>>>>,
    command_dedup: Arc<Mutex<HashMap<(Uuid, Uuid), ([u8; 32], Instant)>>>,
    unauthenticated_ws: Arc<Semaphore>,
}

impl ApiState {
    pub(crate) fn new(
        backend: Arc<dyn RemoteBackend>,
        sessions: Arc<SessionManager>,
        identity: Arc<HostIdentity>,
        settings: Arc<RwLock<RemoteAccessSettings>>,
        server_epoch: Uuid,
    ) -> Self {
        sessions.start_maintenance();
        Self {
            backend,
            sessions,
            identity,
            settings,
            server_epoch,
            login_throttle: Arc::new(Mutex::new(LoginThrottle {
                attempts: HashMap::new(),
            })),
            mutation_rate: Arc::new(Mutex::new(HashMap::new())),
            command_dedup: Arc::new(Mutex::new(HashMap::new())),
            unauthenticated_ws: Arc::new(Semaphore::new(8)),
        }
    }
}

pub(crate) fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/auth/refresh", post(rest_refresh))
        .route("/api/v1/auth/logout", post(rest_logout))
        .route("/api/v1/session/heartbeat", post(rest_heartbeat))
        .route("/api/v1/snapshot", get(rest_snapshot))
        .route("/api/v1/capabilities", get(rest_capabilities))
        .route("/api/v1/catalogs/presets", get(rest_presets))
        .route("/api/v1/catalogs/zone-presets", get(rest_zone_presets))
        .route("/api/v1/logs", get(rest_logs))
        .route("/api/v1/commands", post(rest_command))
        .route("/api/v1/ws", get(ws_upgrade))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_FRAME_BYTES))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

/// Test-only production API fixture used by isolated multi-process acceptance tests.
#[cfg(feature = "test-support")]
pub mod test_support {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use super::{ApiState, Router, router};
    use crate::{
        BackendError, HostIdentity, RemoteBackend,
        auth::SessionManager,
        protocol::{LogPageDto, PresetCatalogDto, SnapshotDto, ZonePresetCatalogDto},
        settings::RemoteAccessSettings,
    };
    use uuid::Uuid;

    pub fn production_api_router(
        backend: Arc<dyn RemoteBackend>,
        sessions: Arc<SessionManager>,
        identity: Arc<HostIdentity>,
        server_epoch: Uuid,
    ) -> Router {
        router(ApiState::new(
            backend,
            sessions,
            identity,
            Arc::new(std::sync::RwLock::new(RemoteAccessSettings::default())),
            server_epoch,
        ))
    }

    /// Production API state for isolated process tests that serve the API over a
    /// real WebRTC DataChannel instead of an HTTP listener.
    pub fn production_api_state(
        backend: Arc<dyn RemoteBackend>,
        sessions: Arc<SessionManager>,
        identity: Arc<HostIdentity>,
        server_epoch: Uuid,
    ) -> ApiState {
        ApiState::new(
            backend,
            sessions,
            identity,
            Arc::new(std::sync::RwLock::new(RemoteAccessSettings {
                remote_enabled: true,
                ..RemoteAccessSettings::default()
            })),
            server_epoch,
        )
    }

    /// A headless backend that emits an incrementing authoritative snapshot so
    /// process-level reconnect tests can distinguish fresh state from stale state.
    #[derive(Default)]
    pub struct SnapshotBackend {
        revision: AtomicU64,
    }

    impl RemoteBackend for SnapshotBackend {
        fn snapshot(&self) -> Result<SnapshotDto, BackendError> {
            let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(snapshot(revision))
        }

        fn presets(&self) -> Result<PresetCatalogDto, BackendError> {
            Ok(PresetCatalogDto { entries: Vec::new() })
        }

        fn zone_presets(&self) -> Result<ZonePresetCatalogDto, BackendError> {
            Ok(ZonePresetCatalogDto { entries: Vec::new() })
        }

        fn logs(&self, _cursor: Option<u64>, _limit: u16) -> Result<LogPageDto, BackendError> {
            Ok(LogPageDto {
                cursor: "0".into(),
                next_cursor: None,
                lines: Vec::new(),
            })
        }

        fn enqueue(&self, _command: crate::RemoteCommand) -> Result<(), BackendError> {
            Ok(())
        }
    }

    fn snapshot(bridge_revision: u64) -> SnapshotDto {
        use crate::protocol::*;
        let channel_config = || ChannelConfigDto {
            zones: Vec::new(),
            frequency: [0; 4],
            intensity: [0; 4],
            limits: LimitsDto { min: 0, max: 200 },
            aggregation: "mean".into(),
            freq_modulation: [None, None, None, None],
            intensity_modulation: [None, None, None, None],
        };
        let channel_status = || ChannelStatusDto {
            raw_level: 0.0,
            strength: 0,
            frequency: [0; 4],
            active_zones: Vec::new(),
            kinematics: KinematicsDto {
                depth: 0.0,
                speed: 0.0,
                acc: 0.0,
                recoil: 0.0,
            },
        };
        SnapshotDto {
            bridge_revision,
            configuration_state: None,
            host_display_name: Some("Isolated test Host".into()),
            active_device: 0,
            active_device_mac: None,
            devices: Vec::new(),
            vrchat: VrchatDto {
                connected: false,
                osc_port: 0,
                oscquery_port: None,
            },
            avatar_zones: Vec::new(),
            config: ConfigDto {
                channel_a: channel_config(),
                channel_b: channel_config(),
                ukf: UkfDto {
                    q: 0.0,
                    r: 0.0,
                    alpha: 0.0,
                    beta: 0.0,
                    kappa: 0.0,
                },
                norms: MotionNormsDto {
                    speed: 0.0,
                    acc: 0.0,
                    recoil: 0.0,
                },
            },
            status: StatusDto {
                device_connected: false,
                channel_a: channel_status(),
                channel_b: channel_status(),
            },
            alarm: AlarmDto {
                config: AlarmConfigDto {
                    enabled: false,
                    hour: 0,
                    minute: 0,
                    channels: "A".into(),
                    start_strength: 0,
                    peak_strength: 0,
                    ramp_secs: 0,
                    max_duration_secs: 0,
                    repeats: 0,
                    pulse_on_ms: 0,
                    pulse_off_ms: 0,
                    snooze_mins: 0,
                },
                status: AlarmStatusDto {
                    phase: "idle".into(),
                    strength: 0,
                    elapsed_secs: 0,
                    auto_stop_in_secs: 0,
                    snooze_left_secs: 0,
                    next_fire_in_mins: 0,
                    attempt: 0,
                    retrying: false,
                    test: false,
                },
            },
            prefs: None,
            remote_policy: RemotePolicyDto {
                remote_strength: RemoteStrengthDto {
                    enabled: false,
                    max_by_channel: std::collections::BTreeMap::from([("A".into(), None), ("B".into(), None)]),
                    scope: "power_limits_only",
                },
                max_remote_sessions: 1,
            },
        }
    }
}

async fn no_store(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn health() -> Json<HealthDto> {
    Json(HealthDto {
        protocol_min: PROTOCOL_VERSION,
        protocol_max: PROTOCOL_VERSION,
    })
}

async fn rest_refresh(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(token) = bearer(&headers) else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED", "Authentication required", None);
    };
    match state.sessions.refresh(&token) {
        Ok(refresh) => json_response(StatusCode::OK, refresh),
        Err(error) => auth_error(error, None),
    }
}

async fn rest_logout(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(token) = bearer(&headers) else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED", "Authentication required", None);
    };
    match state.sessions.logout(&token) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => auth_error(error, None),
    }
}

async fn rest_heartbeat(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(token) = bearer(&headers) else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED", "Authentication required", None);
    };
    match state.sessions.heartbeat(&token) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => auth_error(error, None),
    }
}

async fn rest_snapshot(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(token) = bearer(&headers) else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED", "Authentication required", None);
    };
    match state.sessions.admit_bearer(&token, || Ok(snapshot(&state))) {
        Ok(Ok(snapshot)) => snapshot_response(snapshot),
        Ok(Err(error)) => backend_error(error),
        Err(error) => auth_error(error, None),
    }
}

async fn rest_capabilities(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(token) = bearer(&headers) else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED", "Authentication required", None);
    };
    let result = state.sessions.admit_bearer(&token, || {
        let prefs_available = state
            .backend
            .snapshot()
            .map(|snapshot| snapshot.prefs.is_some())
            .unwrap_or(false);
        Ok(capabilities(prefs_available))
    });
    match result {
        Ok(dto) => json_response(StatusCode::OK, dto),
        Err(error) => auth_error(error, None),
    }
}

async fn rest_presets(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(token) = bearer(&headers) else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED", "Authentication required", None);
    };
    match state.sessions.admit_bearer(&token, || Ok(state.backend.presets())) {
        Ok(Ok(dto)) => json_response(StatusCode::OK, dto),
        Ok(Err(error)) => backend_error(error),
        Err(error) => auth_error(error, None),
    }
}

async fn rest_zone_presets(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(token) = bearer(&headers) else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED", "Authentication required", None);
    };
    match state.sessions.admit_bearer(&token, || Ok(state.backend.zone_presets())) {
        Ok(Ok(dto)) => json_response(StatusCode::OK, dto),
        Ok(Err(error)) => backend_error(error),
        Err(error) => auth_error(error, None),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogsQuery {
    cursor: Option<String>,
    limit: Option<u16>,
}

async fn rest_logs(State(state): State<ApiState>, headers: HeaderMap, Query(query): Query<LogsQuery>) -> Response {
    let Some(token) = bearer(&headers) else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED", "Authentication required", None);
    };
    let cursor = match query.cursor.as_deref().map(str::parse::<u64>).transpose() {
        Ok(cursor) => cursor,
        Err(_) => return api_error(StatusCode::BAD_REQUEST, "INVALID_ARGUMENT", "Invalid log cursor", None),
    };
    let limit = query.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "Log limit must be between 1 and 200",
            None,
        );
    }
    match state
        .sessions
        .admit_bearer(&token, || Ok(state.backend.logs(cursor, limit)))
    {
        Ok(Ok(dto)) => json_response(StatusCode::OK, dto),
        Ok(Err(error)) => backend_error(error),
        Err(error) => auth_error(error, None),
    }
}

async fn rest_command(State(state): State<ApiState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(token) = bearer(&headers) else {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED", "Authentication required", None);
    };
    let command = match parse_rest_command(&body) {
        Ok(command) => command,
        Err(error) => return protocol_error(error, None),
    };
    let request_id = command.request_id;
    match state
        .sessions
        .admit_bearer_with_session(&token, |session_id| Ok(dispatch_command(&state, &session_id, command)))
    {
        Ok(Ok(accepted)) => json_response(StatusCode::ACCEPTED, accepted),
        Ok(Err(error)) => api_error_with_id(error.status, &error.code, &error.message, Some(request_id)),
        Err(error) => auth_error(error, Some(request_id)),
    }
}

#[derive(Debug)]
struct DispatchError {
    status: StatusCode,
    code: String,
    message: String,
}

fn dispatch_command(
    state: &ApiState,
    session_id: &Uuid,
    command: RemoteCommand,
) -> Result<CommandAcceptedDto, DispatchError> {
    let now = Instant::now();
    let request_id = command.request_id;
    let payload_hash: [u8; 32] = {
        let bytes = serde_json::to_vec(&(&command.op, &command.args)).map_err(|_| DispatchError {
            status: StatusCode::BAD_REQUEST,
            code: "INVALID_ARGUMENT".into(),
            message: "Invalid command payload".into(),
        })?;
        Sha256::digest(bytes).into()
    };

    let mut dedup = state.command_dedup.lock().map_err(|_| DispatchError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "SERVER_BUSY".into(),
        message: "Remote dispatch is unavailable".into(),
    })?;
    dedup.retain(|_, (_, expires)| *expires > now);
    let key = (*session_id, request_id);
    if let Some((previous_hash, _)) = dedup.get(&key) {
        if bool::from(previous_hash.ct_eq(&payload_hash)) {
            return Ok(CommandAcceptedDto {
                request_id,
                queued: true,
            });
        }
        return Err(DispatchError {
            status: StatusCode::CONFLICT,
            code: "DUPLICATE_REQUEST_CONFLICT".into(),
            message: "request_id was already used for a different command".into(),
        });
    }

    let mut rate = state.mutation_rate.lock().map_err(|_| DispatchError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "SERVER_BUSY".into(),
        message: "Remote dispatch is unavailable".into(),
    })?;
    rate.retain(|_, attempts| {
        while attempts
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(1))
        {
            attempts.pop_front();
        }
        !attempts.is_empty()
    });
    let attempts = rate.entry(*session_id).or_default();
    if attempts.len() >= 30 {
        return Err(DispatchError {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "RATE_LIMITED".into(),
            message: "Command rate limit exceeded".into(),
        });
    }
    attempts.push_back(now);
    drop(rate);

    enforce_remote_strength(state, &command)?;
    state.backend.enqueue(command).map_err(|error| DispatchError {
        status: if error.code == "INVALID_ARGUMENT" {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        code: error.code.into(),
        message: error.message,
    })?;
    if dedup.len() >= 4096 {
        if let Some(oldest) = dedup
            .iter()
            .min_by_key(|(_, (_, expires))| *expires)
            .map(|(key, _)| *key)
        {
            dedup.remove(&oldest);
        }
    }
    dedup.insert(key, (payload_hash, now + Duration::from_secs(120)));
    Ok(CommandAcceptedDto {
        request_id,
        queued: true,
    })
}

fn enforce_remote_strength(state: &ApiState, command: &RemoteCommand) -> Result<(), DispatchError> {
    if command.op != "channels.set_limit_min" && command.op != "channels.set_limit_max" {
        return Ok(());
    }
    let policy = state
        .settings
        .read()
        .map_err(|_| DispatchError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "POLICY_UNAVAILABLE".into(),
            message: "Remote policy is unavailable".into(),
        })?
        .remote_strength
        .clone();
    let channel = match command.args.get("channel").and_then(Value::as_str) {
        Some("A") => RemoteChannel::A,
        Some("B") => RemoteChannel::B,
        _ => {
            return Err(DispatchError {
                status: StatusCode::BAD_REQUEST,
                code: "INVALID_ARGUMENT".into(),
                message: "Invalid channel".into(),
            });
        }
    };
    let value = command
        .args
        .get("value")
        .and_then(Value::as_u64)
        .filter(|value| *value <= 200)
        .map(|value| value as u8)
        .ok_or_else(|| DispatchError {
            status: StatusCode::BAD_REQUEST,
            code: "INVALID_ARGUMENT".into(),
            message: "Power limit must be between 0 and 200".into(),
        })?;
    let operation = if command.op == "channels.set_limit_min" {
        RemoteLimitOperation::Min
    } else {
        RemoteLimitOperation::Max
    };
    policy.validate_limit_write(operation, channel, value).map_err(|error| {
        let message = match &error {
            StrengthPolicyError::LimitExceeded {
                channel,
                requested,
                cap,
            } => format!("Channel {channel:?} requested {requested}, above the Host remote cap {cap}"),
            StrengthPolicyError::MissingCap { channel } => {
                format!("Remote cap for channel {channel:?} is not configured")
            }
            StrengthPolicyError::InvalidValue { value } => format!("Power limit {value} is outside 0..=200"),
        };
        let code = if matches!(error, StrengthPolicyError::LimitExceeded { .. }) {
            "REMOTE_LIMIT_EXCEEDED"
        } else {
            "INVALID_ARGUMENT"
        };
        DispatchError {
            status: StatusCode::CONFLICT,
            code: code.into(),
            message,
        }
    })
}

fn snapshot(state: &ApiState) -> Result<SnapshotDto, BackendError> {
    let mut snapshot = state.backend.snapshot()?;
    if let Ok(settings) = state.settings.read() {
        snapshot.host_display_name =
            (!settings.host_display_name.trim().is_empty()).then(|| settings.host_display_name.trim().to_owned());
        snapshot.remote_policy.remote_strength.enabled = settings.remote_strength.enabled;
        snapshot
            .remote_policy
            .remote_strength
            .max_by_channel
            .insert("A".into(), settings.remote_strength.max_by_channel.a);
        snapshot
            .remote_policy
            .remote_strength
            .max_by_channel
            .insert("B".into(), settings.remote_strength.max_by_channel.b);
    }
    Ok(snapshot)
}

fn capabilities(prefs_available: bool) -> CapabilitiesDto {
    let mut features = vec![
        "direct.https".to_owned(),
        "direct.wss".to_owned(),
        "session.single_operator".to_owned(),
        "rest_commands".to_owned(),
        "remote_strength_cap".to_owned(),
        "session.resume".to_owned(),
        "logs.export.chunked.v1".to_owned(),
    ];
    if prefs_available {
        features.push("snapshot.prefs".into());
    }
    CapabilitiesDto {
        operations: REMOTE_OPERATIONS.to_vec(),
        features,
    }
}

fn login_response(login: LoginResponse, request_id: Option<Uuid>) -> Value {
    let mut value = serde_json::to_value(login).unwrap_or(Value::Null);
    if let Value::Object(object) = &mut value {
        object.insert("type".into(), Value::String("auth.result".into()));
        object.insert("protocol".into(), Value::from(PROTOCOL_VERSION));
        object.insert("ok".into(), Value::Bool(true));
        object.insert("session_state".into(), Value::String("ACTIVE".into()));
        if let Some(request_id) = request_id {
            object.insert("request_id".into(), Value::String(request_id.to_string()));
        }
    }
    value
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then(|| token.to_owned())
}

fn protocol_error(error: ProtocolError, request_id: Option<Uuid>) -> Response {
    let status = if matches!(&error, ProtocolError::FrameTooLarge) {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::BAD_REQUEST
    };
    let code = protocol_error_code(&error);
    api_error_with_id(status, code, &error.to_string(), request_id)
}

fn protocol_error_code(error: &ProtocolError) -> &'static str {
    match error {
        ProtocolError::FrameTooLarge => "FRAME_TOO_LARGE",
        ProtocolError::Version => "UNSUPPORTED_PROTOCOL",
        ProtocolError::UnknownOperation => "UNSUPPORTED_OPERATION",
        ProtocolError::RequestId => "INVALID_REQUEST_ID",
        _ => "INVALID_ARGUMENT",
    }
}

fn snapshot_response(snapshot: SnapshotDto) -> Response {
    if serde_json::to_vec(&snapshot)
        .map(|encoded| encoded.len() > MAX_FRAME_BYTES)
        .unwrap_or(true)
    {
        return api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "SNAPSHOT_TOO_LARGE",
            "Snapshot exceeds the protocol frame limit",
            None,
        );
    }
    json_response(StatusCode::OK, snapshot)
}

fn auth_error(error: AuthError, request_id: Option<Uuid>) -> Response {
    match error {
        AuthError::Busy => {
            api_error_with_id(StatusCode::CONFLICT, "SESSION_BUSY", "Remote session is busy", request_id)
        }
        AuthError::Failed | AuthError::InvalidInvitation | AuthError::InvalidChallenge | AuthError::DeviceNotPaired => {
            api_error_with_id(StatusCode::UNAUTHORIZED, "AUTH_FAILED", "Authentication failed", request_id)
        }
        AuthError::InvalidResume => api_error_with_id(
            StatusCode::UNAUTHORIZED,
            "RESUME_TICKET_INVALID",
            "Resume ticket is invalid",
            request_id,
        ),
        AuthError::Expired | AuthError::Unavailable => {
            api_error_with_id(StatusCode::UNAUTHORIZED, "AUTH_REQUIRED", "Authentication required", request_id)
        }
        _ => api_error_with_id(
            StatusCode::SERVICE_UNAVAILABLE,
            "AUTH_UNAVAILABLE",
            "Authentication is temporarily unavailable",
            request_id,
        ),
    }
}

fn backend_error(error: BackendError) -> Response {
    api_error(StatusCode::SERVICE_UNAVAILABLE, error.code, &error.message, None)
}

fn api_error(status: StatusCode, code: &str, message: &str, request_id: Option<Uuid>) -> Response {
    api_error_with_id(status, code, message, request_id)
}

fn api_error_with_id(status: StatusCode, code: &str, message: &str, request_id: Option<Uuid>) -> Response {
    json_response(
        status,
        ApiErrorDto {
            code: code.to_owned(),
            message: message.to_owned(),
            request_id,
        },
    )
}

fn json_response(status: StatusCode, value: impl Serialize) -> Response {
    if serde_json::to_vec(&value)
        .map(|encoded| encoded.len() > MAX_FRAME_BYTES)
        .unwrap_or(true)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorDto {
                code: "RESPONSE_TOO_LARGE".into(),
                message: "Response exceeds the protocol frame limit".into(),
                request_id: None,
            }),
        )
            .into_response();
    }
    (status, Json(value)).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WsFrame {
    #[serde(rename = "type")]
    message_type: String,
    #[serde(default)]
    protocol: Option<u8>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    client_nonce: Option<String>,
    #[serde(default)]
    client: Option<String>,
    #[serde(default)]
    client_version: Option<String>,
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    public_key: Option<String>,
    #[serde(default)]
    challenge_id: Option<Uuid>,
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    invitation_id: Option<Uuid>,
    #[serde(default)]
    pairing_secret: Option<String>,
    #[serde(default)]
    session_id: Option<Uuid>,
    #[serde(default)]
    resume_ticket: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u16>,
    #[serde(default)]
    catalog: Option<String>,
    #[serde(default)]
    snapshot_id: Option<Uuid>,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    max_bytes: Option<u16>,
    #[serde(default)]
    start_utc: Option<String>,
    #[serde(default)]
    end_utc: Option<String>,
}

async fn ws_upgrade(
    State(state): State<ApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> Response {
    let permit = match Arc::clone(&state.unauthenticated_ws).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVER_BUSY",
                "Remote WebSocket ingress is busy",
                None,
            );
        }
    };
    ws.max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| websocket_session(state, socket, peer, permit))
}

#[derive(Clone, Copy)]
struct WsSession {
    id: Uuid,
    generation: u64,
}

fn session_ended_frame(server_epoch: Uuid, active: WsSession, reason: SessionEndReason) -> Value {
    json!({
        "type": "session.ended",
        "protocol": PROTOCOL_VERSION,
        "server_epoch": server_epoch,
        "session_id": active.id,
        "transport_generation": active.generation,
        "reason": reason.as_str(),
    })
}

fn session_end_ack_matches(value: &Value, server_epoch: Uuid, active: WsSession) -> bool {
    value_has_only_fields(
        value,
        &["type", "protocol", "server_epoch", "session_id", "transport_generation"],
    ) && value.get("type").and_then(Value::as_str) == Some("session.ended.ack")
        && value.get("protocol").and_then(Value::as_u64) == Some(u64::from(PROTOCOL_VERSION))
        && value
            .get("server_epoch")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            == Some(server_epoch)
        && value
            .get("session_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            == Some(active.id)
        && value.get("transport_generation").and_then(Value::as_u64) == Some(active.generation)
}

async fn wait_for_websocket_session_end_ack<S, E>(receiver: &mut S, server_epoch: Uuid, active: WsSession) -> bool
where
    S: futures_util::Stream<Item = Result<Message, E>> + Unpin,
{
    timeout(SESSION_END_ACK_TIMEOUT, async {
        while let Some(frame) = receiver.next().await {
            match frame {
                Ok(Message::Text(text)) => {
                    if parse_unique_json(text.as_bytes())
                        .is_ok_and(|value| session_end_ack_matches(&value, server_epoch, active))
                    {
                        return true;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => return false,
                _ => {}
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

async fn wait_for_data_channel_session_end_ack(
    receiver: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
    server_epoch: Uuid,
    active: WsSession,
) -> bool {
    timeout(SESSION_END_ACK_TIMEOUT, async {
        while let Some(frame) = receiver.recv().await {
            if parse_unique_json(&frame).is_ok_and(|value| session_end_ack_matches(&value, server_epoch, active)) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

struct DataChannelSessionCleanup<'a> {
    sessions: &'a SessionManager,
    backend: &'a dyn RemoteBackend,
    room_id: String,
    active: Option<WsSession>,
}

impl Drop for DataChannelSessionCleanup<'_> {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            self.backend.end_log_export_session(active.id, active.generation);
            self.sessions.mark_generation_lost(active.id, active.generation);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ApiTrafficTally {
    messages: u64,
    payload_bytes: u64,
}

#[derive(Clone, Copy)]
enum ApiTrafficDirection {
    Tx,
    Rx,
}

const API_TRAFFIC_CATEGORIES: [&str; 6] = ["heartbeat", "state", "command", "authentication", "session", "other"];

struct ApiTrafficRecorder {
    started_at: Instant,
    counters: Mutex<[[ApiTrafficTally; 2]; 6]>,
}

impl Default for ApiTrafficRecorder {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            counters: Mutex::new([[ApiTrafficTally::default(); 2]; 6]),
        }
    }
}

impl ApiTrafficRecorder {
    fn record(&self, direction: ApiTrafficDirection, message_type: Option<&str>, payload_bytes: usize) {
        let category = api_traffic_category_index(message_type);
        let direction = match direction {
            ApiTrafficDirection::Tx => 0,
            ApiTrafficDirection::Rx => 1,
        };
        let mut counters = self.counters.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let tally = &mut counters[category][direction];
        tally.messages = tally.messages.saturating_add(1);
        tally.payload_bytes = tally.payload_bytes.saturating_add(payload_bytes as u64);
    }

    fn log_summary(&self, transport: &str) {
        let elapsed_secs = self.started_at.elapsed().as_secs_f64().max(0.001);
        let counters = *self.counters.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let details = API_TRAFFIC_CATEGORIES
            .iter()
            .zip(counters)
            .map(|(category, directions)| {
                let tx = directions[0];
                let rx = directions[1];
                format!(
                    "{category}[tx_messages={} tx_per_sec={:.3} tx_bytes={} tx_bytes_per_sec={:.1} rx_messages={} rx_per_sec={:.3} rx_bytes={} rx_bytes_per_sec={:.1}]",
                    tx.messages,
                    api_rate_per_second(tx.messages, elapsed_secs),
                    tx.payload_bytes,
                    api_rate_per_second(tx.payload_bytes, elapsed_secs),
                    rx.messages,
                    api_rate_per_second(rx.messages, elapsed_secs),
                    rx.payload_bytes,
                    api_rate_per_second(rx.payload_bytes, elapsed_secs),
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        log::info!(
            "[remote] API traffic metrics monotonic_ms={} transport={} observation_secs={:.1} {}",
            monotonic_ms(),
            transport,
            elapsed_secs,
            details
        );
    }
}

fn api_rate_per_second(count: u64, elapsed_secs: f64) -> f64 {
    count as f64 / elapsed_secs.max(0.001)
}

fn api_traffic_category_index(message_type: Option<&str>) -> usize {
    match message_type.unwrap_or_default() {
        "ping" | "pong" => 0,
        "snapshot"
        | "state.get"
        | "capabilities.get"
        | "capabilities"
        | "telemetry.subscribe"
        | "telemetry.subscribed"
        | "telemetry.update"
        | "catalogs.get"
        | "catalogs"
        | "logs.get"
        | "logs" => 1,
        "command" => 2,
        value if value.starts_with("command.") || value.starts_with("channel.") || value.ends_with(".set") => 2,
        value if value.starts_with("auth.") => 3,
        value if value.starts_with("session.") || value.starts_with("resume.") => 4,
        _ => 5,
    }
}

fn api_traffic_category_name(message_type: Option<&str>) -> &'static str {
    API_TRAFFIC_CATEGORIES[api_traffic_category_index(message_type)]
}

enum ApiSender {
    WebSocket(futures_util::stream::SplitSink<WebSocket, Message>, ApiTrafficRecorder),
    DataChannel(tokio::sync::mpsc::Sender<Vec<u8>>, ApiTrafficRecorder),
    RtcDataChannel(RtcApiOutbound, ApiTrafficRecorder),
}

impl ApiSender {
    async fn send_json(&mut self, value: &Value) -> Result<(), ()> {
        let text = serde_json::to_string(value).map_err(|_| ())?;
        if text.len() > MAX_FRAME_BYTES {
            return Err(());
        }
        let message_type = value.get("type").and_then(Value::as_str);
        let payload_bytes = text.len();
        match self {
            Self::WebSocket(sender, _) => timeout(Duration::from_secs(2), sender.send(Message::Text(text.into())))
                .await
                .map_err(|_| ())?
                .map_err(|_| ())?,
            Self::DataChannel(sender, _) => timeout(Duration::from_secs(2), sender.send(text.into_bytes()))
                .await
                .map_err(|_| ())?
                .map_err(|_| ())?,
            Self::RtcDataChannel(sender, _) => {
                let payload = text.into_bytes();
                let replaceable_kind = if value.get("request_id").is_none() {
                    value.get("type").and_then(Value::as_str)
                } else {
                    None
                };
                if replaceable_kind == Some("snapshot") {
                    timeout(Duration::from_secs(2), async { sender.replace_snapshot(payload).map(|_| ()) })
                        .await
                        .map_err(|_| ())?
                } else if replaceable_kind == Some("telemetry.update") {
                    timeout(Duration::from_secs(2), async { sender.replace_telemetry(payload).map(|_| ()) })
                        .await
                        .map_err(|_| ())?
                } else {
                    timeout(Duration::from_secs(2), sender.send_control(payload))
                        .await
                        .map_err(|_| ())?
                }?;
            }
        }
        match self {
            Self::WebSocket(_, traffic) | Self::DataChannel(_, traffic) | Self::RtcDataChannel(_, traffic) => {
                traffic.record(ApiTrafficDirection::Tx, message_type, payload_bytes);
            }
        }
        Ok(())
    }

    async fn send_message(&mut self, message: Message) -> Result<(), ()> {
        match self {
            Self::WebSocket(sender, _) => timeout(Duration::from_secs(2), sender.send(message))
                .await
                .map_err(|_| ())?
                .map_err(|_| ()),
            Self::DataChannel(_, _) => Err(()),
            Self::RtcDataChannel(_, _) => Err(()),
        }
    }

    fn outbound_queue_metrics(&self) -> Option<(usize, usize, u64, bool)> {
        match self {
            Self::RtcDataChannel(sender, _) => Some((
                sender.control_queue_depth(),
                sender.replaceable_queue_depth(),
                sender.coalesced_state_count(),
                sender.is_congested(),
            )),
            Self::DataChannel(_, _) | Self::WebSocket(_, _) => None,
        }
    }

    fn record_rx(&self, value: &Value, payload_bytes: usize) {
        let message_type = value.get("type").and_then(Value::as_str);
        match self {
            Self::WebSocket(_, traffic) | Self::DataChannel(_, traffic) | Self::RtcDataChannel(_, traffic) => {
                traffic.record(ApiTrafficDirection::Rx, message_type, payload_bytes);
            }
        }
    }

    fn log_traffic_summary(&self, transport: &str) {
        match self {
            Self::WebSocket(_, traffic) | Self::DataChannel(_, traffic) | Self::RtcDataChannel(_, traffic) => {
                traffic.log_summary(transport);
            }
        }
    }
}

async fn websocket_session(
    state: ApiState,
    socket: WebSocket,
    peer: SocketAddr,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let (sender, mut receiver) = socket.split();
    let mut sender = ApiSender::WebSocket(sender, ApiTrafficRecorder::default());
    let first = timeout(Duration::from_secs(10), receiver.next()).await;
    let Some(Ok(Message::Text(first_text))) = first.ok().flatten() else {
        return;
    };
    let hello_value = match parse_unique_json(first_text.as_bytes()) {
        Ok(value) => value,
        Err(error) => {
            let _ = send_error(&mut sender, "INVALID_ARGUMENT", &error.to_string(), None).await;
            return;
        }
    };
    sender.record_rx(&hello_value, first_text.len());
    let hello_fields_ok = value_has_only_fields(
        &hello_value,
        &[
            "type",
            "protocol",
            "request_id",
            "client",
            "client_version",
            "client_nonce",
            "transport",
        ],
    );
    let hello: WsFrame = match serde_json::from_value(hello_value) {
        Ok(hello) => hello,
        Err(_) => {
            let _ = send_error(&mut sender, "INVALID_ARGUMENT", "Invalid hello message", None).await;
            return;
        }
    };
    let request_id = match parse_request_id(hello.request_id.as_deref()) {
        Ok(request_id) => request_id,
        Err(error) => {
            let _ = send_error(&mut sender, "INVALID_REQUEST_ID", &error.to_string(), None).await;
            return;
        }
    };
    if !hello_fields_ok
        || hello.message_type != "hello"
        || hello.protocol != Some(PROTOCOL_VERSION)
        || hello.transport.as_deref() != Some("wss")
        || !hello.client.as_deref().is_some_and(|value| !value.is_empty())
        || !hello.client_version.as_deref().is_some_and(|value| !value.is_empty())
    {
        let _ = send_error(
            &mut sender,
            "UNSUPPORTED_PROTOCOL",
            "Invalid direct WSS hello",
            Some(request_id),
        )
        .await;
        return;
    }
    let Some(client_nonce) = hello.client_nonce.as_deref().and_then(decode_nonce) else {
        let _ = send_error(&mut sender, "INVALID_ARGUMENT", "Invalid client nonce", Some(request_id)).await;
        return;
    };
    let server_nonce = rand::random::<[u8; 32]>();
    let proof_message =
        identity_transcript(state.server_epoch, &client_nonce, &server_nonce, state.identity.server_id());
    let proof = match state.identity.sign(&proof_message) {
        Ok(proof) => proof,
        Err(_) => {
            let _ = send_error(
                &mut sender,
                "IDENTITY_UNAVAILABLE",
                "Host identity is unavailable",
                Some(request_id),
            )
            .await;
            return;
        }
    };
    let hello_ok = json!({
        "type":"hello.ok", "protocol":PROTOCOL_VERSION,
        "request_id":request_id,
        "server_id":state.identity.server_id(),
        "server_epoch":state.server_epoch,
        "server_key":URL_SAFE_NO_PAD.encode(state.identity.public_key()),
        "fingerprint":state.identity.fingerprint(),
        "server_nonce":URL_SAFE_NO_PAD.encode(server_nonce),
        "identity_proof":URL_SAFE_NO_PAD.encode(proof),
        "protocol_min":PROTOCOL_VERSION,
        "protocol_max":PROTOCOL_VERSION
    });
    if send_json(&mut sender, &hello_ok).await.is_err() {
        return;
    }

    let mut session: Option<WsSession> = None;
    let mut unauthenticated_permit = Some(permit);
    let mut telemetry_negotiated = false;
    let mut publish = interval_at(
        tokio::time::Instant::now() + Duration::from_millis(100),
        Duration::from_millis(100),
    );
    publish.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sequence = 0u64;
    let mut state_publisher = SnapshotPublisher::default();
    let reconciliation_interval = state_reconciliation_interval();
    let mut last_metrics_log = Instant::now();
    loop {
        tokio::select! {
            frame = receiver.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        let continue_loop = handle_ws_message(&state, &mut sender, text.as_bytes(), peer, &mut session, &mut sequence, &mut telemetry_negotiated, None).await;
                        if !continue_loop { break; }
                        if session.is_some() {
                            drop(unauthenticated_permit.take());
                            if let Some(active) = session
                                && state_publisher.session != Some((active.id, active.generation))
                                && let Ok(snapshot) = snapshot_for_session(&state, active)
                            {
                                let _ = state_publisher.seed(active, &snapshot, Instant::now());
                            }
                        } else if unauthenticated_permit.is_none() {
                            unauthenticated_permit = Arc::clone(&state.unauthenticated_ws).try_acquire_owned().ok();
                            if unauthenticated_permit.is_none() { break; }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(Message::Ping(payload))) => {
                        if sender.send_message(Message::Pong(payload)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Binary(_))) => {
                        if send_error(&mut sender, "INVALID_ARGUMENT", "Only UTF-8 JSON text frames are supported", None).await.is_err() { break; }
                    }
                }
            }
            _ = publish.tick(), if session.is_some() => {
                if let Some(active) = session {
                    let status = state.sessions.status();
                    if status.phase == SessionPhase::Free {
                        if let Some(reason) = state.sessions.termination_reason(active.id, active.generation) {
                            let ended = session_ended_frame(state.server_epoch, active, reason);
                            if send_json(&mut sender, &ended).await.is_ok() {
                                let acknowledged = wait_for_websocket_session_end_ack(
                                    &mut receiver,
                                    state.server_epoch,
                                    active,
                                ).await;
                                log::debug!("[remote] session end notice delivery completed acknowledged={acknowledged}");
                            }
                        }
                        break;
                    }
                    if status.phase == SessionPhase::Active && state.sessions.active_generation() != Some((active.id, active.generation)) {
                        let _ = sender.send_message(Message::Close(None)).await;
                        break;
                    }
                    if status.phase == SessionPhase::Active && state.sessions.active_generation() == Some((active.id,active.generation)) {
                        let Ok(snapshot) = snapshot_for_session(&state, active) else { break; };
                        let publication = match state_publisher.observe(
                            active, &snapshot, telemetry_negotiated, Instant::now(), reconciliation_interval,
                        ) {
                            Ok(publication) => publication,
                            Err(()) => break,
                        };
                        match publication {
                            StatePublication::FullSnapshot => {
                                sequence = sequence.saturating_add(1);
                                let bytes = match send_snapshot_dto_with_seq(&state, &mut sender, active, sequence, snapshot).await {
                                    Ok(bytes) => bytes,
                                    Err(()) => break,
                                };
                                state_publisher.record_sent(publication, bytes, state.backend.snapshot_age_ms());
                            }
                            StatePublication::Telemetry => {
                                sequence = sequence.saturating_add(1);
                                let response = telemetry_frame(state.server_epoch, sequence, &snapshot);
                                let bytes = serde_json::to_vec(&response).map_or(0, |bytes| bytes.len());
                                if send_json(&mut sender, &response).await.is_err() { break; }
                                state_publisher.record_sent(publication, bytes, state.backend.snapshot_age_ms());
                            }
                            StatePublication::Suppressed => {}
                        }
                        if last_metrics_log.elapsed() >= Duration::from_secs(30) {
                            state_publisher.log_summary("websocket", &sender);
                            sender.log_traffic_summary("websocket");
                            last_metrics_log = Instant::now();
                        }
                    }
                }
            }
        }
    }
    sender.log_traffic_summary("websocket");
    if let Some(active) = session {
        state.sessions.mark_generation_lost(active.id, active.generation);
    }
}

/// Run the existing Remote API v1 session protocol over a reliable DataChannel.
/// The caller must complete the separate ADR-033 DTLS/Host identity binding before
/// delivering any application frame to this adapter.
pub async fn data_channel_session(
    state: ApiState,
    room_id: &str,
    inbound: tokio::sync::mpsc::Receiver<Vec<u8>>,
    outbound: tokio::sync::mpsc::Sender<Vec<u8>>,
) {
    data_channel_session_with_sender(
        state,
        room_id,
        inbound,
        ApiSender::DataChannel(outbound, ApiTrafficRecorder::default()),
    )
    .await;
}

/// Run the DataChannel API with a bounded control FIFO and replaceable state outbox.
pub async fn data_channel_session_optimized(
    state: ApiState,
    room_id: &str,
    inbound: tokio::sync::mpsc::Receiver<Vec<u8>>,
    outbound: RtcApiOutbound,
) {
    data_channel_session_with_sender(
        state,
        room_id,
        inbound,
        ApiSender::RtcDataChannel(outbound, ApiTrafficRecorder::default()),
    )
    .await;
}

async fn data_channel_session_with_sender(
    state: ApiState,
    room_id: &str,
    mut inbound: tokio::sync::mpsc::Receiver<Vec<u8>>,
    mut sender: ApiSender,
) {
    let permit = match Arc::clone(&state.unauthenticated_ws).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            log::warn!(
                "[remote] rejected DataChannel API session because unauthenticated session capacity is full room={room_id}"
            );
            return;
        }
    };
    log::info!(
        "[remote] waiting for Terminal API hello over DataChannel monotonic_ms={} room={room_id}",
        monotonic_ms()
    );
    let first = match timeout(Duration::from_secs(10), inbound.recv()).await {
        Ok(Some(first)) => first,
        Ok(None) => {
            log::warn!("[remote] DataChannel closed before Terminal API hello room={room_id}");
            return;
        }
        Err(_) => {
            log::warn!("[remote] timed out waiting for Terminal API hello over DataChannel room={room_id}");
            return;
        }
    };
    let hello_value = match parse_unique_json(&first) {
        Ok(value) => value,
        Err(error) => {
            log::warn!("[remote] rejected malformed Terminal API hello room={room_id}: {error}");
            let _ = send_error(&mut sender, "INVALID_ARGUMENT", &error.to_string(), None).await;
            return;
        }
    };
    let hello_fields_ok = value_has_only_fields(
        &hello_value,
        &[
            "type",
            "protocol",
            "request_id",
            "client",
            "client_version",
            "client_nonce",
            "transport",
        ],
    );
    let hello: WsFrame = match serde_json::from_value(hello_value) {
        Ok(hello) => hello,
        Err(_) => {
            let _ = send_error(&mut sender, "INVALID_ARGUMENT", "Invalid hello message", None).await;
            return;
        }
    };
    let request_id = match parse_request_id(hello.request_id.as_deref()) {
        Ok(request_id) => request_id,
        Err(error) => {
            let _ = send_error(&mut sender, "INVALID_REQUEST_ID", &error.to_string(), None).await;
            return;
        }
    };
    if !hello_fields_ok
        || hello.message_type != "hello"
        || hello.protocol != Some(PROTOCOL_VERSION)
        || hello.transport.as_deref() != Some("webrtc-datachannel")
        || !hello.client.as_deref().is_some_and(|value| !value.is_empty())
        || !hello.client_version.as_deref().is_some_and(|value| !value.is_empty())
    {
        log::warn!("[remote] rejected unsupported Terminal API hello room={room_id}");
        let _ = send_error(
            &mut sender,
            "UNSUPPORTED_PROTOCOL",
            "Invalid WebRTC DataChannel hello",
            Some(request_id),
        )
        .await;
        return;
    }
    let Some(client_nonce) = hello.client_nonce.as_deref().and_then(decode_nonce) else {
        let _ = send_error(&mut sender, "INVALID_ARGUMENT", "Invalid client nonce", Some(request_id)).await;
        return;
    };
    let server_nonce = rand::random::<[u8; 32]>();
    let proof_message =
        identity_transcript(state.server_epoch, &client_nonce, &server_nonce, state.identity.server_id());
    let proof = match state.identity.sign(&proof_message) {
        Ok(proof) => proof,
        Err(_) => {
            let _ = send_error(
                &mut sender,
                "IDENTITY_UNAVAILABLE",
                "Host identity is unavailable",
                Some(request_id),
            )
            .await;
            return;
        }
    };
    let hello_ok = json!({
        "type":"hello.ok", "protocol":PROTOCOL_VERSION, "request_id":request_id,
        "server_id":state.identity.server_id(), "server_epoch":state.server_epoch,
        "server_key":URL_SAFE_NO_PAD.encode(state.identity.public_key()),
        "fingerprint":state.identity.fingerprint(), "server_nonce":URL_SAFE_NO_PAD.encode(server_nonce),
        "identity_proof":URL_SAFE_NO_PAD.encode(proof), "protocol_min":PROTOCOL_VERSION,
        "protocol_max":PROTOCOL_VERSION
    });
    if send_json(&mut sender, &hello_ok).await.is_err() {
        log::warn!(
            "[remote] Host API hello response failed monotonic_ms={} transport=datachannel room={room_id}",
            monotonic_ms()
        );
        return;
    }
    log::info!(
        "[remote] Host API hello response sent monotonic_ms={} transport=datachannel room={room_id}",
        monotonic_ms()
    );

    let peer = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0);
    let mut session: Option<WsSession> = None;
    let mut cleanup = DataChannelSessionCleanup {
        sessions: &state.sessions,
        backend: state.backend.as_ref(),
        room_id: room_id.to_owned(),
        active: None,
    };
    let mut unauthenticated_permit = Some(permit);
    let mut telemetry_negotiated = false;
    let mut publish = interval_at(
        tokio::time::Instant::now() + Duration::from_millis(100),
        Duration::from_millis(100),
    );
    publish.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sequence = 0_u64;
    let mut state_publisher = SnapshotPublisher::default();
    let reconciliation_interval = state_reconciliation_interval();
    let mut last_metrics_log = Instant::now();
    loop {
        tokio::select! {
            frame = inbound.recv() => {
                let Some(frame) = frame else { break; };
                if !handle_ws_message(&state, &mut sender, &frame, peer, &mut session, &mut sequence, &mut telemetry_negotiated, Some(&mut cleanup)).await { break; }
                cleanup.active = session;
                if session.is_some() {
                    drop(unauthenticated_permit.take());
                    if let Some(active) = session
                        && state_publisher.session != Some((active.id, active.generation))
                        && let Ok(snapshot) = snapshot_for_session(&state, active)
                    {
                        let _ = state_publisher.seed(active, &snapshot, Instant::now());
                    }
                } else if unauthenticated_permit.is_none() {
                    unauthenticated_permit = Arc::clone(&state.unauthenticated_ws).try_acquire_owned().ok();
                    if unauthenticated_permit.is_none() { break; }
                }
            }
            _ = publish.tick(), if session.is_some() => {
                if let Some(active) = session {
                    let status = state.sessions.status();
                    if status.phase == SessionPhase::Free {
                        if let Some(reason) = state.sessions.termination_reason(active.id, active.generation) {
                            let ended = session_ended_frame(state.server_epoch, active, reason);
                            if send_json(&mut sender, &ended).await.is_ok() {
                                let acknowledged = wait_for_data_channel_session_end_ack(
                                    &mut inbound,
                                    state.server_epoch,
                                    active,
                                ).await;
                                log::debug!("[remote] DataChannel session end notice delivery completed room={room_id} acknowledged={acknowledged}");
                            }
                        }
                        break;
                    }
                    if status.phase == SessionPhase::Active && state.sessions.active_generation() != Some((active.id, active.generation)) { break; }
                    if status.phase == SessionPhase::Active && state.sessions.active_generation() == Some((active.id, active.generation)) {
                        let Ok(snapshot) = snapshot_for_session(&state, active) else { break; };
                        let publication = match state_publisher.observe(
                            active, &snapshot, telemetry_negotiated, Instant::now(), reconciliation_interval,
                        ) {
                            Ok(publication) => publication,
                            Err(()) => break,
                        };
                        match publication {
                            StatePublication::FullSnapshot => {
                                sequence = sequence.saturating_add(1);
                                let bytes = match send_snapshot_dto_with_seq(&state, &mut sender, active, sequence, snapshot).await {
                                    Ok(bytes) => bytes,
                                    Err(()) => break,
                                };
                                state_publisher.record_sent(publication, bytes, state.backend.snapshot_age_ms());
                            }
                            StatePublication::Telemetry => {
                                sequence = sequence.saturating_add(1);
                                let response = telemetry_frame(state.server_epoch, sequence, &snapshot);
                                let bytes = serde_json::to_vec(&response).map_or(0, |bytes| bytes.len());
                                if send_json(&mut sender, &response).await.is_err() { break; }
                                state_publisher.record_sent(publication, bytes, state.backend.snapshot_age_ms());
                            }
                            StatePublication::Suppressed => {}
                        }
                        if last_metrics_log.elapsed() >= Duration::from_secs(30) {
                            state_publisher.log_summary("datachannel", &sender);
                            sender.log_traffic_summary("datachannel");
                            last_metrics_log = Instant::now();
                        }
                    }
                }
            }
        }
    }
    sender.log_traffic_summary("datachannel");
}

async fn handle_ws_message(
    state: &ApiState,
    sender: &mut ApiSender,
    bytes: &[u8],
    peer: SocketAddr,
    session: &mut Option<WsSession>,
    sequence: &mut u64,
    telemetry_negotiated: &mut bool,
    mut cleanup: Option<&mut DataChannelSessionCleanup<'_>>,
) -> bool {
    let value = match parse_unique_json(bytes) {
        Ok(value) => value,
        Err(error) => {
            return send_error(sender, "INVALID_ARGUMENT", &error.to_string(), None)
                .await
                .is_ok();
        }
    };
    sender.record_rx(&value, bytes.len());
    let message_type = value.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
    log::debug!(
        "[remote] API message rx monotonic_ms={} message_category={} bytes={}",
        monotonic_ms(),
        api_traffic_category_name(Some(&message_type)),
        bytes.len()
    );
    if message_type == "command" {
        let request_id = value
            .get("request_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok());
        let command: CommandRequest = match serde_json::from_value(value) {
            Ok(command) => command,
            Err(error) => {
                return send_command_error(sender, request_id, "INVALID_ARGUMENT", &error.to_string())
                    .await
                    .is_ok();
            }
        };
        let command = match command.validate() {
            Ok(command) => command,
            Err(error) => {
                return send_command_error(sender, request_id, protocol_error_code(&error), &error.to_string())
                    .await
                    .is_ok();
            }
        };
        let Some(active) = *session else {
            return send_error(sender, "AUTH_REQUIRED", "Authentication required", Some(command.request_id))
                .await
                .is_ok();
        };
        let id = active.id;
        let result = state
            .sessions
            .with_generation(id, active.generation, || Ok(dispatch_command(state, &id, command)));
        match result {
            Ok(Ok(accepted)) => {
                let response = json!({"type":"command.accepted","protocol":PROTOCOL_VERSION,"request_id":accepted.request_id,"queued":accepted.queued});
                send_json(sender, &response).await.is_ok()
            }
            Ok(Err(error)) => send_command_error(sender, request_id, &error.code, &error.message)
                .await
                .is_ok(),
            Err(error) => send_error(sender, "AUTH_REQUIRED", &error.to_string(), request_id)
                .await
                .is_ok(),
        }
    } else {
        let allowed = match message_type.as_str() {
            "auth.device.begin" => &[
                "type",
                "protocol",
                "request_id",
                "device_id",
                "public_key",
                "invitation_id",
            ][..],
            "auth.device.prove" => &[
                "type",
                "protocol",
                "request_id",
                "challenge_id",
                "device_id",
                "public_key",
                "signature",
                "invitation_id",
                "pairing_secret",
            ][..],
            "auth.resume" => &[
                "type",
                "protocol",
                "request_id",
                "session_id",
                "resume_ticket",
                "client_nonce",
            ][..],
            "auth.token.refresh" | "auth.logout" | "state.get" | "capabilities.get" | "telemetry.subscribe" => {
                &["type", "protocol", "request_id"][..]
            }
            "catalogs.get" => &["type", "protocol", "request_id", "catalog"][..],
            "ping" => &["type", "protocol", "request_id", "nonce"][..],
            "logs.get" => &["type", "protocol", "request_id", "cursor", "limit"][..],
            "logs.export.start" => &["type", "protocol", "request_id", "start_utc", "end_utc"][..],
            "logs.export.chunk" => &["type", "protocol", "request_id", "snapshot_id", "offset", "max_bytes"][..],
            "logs.export.cancel" => &["type", "protocol", "request_id", "snapshot_id"][..],
            _ => &[][..],
        };
        if allowed.is_empty() {
            return send_error(sender, "UNSUPPORTED_OPERATION", "Unsupported message type", None)
                .await
                .is_ok();
        }
        if !value_has_only_fields(&value, allowed) {
            return send_error(sender, "INVALID_ARGUMENT", "Malformed message", None)
                .await
                .is_ok();
        }
        let frame: WsFrame = match serde_json::from_value(value) {
            Ok(frame) => frame,
            Err(error) => {
                return send_error(sender, "INVALID_ARGUMENT", &error.to_string(), None)
                    .await
                    .is_ok();
            }
        };
        let request_id = match parse_request_id(frame.request_id.as_deref()) {
            Ok(request_id) => request_id,
            Err(error) => {
                return send_error(sender, "INVALID_REQUEST_ID", &error.to_string(), None)
                    .await
                    .is_ok();
            }
        };
        if frame.protocol != Some(PROTOCOL_VERSION) {
            return send_error(sender, "UNSUPPORTED_PROTOCOL", "Unsupported protocol version", Some(request_id))
                .await
                .is_ok();
        }
        if frame.message_type == "catalogs.get" && !matches!(frame.catalog.as_deref(), Some("presets" | "zone_presets"))
        {
            return send_error(
                sender,
                "INVALID_ARGUMENT",
                "catalog must be presets or zone_presets",
                Some(request_id),
            )
            .await
            .is_ok();
        }
        if frame.message_type == "logs.get" && frame.limit.is_none() {
            return send_error(sender, "INVALID_ARGUMENT", "logs.get requires limit", Some(request_id))
                .await
                .is_ok();
        }
        if frame.message_type == "logs.export.chunk"
            && (frame.snapshot_id.is_none()
                || frame.offset.is_none()
                || !frame
                    .max_bytes
                    .is_some_and(|bytes| (1..=shocking_vrc_diagnostics::MAX_LOG_CHUNK_BYTES as u16).contains(&bytes)))
        {
            return send_error(
                sender,
                "INVALID_ARGUMENT",
                "Log export chunk requires a snapshot, offset, and bounded size",
                Some(request_id),
            )
            .await
            .is_ok();
        }
        if frame.message_type == "logs.export.cancel" && frame.snapshot_id.is_none() {
            return send_error(
                sender,
                "INVALID_ARGUMENT",
                "Log export cancellation requires a snapshot",
                Some(request_id),
            )
            .await
            .is_ok();
        }
        let export_range = if frame.message_type == "logs.export.start" {
            match (frame.start_utc.as_deref(), frame.end_utc.as_deref()) {
                (None, None) => None,
                (Some(start), Some(end)) => match shocking_vrc_diagnostics::LogTimeRange::new(start, end) {
                    Ok(range) => Some(range),
                    Err(message) => {
                        return send_error(sender, "INVALID_ARGUMENT", &message, Some(request_id))
                            .await
                            .is_ok();
                    }
                },
                _ => {
                    return send_error(
                        sender,
                        "INVALID_ARGUMENT",
                        "A filtered log export requires both UTC range bounds",
                        Some(request_id),
                    )
                    .await
                    .is_ok();
                }
            }
        } else {
            None
        };
        match frame.message_type.as_str() {
            "auth.device.begin" => {
                if !state
                    .login_throttle
                    .lock()
                    .map(|mut limiter| limiter.allow(peer.ip()))
                    .unwrap_or(false)
                {
                    return send_error(sender, "AUTH_FAILED", "Authentication failed", Some(request_id))
                        .await
                        .is_ok();
                }
                let (Some(device_id), Some(public_key)) = (frame.device_id, frame.public_key) else {
                    return send_error(sender, "INVALID_ARGUMENT", "Device identity is required", Some(request_id))
                        .await
                        .is_ok();
                };
                let Some(public_key) = decode_fixed::<32>(&public_key) else {
                    return send_error(sender, "INVALID_ARGUMENT", "Device public key is invalid", Some(request_id))
                        .await
                        .is_ok();
                };
                match state.sessions.issue_device_challenge(
                    state.identity.server_id(),
                    request_id,
                    device_id,
                    public_key,
                    frame.invitation_id,
                ) {
                    Ok(challenge) => {
                        let response = json!({
                            "type":"auth.device.challenge", "protocol":PROTOCOL_VERSION,
                            "request_id":challenge.request_id, "challenge_id":challenge.challenge_id,
                            "device_id":challenge.device_id, "server_nonce":URL_SAFE_NO_PAD.encode(challenge.server_nonce),
                        });
                        send_json(sender, &response).await.is_ok()
                    }
                    Err(error) => send_error(sender, auth_code(&error), auth_message(&error), Some(request_id))
                        .await
                        .is_ok(),
                }
            }
            "auth.device.prove" => {
                let (Some(challenge_id), Some(device_id), Some(public_key), Some(signature)) =
                    (frame.challenge_id, frame.device_id, frame.public_key, frame.signature)
                else {
                    return send_error(sender, "INVALID_ARGUMENT", "Device proof is incomplete", Some(request_id))
                        .await
                        .is_ok();
                };
                let (Some(public_key), Some(signature)) =
                    (decode_fixed::<32>(&public_key), decode_fixed::<64>(&signature))
                else {
                    return send_error(
                        sender,
                        "INVALID_ARGUMENT",
                        "Device proof encoding is invalid",
                        Some(request_id),
                    )
                    .await
                    .is_ok();
                };
                let sessions = Arc::clone(&state.sessions);
                let host_id = state.identity.server_id().to_owned();
                let proof_device_id = device_id.clone();
                let pairing_secret = frame.pairing_secret.map(Zeroizing::new);
                let invitation_id = frame.invitation_id;
                let result = tokio::task::spawn_blocking(move || {
                    sessions.authenticate_device(
                        &host_id,
                        challenge_id,
                        request_id,
                        &proof_device_id,
                        public_key,
                        &signature,
                        invitation_id,
                        pairing_secret.as_ref().map(|secret| secret.as_str()),
                    )
                })
                .await;
                match result {
                    Ok(Ok(login)) => {
                        let grant = state
                            .sessions
                            .device_grant(&device_id, state.identity.server_id(), |bytes| {
                                state.identity.sign(bytes).map_err(|_| AuthError::State)
                            });
                        let grant = match grant {
                            Ok(grant) => grant,
                            Err(error) => {
                                return send_error(sender, auth_code(&error), auth_message(&error), Some(request_id))
                                    .await
                                    .is_ok();
                            }
                        };
                        let active = WsSession {
                            id: login.session_id,
                            generation: login.transport_generation,
                        };
                        *session = Some(active);
                        *telemetry_negotiated = false;
                        if let Some(cleanup) = cleanup.as_deref_mut() {
                            cleanup.active = Some(active);
                        }
                        let mut response = login_response(login, Some(request_id));
                        if let Value::Object(object) = &mut response {
                            object.insert("device_id".into(), Value::String(device_id));
                            object.insert("device_grant".into(), Value::String(grant));
                        }
                        if send_json(sender, &response).await.is_err() {
                            return false;
                        }
                        *sequence = sequence.saturating_add(1);
                        send_snapshot_with_seq(state, sender, active, *sequence).await.is_ok()
                    }
                    Ok(Err(error)) => {
                        if matches!(
                            error,
                            AuthError::Failed | AuthError::InvalidInvitation | AuthError::DeviceNotPaired
                        ) {
                            if let Ok(mut limiter) = state.login_throttle.lock() {
                                limiter.record_failure(peer.ip());
                            }
                        }
                        send_error(sender, auth_code(&error), auth_message(&error), Some(request_id))
                            .await
                            .is_ok()
                    }
                    Err(_) => send_error(
                        sender,
                        "AUTH_UNAVAILABLE",
                        "Authentication is temporarily unavailable",
                        Some(request_id),
                    )
                    .await
                    .is_ok(),
                }
            }
            "auth.resume" => {
                log::info!(
                    "[remote] API session resume request received monotonic_ms={} transport={} phase_before_resume={}",
                    monotonic_ms(),
                    if cleanup.is_some() { "datachannel" } else { "wss" },
                    state.sessions.status().phase.as_str()
                );
                let (Some(session_id), Some(ticket)) = (frame.session_id, frame.resume_ticket) else {
                    return send_error(
                        sender,
                        "INVALID_ARGUMENT",
                        "Resume requires session_id and resume_ticket",
                        Some(request_id),
                    )
                    .await
                    .is_ok();
                };
                if !frame.client_nonce.as_deref().is_some_and(|value| !value.is_empty()) {
                    return send_error(
                        sender,
                        "INVALID_ARGUMENT",
                        "Resume requires a valid client nonce",
                        Some(request_id),
                    )
                    .await
                    .is_ok();
                }
                match state.sessions.resume(session_id, &ticket, request_id) {
                    Ok(login) => {
                        let active = WsSession {
                            id: login.session_id,
                            generation: login.transport_generation,
                        };
                        *session = Some(active);
                        *telemetry_negotiated = false;
                        if let Some(cleanup) = cleanup.as_deref_mut() {
                            cleanup.active = Some(active);
                        }
                        if send_json(sender, &login_response(login, Some(request_id)))
                            .await
                            .is_err()
                        {
                            return false;
                        }
                        *sequence = sequence.saturating_add(1);
                        send_snapshot_with_seq(state, sender, active, *sequence).await.is_ok()
                    }
                    Err(AuthError::SessionEnded { reason, generation }) => {
                        let ended = session_ended_frame(
                            state.server_epoch,
                            WsSession {
                                id: session_id,
                                generation,
                            },
                            reason,
                        );
                        send_json(sender, &ended).await.is_ok()
                    }
                    Err(error) => send_error(sender, auth_code(&error), &auth_message(&error), Some(request_id))
                        .await
                        .is_ok(),
                }
            }
            "auth.token.refresh" => {
                let Some(active) = *session else {
                    return send_error(sender, "AUTH_REQUIRED", "Authentication required", Some(request_id))
                        .await
                        .is_ok();
                };
                match state.sessions.refresh_generation(active.id, active.generation) {
                    Ok(refresh) => {
                        let response = json!({"type":"auth.token","protocol":PROTOCOL_VERSION,"request_id":request_id,"access_token":refresh.access_token,"access_expires_in":refresh.access_expires_in});
                        send_json(sender, &response).await.is_ok()
                    }
                    Err(error) => send_error(sender, auth_code(&error), &auth_message(&error), Some(request_id))
                        .await
                        .is_ok(),
                }
            }
            "auth.logout" => {
                let Some(active) = session.take() else {
                    return send_error(sender, "AUTH_REQUIRED", "Authentication required", Some(request_id))
                        .await
                        .is_ok();
                };
                match state.sessions.logout_generation(active.id, active.generation) {
                    Ok(()) => {
                        *telemetry_negotiated = false;
                        let response =
                            json!({"type":"auth.logged_out","protocol":PROTOCOL_VERSION,"request_id":request_id});
                        send_json(sender, &response).await.is_ok()
                    }
                    Err(error) => send_error(sender, auth_code(&error), &auth_message(&error), Some(request_id))
                        .await
                        .is_ok(),
                }
            }
            "telemetry.subscribe" => {
                let Some(active) = *session else {
                    return send_error(sender, "AUTH_REQUIRED", "Authentication required", Some(request_id))
                        .await
                        .is_ok();
                };
                let response = json!({
                    "type":"telemetry.subscribed", "protocol":PROTOCOL_VERSION,
                    "request_id":request_id, "feature":"telemetry.replace.v1"
                });
                if send_json(sender, &response).await.is_err() {
                    return false;
                }
                *telemetry_negotiated = true;
                *sequence = sequence.saturating_add(1);
                send_snapshot_with_seq(state, sender, active, *sequence).await.is_ok()
            }
            "ping" => {
                let Some(nonce) = frame.nonce else {
                    return send_error(sender, "INVALID_ARGUMENT", "Ping requires a nonce", Some(request_id))
                        .await
                        .is_ok();
                };
                let Some(active) = *session else {
                    return send_error(sender, "AUTH_REQUIRED", "Authentication required", Some(request_id))
                        .await
                        .is_ok();
                };
                if let Err(error) = state.sessions.touch_generation(active.id, active.generation) {
                    return send_error(sender, auth_code(&error), &auth_message(&error), Some(request_id))
                        .await
                        .is_ok();
                }
                let response = json!({"type":"pong","protocol":PROTOCOL_VERSION,"request_id":request_id,"nonce":nonce});
                let sent = send_json(sender, &response).await.is_ok();
                log::debug!(
                    "[remote] API heartbeat tx monotonic_ms={} generation={} acknowledged={sent}",
                    monotonic_ms(),
                    active.generation
                );
                sent
            }
            "logs.export.start" | "logs.export.chunk" | "logs.export.cancel" => {
                let Some(active) = *session else {
                    return send_error(sender, "AUTH_REQUIRED", "Authentication required", Some(request_id))
                        .await
                        .is_ok();
                };
                handle_log_export_request(
                    state,
                    sender,
                    active,
                    &frame.message_type,
                    request_id,
                    frame.snapshot_id,
                    frame.offset,
                    frame.max_bytes,
                    export_range,
                    cleanup.as_deref().map(|cleanup| cleanup.room_id.clone()),
                )
                .await
            }
            "state.get" | "capabilities.get" | "catalogs.get" | "logs.get" => {
                let Some(active) = *session else {
                    return send_error(sender, "AUTH_REQUIRED", "Authentication required", Some(request_id))
                        .await
                        .is_ok();
                };
                let result = state.sessions.with_generation(active.id, active.generation, || {
                    let value = match frame.message_type.as_str() {
                        "state.get" => snapshot(state).and_then(|dto| {
                            serde_json::to_value(dto)
                                .map_err(|error| BackendError::new("SERIALIZATION_FAILED", error.to_string()))
                        }),
                        "capabilities.get" => state.backend.snapshot().and_then(|snapshot| {
                            serde_json::to_value(capabilities(snapshot.prefs.is_some()))
                                .map_err(|error| BackendError::new("SERIALIZATION_FAILED", error.to_string()))
                        }),
                        "catalogs.get" => match frame.catalog.as_deref() {
                            Some("presets") => state.backend.presets().and_then(|presets| {
                                serde_json::to_value(json!({"catalog":"presets","entries":presets.entries}))
                                    .map_err(|error| BackendError::new("SERIALIZATION_FAILED", error.to_string()))
                            }),
                            Some("zone_presets") => state.backend.zone_presets().and_then(|zone_presets| {
                                serde_json::to_value(json!({"catalog":"zone_presets","entries":zone_presets.entries}))
                                    .map_err(|error| BackendError::new("SERIALIZATION_FAILED", error.to_string()))
                            }),
                            _ => Err(BackendError::new("INVALID_ARGUMENT", "Invalid catalog")),
                        },
                        _ => {
                            let cursor = frame
                                .cursor
                                .as_deref()
                                .map(str::parse::<u64>)
                                .transpose()
                                .map_err(|_| AuthError::Failed)?;
                            let limit = frame.limit.ok_or(AuthError::Failed)?;
                            if !(1..=200).contains(&limit) {
                                return Err(AuthError::Failed);
                            }
                            state.backend.logs(cursor, limit).and_then(|dto| {
                                serde_json::to_value(dto)
                                    .map_err(|error| BackendError::new("SERIALIZATION_FAILED", error.to_string()))
                            })
                        }
                    };
                    Ok(value)
                });
                match result {
                    Ok(Ok(value)) => {
                        let message_type = match frame.message_type.as_str() {
                            "state.get" => "snapshot",
                            "capabilities.get" => "capabilities",
                            "catalogs.get" => "catalogs",
                            _ => "logs",
                        };
                        if message_type == "snapshot" {
                            *sequence = sequence.saturating_add(1);
                            let response = json!({"type":message_type,"protocol":PROTOCOL_VERSION,"request_id":request_id,"server_epoch":state.server_epoch,"seq":*sequence,"state":value});
                            if serde_json::to_vec(&response)
                                .map(|encoded| encoded.len() > MAX_FRAME_BYTES)
                                .unwrap_or(true)
                            {
                                return send_error(
                                    sender,
                                    "SNAPSHOT_TOO_LARGE",
                                    "Snapshot exceeds the protocol frame limit",
                                    Some(request_id),
                                )
                                .await
                                .is_ok();
                            }
                            return send_json(sender, &response).await.is_ok();
                        }
                        let Value::Object(mut response) = value else {
                            return send_error(
                                sender,
                                "BACKEND_UNAVAILABLE",
                                "Remote state is unavailable",
                                Some(request_id),
                            )
                            .await
                            .is_ok();
                        };
                        response.insert("type".into(), Value::String(message_type.into()));
                        response.insert("protocol".into(), Value::from(PROTOCOL_VERSION));
                        response.insert("request_id".into(), Value::String(request_id.to_string()));
                        if message_type == "logs"
                            && serde_json::to_vec(&Value::Object(response.clone()))
                                .is_ok_and(|encoded| encoded.len() > MAX_FRAME_BYTES)
                        {
                            return send_error(
                                sender,
                                "RESPONSE_TOO_LARGE",
                                "Host log page exceeds the protocol frame limit; request a smaller page",
                                Some(request_id),
                            )
                            .await
                            .is_ok();
                        }
                        send_json(sender, &Value::Object(response)).await.is_ok()
                    }
                    Ok(Err(error)) => send_error(sender, error.code, &error.message, Some(request_id))
                        .await
                        .is_ok(),
                    Err(error) => send_error(sender, auth_code(&error), &auth_message(&error), Some(request_id))
                        .await
                        .is_ok(),
                }
            }
            _ => send_error(sender, "INVALID_ARGUMENT", "Unsupported message type", Some(request_id))
                .await
                .is_ok(),
        }
    }
}

async fn handle_log_export_request(
    state: &ApiState,
    sender: &mut ApiSender,
    active: WsSession,
    request_type: &str,
    request_id: Uuid,
    snapshot_id: Option<Uuid>,
    offset: Option<u64>,
    max_bytes: Option<u16>,
    range: Option<shocking_vrc_diagnostics::LogTimeRange>,
    room_id: Option<String>,
) -> bool {
    if let Err(error) = state.sessions.with_generation(active.id, active.generation, || Ok(())) {
        return send_error(sender, auth_code(&error), &auth_message(&error), Some(request_id))
            .await
            .is_ok();
    }

    let backend = Arc::clone(&state.backend);
    let worker_backend = Arc::clone(&backend);
    let server_epoch = state.server_epoch;
    let request_type = request_type.to_owned();
    let result = tokio::task::spawn_blocking(move || {
        let (body, started_snapshot) = match request_type.as_str() {
            "logs.export.start" => {
                let metadata = worker_backend.begin_log_export(active.id, active.generation, range)?;
                let snapshot_id = metadata.snapshot_id;
                let (version, commit) = worker_backend.application_provenance();
                (
                    json!({
                        "type":"logs.export.started",
                        "snapshot":metadata,
                        "application":{"name":"ShockingVRC Host","version":version,"git_commit":commit},
                        "session":{"server_epoch":server_epoch,"session_id":active.id,"transport_generation":active.generation,"room_id":room_id},
                    }),
                    Some(snapshot_id),
                )
            }
            "logs.export.chunk" => {
                let snapshot_id = snapshot_id.expect("validated snapshot ID");
                let chunk = worker_backend.read_log_export_chunk(
                    active.id,
                    active.generation,
                    snapshot_id,
                    offset.expect("validated chunk offset"),
                    max_bytes.expect("validated chunk size") as usize,
                )?;
                let mut body = serde_json::to_value(chunk)
                    .map_err(|error| BackendError::new("SERIALIZATION_FAILED", error.to_string()))?;
                if let Some(object) = body.as_object_mut() {
                    object.insert("type".into(), Value::String("logs.export.chunk".to_owned()));
                }
                (body, None)
            }
            _ => {
                let snapshot_id = snapshot_id.expect("validated snapshot ID");
                worker_backend.cancel_log_export(active.id, active.generation, snapshot_id);
                (json!({"type":"logs.export.cancelled","snapshot_id":snapshot_id}), None)
            }
        };
        Ok::<_, BackendError>((body, started_snapshot))
    })
    .await;

    let (mut response, started_snapshot) = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            return send_error(sender, error.code, &error.message, Some(request_id))
                .await
                .is_ok();
        }
        Err(_) => {
            return send_error(
                sender,
                "BACKEND_UNAVAILABLE",
                "Host diagnostics are temporarily unavailable",
                Some(request_id),
            )
            .await
            .is_ok();
        }
    };
    if let Err(error) = state.sessions.with_generation(active.id, active.generation, || Ok(())) {
        if let Some(snapshot_id) = started_snapshot {
            backend.cancel_log_export(active.id, active.generation, snapshot_id);
        }
        return send_error(sender, auth_code(&error), &auth_message(&error), Some(request_id))
            .await
            .is_ok();
    }
    if let Some(object) = response.as_object_mut() {
        object.insert("protocol".into(), Value::from(PROTOCOL_VERSION));
        object.insert("request_id".into(), Value::String(request_id.to_string()));
    }
    if serde_json::to_vec(&response).is_ok_and(|encoded| encoded.len() > MAX_FRAME_BYTES) {
        if let Some(snapshot_id) = started_snapshot {
            backend.cancel_log_export(active.id, active.generation, snapshot_id);
        }
        return send_error(
            sender,
            "RESPONSE_TOO_LARGE",
            "Host diagnostic export response exceeded the protocol frame limit",
            Some(request_id),
        )
        .await
        .is_ok();
    }
    send_json(sender, &response).await.is_ok()
}

async fn send_snapshot_with_seq(
    state: &ApiState,
    sender: &mut ApiSender,
    session: WsSession,
    sequence: u64,
) -> Result<(), ()> {
    let snapshot = snapshot_for_session(state, session)?;
    send_snapshot_dto_with_seq(state, sender, session, sequence, snapshot)
        .await
        .map(|_| ())
}

fn snapshot_for_session(state: &ApiState, session: WsSession) -> Result<SnapshotDto, ()> {
    state
        .sessions
        .with_generation(session.id, session.generation, || Ok(snapshot(state)))
        .map_err(|_| ())?
        .map_err(|_| ())
}

fn snapshot_frame(server_epoch: Uuid, sequence: u64, snapshot: &SnapshotDto) -> Value {
    json!({"type":"snapshot","protocol":PROTOCOL_VERSION,"server_epoch":server_epoch,"seq":sequence,"state":snapshot})
}

fn telemetry_frame(server_epoch: Uuid, sequence: u64, snapshot: &SnapshotDto) -> Value {
    json!({
        "type":"telemetry.update", "protocol":PROTOCOL_VERSION,
        "server_epoch":server_epoch, "seq":sequence,
        "telemetry": {
            "avatar_zones": &snapshot.avatar_zones,
            "status": {
                "channel_a": &snapshot.status.channel_a,
                "channel_b": &snapshot.status.channel_b
            }
        }
    })
}

async fn send_snapshot_dto_with_seq(
    state: &ApiState,
    sender: &mut ApiSender,
    session: WsSession,
    sequence: u64,
    snapshot: SnapshotDto,
) -> Result<usize, ()> {
    state
        .sessions
        .with_generation(session.id, session.generation, || Ok(()))
        .map_err(|_| ())?;
    let response = snapshot_frame(state.server_epoch, sequence, &snapshot);
    if serde_json::to_vec(&response).map_err(|_| ())?.len() > MAX_FRAME_BYTES {
        let _ = send_error(sender, "SNAPSHOT_TOO_LARGE", "Snapshot exceeds the protocol frame limit", None).await;
        return Err(());
    }
    let bytes = serde_json::to_vec(&response).map_err(|_| ())?.len();
    send_json(sender, &response).await?;
    Ok(bytes)
}

fn identity_transcript(epoch: Uuid, client_nonce: &[u8], server_nonce: &[u8; 32], server_id: &str) -> Vec<u8> {
    let context = b"ShockingVRC-Term/direct-wss/v1\0";
    let mut transcript = Vec::with_capacity(context.len() + 16 + 2 + client_nonce.len() + 32 + 2 + server_id.len());
    transcript.extend_from_slice(context);
    transcript.extend_from_slice(epoch.as_bytes());
    transcript.extend_from_slice(&(client_nonce.len() as u16).to_be_bytes());
    transcript.extend_from_slice(client_nonce);
    transcript.extend_from_slice(server_nonce);
    transcript.extend_from_slice(&(server_id.len() as u16).to_be_bytes());
    transcript.extend_from_slice(server_id.as_bytes());
    transcript
}

fn decode_nonce(value: &str) -> Option<Vec<u8>> {
    let decoded = URL_SAFE_NO_PAD.decode(value).ok()?;
    (decoded.len() == 32).then_some(decoded)
}

fn decode_fixed<const N: usize>(value: &str) -> Option<[u8; N]> {
    URL_SAFE_NO_PAD.decode(value).ok()?.try_into().ok()
}

fn value_has_only_fields(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
}

fn parse_request_id(value: Option<&str>) -> Result<Uuid, ProtocolError> {
    Uuid::parse_str(value.ok_or(ProtocolError::RequestId)?).map_err(|_| ProtocolError::RequestId)
}

async fn send_error(sender: &mut ApiSender, code: &str, message: &str, request_id: Option<Uuid>) -> Result<(), ()> {
    let mut response = json!({"type":"error","protocol":PROTOCOL_VERSION,"code":code,"message":message});
    if let (Some(request_id), Value::Object(fields)) = (request_id, &mut response) {
        fields.insert("request_id".into(), Value::String(request_id.to_string()));
    }
    send_json(sender, &response).await
}

async fn send_command_error(
    sender: &mut ApiSender,
    request_id: Option<Uuid>,
    code: &str,
    message: &str,
) -> Result<(), ()> {
    let Some(request_id) = request_id else {
        return send_error(sender, code, message, None).await;
    };
    let bounded_message = message.chars().take(4096).collect::<String>();
    let response = json!({
        "type":"command.error",
        "protocol":PROTOCOL_VERSION,
        "request_id":request_id,
        "code":code,
        "message":bounded_message
    });
    send_json(sender, &response).await
}

async fn send_json(sender: &mut ApiSender, value: &Value) -> Result<(), ()> {
    sender.send_json(value).await
}

fn auth_code(error: &AuthError) -> &'static str {
    match error {
        AuthError::Busy => "SESSION_BUSY",
        AuthError::Failed | AuthError::InvalidInvitation | AuthError::DeviceNotPaired | AuthError::InvalidChallenge => {
            "AUTH_FAILED"
        }
        AuthError::InvalidResume => "RESUME_TICKET_INVALID",
        _ => "AUTH_REQUIRED",
    }
}

fn auth_message(error: &AuthError) -> &'static str {
    match error {
        AuthError::Busy => "Remote session is busy",
        AuthError::Failed | AuthError::InvalidInvitation | AuthError::DeviceNotPaired | AuthError::InvalidChallenge => {
            "Authentication failed"
        }
        AuthError::InvalidResume => "Resume ticket is invalid",
        _ => "Authentication required",
    }
}

struct LoginThrottle {
    attempts: HashMap<std::net::IpAddr, VecDeque<Instant>>,
}

impl LoginThrottle {
    fn allow(&mut self, ip: std::net::IpAddr) -> bool {
        let now = Instant::now();
        let attempts = self.attempts.entry(ip).or_default();
        while attempts
            .front()
            .is_some_and(|at| now.duration_since(*at) > Duration::from_secs(60))
        {
            attempts.pop_front();
        }
        attempts.len() < 5
    }

    fn record_failure(&mut self, ip: std::net::IpAddr) {
        self.attempts.entry(ip).or_default().push_back(Instant::now());
        if self.attempts.len() > 256 {
            if let Some(ip) = self
                .attempts
                .iter()
                .min_by_key(|(_, queue)| queue.front().copied())
                .map(|(ip, _)| *ip)
            {
                self.attempts.remove(&ip);
            }
        }
    }
}

#[cfg(test)]
mod device_auth_tests {
    use super::*;
    use crate::controller::{HostRemoteController, TraversalState};
    use base64::Engine;
    use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
    use shocking_vrc_rendezvous::{
        config::{HostAuthorization, LoadedConfig, RoomConfig, ServerConfig, StunConfig, TlsConfig, TurnConfig},
        pairing::{create_route_token, device_auth_transcript, device_id_for_public_key},
        server::run_with_shutdown,
    };
    use shocking_vrc_rtc::{TerminalRtcConfig, accept_host_room, join_terminal_room, register_host};
    use std::{
        collections::BTreeMap,
        fs,
        net::TcpListener,
        path::PathBuf,
        process::{Child, Command, Stdio},
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use tokio::sync::{Notify, mpsc, oneshot};
    use zeroize::Zeroizing;

    const E2E_TEST_NAME: &str = "api::device_auth_tests::localhost_host_terminal_datachannel_remote_api_acceptance";
    const E2E_REGISTRATION_KEY: &str = "ephemeral-local-e2e-registration-key";

    #[test]
    fn host_api_traffic_metrics_aggregate_direction_category_and_size_only() {
        let recorder = ApiTrafficRecorder::default();
        recorder.record(ApiTrafficDirection::Tx, Some("ping"), 88);
        recorder.record(ApiTrafficDirection::Rx, Some("pong"), 96);
        recorder.record(ApiTrafficDirection::Tx, Some("channel.config.set"), 512);
        let counters = *recorder.counters.lock().unwrap();
        assert_eq!(counters[0][0].messages, 1);
        assert_eq!(counters[0][0].payload_bytes, 88);
        assert_eq!(counters[0][1].messages, 1);
        assert_eq!(counters[0][1].payload_bytes, 96);
        assert_eq!(counters[2][0].messages, 1);
        assert_eq!(counters[2][0].payload_bytes, 512);
        assert_eq!(api_rate_per_second(4, 2.0), 2.0);
        assert_eq!(api_rate_per_second(800, 2.0), 400.0);
        assert_eq!(API_TRAFFIC_CATEGORIES.len(), 6);
        assert_eq!(api_traffic_category_name(Some("not-a-protocol-frame")), "other");
    }

    struct E2eRtcLogger;

    static E2E_RTC_LOGGER: E2eRtcLogger = E2eRtcLogger;
    static E2E_HOST_ROOM_CLOSE_NOTICES: AtomicUsize = AtomicUsize::new(0);
    static E2E_HOST_ROOM_CLOSE_ACKS: AtomicUsize = AtomicUsize::new(0);
    static E2E_HOST_TURN_CREDENTIALS_STAGED: AtomicUsize = AtomicUsize::new(0);
    static E2E_HOST_RECEIVE_PUMPS_ENDED: AtomicUsize = AtomicUsize::new(0);
    static E2E_HOST_DATACHANNEL_CLOSE_EVENTS: AtomicUsize = AtomicUsize::new(0);
    static E2E_ICE_DISCONNECTED_EVENTS: AtomicUsize = AtomicUsize::new(0);

    impl log::Log for E2eRtcLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.target().starts_with("shocking_vrc_rtc") || metadata.target().starts_with("shocking_vrc_remote")
        }

        fn log(&self, record: &log::Record<'_>) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let message = record.args().to_string();
            let host_process = std::env::var("SHOCKINGVRC_REMOTE_E2E_ROLE").as_deref() == Ok("host");
            if message.contains("[rtc] ICE connection state") && message.contains("state=Disconnected") {
                E2E_ICE_DISCONNECTED_EVENTS.fetch_add(1, Ordering::Relaxed);
            }
            if host_process && message.contains("[remote] Rendezvous closed the active signaling room") {
                E2E_HOST_ROOM_CLOSE_NOTICES.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "[remote-test] host_room_close_notification monotonic_ms={}",
                    shocking_vrc_rtc::monotonic_ms()
                );
            }
            if host_process && message.contains("[remote] signaling room closed after DataChannel session ended") {
                E2E_HOST_ROOM_CLOSE_ACKS.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "[remote-test] host_room_close_acknowledged monotonic_ms={}",
                    shocking_vrc_rtc::monotonic_ms()
                );
            }
            if host_process && message.contains("[remote] staged fresh TURN credentials for a future ICE gathering") {
                let count = E2E_HOST_TURN_CREDENTIALS_STAGED.fetch_add(1, Ordering::Relaxed) + 1;
                eprintln!(
                    "[remote-test] host_turn_credentials_staged monotonic_ms={} staged_count={count}",
                    shocking_vrc_rtc::monotonic_ms()
                );
            }
            if host_process
                && (message.contains("DataChannel API receive pump stopped because its consumer closed")
                    || message.contains("DataChannel API receive pump ended after local closure"))
            {
                E2E_HOST_RECEIVE_PUMPS_ENDED.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "[remote-test] host_datachannel_receive_pump_ended monotonic_ms={}",
                    shocking_vrc_rtc::monotonic_ms()
                );
            }
            if host_process && message.contains("DataChannel close event monotonic_ms=") {
                E2E_HOST_DATACHANNEL_CLOSE_EVENTS.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "[remote-test] host_datachannel_close_event monotonic_ms={}",
                    shocking_vrc_rtc::monotonic_ms()
                );
            }
            let safe_event = [
                "[rtc] PeerConnection created",
                "[rtc] relay candidate gathered",
                "[rtc] ICE candidate gathered",
                "[rtc] ICE server candidate error",
                "[rtc] ICE connection state",
                "[rtc] ICE gathering state",
                "[rtc] ICE gathering complete: candidates",
                "[rtc] peer connection state",
                "[rtc] DataChannel",
                "[rtc] Host ICE gathering completed",
                "[rtc] Host ICE answer sent",
                "[rtc] Host received ICE offer",
                "[rtc] Host received Terminal DataChannel",
                "[rtc] Host ICE/DTLS transport connected",
                "[rtc] Terminal creating ICE offer",
                "[rtc] selected ICE pair",
                "[rtc] fresh TURN credentials received",
                "[rtc] TURN credential",
                "[rtc] TURN refresh",
                "[rtc-turn]",
                "[remote] API session phase",
                "[remote] API heartbeat",
                "[remote] API hello",
            ]
            .iter()
            .any(|prefix| message.contains(prefix));
            if safe_event {
                eprintln!("[e2e-rtc] {}", message);
            }
        }

        fn flush(&self) {}
    }

    fn init_e2e_rtc_logger() {
        if log::set_logger(&E2E_RTC_LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Debug);
        }
    }

    #[test]
    fn session_end_notice_is_bound_to_server_session_and_generation() {
        let server_epoch = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let value = super::session_ended_frame(
            server_epoch,
            super::WsSession {
                id: session_id,
                generation: 7,
            },
            crate::auth::SessionEndReason::HostKick,
        );

        assert_eq!(value["type"], "session.ended");
        assert_eq!(value["server_epoch"], server_epoch.to_string());
        assert_eq!(value["session_id"], session_id.to_string());
        assert_eq!(value["transport_generation"], 7);
        assert_eq!(value["reason"], "host_kick");
    }

    struct EmptyBackend;

    impl RemoteBackend for EmptyBackend {
        fn snapshot(&self) -> Result<SnapshotDto, BackendError> {
            Ok(empty_snapshot())
        }
        fn presets(&self) -> Result<crate::protocol::PresetCatalogDto, BackendError> {
            Ok(crate::protocol::PresetCatalogDto { entries: Vec::new() })
        }
        fn zone_presets(&self) -> Result<crate::protocol::ZonePresetCatalogDto, BackendError> {
            Ok(crate::protocol::ZonePresetCatalogDto { entries: Vec::new() })
        }
        fn logs(&self, _cursor: Option<u64>, _limit: u16) -> Result<crate::protocol::LogPageDto, BackendError> {
            Ok(crate::protocol::LogPageDto {
                cursor: "0".into(),
                next_cursor: None,
                lines: Vec::new(),
            })
        }
        fn enqueue(&self, _command: crate::RemoteCommand) -> Result<(), BackendError> {
            Ok(())
        }
    }

    struct OversizedLogsBackend;

    impl RemoteBackend for OversizedLogsBackend {
        fn snapshot(&self) -> Result<SnapshotDto, BackendError> {
            Ok(empty_snapshot())
        }
        fn presets(&self) -> Result<crate::protocol::PresetCatalogDto, BackendError> {
            Ok(crate::protocol::PresetCatalogDto { entries: Vec::new() })
        }
        fn zone_presets(&self) -> Result<crate::protocol::ZonePresetCatalogDto, BackendError> {
            Ok(crate::protocol::ZonePresetCatalogDto { entries: Vec::new() })
        }
        fn logs(&self, _cursor: Option<u64>, limit: u16) -> Result<crate::protocol::LogPageDto, BackendError> {
            Ok(crate::protocol::LogPageDto {
                cursor: "0".into(),
                next_cursor: None,
                lines: (0..limit)
                    .map(|seq| crate::protocol::LogLineDto {
                        seq: u64::from(seq) + 1,
                        text: "x".repeat(3_000),
                    })
                    .collect(),
            })
        }
        fn enqueue(&self, _command: crate::RemoteCommand) -> Result<(), BackendError> {
            Ok(())
        }
    }

    struct SnapshotUnavailableBackend;

    impl RemoteBackend for SnapshotUnavailableBackend {
        fn snapshot(&self) -> Result<SnapshotDto, BackendError> {
            Err(BackendError::new("SNAPSHOT_UNAVAILABLE", "stale snapshot fixture"))
        }
        fn presets(&self) -> Result<crate::protocol::PresetCatalogDto, BackendError> {
            Ok(crate::protocol::PresetCatalogDto { entries: Vec::new() })
        }
        fn zone_presets(&self) -> Result<crate::protocol::ZonePresetCatalogDto, BackendError> {
            Ok(crate::protocol::ZonePresetCatalogDto { entries: Vec::new() })
        }
        fn logs(&self, _cursor: Option<u64>, _limit: u16) -> Result<crate::protocol::LogPageDto, BackendError> {
            Ok(crate::protocol::LogPageDto {
                cursor: "0".into(),
                next_cursor: None,
                lines: Vec::new(),
            })
        }
        fn enqueue(&self, _command: crate::RemoteCommand) -> Result<(), BackendError> {
            Ok(())
        }
    }

    struct AcceptanceBackend {
        commands: std::sync::Mutex<Vec<RemoteCommand>>,
        command_seen: Notify,
        log_store: shocking_vrc_diagnostics::PersistentLogStore,
        log_exports: Mutex<HashMap<(Uuid, u64, Uuid), shocking_vrc_diagnostics::LogSnapshot>>,
    }

    impl AcceptanceBackend {
        fn new() -> Self {
            let log_root = std::env::temp_dir().join(format!("shockingvrc-host-export-e2e-{}", Uuid::new_v4()));
            let log_store = shocking_vrc_diagnostics::PersistentLogStore::open(&log_root)
                .expect("open isolated Host E2E diagnostic log store");
            for index in 0..=1_200_u64 {
                let elapsed_seconds = index * 10;
                let record = shocking_vrc_diagnostics::DiagnosticLogRecord {
                    timestamp_utc: Some(format!(
                        "2026-10-23T{:02}:{:02}:{:02}.000Z",
                        elapsed_seconds / 3_600,
                        (elapsed_seconds / 60) % 60,
                        elapsed_seconds % 60,
                    )),
                    timestamp_local: format!(
                        "2026-10-23T{:02}:{:02}:{:02}.000+00:00",
                        elapsed_seconds / 3_600,
                        (elapsed_seconds / 60) % 60,
                        elapsed_seconds % 60,
                    ),
                    monotonic_ms: elapsed_seconds * 1_000,
                    level: "INFO".into(),
                    target: "e2e.host".into(),
                    message: format!("generated persistent host record {index}: {}", "x".repeat(180)),
                    source_timestamp: None,
                    imported_at_utc: None,
                };
                assert!(log_store.append(record), "append generated Host E2E record");
            }
            log_store.flush().expect("flush generated Host E2E records");
            Self {
                commands: std::sync::Mutex::new(Vec::new()),
                command_seen: Notify::new(),
                log_store,
                log_exports: Mutex::new(HashMap::new()),
            }
        }

        fn active_log_exports(&self) -> usize {
            self.log_exports.lock().expect("Host E2E export snapshots").len()
        }
    }

    impl RemoteBackend for AcceptanceBackend {
        fn snapshot(&self) -> Result<SnapshotDto, BackendError> {
            Ok(empty_snapshot())
        }
        fn presets(&self) -> Result<crate::protocol::PresetCatalogDto, BackendError> {
            Ok(crate::protocol::PresetCatalogDto { entries: Vec::new() })
        }
        fn zone_presets(&self) -> Result<crate::protocol::ZonePresetCatalogDto, BackendError> {
            Ok(crate::protocol::ZonePresetCatalogDto { entries: Vec::new() })
        }
        fn logs(&self, cursor: Option<u64>, limit: u16) -> Result<crate::protocol::LogPageDto, BackendError> {
            let records = self
                .log_store
                .tail_records(2_000)
                .map_err(|_| BackendError::new("LOG_UNAVAILABLE", "Host test log store is unavailable"))?;
            let lines = records
                .into_iter()
                .map(|record| {
                    format!(
                        "[{}] {} [{}] {}",
                        record.level, record.timestamp_local, record.target, record.message
                    )
                })
                .collect::<Vec<_>>();
            let first_sequence = 1_u64;
            let last_sequence = lines.len() as u64;
            let max_start = last_sequence
                .saturating_sub(u64::from(limit))
                .saturating_add(1)
                .max(first_sequence);
            let start_sequence = cursor
                .map(|value| value.saturating_add(1))
                .unwrap_or(max_start)
                .max(first_sequence);
            let offset = start_sequence.saturating_sub(first_sequence) as usize;
            let selected = lines
                .iter()
                .skip(offset)
                .take(usize::from(limit))
                .enumerate()
                .map(|(index, text)| crate::protocol::LogLineDto {
                    seq: start_sequence + index as u64,
                    text: text.clone(),
                })
                .collect::<Vec<_>>();
            let last_returned = selected
                .last()
                .map(|line| line.seq)
                .unwrap_or(start_sequence.saturating_sub(1));
            Ok(crate::protocol::LogPageDto {
                cursor: start_sequence.saturating_sub(1).to_string(),
                next_cursor: (last_returned < last_sequence).then(|| last_returned.to_string()),
                lines: selected,
            })
        }
        fn begin_log_export(
            &self,
            session_id: Uuid,
            generation: u64,
            range: Option<shocking_vrc_diagnostics::LogTimeRange>,
        ) -> Result<shocking_vrc_diagnostics::LogSnapshotMetadata, BackendError> {
            let snapshot = self
                .log_store
                .snapshot_filtered(range.as_ref())
                .map_err(|error| BackendError::new("LOG_EXPORT_FAILED", error.to_string()))?;
            let metadata = snapshot.metadata().clone();
            self.log_exports
                .lock()
                .map_err(|_| BackendError::new("SERVER_BUSY", "Host E2E export store is unavailable"))?
                .insert((session_id, generation, metadata.snapshot_id), snapshot);
            Ok(metadata)
        }
        fn read_log_export_chunk(
            &self,
            session_id: Uuid,
            generation: u64,
            snapshot_id: Uuid,
            offset: u64,
            max_bytes: usize,
        ) -> Result<shocking_vrc_diagnostics::LogExportChunk, BackendError> {
            self.log_exports
                .lock()
                .map_err(|_| BackendError::new("SERVER_BUSY", "Host E2E export store is unavailable"))?
                .get_mut(&(session_id, generation, snapshot_id))
                .ok_or_else(|| BackendError::new("LOG_EXPORT_EXPIRED", "Host E2E snapshot expired"))?
                .next_chunk(offset, max_bytes)
                .map_err(|error| BackendError::new("LOG_EXPORT_INVALID_OFFSET", error.to_string()))
        }
        fn cancel_log_export(&self, session_id: Uuid, generation: u64, snapshot_id: Uuid) {
            if let Ok(mut exports) = self.log_exports.lock() {
                exports.remove(&(session_id, generation, snapshot_id));
            }
        }
        fn end_log_export_session(&self, session_id: Uuid, generation: u64) {
            if let Ok(mut exports) = self.log_exports.lock() {
                exports.retain(|(owner, owner_generation, _), _| {
                    *owner != session_id || *owner_generation != generation
                });
            }
        }
        fn enqueue(&self, command: RemoteCommand) -> Result<(), BackendError> {
            self.commands
                .lock()
                .map_err(|_| BackendError::new("SERVER_BUSY", "test queue unavailable"))?
                .push(command);
            self.command_seen.notify_one();
            Ok(())
        }
    }

    fn empty_snapshot() -> SnapshotDto {
        use crate::protocol::*;
        let channel_config = || ChannelConfigDto {
            zones: Vec::new(),
            frequency: [0; 4],
            intensity: [0; 4],
            limits: LimitsDto { min: 0, max: 200 },
            aggregation: "mean".into(),
            freq_modulation: [None, None, None, None],
            intensity_modulation: [None, None, None, None],
        };
        let channel_status = || ChannelStatusDto {
            raw_level: 0.0,
            strength: 0,
            frequency: [0; 4],
            active_zones: Vec::new(),
            kinematics: KinematicsDto {
                depth: 0.0,
                speed: 0.0,
                acc: 0.0,
                recoil: 0.0,
            },
        };
        SnapshotDto {
            bridge_revision: 1,
            configuration_state: None,
            host_display_name: None,
            active_device: 0,
            active_device_mac: None,
            devices: Vec::new(),
            vrchat: VrchatDto {
                connected: false,
                osc_port: 0,
                oscquery_port: None,
            },
            avatar_zones: Vec::new(),
            config: ConfigDto {
                channel_a: channel_config(),
                channel_b: channel_config(),
                ukf: UkfDto {
                    q: 0.0,
                    r: 0.0,
                    alpha: 0.0,
                    beta: 0.0,
                    kappa: 0.0,
                },
                norms: MotionNormsDto {
                    speed: 0.0,
                    acc: 0.0,
                    recoil: 0.0,
                },
            },
            status: StatusDto {
                device_connected: false,
                channel_a: channel_status(),
                channel_b: channel_status(),
            },
            alarm: AlarmDto {
                config: AlarmConfigDto {
                    enabled: false,
                    hour: 0,
                    minute: 0,
                    channels: "A".into(),
                    start_strength: 0,
                    peak_strength: 0,
                    ramp_secs: 0,
                    max_duration_secs: 0,
                    repeats: 0,
                    pulse_on_ms: 0,
                    pulse_off_ms: 0,
                    snooze_mins: 0,
                },
                status: AlarmStatusDto {
                    phase: "idle".into(),
                    strength: 0,
                    elapsed_secs: 0,
                    auto_stop_in_secs: 0,
                    snooze_left_secs: 0,
                    next_fire_in_mins: 0,
                    attempt: 0,
                    retrying: false,
                    test: false,
                },
            },
            prefs: None,
            remote_policy: RemotePolicyDto {
                remote_strength: RemoteStrengthDto {
                    enabled: false,
                    max_by_channel: BTreeMap::from([("A".into(), None), ("B".into(), None)]),
                    scope: "power_limits_only",
                },
                max_remote_sessions: 1,
            },
        }
    }

    fn dispatch_test_state(policy: crate::settings::RemoteStrengthPolicy) -> ApiState {
        dispatch_test_state_with_backend(policy, Arc::new(EmptyBackend))
    }

    fn dispatch_test_state_with_backend(
        policy: crate::settings::RemoteStrengthPolicy,
        backend: Arc<dyn RemoteBackend>,
    ) -> ApiState {
        let host_identity = Arc::new(HostIdentity::ephemeral_for_test());
        let epoch = Uuid::new_v4();
        let trust_path =
            PathBuf::from(std::env::temp_dir()).join(format!("shockingvrc-api-policy-{}.json", Uuid::new_v4()));
        let sessions = Arc::new(SessionManager::load_from_path(epoch, trust_path).unwrap());
        let mut settings = RemoteAccessSettings::default();
        settings.remote_strength = policy;
        ApiState::new(backend, sessions, host_identity, Arc::new(RwLock::new(settings)), epoch)
    }

    fn limit_command(op: &str, value: u64) -> RemoteCommand {
        RemoteCommand {
            request_id: Uuid::new_v4(),
            op: op.to_owned(),
            args: json!({ "channel": "A", "value": value }),
        }
    }

    #[test]
    fn api_dispatch_leaves_valid_0_to_200_writes_uncapped_when_policy_is_off() {
        let policy = crate::settings::RemoteStrengthPolicy {
            enabled: false,
            max_by_channel: crate::settings::ChannelCaps {
                a: Some(50),
                b: Some(60),
            },
        };
        let state = dispatch_test_state(policy);
        let session_id = Uuid::new_v4();

        for (operation, value) in [("channels.set_limit_min", 0), ("channels.set_limit_max", 200)] {
            let result = dispatch_command(&state, &session_id, limit_command(operation, value));
            assert!(result.is_ok(), "disabled policy rejected {operation}={value}: {result:?}");
        }

        let invalid = dispatch_command(&state, &session_id, limit_command("channels.set_limit_max", 201));
        assert_eq!(invalid.unwrap_err().code, "INVALID_ARGUMENT");
    }

    #[test]
    fn api_dispatch_enforces_configured_cap_when_policy_is_on() {
        let policy = crate::settings::RemoteStrengthPolicy {
            enabled: true,
            max_by_channel: crate::settings::ChannelCaps {
                a: Some(50),
                b: Some(60),
            },
        };
        let state = dispatch_test_state(policy);
        let session_id = Uuid::new_v4();

        assert!(dispatch_command(&state, &session_id, limit_command("channels.set_limit_max", 50)).is_ok());
        let rejected = dispatch_command(&state, &session_id, limit_command("channels.set_limit_max", 51)).unwrap_err();
        assert_eq!(rejected.code, "REMOTE_LIMIT_EXCEEDED");
    }

    #[test]
    fn cap_validation_does_not_depend_on_a_stale_or_unavailable_snapshot() {
        let policy = crate::settings::RemoteStrengthPolicy {
            enabled: true,
            max_by_channel: crate::settings::ChannelCaps {
                a: Some(50),
                b: Some(60),
            },
        };
        let state = dispatch_test_state_with_backend(policy, Arc::new(SnapshotUnavailableBackend));
        let session_id = Uuid::new_v4();

        let accepted = dispatch_command(&state, &session_id, limit_command("channels.set_limit_max", 50));
        assert!(
            accepted.is_ok(),
            "Worker can normalize the min/max pair without a snapshot: {accepted:?}"
        );
    }

    #[tokio::test]
    async fn device_proof_creates_active_session_and_initial_snapshot_on_datachannel() {
        let host_identity = Arc::new(HostIdentity::ephemeral_for_test());
        let epoch = Uuid::new_v4();
        let trust_path =
            PathBuf::from(std::env::temp_dir()).join(format!("shockingvrc-api-auth-{}.json", Uuid::new_v4()));
        let sessions =
            Arc::new(SessionManager::load_from_path(epoch, trust_path.clone()).expect("test SessionManager"));
        let state = ApiState::new(
            Arc::new(EmptyBackend),
            Arc::clone(&sessions),
            Arc::clone(&host_identity),
            Arc::new(RwLock::new(RemoteAccessSettings::default())),
            epoch,
        );
        let peer: SocketAddr = "127.0.0.1:4318".parse().unwrap();
        let (outbound, mut inbound) = tokio::sync::mpsc::channel(16);
        let mut sender = ApiSender::DataChannel(outbound, ApiTrafficRecorder::default());
        let mut session = None;
        let mut sequence = 0;
        let mut telemetry_negotiated = false;

        let terminal_pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let terminal_key = Ed25519KeyPair::from_pkcs8(terminal_pkcs8.as_ref()).unwrap();
        let terminal_public: [u8; 32] = terminal_key.public_key().as_ref().try_into().unwrap();
        let device_id = device_id_for_public_key(&terminal_public).unwrap();
        let invitation_id = Uuid::new_v4();
        let raw_secret = [17_u8; 32];
        let pairing_secret = URL_SAFE_NO_PAD.encode(raw_secret);
        let pairing_secret_hash: [u8; 32] = Sha256::digest(raw_secret).into();
        sessions.create_invitation(invitation_id, pairing_secret_hash).unwrap();
        let request_id = Uuid::new_v4();
        let begin = json!({
            "type":"auth.device.begin", "protocol":PROTOCOL_VERSION, "request_id":request_id,
            "device_id":device_id, "public_key":URL_SAFE_NO_PAD.encode(terminal_public), "invitation_id":invitation_id,
        })
        .to_string();
        assert!(
            handle_ws_message(
                &state,
                &mut sender,
                begin.as_bytes(),
                peer,
                &mut session,
                &mut sequence,
                &mut telemetry_negotiated,
                None
            )
            .await
        );
        let challenge: Value = serde_json::from_slice(&inbound.recv().await.unwrap()).unwrap();
        assert_eq!(challenge["type"], "auth.device.challenge");
        let challenge_id = Uuid::parse_str(challenge["challenge_id"].as_str().unwrap()).unwrap();
        let nonce: [u8; 32] = URL_SAFE_NO_PAD
            .decode(challenge["server_nonce"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let transcript = device_auth_transcript(
            host_identity.server_id(),
            epoch,
            request_id,
            challenge_id,
            nonce,
            &device_id,
            terminal_public,
            Some(invitation_id),
            Some(pairing_secret_hash),
        );
        let signature = URL_SAFE_NO_PAD.encode(terminal_key.sign(&transcript).as_ref());
        let prove = json!({
            "type":"auth.device.prove", "protocol":PROTOCOL_VERSION, "request_id":request_id,
            "challenge_id":challenge_id, "device_id":device_id,
            "public_key":URL_SAFE_NO_PAD.encode(terminal_public), "signature":signature,
            "invitation_id":invitation_id, "pairing_secret":pairing_secret,
        })
        .to_string();
        assert!(
            handle_ws_message(
                &state,
                &mut sender,
                prove.as_bytes(),
                peer,
                &mut session,
                &mut sequence,
                &mut telemetry_negotiated,
                None
            )
            .await
        );
        let auth_result: Value = serde_json::from_slice(&inbound.recv().await.unwrap()).unwrap();
        let snapshot: Value = serde_json::from_slice(&inbound.recv().await.unwrap()).unwrap();
        assert_eq!(auth_result["type"], "auth.result");
        assert_eq!(auth_result["ok"], true);
        assert_eq!(auth_result["session_state"], "ACTIVE");
        assert!(auth_result["device_grant"].as_str().unwrap().starts_with("dg1."));
        assert_eq!(snapshot["type"], "snapshot");
        assert_eq!(snapshot["state"]["bridge_revision"], 1);
        assert!(session.is_some());
        assert_eq!(sessions.trusted_terminals().len(), 1);

        let subscribe_id = Uuid::new_v4();
        let subscribe = json!({
            "type":"telemetry.subscribe", "protocol":PROTOCOL_VERSION, "request_id":subscribe_id,
        })
        .to_string();
        assert!(
            handle_ws_message(
                &state,
                &mut sender,
                subscribe.as_bytes(),
                peer,
                &mut session,
                &mut sequence,
                &mut telemetry_negotiated,
                None,
            )
            .await
        );
        let subscribed: Value = serde_json::from_slice(&inbound.recv().await.unwrap()).unwrap();
        let keyframe: Value = serde_json::from_slice(&inbound.recv().await.unwrap()).unwrap();
        assert_eq!(subscribed["type"], "telemetry.subscribed");
        assert_eq!(subscribed["feature"], "telemetry.replace.v1");
        assert_eq!(subscribed["request_id"], subscribe_id.to_string());
        assert_eq!(keyframe["type"], "snapshot");
        assert_eq!(keyframe["seq"], 2);
        assert!(telemetry_negotiated);

        let oversized_state = ApiState::new(
            Arc::new(OversizedLogsBackend),
            Arc::clone(&sessions),
            Arc::clone(&host_identity),
            Arc::new(RwLock::new(RemoteAccessSettings::default())),
            epoch,
        );
        let logs_request = Uuid::new_v4();
        let logs = json!({
            "type":"logs.get", "protocol":PROTOCOL_VERSION, "request_id":logs_request,
            "cursor":null, "limit":100,
        })
        .to_string();
        assert!(
            handle_ws_message(
                &oversized_state,
                &mut sender,
                logs.as_bytes(),
                peer,
                &mut session,
                &mut sequence,
                &mut telemetry_negotiated,
                None,
            )
            .await,
            "an oversized Host log page must return an error without ending the API session"
        );
        let logs_error: Value = serde_json::from_slice(&inbound.recv().await.unwrap()).unwrap();
        assert_eq!(logs_error["type"], "error");
        assert_eq!(logs_error["request_id"], logs_request.to_string());

        drop(state);
        drop(sessions);
        let _ = std::fs::remove_file(trust_path);
    }

    #[tokio::test]
    async fn datachannel_host_kick_waits_for_the_bound_terminal_ack() {
        let host_identity = Arc::new(HostIdentity::ephemeral_for_test());
        let epoch = Uuid::new_v4();
        let trust_path =
            PathBuf::from(std::env::temp_dir()).join(format!("shockingvrc-api-kick-{}.json", Uuid::new_v4()));
        let sessions =
            Arc::new(SessionManager::load_from_path(epoch, trust_path.clone()).expect("test SessionManager"));
        let state = ApiState::new(
            Arc::new(EmptyBackend),
            Arc::clone(&sessions),
            Arc::clone(&host_identity),
            Arc::new(RwLock::new(RemoteAccessSettings::default())),
            epoch,
        );
        let (terminal_outbound, host_inbound) = tokio::sync::mpsc::channel(16);
        let (host_outbound, mut terminal_inbound) = tokio::sync::mpsc::channel(16);
        let room_id = Uuid::new_v4().to_string();
        let api_room_id = room_id.clone();
        let mut api_task = tokio::spawn(async move {
            data_channel_session_with_sender(
                state,
                &api_room_id,
                host_inbound,
                ApiSender::DataChannel(host_outbound, ApiTrafficRecorder::default()),
            )
            .await
        });

        let hello_request = Uuid::new_v4();
        let client_nonce = [0x39_u8; 32];
        send_test_api_frame(
            &terminal_outbound,
            json!({
                "type":"hello", "protocol":PROTOCOL_VERSION, "request_id":hello_request,
                "client":"ShockingVRC local test", "client_version":"test",
                "client_nonce":URL_SAFE_NO_PAD.encode(client_nonce), "transport":"webrtc-datachannel",
            }),
        )
        .await;
        let hello = recv_api_frame(&mut terminal_inbound, "hello.ok", Some(hello_request)).await;
        assert_eq!(hello["server_id"], host_identity.server_id());

        let terminal_pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let terminal_key = Ed25519KeyPair::from_pkcs8(terminal_pkcs8.as_ref()).unwrap();
        let terminal_public: [u8; 32] = terminal_key.public_key().as_ref().try_into().unwrap();
        let device_id = device_id_for_public_key(&terminal_public).unwrap();
        let invitation_id = Uuid::new_v4();
        let raw_secret = [23_u8; 32];
        let pairing_secret = URL_SAFE_NO_PAD.encode(raw_secret);
        let pairing_secret_hash: [u8; 32] = Sha256::digest(raw_secret).into();
        sessions.create_invitation(invitation_id, pairing_secret_hash).unwrap();
        let request_id = Uuid::new_v4();
        send_test_api_frame(&terminal_outbound, json!({
            "type":"auth.device.begin", "protocol":PROTOCOL_VERSION, "request_id":request_id,
            "device_id":device_id, "public_key":URL_SAFE_NO_PAD.encode(terminal_public), "invitation_id":invitation_id,
        })).await;
        let challenge = recv_api_frame(&mut terminal_inbound, "auth.device.challenge", Some(request_id)).await;
        let challenge_id = Uuid::parse_str(challenge["challenge_id"].as_str().unwrap()).unwrap();
        let nonce: [u8; 32] = URL_SAFE_NO_PAD
            .decode(challenge["server_nonce"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let transcript = device_auth_transcript(
            host_identity.server_id(),
            epoch,
            request_id,
            challenge_id,
            nonce,
            &device_id,
            terminal_public,
            Some(invitation_id),
            Some(pairing_secret_hash),
        );
        let signature = URL_SAFE_NO_PAD.encode(terminal_key.sign(&transcript).as_ref());
        send_test_api_frame(
            &terminal_outbound,
            json!({
                "type":"auth.device.prove", "protocol":PROTOCOL_VERSION, "request_id":request_id,
                "challenge_id":challenge_id, "device_id":device_id,
                "public_key":URL_SAFE_NO_PAD.encode(terminal_public), "signature":signature,
                "invitation_id":invitation_id, "pairing_secret":pairing_secret,
            }),
        )
        .await;
        let auth = recv_api_frame(&mut terminal_inbound, "auth.result", Some(request_id)).await;
        assert_eq!(auth["ok"], true);
        let _ = recv_api_frame(&mut terminal_inbound, "snapshot", None).await;

        sessions.terminate_current(SessionEndReason::HostKick);
        let ended = recv_api_frame(&mut terminal_inbound, "session.ended", None).await;
        assert_eq!(ended["reason"], "host_kick");
        assert_eq!(ended["server_epoch"], epoch.to_string());
        assert_eq!(ended["session_id"], auth["session_id"]);
        assert_eq!(ended["transport_generation"], auth["transport_generation"]);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut api_task)
                .await
                .is_err(),
            "Host must keep the signaling DataChannel open briefly for the Terminal's end acknowledgement"
        );

        send_test_api_frame(
            &terminal_outbound,
            json!({
                "type":"session.ended.ack", "protocol":PROTOCOL_VERSION,
                "server_epoch":epoch, "session_id":auth["session_id"],
                "transport_generation":auth["transport_generation"].as_u64().unwrap() + 1,
            }),
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut api_task)
                .await
                .is_err(),
            "an acknowledgement from another transport generation must not close the Host API session"
        );

        send_test_api_frame(
            &terminal_outbound,
            json!({
                "type":"session.ended.ack", "protocol":PROTOCOL_VERSION,
                "server_epoch":epoch, "session_id":auth["session_id"],
                "transport_generation":auth["transport_generation"],
            }),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(1), api_task)
            .await
            .expect("Host ended the session after the matching acknowledgement")
            .expect("DataChannel API task completed");
        let _ = std::fs::remove_file(trust_path);
    }

    #[tokio::test]
    async fn datachannel_session_end_ack_wait_is_bounded_when_the_terminal_is_old_or_unreachable() {
        let (_terminal_outbound, mut receiver) = tokio::sync::mpsc::channel(1);
        let started = tokio::time::Instant::now();
        let acknowledged = tokio::time::timeout(
            Duration::from_millis(1_500),
            wait_for_data_channel_session_end_ack(
                &mut receiver,
                Uuid::new_v4(),
                WsSession {
                    id: Uuid::new_v4(),
                    generation: 1,
                },
            ),
        )
        .await
        .expect("Host must not wait indefinitely for old or unreachable Terminals");

        assert!(!acknowledged);
        assert!(started.elapsed() < Duration::from_millis(1_500));
    }

    /// Separate Host and Terminal test processes exercise the complete local
    /// traversal path and the production Remote API DataChannel adapter. The
    /// Rendezvous fixture has no Direct listener or TURN service, and the
    /// Terminal joins with no Direct endpoint, so a Direct WSS path cannot pass.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn localhost_host_terminal_datachannel_remote_api_acceptance() {
        run_local_host_terminal_e2e(false).await;
    }

    /// Isolated real-coturn proof. Run explicitly with the disposable fixture
    /// variables set; this deliberately exceeds two 600-second REST expiries.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires the disposable local coturn REST-auth fixture"]
    async fn continuous_turn_relay_survives_rest_credential_expiry() {
        run_local_host_terminal_e2e(true).await;
    }

    async fn run_local_host_terminal_e2e(long_turn: bool) {
        if let Ok(role) = std::env::var("SHOCKINGVRC_REMOTE_E2E_ROLE") {
            init_e2e_rtc_logger();
            match role.as_str() {
                "host" => run_remote_e2e_host().await,
                "terminal" => run_remote_e2e_terminal().await,
                _ => panic!("unknown local E2E child role"),
            }
            return;
        }

        let identity = HostIdentity::ephemeral_for_test();
        let host_id = identity.server_id().to_owned();
        let host_fingerprint = identity.fingerprint().to_owned();
        let host_public_key = URL_SAFE_NO_PAD.encode(identity.public_key());
        let host_private_key = URL_SAFE_NO_PAD.encode(identity.private_key_bytes_for_test());
        let server_epoch = Uuid::new_v4();
        let invitation_id = Uuid::new_v4();
        let pairing_secret = URL_SAFE_NO_PAD.encode([0x4d_u8; 32]);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let route_token = create_route_token(E2E_REGISTRATION_KEY.as_bytes(), &host_id, invitation_id, now + 900);

        let (turn, turn_secret) = if long_turn {
            let turn_uri = std::env::var("SHOCKINGVRC_REMOTE_E2E_TURN_URI")
                .expect("isolated coturn URI is required for the long TURN test");
            let secret = std::env::var("SHOCKINGVRC_REMOTE_E2E_TURN_SECRET")
                .expect("isolated coturn REST secret is required for the long TURN test");
            (
                TurnConfig {
                    enabled: true,
                    uris: vec![turn_uri],
                    realm: "shockingvrc-continuous-turn-test".into(),
                    secret_file: String::new(),
                    credential_ttl_seconds: 600,
                },
                Some(secret.into_bytes()),
            )
        } else {
            (
                TurnConfig {
                    enabled: false,
                    uris: Vec::new(),
                    realm: String::new(),
                    secret_file: String::new(),
                    credential_ttl_seconds: 60,
                },
                None,
            )
        };

        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve local Rendezvous port");
        let address = listener.local_addr().expect("Rendezvous loopback address");
        drop(listener);
        let rendezvous_url = format!("ws://{address}/ws");
        let config = LoadedConfig {
            server: ServerConfig {
                bind: address.to_string(),
                public_url: String::new(),
                health_path: "/healthz".into(),
                signal_path: "/ws".into(),
                version_path: "/version".into(),
                tls: TlsConfig::default(),
            },
            rooms: RoomConfig {
                ttl_seconds: if long_turn { 1_800 } else { 90 },
                max_candidates_per_peer: 64,
                max_signal_frame_bytes: 64 * 1024,
            },
            turn,
            stun: StunConfig::default(),
            hosts: BTreeMap::from([(
                host_id.clone(),
                HostAuthorization {
                    registration_key: E2E_REGISTRATION_KEY.into(),
                },
            )])
            .into_iter()
            .collect(),
            turn_secret,
            config_dir: std::env::temp_dir(),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let rendezvous = tokio::spawn(run_with_shutdown(config, async move {
            let _ = shutdown_rx.await;
        }));

        let ready_path = std::env::temp_dir().join(format!("shockingvrc-remote-e2e-{}.ready", Uuid::new_v4()));
        let terminal_done_path = std::env::temp_dir().join(format!("shockingvrc-remote-e2e-{}.done", Uuid::new_v4()));
        let host_verified_path =
            std::env::temp_dir().join(format!("shockingvrc-remote-e2e-{}.verified", Uuid::new_v4()));
        let progress_path =
            std::env::temp_dir().join(format!("shockingvrc-remote-e2e-{}.progress.json", Uuid::new_v4()));
        let trust_path = std::env::temp_dir().join(format!("shockingvrc-remote-e2e-{}.json", Uuid::new_v4()));
        let _cleanup = RemoveTestFiles(vec![
            ready_path.clone(),
            terminal_done_path.clone(),
            host_verified_path.clone(),
            progress_path.clone(),
            trust_path,
        ]);
        let mut host = ChildGuard(spawn_e2e_child(
            "host",
            &rendezvous_url,
            &host_id,
            &host_fingerprint,
            &host_public_key,
            &host_private_key,
            server_epoch,
            invitation_id,
            &pairing_secret,
            &route_token,
            &ready_path,
            &terminal_done_path,
            &host_verified_path,
            &progress_path,
            long_turn,
        ));

        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if ready_path.exists() {
                    break;
                }
                if let Some(status) = host.0.try_wait().expect("poll Host E2E child") {
                    panic!("Host E2E child exited before Rendezvous registration: {status}");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("Host child registered with the local Rendezvous");

        let terminal = tokio::task::spawn_blocking({
            let rendezvous_url = rendezvous_url.clone();
            let host_id = host_id.clone();
            let host_fingerprint = host_fingerprint.clone();
            let host_public_key = host_public_key.clone();
            let ready_path = ready_path.clone();
            let terminal_done_path = terminal_done_path.clone();
            let host_verified_path = host_verified_path.clone();
            let pairing_secret = pairing_secret.clone();
            let route_token = route_token.clone();
            move || {
                Command::new(std::env::current_exe().expect("current remote test executable"))
                    .args(["--exact", E2E_TEST_NAME, "--nocapture"])
                    .env("SHOCKINGVRC_REMOTE_E2E_ROLE", "terminal")
                    .env("SHOCKINGVRC_REMOTE_E2E_RENDEZVOUS", rendezvous_url)
                    .env("SHOCKINGVRC_REMOTE_E2E_HOST_ID", host_id)
                    .env("SHOCKINGVRC_REMOTE_E2E_HOST_PIN", host_fingerprint)
                    .env("SHOCKINGVRC_REMOTE_E2E_HOST_PUBLIC", host_public_key)
                    .env("SHOCKINGVRC_REMOTE_E2E_SERVER_EPOCH", server_epoch.to_string())
                    .env("SHOCKINGVRC_REMOTE_E2E_INVITATION", invitation_id.to_string())
                    .env("SHOCKINGVRC_REMOTE_E2E_PAIRING_SECRET", pairing_secret)
                    .env("SHOCKINGVRC_REMOTE_E2E_ROUTE_TOKEN", route_token)
                    .env("SHOCKINGVRC_REMOTE_E2E_TERMINAL_DONE", terminal_done_path)
                    .env("SHOCKINGVRC_REMOTE_E2E_HOST_VERIFIED", host_verified_path)
                    .env("SHOCKINGVRC_REMOTE_E2E_PROGRESS", progress_path)
                    .env("SHOCKINGVRC_REMOTE_E2E_LONG_TURN", long_turn.to_string())
                    .env("SHOCKINGVRC_REMOTE_E2E_READY", ready_path)
                    .env_remove("SHOCKINGVRC_REMOTE_E2E_TURN_SECRET")
                    .env_remove("SHOCKINGVRC_REMOTE_E2E_TURN_URI")
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .status()
            }
        })
        .await
        .expect("join Terminal test process")
        .expect("run Terminal test process");
        assert!(terminal.success(), "Terminal E2E child failed: {terminal}");

        let host_status = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(status) = host.0.try_wait().expect("poll Host E2E child") {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("Host completed API operation and revoke acceptance");
        assert!(host_status.success(), "Host E2E child failed: {host_status}");

        let _ = shutdown_tx.send(());
        rendezvous
            .await
            .expect("join local Rendezvous")
            .expect("local Rendezvous shutdown");
    }

    fn spawn_e2e_child(
        role: &str,
        rendezvous_url: &str,
        host_id: &str,
        host_pin: &str,
        host_public: &str,
        host_private: &str,
        server_epoch: Uuid,
        invitation_id: Uuid,
        pairing_secret: &str,
        route_token: &str,
        ready_path: &PathBuf,
        terminal_done_path: &PathBuf,
        host_verified_path: &PathBuf,
        progress_path: &PathBuf,
        long_turn: bool,
    ) -> Child {
        Command::new(std::env::current_exe().expect("current remote test executable"))
            .args(["--exact", E2E_TEST_NAME, "--nocapture"])
            .env("SHOCKINGVRC_REMOTE_E2E_ROLE", role)
            .env("SHOCKINGVRC_REMOTE_E2E_RENDEZVOUS", rendezvous_url)
            .env("SHOCKINGVRC_REMOTE_E2E_HOST_ID", host_id)
            .env("SHOCKINGVRC_REMOTE_E2E_HOST_PIN", host_pin)
            .env("SHOCKINGVRC_REMOTE_E2E_HOST_PUBLIC", host_public)
            .env("SHOCKINGVRC_REMOTE_E2E_HOST_PRIVATE", host_private)
            .env("SHOCKINGVRC_REMOTE_E2E_REGISTRATION_KEY", E2E_REGISTRATION_KEY)
            .env("SHOCKINGVRC_REMOTE_E2E_SERVER_EPOCH", server_epoch.to_string())
            .env("SHOCKINGVRC_REMOTE_E2E_INVITATION", invitation_id.to_string())
            .env("SHOCKINGVRC_REMOTE_E2E_PAIRING_SECRET", pairing_secret)
            .env("SHOCKINGVRC_REMOTE_E2E_ROUTE_TOKEN", route_token)
            .env("SHOCKINGVRC_REMOTE_E2E_READY", ready_path)
            .env("SHOCKINGVRC_REMOTE_E2E_TERMINAL_DONE", terminal_done_path)
            .env("SHOCKINGVRC_REMOTE_E2E_HOST_VERIFIED", host_verified_path)
            .env("SHOCKINGVRC_REMOTE_E2E_PROGRESS", progress_path)
            .env("SHOCKINGVRC_REMOTE_E2E_LONG_TURN", long_turn.to_string())
            .env_remove("SHOCKINGVRC_REMOTE_E2E_TURN_SECRET")
            .env_remove("SHOCKINGVRC_REMOTE_E2E_TURN_URI")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn E2E child")
    }

    async fn run_remote_e2e_host() {
        let host_id = std::env::var("SHOCKINGVRC_REMOTE_E2E_HOST_ID").unwrap();
        let key = URL_SAFE_NO_PAD
            .decode(std::env::var("SHOCKINGVRC_REMOTE_E2E_HOST_PRIVATE").unwrap())
            .unwrap();
        let identity = Arc::new(HostIdentity::from_private_key_for_test(key).expect("rebuild ephemeral Host identity"));
        assert_eq!(identity.server_id(), host_id);
        let server_epoch = Uuid::parse_str(&std::env::var("SHOCKINGVRC_REMOTE_E2E_SERVER_EPOCH").unwrap()).unwrap();
        let invitation_id = Uuid::parse_str(&std::env::var("SHOCKINGVRC_REMOTE_E2E_INVITATION").unwrap()).unwrap();
        let raw_secret = URL_SAFE_NO_PAD
            .decode(std::env::var("SHOCKINGVRC_REMOTE_E2E_PAIRING_SECRET").unwrap())
            .unwrap();
        let pairing_secret_hash: [u8; 32] = Sha256::digest(&raw_secret).into();
        let trust_path =
            PathBuf::from(std::env::temp_dir()).join(format!("shockingvrc-remote-child-{}.json", Uuid::new_v4()));
        let sessions =
            Arc::new(SessionManager::load_from_path(server_epoch, trust_path.clone()).expect("Host sessions"));
        sessions
            .create_invitation(invitation_id, pairing_secret_hash)
            .expect("single-use pairing invitation");
        let backend = Arc::new(AcceptanceBackend::new());
        let log_root = backend.log_store.root().to_path_buf();
        if long_turn_e2e_enabled() {
            run_remote_e2e_host_through_controller(identity, sessions, backend, trust_path).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = fs::remove_dir_all(log_root);
            return;
        }
        let state = ApiState::new(
            backend.clone(),
            Arc::clone(&sessions),
            Arc::clone(&identity),
            Arc::new(RwLock::new(RemoteAccessSettings::default())),
            server_epoch,
        );
        let mut signaling = register_host(
            &std::env::var("SHOCKINGVRC_REMOTE_E2E_RENDEZVOUS").unwrap(),
            identity.as_ref(),
            Zeroizing::new(std::env::var("SHOCKINGVRC_REMOTE_E2E_REGISTRATION_KEY").unwrap()),
            server_epoch,
        )
        .await
        .expect("Host Rendezvous registration");
        fs::write(std::env::var("SHOCKINGVRC_REMOTE_E2E_READY").unwrap(), "registered").expect("Host ready marker");
        let udp_addrs = if long_turn_e2e_enabled() {
            vec!["0.0.0.0:0".into()]
        } else {
            vec!["127.0.0.1:0".into()]
        };
        let mut rtc = tokio::time::timeout(
            Duration::from_secs(90),
            accept_host_room(&mut signaling, identity.as_ref(), &[], udp_addrs),
        )
        .await
        .expect("Host RTC room timeout")
        .expect("Host traversal DataChannel session");
        assert!(
            rtc.peer.selected_candidate_pair_label().await.is_some(),
            "RTC DataChannel selected an ICE candidate pair"
        );
        let (inbound, outbound) = rtc.channels.take_optimized().expect("Host API DataChannel");
        let room_id = rtc.room_id.to_string();
        let api_task =
            tokio::spawn(async move { data_channel_session_optimized(state, &room_id, inbound, outbound).await });

        tokio::time::timeout(Duration::from_secs(60), backend.command_seen.notified())
            .await
            .expect("Terminal safe operation reached Host backend");
        let commands = backend.commands.lock().expect("test command queue");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].op, "modulation.clear_segment");
        assert_eq!(commands[0].args["channel"], "A");
        drop(commands);
        let terminal_done = PathBuf::from(std::env::var("SHOCKINGVRC_REMOTE_E2E_TERMINAL_DONE").unwrap());
        tokio::time::timeout(Duration::from_secs(10), async {
            while !terminal_done.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Terminal observed the accepted safe operation");
        let device_id = sessions
            .trusted_terminals()
            .first()
            .expect("paired Terminal is trusted")
            .device_id
            .clone();
        sessions
            .revoke_terminal(&device_id)
            .expect("Host revokes paired Terminal");
        assert_eq!(
            sessions.status().phase,
            SessionPhase::Free,
            "Host revocation clears the active session"
        );
        tokio::time::timeout(Duration::from_secs(10), api_task)
            .await
            .expect("DataChannel API session exits after revoke")
            .expect("API task join");
        assert_eq!(
            backend.active_log_exports(),
            0,
            "session-generation cleanup released the uncanceled Host export snapshot"
        );
        assert_eq!(sessions.status().phase, SessionPhase::Free);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if fs::read_to_string(&terminal_done).is_ok_and(|value| value == "revoked") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Terminal observed the session revoke notification");
        rtc.channels.close(&rtc.peer).await;
        rtc.peer.close().await;
        let _ = fs::remove_file(trust_path);
        drop(backend);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = fs::remove_dir_all(log_root);
    }

    async fn run_remote_e2e_host_through_controller(
        identity: Arc<HostIdentity>,
        sessions: Arc<SessionManager>,
        backend: Arc<AcceptanceBackend>,
        trust_path: PathBuf,
    ) {
        let server_epoch = Uuid::parse_str(&std::env::var("SHOCKINGVRC_REMOTE_E2E_SERVER_EPOCH").unwrap()).unwrap();
        let mut settings = RemoteAccessSettings::default();
        settings.remote_enabled = true;
        settings.traversal_enabled = true;
        settings.rendezvous_url = std::env::var("SHOCKINGVRC_REMOTE_E2E_RENDEZVOUS").unwrap();

        let controller = HostRemoteController::new_for_test(backend, settings, server_epoch);
        controller.configure_test_runtime(identity, Arc::clone(&sessions));
        controller
            .start_traversal_for_test(Zeroizing::new(E2E_REGISTRATION_KEY.to_owned()))
            .await;

        let ready_path = PathBuf::from(std::env::var("SHOCKINGVRC_REMOTE_E2E_READY").unwrap());
        let terminal_done = PathBuf::from(std::env::var("SHOCKINGVRC_REMOTE_E2E_TERMINAL_DONE").unwrap());
        let host_verified = PathBuf::from(std::env::var("SHOCKINGVRC_REMOTE_E2E_HOST_VERIFIED").unwrap());
        let progress_path = PathBuf::from(std::env::var("SHOCKINGVRC_REMOTE_E2E_PROGRESS").unwrap());
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if controller.status().traversal == TraversalState::Registered {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("production Host traversal registered with local Rendezvous");
        fs::write(&ready_path, "registered").expect("publish Host registration readiness");

        let required_duration_seconds = continuous_turn_duration_seconds();
        let deadline = Duration::from_secs(required_duration_seconds.saturating_add(120));
        let mut initial_room_id: Option<Value> = None;
        let mut initial_candidate_pair: Option<Value> = None;
        let mut active_generation: Option<u64> = None;
        tokio::time::timeout(deadline, async {
            loop {
                if fs::read_to_string(&terminal_done).is_ok_and(|value| value == "continuous-turn-ready") {
                    break;
                }

                let status = controller.status();
                assert_ne!(
                    status.traversal,
                    TraversalState::Error,
                    "production Host traversal did not fail during Force Relay"
                );
                assert!(
                    matches!(status.traversal, TraversalState::Registered | TraversalState::Active),
                    "production Host traversal remained available"
                );
                if let Ok(bytes) = fs::read(&progress_path)
                    && let Ok(progress) = serde_json::from_slice::<Value>(&bytes)
                {
                    assert_eq!(
                        status.traversal,
                        TraversalState::Active,
                        "Host controller retained the active room"
                    );
                    assert_eq!(
                        status.session,
                        SessionPhase::Active,
                        "Host API session remained ACTIVE during authenticated heartbeat traffic"
                    );
                    assert_eq!(sessions.status().phase, SessionPhase::Active);
                    let generation = progress["transport_generation"]
                        .as_u64()
                        .expect("Terminal transport generation");
                    assert_eq!(
                        sessions.status().transport_generation,
                        Some(generation),
                        "Host API generation matched Terminal progress"
                    );
                    assert!(
                        status
                            .ice_candidate_pair
                            .as_deref()
                            .is_some_and(|pair| pair.contains("RELAY/UDP")),
                        "Host controller reports a relay/UDP pair"
                    );

                    if let Some(expected) = initial_room_id.as_ref() {
                        assert_eq!(&progress["room_id"], expected, "Host observed a stable signaling room");
                        assert_eq!(
                            &progress["candidate_pair"],
                            initial_candidate_pair.as_ref().unwrap(),
                            "Terminal retained its selected relay pair"
                        );
                        assert_eq!(
                            Some(generation),
                            active_generation,
                            "Terminal retained its original transport generation"
                        );
                    } else {
                        initial_room_id = Some(progress["room_id"].clone());
                        initial_candidate_pair = Some(progress["candidate_pair"].clone());
                        active_generation = Some(generation);
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
        .await
        .expect("authenticated Force Relay test reached its active-session end marker");

        let final_progress: Value =
            serde_json::from_slice(&fs::read(&progress_path).expect("final Terminal metrics before cleanup"))
                .expect("valid final Terminal progress JSON");
        let minimum_authenticated_cycles = 400.min((required_duration_seconds / 3).max(1));
        assert!(final_progress["elapsed_seconds"].as_u64().unwrap_or_default() >= required_duration_seconds);
        assert!(final_progress["ping_count"].as_u64().unwrap_or_default() >= minimum_authenticated_cycles);
        assert!(final_progress["snapshot_count"].as_u64().unwrap_or_default() >= minimum_authenticated_cycles);
        assert!(
            final_progress["credentials_staged_count"].as_u64().unwrap_or_default() >= 3
                || required_duration_seconds < 1_800
        );
        assert_eq!(Some(&final_progress["room_id"]), initial_room_id.as_ref());
        assert_eq!(Some(&final_progress["candidate_pair"]), initial_candidate_pair.as_ref());
        assert_eq!(
            sessions.status().phase,
            SessionPhase::Active,
            "Host API session remained active until deliberate disconnect"
        );

        let host_credential_stages = E2E_HOST_TURN_CREDENTIALS_STAGED.load(Ordering::Relaxed);
        assert!(
            host_credential_stages >= 3 || required_duration_seconds < 1_800,
            "Host controller renewed credentials during the long run"
        );
        assert_eq!(
            E2E_ICE_DISCONNECTED_EVENTS.load(Ordering::Relaxed),
            0,
            "Host observed no ICE Disconnected event during active Force Relay"
        );
        fs::write(&host_verified, "host-verified-active").expect("allow Terminal's deliberate test-end disconnect");
        eprintln!(
            "[remote-test] FINAL active duration_seconds={} heartbeat_round_trips={} snapshots={} terminal_turn_credential_refreshes={} host_turn_credential_refreshes={} room_stable=true generation_stable=true selected_transport=RELAY/UDP",
            final_progress["elapsed_seconds"].as_u64().unwrap_or_default(),
            final_progress["ping_count"].as_u64().unwrap_or_default(),
            final_progress["snapshot_count"].as_u64().unwrap_or_default(),
            final_progress["credentials_staged_count"].as_u64().unwrap_or_default(),
            host_credential_stages,
        );

        let close_owner = std::env::var("SHOCKINGVRC_REMOTE_E2E_CLOSE_OWNER").unwrap_or_else(|_| "terminal".to_owned());
        assert!(matches!(close_owner.as_str(), "terminal" | "host"));
        if close_owner == "host" {
            let shutdown_started = tokio::time::Instant::now();
            tokio::time::timeout(Duration::from_secs(15), controller.stop_traversal_for_test())
                .await
                .expect("Host cancellation closed the active traversal within the existing 15-second bound");
            assert_eq!(controller.status().traversal, TraversalState::Disabled);
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if E2E_HOST_RECEIVE_PUMPS_ENDED.load(Ordering::Relaxed) > 0
                        && sessions.status().phase == SessionPhase::Free
                        && sessions.active_generation().is_none()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("Host cancellation ended the API receive pump and revoked the old active generation");
            fs::write(&host_verified, "host-initiated-close").expect("allow Terminal to verify Host-initiated closure");
            tokio::time::timeout(Duration::from_secs(15), async {
                while !fs::read_to_string(&terminal_done).is_ok_and(|value| value == "host-initiated-close-observed") {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("Terminal observed Host-initiated room closure");
            eprintln!(
                "[remote-test] SHUTDOWN host_initiated_close=true api_receive_pump_ended=true active_generation_released=true session_phase=Free controller_status=DISABLED elapsed_ms={}",
                shutdown_started.elapsed().as_millis(),
            );
            let _ = fs::remove_file(trust_path);
            return;
        }

        let shutdown_started = tokio::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let status = controller.status();
                let room_closed = E2E_HOST_ROOM_CLOSE_NOTICES.load(Ordering::Relaxed)
                    + E2E_HOST_ROOM_CLOSE_ACKS.load(Ordering::Relaxed)
                    > 0;
                let receive_pump_ended = E2E_HOST_RECEIVE_PUMPS_ENDED.load(Ordering::Relaxed) > 0;
                if room_closed
                    && receive_pump_ended
                    && status.traversal == TraversalState::Registered
                    && status.session != SessionPhase::Active
                    && sessions.active_generation().is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("Host controller received RoomClose, stopped the API receive pump and released active session authority within 15 seconds");

        let session_phase = sessions.status().phase;
        assert_eq!(
            session_phase,
            SessionPhase::Reserved,
            "disconnected session retained only its bounded resume reservation"
        );
        assert!(
            sessions.active_generation().is_none(),
            "old transport generation no longer has active authority"
        );
        let datachannel_close_events = E2E_HOST_DATACHANNEL_CLOSE_EVENTS.load(Ordering::Relaxed);
        let room_close_notices = E2E_HOST_ROOM_CLOSE_NOTICES.load(Ordering::Relaxed);
        let room_close_acks = E2E_HOST_ROOM_CLOSE_ACKS.load(Ordering::Relaxed);
        let receive_pump_ends = E2E_HOST_RECEIVE_PUMPS_ENDED.load(Ordering::Relaxed);
        eprintln!(
            "[remote-test] SHUTDOWN terminal_disconnect=true room_close_notices={room_close_notices} room_close_acknowledgements={room_close_acks} receive_pump_ends={receive_pump_ends} datachannel_close_events={datachannel_close_events} active_generation_released=true session_phase={session_phase:?} elapsed_ms={}",
            shutdown_started.elapsed().as_millis(),
        );

        tokio::time::timeout(Duration::from_secs(15), controller.stop_traversal_for_test())
            .await
            .expect("Host traversal worker stopped within the existing 15-second test bound");
        assert_eq!(
            controller.status().traversal,
            TraversalState::Disabled,
            "Host traversal worker is fully stopped"
        );
        eprintln!("[remote-test] SHUTDOWN host_traversal_stopped=true controller_status=DISABLED");
        let _ = fs::remove_file(trust_path);
    }

    async fn run_remote_e2e_terminal() {
        let rendezvous_url = std::env::var("SHOCKINGVRC_REMOTE_E2E_RENDEZVOUS").unwrap();
        let host_id = std::env::var("SHOCKINGVRC_REMOTE_E2E_HOST_ID").unwrap();
        let expected_pin = std::env::var("SHOCKINGVRC_REMOTE_E2E_HOST_PIN").unwrap();
        let expected_public = URL_SAFE_NO_PAD
            .decode(std::env::var("SHOCKINGVRC_REMOTE_E2E_HOST_PUBLIC").unwrap())
            .unwrap();
        let server_epoch = Uuid::parse_str(&std::env::var("SHOCKINGVRC_REMOTE_E2E_SERVER_EPOCH").unwrap()).unwrap();
        let invitation_id = Uuid::parse_str(&std::env::var("SHOCKINGVRC_REMOTE_E2E_INVITATION").unwrap()).unwrap();
        let route_token = std::env::var("SHOCKINGVRC_REMOTE_E2E_ROUTE_TOKEN").unwrap();
        let pairing_secret = std::env::var("SHOCKINGVRC_REMOTE_E2E_PAIRING_SECRET").unwrap();
        let mut rtc = tokio::time::timeout(
            Duration::from_secs(45),
            join_terminal_room(TerminalRtcConfig {
                rendezvous_url,
                host_id: host_id.clone(),
                rendezvous_authorization: Zeroizing::new(route_token),
                expected_fingerprint: expected_pin.clone(),
                ice_servers: Vec::new(),
                udp_addrs: if long_turn_e2e_enabled() {
                    vec!["0.0.0.0:0".into()]
                } else {
                    vec!["127.0.0.1:0".into()]
                },
                force_relay: long_turn_e2e_enabled(),
                resume_session_id: None,
            }),
        )
        .await
        .expect("Terminal RTC timeout")
        .expect("signed pinned Host identity and WebRTC DataChannel");
        assert!(
            rtc.peer.selected_candidate_pair_label().await.is_some(),
            "Terminal selected an ICE candidate pair"
        );
        let (mut inbound, outbound) = rtc.channels.take_optimized().expect("Terminal API DataChannel");

        let client_nonce = [0x71_u8; 32];
        let hello_request = Uuid::new_v4();
        send_api_frame(&outbound, json!({
            "type":"hello", "protocol":PROTOCOL_VERSION, "request_id":hello_request,
            "client":"ShockingVRC local acceptance", "client_version":"test", "client_nonce":URL_SAFE_NO_PAD.encode(client_nonce),
            "transport":"webrtc-datachannel"
        })).await;
        let hello = recv_api_frame(&mut inbound, "hello.ok", Some(hello_request)).await;
        assert_eq!(hello["server_id"], host_id);
        assert_eq!(hello["fingerprint"], expected_pin);
        let server_public = URL_SAFE_NO_PAD.decode(hello["server_key"].as_str().unwrap()).unwrap();
        assert_eq!(server_public, expected_public, "pinned Host public key");
        let server_nonce: [u8; 32] = URL_SAFE_NO_PAD
            .decode(hello["server_nonce"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let identity_proof = URL_SAFE_NO_PAD
            .decode(hello["identity_proof"].as_str().unwrap())
            .unwrap();
        let transcript = identity_transcript(server_epoch, &client_nonce, &server_nonce, &host_id);
        UnparsedPublicKey::new(&ED25519, &server_public)
            .verify(&transcript, &identity_proof)
            .expect("signed Host API identity proof");

        let unauthorized_export_request = Uuid::new_v4();
        send_api_frame(
            &outbound,
            json!({
                "type":"logs.export.start",
                "protocol":PROTOCOL_VERSION,
                "request_id":unauthorized_export_request,
            }),
        )
        .await;
        let denied: Value = serde_json::from_slice(
            &tokio::time::timeout(Duration::from_secs(15), inbound.recv())
                .await
                .expect("unauthorized export denial deadline")
                .expect("unauthorized export denial frame"),
        )
        .expect("unauthorized export denial JSON");
        assert_eq!(denied["type"], "error");
        assert_eq!(denied["code"], "AUTH_REQUIRED");
        assert_eq!(denied["request_id"], unauthorized_export_request.to_string());

        let terminal_pkcs8 =
            Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).expect("ephemeral Terminal key");
        let terminal_key = Ed25519KeyPair::from_pkcs8(terminal_pkcs8.as_ref()).expect("parse ephemeral Terminal key");
        let terminal_public: [u8; 32] = terminal_key
            .public_key()
            .as_ref()
            .try_into()
            .expect("Terminal Ed25519 key");
        let device_id = device_id_for_public_key(&terminal_public).expect("Terminal device ID");
        let pairing_request = Uuid::new_v4();
        send_api_frame(&outbound, json!({
            "type":"auth.device.begin", "protocol":PROTOCOL_VERSION, "request_id":pairing_request,
            "device_id":device_id, "public_key":URL_SAFE_NO_PAD.encode(terminal_public), "invitation_id":invitation_id
        })).await;
        let challenge = recv_api_frame(&mut inbound, "auth.device.challenge", Some(pairing_request)).await;
        let challenge_id = Uuid::parse_str(challenge["challenge_id"].as_str().unwrap()).unwrap();
        let server_nonce: [u8; 32] = URL_SAFE_NO_PAD
            .decode(challenge["server_nonce"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let raw_secret = URL_SAFE_NO_PAD.decode(&pairing_secret).unwrap();
        let pairing_secret_hash: [u8; 32] = Sha256::digest(&raw_secret).into();
        let pairing_transcript = device_auth_transcript(
            &host_id,
            server_epoch,
            pairing_request,
            challenge_id,
            server_nonce,
            &device_id,
            terminal_public,
            Some(invitation_id),
            Some(pairing_secret_hash),
        );
        let signature = URL_SAFE_NO_PAD.encode(terminal_key.sign(&pairing_transcript).as_ref());
        send_api_frame(&outbound, json!({
            "type":"auth.device.prove", "protocol":PROTOCOL_VERSION, "request_id":pairing_request,
            "challenge_id":challenge_id, "device_id":device_id, "public_key":URL_SAFE_NO_PAD.encode(terminal_public),
            "signature":signature, "invitation_id":invitation_id, "pairing_secret":pairing_secret
        })).await;
        let auth = recv_api_frame(&mut inbound, "auth.result", Some(pairing_request)).await;
        assert_eq!(auth["ok"], true);
        assert_eq!(auth["session_state"], "ACTIVE");
        assert!(auth["device_grant"].as_str().unwrap().starts_with("dg1."));
        let initial_snapshot = recv_api_frame(&mut inbound, "snapshot", None).await;
        assert_eq!(initial_snapshot["state"]["status"]["device_connected"], false);
        assert_eq!(initial_snapshot["state"]["bridge_revision"], 1);

        if !long_turn_e2e_enabled() {
            verify_host_log_page_over_datachannel(&outbound, &mut inbound).await;
            verify_host_log_export_over_datachannel(&outbound, &mut inbound, &auth, rtc.room_id).await;
        }

        if long_turn_e2e_enabled() {
            assert_eq!(
                rtc.peer.selected_pair_uses_relay().await,
                Some(true),
                "Terminal must select a TURN relay pair"
            );
            let initial_pair = rtc
                .peer
                .selected_candidate_pair_label()
                .await
                .expect("selected Terminal ICE pair");
            let progress_path = PathBuf::from(std::env::var("SHOCKINGVRC_REMOTE_E2E_PROGRESS").unwrap());
            let terminal_done = PathBuf::from(std::env::var("SHOCKINGVRC_REMOTE_E2E_TERMINAL_DONE").unwrap());
            let host_verified = PathBuf::from(std::env::var("SHOCKINGVRC_REMOTE_E2E_HOST_VERIFIED").unwrap());
            let started = tokio::time::Instant::now();
            let initial_room_id = rtc.room_id;
            let mut interval = tokio::time::interval(Duration::from_secs(3));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut ping_count = 0_u64;
            let mut snapshot_count = 0_u64;
            let mut next_turn_refresh = rtc
                .turn_refresh_at
                .expect("Terminal received the initial TURN credential refresh deadline");
            let mut credentials_staged_count = 0_u64;
            loop {
                interval.tick().await;
                let ping_request = Uuid::new_v4();
                let ping_nonce = URL_SAFE_NO_PAD.encode([((ping_count % 251) + 1) as u8; 16]);
                send_api_frame(
                    &outbound,
                    json!({
                        "type":"ping", "protocol":PROTOCOL_VERSION, "request_id":ping_request, "nonce":ping_nonce
                    }),
                )
                .await;
                let pong = recv_api_frame(&mut inbound, "pong", Some(ping_request)).await;
                assert_eq!(pong["nonce"], ping_nonce, "authenticated heartbeat round trip");
                ping_count += 1;

                let snapshot_request = Uuid::new_v4();
                send_api_frame(
                    &outbound,
                    json!({"type":"state.get", "protocol":PROTOCOL_VERSION, "request_id":snapshot_request}),
                )
                .await;
                let snapshot = recv_api_frame(&mut inbound, "snapshot", Some(snapshot_request)).await;
                assert_eq!(
                    snapshot["state"]["status"]["device_connected"], false,
                    "authenticated Snapshot arrived over the same DataChannel"
                );
                snapshot_count += 1;

                if tokio::time::Instant::now() >= next_turn_refresh {
                    let credentials = rtc
                        .signaling
                        .as_ref()
                        .expect("Terminal retained its active room signaling lease")
                        .refresh_turn_credentials()
                        .await
                        .expect("Terminal completed in-room TURN credential refresh")
                        .expect("Rendezvous returned fresh TURN credentials");
                    next_turn_refresh = credentials.refresh_at;
                    rtc.peer
                        .update_turn_credentials(credentials.server)
                        .await
                        .expect("Terminal staged fresh TURN credentials for a future ICE gathering");
                    credentials_staged_count += 1;
                    log::info!(
                        "[terminal-test] TURN credentials staged monotonic_ms={} staged_count={} room_unchanged=true",
                        shocking_vrc_rtc::monotonic_ms(),
                        credentials_staged_count
                    );
                }

                assert_eq!(rtc.room_id, initial_room_id, "Terminal remained in the original signaling room");
                assert_eq!(
                    rtc.peer.selected_pair_uses_relay().await,
                    Some(true),
                    "Terminal remained on relay"
                );
                assert_eq!(
                    rtc.peer.selected_candidate_pair_label().await.as_deref(),
                    Some(initial_pair.as_str()),
                    "selected relay pair remained stable"
                );
                let elapsed_seconds = started.elapsed().as_secs();
                let progress = json!({
                    "room_id":initial_room_id,
                    "session_id":auth["session_id"],
                    "transport_generation":auth["transport_generation"],
                    "candidate_pair":initial_pair,
                    "elapsed_seconds":elapsed_seconds,
                    "ping_count":ping_count,
                    "snapshot_count":snapshot_count,
                    "credentials_staged_count":credentials_staged_count,
                });
                fs::write(
                    &progress_path,
                    serde_json::to_vec(&progress).expect("serialize sanitized TURN test progress"),
                )
                .expect("write sanitized progress without credentials or network addresses");
                if elapsed_seconds >= continuous_turn_duration_seconds() {
                    break;
                }
            }
            let final_progress: Value =
                serde_json::from_slice(&fs::read(&progress_path).expect("preserve final active counters"))
                    .expect("final progress JSON");
            assert_eq!(
                E2E_ICE_DISCONNECTED_EVENTS.load(Ordering::Relaxed),
                0,
                "Terminal observed no ICE Disconnected event during active Force Relay"
            );
            eprintln!(
                "[remote-test] FINAL terminal_active duration_seconds={} heartbeat_round_trips={} snapshots={} turn_credential_refreshes={} room_stable=true generation_stable=true selected_transport=RELAY/UDP",
                final_progress["elapsed_seconds"].as_u64().unwrap_or_default(),
                final_progress["ping_count"].as_u64().unwrap_or_default(),
                final_progress["snapshot_count"].as_u64().unwrap_or_default(),
                final_progress["credentials_staged_count"].as_u64().unwrap_or_default(),
            );
            let required_duration_seconds = continuous_turn_duration_seconds();
            let minimum_authenticated_cycles = 400.min((required_duration_seconds / 3).max(1));
            assert!(final_progress["elapsed_seconds"].as_u64().unwrap_or_default() >= required_duration_seconds);
            assert!(final_progress["ping_count"].as_u64().unwrap_or_default() >= minimum_authenticated_cycles);
            assert!(final_progress["snapshot_count"].as_u64().unwrap_or_default() >= minimum_authenticated_cycles);
            assert!(
                final_progress["credentials_staged_count"].as_u64().unwrap_or_default() >= 3
                    || required_duration_seconds < 1_800
            );
            fs::write(&terminal_done, "continuous-turn-ready")
                .expect("publish continuous TURN readiness for Host verification");
            let close_owner =
                std::env::var("SHOCKINGVRC_REMOTE_E2E_CLOSE_OWNER").unwrap_or_else(|_| "terminal".to_owned());
            let expected_host_marker = if close_owner == "host" {
                "host-initiated-close"
            } else {
                "host-verified-active"
            };
            tokio::time::timeout(Duration::from_secs(30), async {
                while !fs::read_to_string(&host_verified).is_ok_and(|value| value == expected_host_marker) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("Host verified the authenticated session remained active before test-end disconnect");
            if close_owner == "host" {
                let mut signaling_status = rtc
                    .signaling
                    .as_ref()
                    .expect("Terminal retained its signaling lease until Host closure")
                    .subscribe_status();
                tokio::time::timeout(Duration::from_secs(15), async {
                    loop {
                        if *signaling_status.borrow() == shocking_vrc_rtc::SignalingLeaseStatus::RoomClosed {
                            break;
                        }
                        signaling_status
                            .changed()
                            .await
                            .expect("Terminal signaling lease status sender");
                    }
                })
                .await
                .expect("Rendezvous closed the Terminal signaling lease after Host shutdown");
                rtc.channels
                    .close_with_reason(&rtc.peer, shocking_vrc_rtc::PeerCloseReason::RoomClosed)
                    .await;
                rtc.peer.close().await;
                let terminal_receive_pump_ended =
                    tokio::time::timeout(Duration::from_secs(2), async { while inbound.recv().await.is_some() {} })
                        .await
                        .is_ok();
                assert!(terminal_receive_pump_ended, "Terminal receiver released the closed API channel");
                fs::write(&terminal_done, "host-initiated-close-observed")
                    .expect("record Host-initiated closure and Terminal cleanup");
                eprintln!(
                    "[remote-test] SHUTDOWN terminal_observed_host_room_close=true datachannel_eof=true signaling_status=RoomClosed"
                );
                if let Some(signaling) = rtc.signaling.take() {
                    signaling.close().await;
                }
                return;
            }
            if let Some(signaling) = rtc.signaling.take() {
                signaling.close().await;
            }
            rtc.channels.close(&rtc.peer).await;
            rtc.peer.close().await;
            return;
        }

        let snapshot_request = Uuid::new_v4();
        send_api_frame(
            &outbound,
            json!({"type":"state.get", "protocol":PROTOCOL_VERSION, "request_id":snapshot_request}),
        )
        .await;
        let snapshot = recv_api_frame(&mut inbound, "snapshot", Some(snapshot_request)).await;
        assert_eq!(snapshot["state"]["status"]["device_connected"], false);

        let ping_request = Uuid::new_v4();
        let ping_nonce = URL_SAFE_NO_PAD.encode([0x18_u8; 16]);
        send_api_frame(
            &outbound,
            json!({"type":"ping", "protocol":PROTOCOL_VERSION, "request_id":ping_request, "nonce":ping_nonce}),
        )
        .await;
        let pong = recv_api_frame(&mut inbound, "pong", Some(ping_request)).await;
        assert_eq!(pong["nonce"], ping_nonce, "authenticated heartbeat");

        let command_request = Uuid::new_v4();
        send_api_frame(
            &outbound,
            json!({
                "type":"command", "protocol":PROTOCOL_VERSION, "request_id":command_request,
                "op":"modulation.clear_segment", "args":{"channel":"A","kind":"frequency","segment":0}
            }),
        )
        .await;
        let accepted = recv_api_frame(&mut inbound, "command.accepted", Some(command_request)).await;
        assert_eq!(accepted["queued"], true, "command acceptance is queue acceptance only");
        fs::write(std::env::var("SHOCKINGVRC_REMOTE_E2E_TERMINAL_DONE").unwrap(), "accepted")
            .expect("Terminal completion marker");
        let ended = recv_api_frame(&mut inbound, "session.ended", None).await;
        assert_eq!(ended["reason"], "session_revoked");
        assert_eq!(ended["server_epoch"], server_epoch.to_string());
        assert_eq!(ended["session_id"], auth["session_id"]);
        assert_eq!(ended["transport_generation"], auth["transport_generation"]);
        send_api_frame(
            &outbound,
            json!({
                "type":"session.ended.ack", "protocol":PROTOCOL_VERSION,
                "server_epoch":ended["server_epoch"],
                "session_id":ended["session_id"],
                "transport_generation":ended["transport_generation"],
            }),
        )
        .await;
        fs::write(std::env::var("SHOCKINGVRC_REMOTE_E2E_TERMINAL_DONE").unwrap(), "revoked")
            .expect("Terminal revoke marker");
        rtc.channels.close(&rtc.peer).await;
        rtc.peer.close().await;
    }

    async fn verify_host_log_export_over_datachannel(
        outbound: &RtcApiOutbound,
        inbound: &mut mpsc::Receiver<Vec<u8>>,
        auth: &Value,
        room_id: Uuid,
    ) {
        let malformed_start = Uuid::new_v4();
        send_api_frame(
            outbound,
            json!({
                "type":"logs.export.start",
                "protocol":PROTOCOL_VERSION,
                "request_id":malformed_start,
                "start_utc":"2026-10-23T00:30:00Z",
            }),
        )
        .await;
        let malformed = recv_api_frame(inbound, "error", Some(malformed_start)).await;
        assert_eq!(malformed["code"], "INVALID_ARGUMENT");

        let start_request = Uuid::new_v4();
        send_api_frame(
            outbound,
            json!({
                "type":"logs.export.start",
                "protocol":PROTOCOL_VERSION,
                "request_id":start_request,
            }),
        )
        .await;
        let started = recv_api_frame(inbound, "logs.export.started", Some(start_request)).await;
        let metadata = &started["snapshot"];
        let snapshot_id = Uuid::parse_str(metadata["snapshot_id"].as_str().expect("snapshot ID"))
            .expect("valid snapshot ID");
        assert!(metadata["total_bytes"].as_u64().unwrap_or_default() > 8 * 1024);
        assert_eq!(metadata["file_count"], 1);
        assert_eq!(metadata["retention_truncated"], false);
        assert!(metadata["coverage_start_utc"].as_str().is_some());
        assert!(metadata["coverage_end_utc"].as_str().is_some());
        assert_eq!(started["application"]["name"], "ShockingVRC Host");
        assert_eq!(started["session"]["session_id"], auth["session_id"]);
        assert_eq!(started["session"]["transport_generation"], auth["transport_generation"]);
        assert_eq!(started["session"]["room_id"], room_id.to_string());

        let mut offset = 0_u64;
        let mut exported = Vec::new();
        let mut chunk_count = 0_usize;
        let final_hash;
        loop {
            let request_id = Uuid::new_v4();
            send_api_frame(
                outbound,
                json!({
                    "type":"logs.export.chunk",
                    "protocol":PROTOCOL_VERSION,
                    "request_id":request_id,
                    "snapshot_id":snapshot_id,
                    "offset":offset,
                    "max_bytes":shocking_vrc_diagnostics::MAX_LOG_CHUNK_BYTES,
                }),
            )
            .await;
            let response = recv_api_frame(inbound, "logs.export.chunk", Some(request_id)).await;
            let chunk: shocking_vrc_diagnostics::LogExportChunk =
                serde_json::from_value(response).expect("typed Host diagnostic chunk");
            assert_eq!(chunk.snapshot_id, snapshot_id);
            assert_eq!(chunk.offset, offset);
            assert!(chunk.bytes.len() <= shocking_vrc_diagnostics::MAX_LOG_CHUNK_BYTES);

            if chunk_count == 0 {
                let duplicate_request = Uuid::new_v4();
                send_api_frame(
                    outbound,
                    json!({
                        "type":"logs.export.chunk",
                        "protocol":PROTOCOL_VERSION,
                        "request_id":duplicate_request,
                        "snapshot_id":snapshot_id,
                        "offset":offset,
                        "max_bytes":shocking_vrc_diagnostics::MAX_LOG_CHUNK_BYTES,
                    }),
                )
                .await;
                let duplicate = recv_api_frame(inbound, "logs.export.chunk", Some(duplicate_request)).await;
                let duplicate: shocking_vrc_diagnostics::LogExportChunk =
                    serde_json::from_value(duplicate).expect("typed idempotent Host diagnostic chunk");
                assert_eq!(duplicate, chunk, "retrying an offset returned identical bytes and metadata");
            }

            offset = chunk.next_offset;
            exported.extend_from_slice(&chunk.bytes);
            chunk_count += 1;
            if chunk.eof {
                final_hash = chunk.sha256;
                break;
            }
        }
        assert!(chunk_count > 1, "the export crossed multiple bounded DataChannel chunks");
        assert_eq!(offset, metadata["total_bytes"].as_u64().unwrap());
        let expected_hash = format!("{:x}", Sha256::digest(&exported));
        assert_eq!(final_hash.as_deref(), Some(expected_hash.as_str()));
        let records = exported
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_slice::<shocking_vrc_diagnostics::DiagnosticLogRecord>(line)
                    .expect("valid exported Host JSONL record")
            })
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 1_201);
        assert_eq!(records.first().unwrap().timestamp_utc.as_deref(), Some("2026-10-23T00:00:00.000Z"));
        assert_eq!(records.last().unwrap().timestamp_utc.as_deref(), Some("2026-10-23T03:20:00.000Z"));

        let cancel_request = Uuid::new_v4();
        send_api_frame(
            outbound,
            json!({
                "type":"logs.export.cancel",
                "protocol":PROTOCOL_VERSION,
                "request_id":cancel_request,
                "snapshot_id":snapshot_id,
            }),
        )
        .await;
        let cancelled = recv_api_frame(inbound, "logs.export.cancelled", Some(cancel_request)).await;
        assert_eq!(cancelled["snapshot_id"], snapshot_id.to_string());

        let filtered_start = Uuid::new_v4();
        send_api_frame(
            outbound,
            json!({
                "type":"logs.export.start",
                "protocol":PROTOCOL_VERSION,
                "request_id":filtered_start,
                "start_utc":"2026-10-23T00:30:00Z",
                "end_utc":"2026-10-23T01:00:00Z",
            }),
        )
        .await;
        let filtered_started = recv_api_frame(inbound, "logs.export.started", Some(filtered_start)).await;
        let filtered_meta = &filtered_started["snapshot"];
        let filtered_snapshot_id = Uuid::parse_str(filtered_meta["snapshot_id"].as_str().unwrap()).unwrap();
        assert_eq!(filtered_meta["retained_record_count"], 1_201);
        assert_eq!(filtered_meta["exported_record_count"], 180);
        assert_eq!(filtered_meta["excluded_by_time_range_count"], 1_021);
        assert_eq!(filtered_meta["unknown_timestamp_count"], 0);
        assert_eq!(filtered_meta["requested_time_range"]["start_utc"], "2026-10-23T00:30:00.000Z");
        assert!(filtered_meta["total_bytes"].as_u64().unwrap() < metadata["total_bytes"].as_u64().unwrap());

        let mut filtered_offset = 0_u64;
        let mut filtered_bytes = Vec::new();
        let filtered_hash;
        let mut sent_heartbeat_during_transfer = false;
        loop {
            let chunk_request = Uuid::new_v4();
            send_api_frame(
                outbound,
                json!({
                    "type":"logs.export.chunk",
                    "protocol":PROTOCOL_VERSION,
                    "request_id":chunk_request,
                    "snapshot_id":filtered_snapshot_id,
                    "offset":filtered_offset,
                    "max_bytes":shocking_vrc_diagnostics::MAX_LOG_CHUNK_BYTES,
                }),
            )
            .await;
            let response = recv_api_frame(inbound, "logs.export.chunk", Some(chunk_request)).await;
            let chunk: shocking_vrc_diagnostics::LogExportChunk = serde_json::from_value(response).unwrap();
            filtered_offset = chunk.next_offset;
            filtered_bytes.extend_from_slice(&chunk.bytes);
            if !sent_heartbeat_during_transfer {
                let ping_request = Uuid::new_v4();
                send_api_frame(
                    outbound,
                    json!({"type":"ping","protocol":PROTOCOL_VERSION,"request_id":ping_request,"nonce":"range-export-heartbeat"}),
                )
                .await;
                let _pong = recv_api_frame(inbound, "pong", Some(ping_request)).await;
                sent_heartbeat_during_transfer = true;
            }
            if chunk.eof {
                filtered_hash = chunk.sha256;
                break;
            }
        }
        assert!(sent_heartbeat_during_transfer);
        assert_eq!(filtered_offset, filtered_meta["total_bytes"].as_u64().unwrap());
        assert_eq!(
            filtered_hash.as_deref(),
            Some(format!("{:x}", Sha256::digest(&filtered_bytes)).as_str())
        );
        let filtered_records = filtered_bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<shocking_vrc_diagnostics::DiagnosticLogRecord>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(filtered_records.len(), 180);
        assert_eq!(
            filtered_records.first().unwrap().timestamp_utc.as_deref(),
            Some("2026-10-23T00:30:00.000Z")
        );
        assert_eq!(
            filtered_records.last().unwrap().timestamp_utc.as_deref(),
            Some("2026-10-23T00:59:50.000Z")
        );

        let filtered_cancel = Uuid::new_v4();
        send_api_frame(
            outbound,
            json!({
                "type":"logs.export.cancel",
                "protocol":PROTOCOL_VERSION,
                "request_id":filtered_cancel,
                "snapshot_id":filtered_snapshot_id,
            }),
        )
        .await;
        let _ = recv_api_frame(inbound, "logs.export.cancelled", Some(filtered_cancel)).await;

        // Leave one snapshot live so Host session teardown proves it releases
        // exports that were not explicitly canceled by the peer.
        let pending_request = Uuid::new_v4();
        send_api_frame(
            outbound,
            json!({
                "type":"logs.export.start",
                "protocol":PROTOCOL_VERSION,
                "request_id":pending_request,
            }),
        )
        .await;
        let pending = recv_api_frame(inbound, "logs.export.started", Some(pending_request)).await;
        assert!(pending["snapshot"]["snapshot_id"].as_str().is_some());
    }

    async fn verify_host_log_page_over_datachannel(
        outbound: &RtcApiOutbound,
        inbound: &mut mpsc::Receiver<Vec<u8>>,
    ) {
        let first_request = Uuid::new_v4();
        send_api_frame(
            outbound,
            json!({
                "type":"logs.get", "protocol":PROTOCOL_VERSION, "request_id":first_request,
                "cursor":"0", "limit":2,
            }),
        )
        .await;
        let first: Value = recv_api_frame(inbound, "logs", Some(first_request)).await;
        assert_eq!(first["lines"].as_array().map(Vec::len), Some(2));
        assert!(first["lines"][0]["text"].as_str().unwrap().contains("generated persistent host record 0"));
        assert!(first["lines"][1]["text"].as_str().unwrap().contains("generated persistent host record 1"));
        assert_eq!(first["next_cursor"], "2");

        let second_request = Uuid::new_v4();
        send_api_frame(
            outbound,
            json!({
                "type":"logs.get", "protocol":PROTOCOL_VERSION, "request_id":second_request,
                "cursor":first["next_cursor"], "limit":2,
            }),
        )
        .await;
        let second: Value = recv_api_frame(inbound, "logs", Some(second_request)).await;
        assert_eq!(second["lines"].as_array().map(Vec::len), Some(2));
        assert!(second["lines"][0]["text"].as_str().unwrap().contains("generated persistent host record 2"));
        assert!(second["lines"][1]["text"].as_str().unwrap().contains("generated persistent host record 3"));
    }

    async fn send_api_frame(outbound: &RtcApiOutbound, value: Value) {
        outbound
            .send_control(serde_json::to_vec(&value).unwrap())
            .await
            .expect("send API frame over RTC DataChannel");
    }

    async fn send_test_api_frame(outbound: &mpsc::Sender<Vec<u8>>, value: Value) {
        outbound
            .send(serde_json::to_vec(&value).unwrap())
            .await
            .expect("send API frame through the bounded test channel");
    }

    async fn recv_api_frame(
        inbound: &mut mpsc::Receiver<Vec<u8>>,
        expected_type: &str,
        request_id: Option<Uuid>,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let bytes = inbound.recv().await.expect("Remote API DataChannel frame");
                let value: Value = serde_json::from_slice(&bytes).expect("Remote API JSON frame");
                if value["type"] == expected_type && request_id.is_none_or(|id| value["request_id"] == id.to_string()) {
                    return value;
                }
                assert_ne!(value["type"], "error", "unexpected Remote API error: {value}");
                assert_ne!(
                    value["type"], "session.revoked",
                    "session was revoked before expected response {expected_type}"
                );
                assert_ne!(
                    value["type"], "session.ended",
                    "session ended before expected response {expected_type}"
                );
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for Remote API {expected_type}"))
    }

    fn long_turn_e2e_enabled() -> bool {
        std::env::var("SHOCKINGVRC_REMOTE_E2E_LONG_TURN").is_ok_and(|value| value == "true")
    }

    fn continuous_turn_duration_seconds() -> u64 {
        std::env::var("SHOCKINGVRC_REMOTE_E2E_DURATION_SECONDS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1_300)
    }

    struct RemoveTestFiles(Vec<PathBuf>);

    impl Drop for RemoveTestFiles {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = fs::remove_file(path);
            }
        }
    }

    struct ChildGuard(Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }

    #[test]
    fn snapshot_publisher_ignores_bridge_revision_and_suppresses_duplicate_state() {
        let now = Instant::now();
        let session = WsSession {
            id: Uuid::new_v4(),
            generation: 7,
        };
        let mut publisher = SnapshotPublisher::default();
        let mut first = empty_snapshot();
        assert_eq!(
            publisher
                .observe(session, &first, true, now, Duration::from_secs(2))
                .unwrap(),
            StatePublication::FullSnapshot,
        );

        first.bridge_revision += 1;
        assert_eq!(
            publisher
                .observe(session, &first, true, now + Duration::from_millis(100), Duration::from_secs(2))
                .unwrap(),
            StatePublication::Suppressed,
        );
    }

    #[test]
    fn snapshot_publisher_separates_telemetry_from_authoritative_changes() {
        let now = Instant::now();
        let session = WsSession {
            id: Uuid::new_v4(),
            generation: 1,
        };
        let mut publisher = SnapshotPublisher::default();
        let mut snapshot = empty_snapshot();
        publisher
            .observe(session, &snapshot, true, now, Duration::from_secs(2))
            .unwrap();

        snapshot.status.channel_a.raw_level = 0.5;
        assert_eq!(
            publisher
                .observe(
                    session,
                    &snapshot,
                    true,
                    now + Duration::from_millis(100),
                    Duration::from_secs(2)
                )
                .unwrap(),
            StatePublication::Telemetry,
        );

        snapshot.config.channel_a.intensity[0] = snapshot.config.channel_a.intensity[0].saturating_add(1);
        assert_eq!(
            publisher
                .observe(
                    session,
                    &snapshot,
                    true,
                    now + Duration::from_millis(200),
                    Duration::from_secs(2)
                )
                .unwrap(),
            StatePublication::FullSnapshot,
        );

        snapshot.status.device_connected = !snapshot.status.device_connected;
        assert_eq!(
            publisher
                .observe(
                    session,
                    &snapshot,
                    true,
                    now + Duration::from_millis(300),
                    Duration::from_secs(2)
                )
                .unwrap(),
            StatePublication::FullSnapshot,
        );
    }

    #[test]
    fn telemetry_keyframe_contains_only_replaceable_channel_status() {
        let mut snapshot = empty_snapshot();
        snapshot.status.device_connected = true;
        let frame = telemetry_frame(Uuid::new_v4(), 1, &snapshot);
        assert!(frame["telemetry"]["status"].get("channel_a").is_some());
        assert!(frame["telemetry"]["status"].get("channel_b").is_some());
        assert!(frame["telemetry"]["status"].get("device_connected").is_none());
    }

    #[test]
    fn legacy_peer_gets_full_snapshot_for_telemetry_and_reconciliation_is_bounded() {
        let now = Instant::now();
        let session = WsSession {
            id: Uuid::new_v4(),
            generation: 1,
        };
        let mut publisher = SnapshotPublisher::default();
        let mut snapshot = empty_snapshot();
        assert_eq!(
            publisher
                .observe(session, &snapshot, false, now, Duration::from_secs(2))
                .unwrap(),
            StatePublication::FullSnapshot
        );

        snapshot.status.channel_a.raw_level = 0.25;
        assert_eq!(
            publisher
                .observe(
                    session,
                    &snapshot,
                    false,
                    now + Duration::from_millis(100),
                    Duration::from_secs(2)
                )
                .unwrap(),
            StatePublication::FullSnapshot
        );
        assert_eq!(
            publisher
                .observe(session, &snapshot, false, now + Duration::from_secs(2), Duration::from_secs(2))
                .unwrap(),
            StatePublication::Suppressed
        );
        assert_eq!(
            publisher
                .observe(
                    session,
                    &snapshot,
                    false,
                    now + Duration::from_millis(2_100),
                    Duration::from_secs(2)
                )
                .unwrap(),
            StatePublication::FullSnapshot
        );
    }

    #[test]
    fn snapshot_publisher_forces_full_keyframe_for_new_session_generation() {
        let now = Instant::now();
        let mut publisher = SnapshotPublisher::default();
        let snapshot = empty_snapshot();
        let first = WsSession {
            id: Uuid::new_v4(),
            generation: 1,
        };
        let second = WsSession {
            id: first.id,
            generation: 2,
        };
        publisher
            .observe(first, &snapshot, true, now, Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            publisher
                .observe(second, &snapshot, true, now + Duration::from_millis(1), Duration::from_secs(2))
                .unwrap(),
            StatePublication::FullSnapshot,
        );
    }

    #[test]
    fn reconciliation_interval_defaults_and_clamps_to_safe_bounds() {
        assert_eq!(bounded_state_reconciliation_interval(None), Duration::from_secs(2));
        assert_eq!(bounded_state_reconciliation_interval(Some("invalid")), Duration::from_secs(2));
        assert_eq!(bounded_state_reconciliation_interval(Some("50")), Duration::from_millis(500));
        assert_eq!(bounded_state_reconciliation_interval(Some("5000")), Duration::from_secs(5));
        assert_eq!(bounded_state_reconciliation_interval(Some("60000")), Duration::from_secs(10));
    }

    #[test]
    fn idle_delivery_reduces_source_derived_ten_hz_snapshot_frames_and_bytes() {
        let start = Instant::now();
        let session = WsSession {
            id: Uuid::new_v4(),
            generation: 1,
        };
        let epoch = Uuid::new_v4();
        let mut state = empty_snapshot();
        let initial = serde_json::to_vec(&snapshot_frame(epoch, 0, &state)).unwrap();

        // Baseline emits a complete snapshot every 100 ms; optimized starts with the same keyframe.
        let mut baseline_frames = 1_u64;
        let mut baseline_bytes = initial.len() as u64;
        let mut optimized_frames = 1_u64;
        let mut optimized_bytes = initial.len() as u64;
        let mut publisher = SnapshotPublisher::default();
        publisher.seed(session, &state, start).unwrap();

        for tick in 1..=30_u64 {
            state.bridge_revision = tick + 1;
            let now = start + Duration::from_millis(tick * 100);
            let legacy = serde_json::to_vec(&snapshot_frame(epoch, tick, &state)).unwrap();
            baseline_frames += 1;
            baseline_bytes += legacy.len() as u64;
            match publisher
                .observe(session, &state, true, now, Duration::from_secs(2))
                .unwrap()
            {
                StatePublication::FullSnapshot => {
                    optimized_frames += 1;
                    optimized_bytes += legacy.len() as u64;
                }
                StatePublication::Telemetry => panic!("idle state cannot emit telemetry"),
                StatePublication::Suppressed => {}
            }
        }

        assert_eq!(baseline_frames, 31);
        assert_eq!(optimized_frames, 2, "initial keyframe plus two-second reconciliation");
        assert!(optimized_bytes < baseline_bytes);
        println!(
            "SOURCE_DERIVED_IDLE_DELIVERY baseline_frames={baseline_frames} optimized_frames={optimized_frames} baseline_payload_bytes={baseline_bytes} optimized_payload_bytes={optimized_bytes} observation_seconds=3 interval_ms=100 reconciliation_ms=2000"
        );
    }
}

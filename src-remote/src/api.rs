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
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::{
    sync::Semaphore,
    time::{interval, interval_at, timeout},
};
use uuid::Uuid;

use crate::{
    BackendError, HostIdentity, RemoteBackend,
    auth::{AuthError, LoginResponse, SessionManager, SessionPhase},
    operations::REMOTE_OPERATIONS,
    protocol::{
        ApiErrorDto, CapabilitiesDto, CommandAcceptedDto, CommandRequest, HealthDto, MAX_FRAME_BYTES, PROTOCOL_VERSION,
        ProtocolError, RemoteCommand, SnapshotDto, parse_rest_command, parse_unique_json,
    },
    settings::{RemoteAccessSettings, RemoteChannel, RemoteLimitOperation, StrengthPolicyError},
};

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
        .route("/api/v1/auth/login", post(rest_login))
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn rest_login(
    State(state): State<ApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    let request = match decode_body::<LoginRequest>(&body) {
        Ok(request) if valid_login(&request) => request,
        Ok(_) => return api_error(StatusCode::BAD_REQUEST, "INVALID_ARGUMENT", "Invalid login input", None),
        Err(error) => return protocol_error(error, None),
    };
    if !state
        .login_throttle
        .lock()
        .map(|mut limiter| limiter.allow(peer.ip()))
        .unwrap_or(false)
    {
        return api_error(StatusCode::UNAUTHORIZED, "AUTH_FAILED", "Authentication failed", None);
    }
    let sessions = Arc::clone(&state.sessions);
    let username = request.username;
    let password = request.password;
    let result = tokio::task::spawn_blocking(move || sessions.login(&username, &password, true)).await;
    match result {
        Ok(Ok(login)) => json_response(StatusCode::OK, login),
        Ok(Err(AuthError::Failed)) => {
            if let Ok(mut limiter) = state.login_throttle.lock() {
                limiter.record_failure(peer.ip());
            }
            api_error(StatusCode::UNAUTHORIZED, "AUTH_FAILED", "Authentication failed", None)
        }
        Ok(Err(error)) => auth_error(error, None),
        Err(_) => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "AUTH_UNAVAILABLE",
            "Authentication is temporarily unavailable",
            None,
        ),
    }
}

fn valid_login(request: &LoginRequest) -> bool {
    valid_credentials(&request.username, &request.password)
}

fn valid_credentials(username: &str, password: &str) -> bool {
    !username.is_empty() && username.chars().count() <= 128 && !password.is_empty() && password.chars().count() <= 1024
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
    let current_min = if policy.enabled {
        let snapshot = state.backend.snapshot().map_err(|error| DispatchError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: error.code.into(),
            message: error.message,
        })?;
        match channel {
            RemoteChannel::A => snapshot.config.channel_a.limits.min,
            RemoteChannel::B => snapshot.config.channel_b.limits.min,
        }
    } else {
        0
    };
    let operation = if command.op == "channels.set_limit_min" {
        RemoteLimitOperation::Min
    } else {
        RemoteLimitOperation::Max
    };
    policy
        .validate_limit_write(operation, channel, value, current_min)
        .map_err(|error| {
            let message = match &error {
                StrengthPolicyError::LimitExceeded {
                    channel,
                    requested,
                    cap,
                } => format!("Channel {channel:?} requested {requested}, above the Host remote cap {cap}"),
                StrengthPolicyError::MaxWouldNormalizeAboveCap {
                    channel,
                    current_min,
                    requested_max,
                    cap,
                } => format!(
                    "Channel {channel:?} minimum {current_min} would normalize maximum {requested_max} above cap {cap}"
                ),
                StrengthPolicyError::MissingCap { channel } => {
                    format!("Remote cap for channel {channel:?} is not configured")
                }
                StrengthPolicyError::InvalidValue { value } => format!("Power limit {value} is outside 0..=200"),
            };
            let code = if matches!(
                error,
                StrengthPolicyError::LimitExceeded { .. } | StrengthPolicyError::MaxWouldNormalizeAboveCap { .. }
            ) {
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

fn decode_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, ProtocolError> {
    let value = parse_unique_json(body)?;
    serde_json::from_value(value).map_err(|error| ProtocolError::InvalidArguments(error.to_string()))
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
        AuthError::Failed | AuthError::InvalidCredential => {
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
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    session_id: Option<Uuid>,
    #[serde(default)]
    resume_ticket: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u16>,
    #[serde(default)]
    catalog: Option<String>,
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

enum ApiSender {
    WebSocket(futures_util::stream::SplitSink<WebSocket, Message>),
    DataChannel(tokio::sync::mpsc::Sender<Vec<u8>>),
}

impl ApiSender {
    async fn send_json(&mut self, value: &Value) -> Result<(), ()> {
        let text = serde_json::to_string(value).map_err(|_| ())?;
        if text.len() > MAX_FRAME_BYTES { return Err(()); }
        match self {
            Self::WebSocket(sender) => timeout(Duration::from_secs(2), sender.send(Message::Text(text.into())))
                .await.map_err(|_| ())?.map_err(|_| ()),
            Self::DataChannel(sender) => timeout(Duration::from_secs(2), sender.send(text.into_bytes()))
                .await.map_err(|_| ())?.map_err(|_| ()),
        }
    }

    async fn send_message(&mut self, message: Message) -> Result<(), ()> {
        match self {
            Self::WebSocket(sender) => timeout(Duration::from_secs(2), sender.send(message))
                .await.map_err(|_| ())?.map_err(|_| ()),
            Self::DataChannel(_) => Err(()),
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
    let mut sender = ApiSender::WebSocket(sender);
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
    let mut publish = interval_at(
        tokio::time::Instant::now() + Duration::from_millis(100),
        Duration::from_millis(100),
    );
    publish.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut maintenance = interval(Duration::from_secs(1));
    let mut sequence = 0u64;
    loop {
        tokio::select! {
            frame = receiver.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        let continue_loop = handle_ws_message(&state, &mut sender, text.as_bytes(), peer, &mut session, &mut sequence).await;
                        if !continue_loop { break; }
                        if session.is_some() {
                            drop(unauthenticated_permit.take());
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
                        let revoked = json!({"type":"session.revoked","protocol":PROTOCOL_VERSION,"server_epoch":state.server_epoch,"reason":"host_kick"});
                        let _ = send_json(&mut sender, &revoked).await;
                        break;
                    }
                    if status.phase == SessionPhase::Active && state.sessions.active_generation() != Some((active.id, active.generation)) {
                        let _ = sender.send_message(Message::Close(None)).await;
                        break;
                    }
                    if status.phase == SessionPhase::Active && state.sessions.active_generation() == Some((active.id,active.generation)) {
                        sequence = sequence.saturating_add(1);
                        if send_snapshot_with_seq(&state, &mut sender, active, sequence).await.is_err() { break; }
                    }
                }
            }
            _ = maintenance.tick() => state.sessions.maintain(),
        }
    }
    if let Some(active) = session {
        state.sessions.mark_generation_lost(active.id, active.generation);
    }
}

/// Run the existing Remote API v1 session protocol over a reliable DataChannel.
/// The caller must complete the separate ADR-033 DTLS/Host identity binding before
/// delivering any application frame to this adapter.
pub async fn data_channel_session(
    state: ApiState,
    mut inbound: tokio::sync::mpsc::Receiver<Vec<u8>>,
    outbound: tokio::sync::mpsc::Sender<Vec<u8>>,
) {
    let mut sender = ApiSender::DataChannel(outbound);
    let permit = match Arc::clone(&state.unauthenticated_ws).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return,
    };
    let first = timeout(Duration::from_secs(10), inbound.recv()).await;
    let Some(first) = first.ok().flatten() else { return; };
    let hello_value = match parse_unique_json(&first) {
        Ok(value) => value,
        Err(error) => {
            let _ = send_error(&mut sender, "INVALID_ARGUMENT", &error.to_string(), None).await;
            return;
        }
    };
    let hello_fields_ok = value_has_only_fields(
        &hello_value,
        &["type", "protocol", "request_id", "client", "client_version", "client_nonce", "transport"],
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
        let _ = send_error(&mut sender, "UNSUPPORTED_PROTOCOL", "Invalid WebRTC DataChannel hello", Some(request_id)).await;
        return;
    }
    let Some(client_nonce) = hello.client_nonce.as_deref().and_then(decode_nonce) else {
        let _ = send_error(&mut sender, "INVALID_ARGUMENT", "Invalid client nonce", Some(request_id)).await;
        return;
    };
    let server_nonce = rand::random::<[u8; 32]>();
    let proof_message = identity_transcript(state.server_epoch, &client_nonce, &server_nonce, state.identity.server_id());
    let proof = match state.identity.sign(&proof_message) {
        Ok(proof) => proof,
        Err(_) => {
            let _ = send_error(&mut sender, "IDENTITY_UNAVAILABLE", "Host identity is unavailable", Some(request_id)).await;
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
    if send_json(&mut sender, &hello_ok).await.is_err() { return; }

    let peer = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0);
    let mut session: Option<WsSession> = None;
    let mut unauthenticated_permit = Some(permit);
    let mut publish = interval_at(tokio::time::Instant::now() + Duration::from_millis(100), Duration::from_millis(100));
    publish.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut maintenance = interval(Duration::from_secs(1));
    let mut sequence = 0_u64;
    loop {
        tokio::select! {
            frame = inbound.recv() => {
                let Some(frame) = frame else { break; };
                if !handle_ws_message(&state, &mut sender, &frame, peer, &mut session, &mut sequence).await { break; }
                if session.is_some() {
                    drop(unauthenticated_permit.take());
                } else if unauthenticated_permit.is_none() {
                    unauthenticated_permit = Arc::clone(&state.unauthenticated_ws).try_acquire_owned().ok();
                    if unauthenticated_permit.is_none() { break; }
                }
            }
            _ = publish.tick(), if session.is_some() => {
                if let Some(active) = session {
                    let status = state.sessions.status();
                    if status.phase == SessionPhase::Free {
                        let revoked = json!({"type":"session.revoked","protocol":PROTOCOL_VERSION,"server_epoch":state.server_epoch,"reason":"host_kick"});
                        let _ = send_json(&mut sender, &revoked).await;
                        break;
                    }
                    if status.phase == SessionPhase::Active && state.sessions.active_generation() != Some((active.id, active.generation)) { break; }
                    if status.phase == SessionPhase::Active && state.sessions.active_generation() == Some((active.id, active.generation)) {
                        sequence = sequence.saturating_add(1);
                        if send_snapshot_with_seq(&state, &mut sender, active, sequence).await.is_err() { break; }
                    }
                }
            }
            _ = maintenance.tick() => state.sessions.maintain(),
        }
    }
    if let Some(active) = session {
        state.sessions.mark_generation_lost(active.id, active.generation);
    }
}

async fn handle_ws_message(
    state: &ApiState,
    sender: &mut ApiSender,
    bytes: &[u8],
    peer: SocketAddr,
    session: &mut Option<WsSession>,
    sequence: &mut u64,
) -> bool {
    let value = match parse_unique_json(bytes) {
        Ok(value) => value,
        Err(error) => {
            return send_error(sender, "INVALID_ARGUMENT", &error.to_string(), None)
                .await
                .is_ok();
        }
    };
    let message_type = value.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
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
            "auth.login" => &["type", "protocol", "request_id", "username", "password"][..],
            "auth.resume" => &[
                "type",
                "protocol",
                "request_id",
                "session_id",
                "resume_ticket",
                "client_nonce",
            ][..],
            "auth.attach" => &["type", "protocol", "request_id", "access_token"][..],
            "auth.token.refresh" | "auth.logout" | "state.get" | "capabilities.get" => {
                &["type", "protocol", "request_id"][..]
            }
            "catalogs.get" => &["type", "protocol", "request_id", "catalog"][..],
            "ping" => &["type", "protocol", "request_id", "nonce"][..],
            "logs.get" => &["type", "protocol", "request_id", "cursor", "limit"][..],
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
        if frame.message_type == "catalogs.get"
            && !matches!(frame.catalog.as_deref(), Some("presets" | "zone_presets"))
        {
            return send_error(sender, "INVALID_ARGUMENT", "catalog must be presets or zone_presets", Some(request_id))
                .await
                .is_ok();
        }
        if frame.message_type == "logs.get" && frame.limit.is_none() {
            return send_error(sender, "INVALID_ARGUMENT", "logs.get requires limit", Some(request_id))
                .await
                .is_ok();
        }
        match frame.message_type.as_str() {
            "auth.login" => {
                let (Some(username), Some(password)) = (frame.username, frame.password) else {
                    return send_error(
                        sender,
                        "INVALID_ARGUMENT",
                        "Login requires username and password",
                        Some(request_id),
                    )
                    .await
                    .is_ok();
                };
                if !valid_credentials(&username, &password) {
                    return send_error(sender, "INVALID_ARGUMENT", "Invalid login input", Some(request_id))
                        .await
                        .is_ok();
                }
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
                let sessions = Arc::clone(&state.sessions);
                let result = tokio::task::spawn_blocking(move || sessions.login(&username, &password, false)).await;
                match result {
                    Ok(Ok(login)) => {
                        let active = WsSession {
                            id: login.session_id,
                            generation: login.transport_generation,
                        };
                        *session = Some(active);
                        if send_json(sender, &login_response(login, Some(request_id)))
                            .await
                            .is_err()
                        {
                            return false;
                        }
                        *sequence = sequence.saturating_add(1);
                        send_snapshot_with_seq(state, sender, active, *sequence).await.is_ok()
                    }
                    Ok(Err(AuthError::Failed)) => {
                        if let Ok(mut limiter) = state.login_throttle.lock() {
                            limiter.record_failure(peer.ip());
                        }
                        send_error(sender, "AUTH_FAILED", "Authentication failed", Some(request_id))
                            .await
                            .is_ok()
                    }
                    Ok(Err(error)) => send_error(sender, auth_code(&error), &auth_message(&error), Some(request_id))
                        .await
                        .is_ok(),
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
                        if send_json(sender, &login_response(login, Some(request_id)))
                            .await
                            .is_err()
                        {
                            return false;
                        }
                        *sequence = sequence.saturating_add(1);
                        send_snapshot_with_seq(state, sender, active, *sequence).await.is_ok()
                    }
                    Err(error) => send_error(sender, auth_code(&error), &auth_message(&error), Some(request_id))
                        .await
                        .is_ok(),
                }
            }
            "auth.attach" => {
                let Some(token) = frame.access_token.as_deref() else {
                    return send_error(
                        sender,
                        "AUTH_REQUIRED",
                        "WSS attach requires an access token",
                        Some(request_id),
                    )
                    .await
                    .is_ok();
                };
                match state.sessions.attach(&token) {
                    Ok(login) => {
                        let active = WsSession {
                            id: login.session_id,
                            generation: login.transport_generation,
                        };
                        *session = Some(active);
                        let response = login_response(login, Some(request_id));
                        if send_json(sender, &response).await.is_err() {
                            return false;
                        }
                        *sequence = sequence.saturating_add(1);
                        send_snapshot_with_seq(state, sender, active, *sequence).await.is_ok()
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
                        let response =
                            json!({"type":"auth.logged_out","protocol":PROTOCOL_VERSION,"request_id":request_id});
                        send_json(sender, &response).await.is_ok()
                    }
                    Err(error) => send_error(sender, auth_code(&error), &auth_message(&error), Some(request_id))
                        .await
                        .is_ok(),
                }
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
                send_json(sender, &response).await.is_ok()
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

async fn send_snapshot_with_seq(
    state: &ApiState,
    sender: &mut ApiSender,
    session: WsSession,
    sequence: u64,
) -> Result<(), ()> {
    let result = state
        .sessions
        .with_generation(session.id, session.generation, || Ok(snapshot(state)));
    let snapshot = result.map_err(|_| ())?.map_err(|_| ())?;
    let response = json!({"type":"snapshot","protocol":PROTOCOL_VERSION,"server_epoch":state.server_epoch,"seq":sequence,"state":snapshot});
    if serde_json::to_vec(&response).map_err(|_| ())?.len() > MAX_FRAME_BYTES {
        let _ = send_error(sender, "SNAPSHOT_TOO_LARGE", "Snapshot exceeds the protocol frame limit", None).await;
        return Err(());
    }
    send_json(sender, &response).await
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

fn value_has_only_fields(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
}

fn parse_request_id(value: Option<&str>) -> Result<Uuid, ProtocolError> {
    Uuid::parse_str(value.ok_or(ProtocolError::RequestId)?).map_err(|_| ProtocolError::RequestId)
}

async fn send_error(
    sender: &mut ApiSender,
    code: &str,
    message: &str,
    request_id: Option<Uuid>,
) -> Result<(), ()> {
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
        AuthError::Failed | AuthError::InvalidCredential => "AUTH_FAILED",
        AuthError::InvalidResume => "RESUME_TICKET_INVALID",
        _ => "AUTH_REQUIRED",
    }
}

fn auth_message(error: &AuthError) -> &'static str {
    match error {
        AuthError::Busy => "Remote session is busy",
        AuthError::Failed | AuthError::InvalidCredential => "Authentication failed",
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

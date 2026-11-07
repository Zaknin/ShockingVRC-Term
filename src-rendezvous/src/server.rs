use std::{future::Future, net::SocketAddr, path::PathBuf, time::Duration};

use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::{
    net::TcpListener,
    sync::mpsc,
    time::{Instant, interval_at},
};
use uuid::Uuid;

use crate::{
    config::LoadedConfig,
    protocol::{SIGNAL_PROTOCOL, SignalMessage},
    state::{AppState, OUTBOUND_CAPACITY, Outbound, Peer, StateError},
};

const SOCKET_PING_INTERVAL: Duration = Duration::from_secs(15);

pub async fn run(config: LoadedConfig) -> Result<(), String> {
    run_with_shutdown(config, shutdown_signal()).await
}

/// Runs the rendezvous listener until the supplied shutdown future resolves.
/// This is also used by the bounded localhost integration proof.
pub async fn run_with_shutdown<F>(config: LoadedConfig, shutdown: F) -> Result<(), String>
where
    F: Future<Output = ()> + Send + 'static,
{
    let bind: SocketAddr = config
        .server
        .bind
        .parse()
        .map_err(|_| "invalid server.bind".to_owned())?;
    let health_path = config.server.health_path.clone();
    let version_path = config.server.version_path.clone();
    let signal_path = config.server.signal_path.clone();
    let state = AppState::new(config);
    let sweep_state = state.clone();
    let sweep_task = tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        loop {
            timer.tick().await;
            let _ = sweep_state.expire_rooms().await;
        }
    });
    let app = Router::new()
        .route(&health_path, get(healthz))
        .route(&version_path, get(version))
        .route(&signal_path, get(signal_upgrade))
        .with_state(state.clone());

    log::info!("rendezvous starting on {bind}");
    let tls_cert = state.config.server.tls.cert_file.clone();
    let tls_key = state.config.server.tls.key_file.clone();
    let result = if tls_cert.is_empty() {
        let listener = TcpListener::bind(bind)
            .await
            .map_err(|error| format!("cannot bind rendezvous listener: {error}"))?;
        log::info!("rendezvous listener ready (HTTP; place behind an operator-managed TLS proxy for public use)");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|error| format!("rendezvous server failed: {error}"))
    } else {
        let cert_path = resolve_path(&state.config.config_dir, &tls_cert);
        let key_path = resolve_path(&state.config.config_dir, &tls_key);
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path)
            .await
            .map_err(|error| format!("cannot load rendezvous TLS certificate/key: {error}"))?;
        let handle = axum_server::Handle::new();
        let shutdown_handle = handle.clone();
        let shutdown_task = tokio::spawn(async move {
            shutdown.await;
            shutdown_handle.graceful_shutdown(Some(Duration::from_secs(5)));
        });
        log::info!("rendezvous listener ready (HTTPS/WSS)");
        let result = axum_server::bind_rustls(bind, tls_config)
            .handle(handle)
            .serve(app.into_make_service())
            .await
            .map_err(|error| format!("rendezvous server failed: {error}"));
        shutdown_task.abort();
        result
    };
    sweep_task.abort();
    result
}

fn resolve_path(base: &std::path::Path, configured: &str) -> PathBuf {
    let path = PathBuf::from(configured);
    if path.is_absolute() { path } else { base.join(path) }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("could not install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

async fn version() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "service": "shocking-vrc-rendezvous",
            "protocol": SIGNAL_PROTOCOL,
            "version": env!("CARGO_PKG_VERSION")
        })),
    )
}

async fn signal_upgrade(State(state): State<AppState>, websocket: WebSocketUpgrade) -> impl IntoResponse {
    let max_bytes = state.config.rooms.max_signal_frame_bytes;
    websocket
        .max_message_size(max_bytes)
        .max_frame_size(max_bytes)
        .on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let (outgoing, mut output_rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
    let mut writer = tokio::spawn(async move {
        while let Some(output) = output_rx.recv().await {
            let message = match output {
                Outbound::Message(message) => match serde_json::to_string(&message) {
                    Ok(text) => Message::Text(text.into()),
                    Err(_) => continue,
                },
                Outbound::Close => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
                Outbound::Ping(payload) => Message::Ping(payload.into()),
                Outbound::Pong(payload) => Message::Pong(payload.into()),
            };
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });

    let first = match next_signal(&mut stream, &outgoing).await {
        Some(Ok(message)) => message,
        Some(Err((request_id, code, message))) => {
            queue_error(&outgoing, request_id, code, message);
            finish_writer(outgoing, writer).await;
            return;
        }
        None => {
            finish_writer(outgoing, writer).await;
            return;
        }
    };
    match first {
        SignalMessage::Register {
            protocol: _,
            request_id,
            host_id,
            identity_public_key,
            authorization,
        } => {
            let Some(policy) = state.config.hosts.get(&host_id) else {
                queue_state_error(&outgoing, request_id, StateError::AuthRequired);
                finish_writer(outgoing, writer).await;
                return;
            };
            if !policy.registration_matches(&authorization) {
                queue_state_error(&outgoing, request_id, StateError::AuthRequired);
                finish_writer(outgoing, writer).await;
                return;
            }
            if crate::pairing::verify_host_identity_key(&host_id, &identity_public_key).is_err() {
                queue_state_error(&outgoing, request_id, StateError::AuthRequired);
                finish_writer(outgoing, writer).await;
                return;
            }
            let (generation, registration_epoch) = state
                .register_host(&host_id, identity_public_key, outgoing.clone())
                .await;
            let registered = SignalMessage::Registered {
                protocol: SIGNAL_PROTOCOL,
                request_id,
                host_id: host_id.clone(),
                registration_epoch,
            };
            if outgoing.try_send(Outbound::Message(registered)).is_err() {
                state.unregister_host(&host_id, generation).await;
                finish_writer(outgoing, writer).await;
                return;
            }
            log::info!("Host registration active for {host_id}");
            let peer = Peer::Host {
                host_id: host_id.clone(),
                generation,
            };
            run_peer_loop(&mut stream, &outgoing, &mut writer, &state, peer).await;
            state.unregister_host(&host_id, generation).await;
        }
        SignalMessage::Join {
            request_id,
            host_id,
            authorization,
            ..
        } => {
            let Some(_policy) = state.config.hosts.get(&host_id) else {
                queue_state_error(&outgoing, request_id, StateError::HostNotFound);
                finish_writer(outgoing, writer).await;
                return;
            };
            if !state.authorize_terminal_join(&host_id, &authorization).await {
                queue_state_error(&outgoing, request_id, StateError::AuthRequired);
                finish_writer(outgoing, writer).await;
                return;
            }
            match state.create_room(&host_id, request_id, outgoing.clone()).await {
                Ok(room_id) => {
                    log::info!("Signaling room opened for Host {host_id}");
                    let peer = Peer::Terminal {
                        room_id: room_id.clone(),
                    };
                    run_peer_loop(&mut stream, &outgoing, &mut writer, &state, peer).await;
                    state.unregister_terminal(&room_id).await;
                }
                Err(error) => {
                    queue_state_error(&outgoing, request_id, error);
                    finish_writer(outgoing, writer).await;
                }
            }
        }
        message => {
            queue_state_error(&outgoing, message.request_id(), StateError::AuthRequired);
            finish_writer(outgoing, writer).await;
        }
    }
}

async fn run_peer_loop(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    outgoing: &mpsc::Sender<Outbound>,
    writer: &mut tokio::task::JoinHandle<()>,
    state: &AppState,
    peer: Peer,
) {
    let mut ping = interval_at(Instant::now() + SOCKET_PING_INTERVAL, SOCKET_PING_INTERVAL);
    loop {
        tokio::select! {
            _ = &mut *writer => break,
            _ = ping.tick() => {
                if outgoing.try_send(Outbound::Ping(Vec::new())).is_err() { break; }
            }
            frame = stream.next() => {
                let Some(Ok(frame)) = frame else { break; };
                match frame {
                    Message::Text(text) => {
                        let parsed = parse_message(&text);
                        let (message, request_id) = match parsed {
                            Ok(message) => {
                                let request_id = message.request_id();
                                (message, request_id)
                            }
                            Err((request_id, code, description)) => {
                                queue_error(outgoing, request_id, code, description);
                                continue;
                            }
                        };
                        if handle_peer_message(state, &peer, request_id, message, outgoing).await {
                            break;
                        }
                    }
                    Message::Ping(bytes) => { let _ = outgoing.try_send(Outbound::Pong(bytes.to_vec())); }
                    Message::Close(_) => break,
                    Message::Binary(_) => queue_error(outgoing, Uuid::new_v4(), "invalid_message", "Binary signaling frames are not supported."),
                    Message::Pong(_) => {
                        match state.renew_room_lease(&peer).await {
                            Ok(true) => log::debug!("[rendezvous] signaling heartbeat renewed active room lease"),
                            Ok(false) => {}
                            Err(error) => log::debug!("[rendezvous] signaling heartbeat did not renew room lease: {}", error.code()),
                        }
                    }
                }
            }
        }
    }
}

async fn handle_peer_message(
    state: &AppState,
    peer: &Peer,
    request_id: Uuid,
    message: SignalMessage,
    outgoing: &mpsc::Sender<Outbound>,
) -> bool {
    match message {
        SignalMessage::SignalOffer { .. }
        | SignalMessage::SignalAnswer { .. }
        | SignalMessage::SignalCandidate { .. }
        | SignalMessage::SignalEndOfCandidates { .. } => {
            if let Err(error) = state.forward(peer, message).await {
                queue_state_error(outgoing, request_id, error);
            }
            false
        }
        SignalMessage::TurnCredentialsRequest { room_id, .. } => {
            match state.turn_credentials(peer, request_id, &room_id).await {
                Ok(result) => {
                    let _ = outgoing.try_send(Outbound::Message(result));
                }
                Err(error) => queue_state_error(outgoing, request_id, error),
            }
            false
        }
        SignalMessage::RoomClose { room_id, .. } => {
            match state.close_room(peer, request_id, &room_id).await {
                Ok(()) => {
                    let _ = outgoing.try_send(Outbound::Message(SignalMessage::RoomClose {
                        protocol: SIGNAL_PROTOCOL,
                        request_id,
                        room_id,
                    }));
                }
                Err(error) => queue_state_error(outgoing, request_id, error),
            }
            matches!(peer, Peer::Terminal { .. })
        }
        _ => {
            queue_state_error(outgoing, request_id, StateError::UnexpectedMessage);
            false
        }
    }
}

async fn next_signal(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    outgoing: &mpsc::Sender<Outbound>,
) -> Option<Result<SignalMessage, (Uuid, &'static str, &'static str)>> {
    loop {
        let frame = stream.next().await?;
        let Ok(frame) = frame else { return None };
        match frame {
            Message::Text(text) => return Some(parse_message(&text)),
            Message::Ping(bytes) => {
                let _ = outgoing.try_send(Outbound::Pong(bytes.to_vec()));
            }
            Message::Close(_) => return None,
            Message::Binary(_) => {
                return Some(Err((
                    Uuid::new_v4(),
                    "invalid_message",
                    "Binary signaling frames are not supported.",
                )));
            }
            Message::Pong(_) => {}
        }
    }
}

fn parse_message(text: &str) -> Result<SignalMessage, (Uuid, &'static str, &'static str)> {
    let message: SignalMessage = serde_json::from_str(text).map_err(|_| {
        (
            Uuid::new_v4(),
            "invalid_message",
            "The signaling frame does not match rendezvous protocol v1.",
        )
    })?;
    let request_id = message.request_id();
    message.validate().map_err(|error| match error {
        crate::protocol::ProtocolError::UnsupportedVersion => (
            request_id,
            "unsupported_version",
            "This rendezvous protocol version is not supported.",
        ),
        crate::protocol::ProtocolError::InvalidField(_) => {
            (request_id, "invalid_message", "The signaling frame contains an invalid field.")
        }
        crate::protocol::ProtocolError::InvalidJson => {
            (request_id, "invalid_message", "The signaling frame is not valid JSON.")
        }
    })?;
    Ok(message)
}

fn queue_state_error(outgoing: &mpsc::Sender<Outbound>, request_id: Uuid, error: StateError) {
    queue_error(outgoing, request_id, error.code(), error.message());
}

fn queue_error(
    outgoing: &mpsc::Sender<Outbound>,
    request_id: Uuid,
    code: impl Into<String>,
    message: impl Into<String>,
) {
    let _ = outgoing.try_send(Outbound::Message(SignalMessage::error(request_id, code, message)));
}

async fn finish_writer(outgoing: mpsc::Sender<Outbound>, writer: tokio::task::JoinHandle<()>) {
    drop(outgoing);
    let _ = writer.await;
}

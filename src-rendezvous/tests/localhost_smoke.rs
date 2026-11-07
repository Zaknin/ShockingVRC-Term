use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use rcgen::generate_simple_self_signed;
use reqwest::Certificate;
use rustls::{ClientConfig, RootCertStore, pki_types::CertificateDer};
use sha1::Sha1;
use shocking_vrc_rendezvous::protocol::SignalMessage;
use tokio::{
    net::TcpStream,
    time::{sleep, timeout},
};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest},
};
use uuid::Uuid;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type HmacSha1 = Hmac<Sha1>;

const HOST_ID: &str = "localhost-host-01";
const REGISTRATION_KEY: &str = "test-registration-key-not-for-use";
const INVITE_TOKEN: &str = "test-invite-token-not-for-use";
const TURN_SECRET: &str = "test-turn-secret-not-for-use";

#[tokio::test]
async fn localhost_wss_signal_turn_and_restart_smoke() {
    let temp = TestDirectory::new();
    let (certificate, private_key, certificate_der) = make_certificate();
    fs::write(temp.path.join("server.pem"), certificate).unwrap();
    fs::write(temp.path.join("server-key.pem"), private_key).unwrap();
    fs::write(temp.path.join("turn-secret"), TURN_SECRET).unwrap();
    fs::write(temp.path.join("policy.toml"), format!(
        "[[hosts]]\nhost_id = \"{HOST_ID}\"\nregistration_key = \"{REGISTRATION_KEY}\"\ninvite_token = \"{INVITE_TOKEN}\"\n"
    )).unwrap();

    let port = free_local_port();
    fs::write(temp.path.join("config.toml"), config_text(port)).unwrap();
    let http = reqwest::Client::builder()
        .add_root_certificate(Certificate::from_pem(&fs::read(temp.path.join("server.pem")).unwrap()).unwrap())
        .build()
        .unwrap();
    let (mut process, base_url) = start_service(&temp.path.join("config.toml"), port, &http).await;

    let health: serde_json::Value = http
        .get(format!("{base_url}/healthz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"], "ok");
    let version: serde_json::Value = http
        .get(format!("{base_url}/version"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(version["service"], "shocking-vrc-rendezvous");
    assert_eq!(version["protocol"], 1);

    let tls = client_tls(certificate_der);
    let signal_url = format!("wss://localhost:{port}/api/signal/v1");
    let mut host = connect(&signal_url, tls.clone()).await;
    send(&mut host, register(REGISTRATION_KEY)).await;
    assert!(matches!(receive(&mut host).await, SignalMessage::Registered { .. }));
    let mut replacement_host = connect(&signal_url, tls.clone()).await;
    send(&mut replacement_host, register(REGISTRATION_KEY)).await;
    assert!(matches!(receive(&mut replacement_host).await, SignalMessage::Registered { .. }));
    assert!(matches!(
        timeout(Duration::from_secs(2), host.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Message::Close(_)
    ));
    host = replacement_host;

    let mut wrong_host = connect(&signal_url, tls.clone()).await;
    send(&mut wrong_host, register("wrong-registration-key")).await;
    assert_error(receive(&mut wrong_host).await, "auth_required");
    drop(wrong_host);

    let mut unknown_terminal = connect(&signal_url, tls.clone()).await;
    send(&mut unknown_terminal, join("not-registered", INVITE_TOKEN)).await;
    assert_error(receive(&mut unknown_terminal).await, "host_not_found");
    drop(unknown_terminal);

    let mut wrong_invite = connect(&signal_url, tls.clone()).await;
    send(&mut wrong_invite, join(HOST_ID, "wrong-invite-token")).await;
    assert_error(receive(&mut wrong_invite).await, "auth_required");
    drop(wrong_invite);

    let mut terminal = connect(&signal_url, tls.clone()).await;
    send(&mut terminal, join(HOST_ID, INVITE_TOKEN)).await;
    let room_id = match receive(&mut terminal).await {
        SignalMessage::JoinAccepted { room_id, .. } => room_id,
        other => panic!("expected join.accepted, got {other:?}"),
    };
    assert!(matches!(receive(&mut host).await, SignalMessage::JoinAccepted { .. }));

    let mut occupied = connect(&signal_url, tls.clone()).await;
    send(&mut occupied, join(HOST_ID, INVITE_TOKEN)).await;
    assert_error(receive(&mut occupied).await, "room_busy");
    drop(occupied);

    let offer = SignalMessage::SignalOffer {
        protocol: 1,
        request_id: Uuid::new_v4(),
        room_id: room_id.clone(),
        sdp: "v=0\r\n".into(),
    };
    send(&mut terminal, offer.clone()).await;
    assert_message_eq(receive(&mut host).await, offer);
    let answer = SignalMessage::SignalAnswer {
        protocol: 1,
        request_id: Uuid::new_v4(),
        room_id: room_id.clone(),
        sdp: "v=0\r\na=answer\r\n".into(),
    };
    send(&mut host, answer.clone()).await;
    assert_message_eq(receive(&mut terminal).await, answer);

    for index in 0..64 {
        let candidate = SignalMessage::SignalCandidate {
            protocol: 1,
            request_id: Uuid::new_v4(),
            room_id: room_id.clone(),
            candidate: format!("candidate:terminal-{index}"),
        };
        send(&mut terminal, candidate.clone()).await;
        assert_message_eq(receive(&mut host).await, candidate);
    }
    let over_limit = SignalMessage::SignalCandidate {
        protocol: 1,
        request_id: Uuid::new_v4(),
        room_id: room_id.clone(),
        candidate: "candidate:over-limit".into(),
    };
    send(&mut terminal, over_limit).await;
    assert_error(receive(&mut terminal).await, "candidate_limit");
    let host_candidate = SignalMessage::SignalCandidate {
        protocol: 1,
        request_id: Uuid::new_v4(),
        room_id: room_id.clone(),
        candidate: "candidate:host".into(),
    };
    send(&mut host, host_candidate.clone()).await;
    assert_message_eq(receive(&mut terminal).await, host_candidate);
    let terminal_eoc = SignalMessage::SignalEndOfCandidates {
        protocol: 1,
        request_id: Uuid::new_v4(),
        room_id: room_id.clone(),
    };
    send(&mut terminal, terminal_eoc.clone()).await;
    assert_message_eq(receive(&mut host).await, terminal_eoc);
    let host_eoc = SignalMessage::SignalEndOfCandidates {
        protocol: 1,
        request_id: Uuid::new_v4(),
        room_id: room_id.clone(),
    };
    send(&mut host, host_eoc.clone()).await;
    assert_message_eq(receive(&mut terminal).await, host_eoc);

    let turn_request = SignalMessage::TurnCredentialsRequest {
        protocol: 1,
        request_id: Uuid::new_v4(),
        room_id: room_id.clone(),
    };
    let turn_request_id = turn_request.request_id();
    send(&mut terminal, turn_request).await;
    let turn_result = receive(&mut terminal).await;
    verify_turn_result(turn_result, turn_request_id, &room_id);
    assert!(
        timeout(Duration::from_millis(200), host.next()).await.is_err(),
        "TURN credentials must not be sent to the other peer"
    );

    let close = SignalMessage::RoomClose {
        protocol: 1,
        request_id: Uuid::new_v4(),
        room_id: room_id.clone(),
    };
    send(&mut terminal, close).await;
    assert!(matches!(receive(&mut terminal).await, SignalMessage::RoomClose { .. }));
    assert!(matches!(receive(&mut host).await, SignalMessage::RoomClose { .. }));
    drop(terminal);

    let mut ttl_terminal = connect(&signal_url, tls.clone()).await;
    send(&mut ttl_terminal, join(HOST_ID, INVITE_TOKEN)).await;
    let expired_room_id = match receive(&mut ttl_terminal).await {
        SignalMessage::JoinAccepted { room_id, .. } => room_id,
        other => panic!("expected TTL room, got {other:?}"),
    };
    assert!(matches!(receive(&mut host).await, SignalMessage::JoinAccepted { .. }));
    let expired_close = timeout(Duration::from_secs(6), async {
        loop {
            if matches!(receive(&mut ttl_terminal).await, SignalMessage::RoomClose { .. }) {
                break;
            }
        }
    })
    .await;
    assert!(expired_close.is_ok(), "room TTL should close its peers");
    assert!(matches!(receive(&mut host).await, SignalMessage::RoomClose { .. }));
    drop(ttl_terminal);

    let mut restart_terminal = connect(&signal_url, tls.clone()).await;
    send(&mut restart_terminal, join(HOST_ID, INVITE_TOKEN)).await;
    let restart_room_id = match receive(&mut restart_terminal).await {
        SignalMessage::JoinAccepted { room_id, .. } => room_id,
        other => panic!("expected restart room, got {other:?}"),
    };
    assert!(matches!(receive(&mut host).await, SignalMessage::JoinAccepted { .. }));
    stop_service(&mut process);
    drop(host);
    drop(restart_terminal);

    let (mut process, restarted_url) = start_service(&temp.path.join("config.toml"), port, &http).await;
    assert_eq!(base_url, restarted_url);
    let mut new_host = connect(&signal_url, tls.clone()).await;
    send(&mut new_host, register(REGISTRATION_KEY)).await;
    assert!(matches!(receive(&mut new_host).await, SignalMessage::Registered { .. }));
    let mut new_terminal = connect(&signal_url, tls).await;
    send(&mut new_terminal, join(HOST_ID, INVITE_TOKEN)).await;
    let new_room_id = match receive(&mut new_terminal).await {
        SignalMessage::JoinAccepted { room_id, .. } => room_id,
        other => panic!("expected post-restart room, got {other:?}"),
    };
    assert_ne!(expired_room_id, new_room_id);
    let old_room_signal = SignalMessage::SignalCandidate {
        protocol: 1,
        request_id: Uuid::new_v4(),
        room_id: restart_room_id,
        candidate: "candidate:stale".into(),
    };
    send(&mut new_terminal, old_room_signal).await;
    assert_error(receive(&mut new_terminal).await, "room_not_found");
    send(
        &mut new_terminal,
        SignalMessage::RoomClose {
            protocol: 1,
            request_id: Uuid::new_v4(),
            room_id: new_room_id,
        },
    )
    .await;
    let _ = receive(&mut new_terminal).await;
    stop_service(&mut process);
}

fn make_certificate() -> (String, String, CertificateDer<'static>) {
    let certified = generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let certificate_der = certified.cert.der().clone();
    let certificate = pem_block("CERTIFICATE", certificate_der.as_ref());
    let private_key = pem_block("PRIVATE KEY", &certified.signing_key.serialize_der());
    (certificate, private_key, certificate_der)
}

fn pem_block(name: &str, bytes: &[u8]) -> String {
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    let lines = encoded
        .as_bytes()
        .chunks(64)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    format!("-----BEGIN {name}-----\n{lines}\n-----END {name}-----\n")
}

fn client_tls(certificate: CertificateDer<'static>) -> Arc<ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = RootCertStore::empty();
    roots.add(certificate).unwrap();
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn config_text(port: u16) -> String {
    format!(
        r#"
[server]
bind = "127.0.0.1:{port}"
public_url = ""
health_path = "/healthz"
signal_path = "/api/signal/v1"
version_path = "/version"

[server.tls]
cert_file = "server.pem"
key_file = "server-key.pem"

[rooms]
ttl_seconds = 3
max_candidates_per_peer = 64
max_signal_frame_bytes = 65536

[turn]
enabled = true
uris = ["turn:127.0.0.1:3478?transport=udp"]
realm = "localhost"
secret_file = "turn-secret"
credential_ttl_seconds = 600

[registration]
policy_file = "policy.toml"
"#
    )
}

async fn start_service(config: &Path, port: u16, client: &reqwest::Client) -> (RunningService, String) {
    let executable = env!("CARGO_BIN_EXE_shocking-vrc-rendezvous");
    let child = Command::new(executable)
        .args(["--config", config.to_str().unwrap()])
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let base_url = format!("https://localhost:{port}");
    for _ in 0..120 {
        if let Ok(response) = client.get(format!("{base_url}/healthz")).send().await
            && response.status().is_success()
        {
            return (RunningService { child }, base_url);
        }
        sleep(Duration::from_millis(50)).await;
    }
    let mut child = child;
    let status = child.try_wait().unwrap();
    let _ = child.kill();
    panic!("rendezvous did not become ready; early status: {status:?}");
}

fn stop_service(process: &mut RunningService) {
    if process.child.try_wait().unwrap().is_none() {
        process.child.kill().unwrap();
    }
    let _ = process.child.wait();
}

async fn connect(url: &str, tls: Arc<ClientConfig>) -> Socket {
    let request = url.into_client_request().unwrap();
    let (socket, _) = connect_async_tls_with_config(request, None, false, Some(Connector::Rustls(tls)))
        .await
        .unwrap();
    socket
}

async fn send(socket: &mut Socket, message: SignalMessage) {
    socket
        .send(Message::Text(serde_json::to_string(&message).unwrap().into()))
        .await
        .unwrap();
}

async fn receive(socket: &mut Socket) -> SignalMessage {
    loop {
        let message = timeout(Duration::from_secs(10), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match message {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
            other => panic!("unexpected WebSocket frame: {other:?}"),
        }
    }
}

fn register(key: &str) -> SignalMessage {
    SignalMessage::Register {
        protocol: 1,
        request_id: Uuid::new_v4(),
        host_id: HOST_ID.into(),
        identity_public_key: "cHVibGljLXRlc3Qta2V5".into(),
        authorization: key.into(),
    }
}

fn join(host_id: &str, token: &str) -> SignalMessage {
    SignalMessage::Join {
        protocol: 1,
        request_id: Uuid::new_v4(),
        host_id: host_id.into(),
        authorization: token.into(),
    }
}

fn assert_error(message: SignalMessage, expected: &str) {
    match message {
        SignalMessage::Error { code, .. } => assert_eq!(code, expected),
        other => panic!("expected {expected} error, got {other:?}"),
    }
}

fn assert_message_eq(actual: SignalMessage, expected: SignalMessage) {
    assert_eq!(serde_json::to_value(actual).unwrap(), serde_json::to_value(expected).unwrap());
}

fn verify_turn_result(message: SignalMessage, expected_request: Uuid, expected_room: &str) {
    let SignalMessage::TurnCredentialsResult {
        request_id,
        room_id,
        turn_uris,
        username,
        credential,
        expires_at,
        ..
    } = message
    else {
        panic!("expected TURN credential result")
    };
    assert_eq!(request_id, expected_request);
    assert_eq!(room_id, expected_room);
    assert_eq!(turn_uris, ["turn:127.0.0.1:3478?transport=udp"]);
    let (expiry, member_id) = username.split_once(':').unwrap();
    assert_eq!(expiry.parse::<u64>().unwrap(), expires_at);
    assert!(Uuid::parse_str(member_id).is_ok());
    let mut mac = HmacSha1::new_from_slice(TURN_SECRET.as_bytes()).unwrap();
    mac.update(username.as_bytes());
    assert_eq!(
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, credential).unwrap(),
        mac.finalize().into_bytes().as_slice()
    );
}

fn free_local_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

struct RunningService {
    child: Child,
}

impl Drop for RunningService {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("shockingvrc-rendezvous-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

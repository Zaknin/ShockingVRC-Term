use std::{
    collections::HashMap,
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use shocking_vrc_rendezvous::{
    config::{HostAuthorization, LoadedConfig, RoomConfig, ServerConfig, TlsConfig, TurnConfig},
    pairing::create_route_token,
    protocol::{SIGNAL_PROTOCOL, SignalMessage},
    server::run_with_shutdown,
};
use shocking_vrc_rtc::{
    HostIdentitySigner, IceServerConfig, SignalSocket, TerminalRtcConfig, accept_host_room, close_host_room,
    identity_fingerprint, join_message, join_terminal_room, register_host, server_id,
};
use tokio::sync::oneshot;
use uuid::Uuid;
use zeroize::Zeroizing;

const ICE_TEST_NAME: &str = "localhost_twenty_cycle_ice_reconnect";
const TURN_TEST_NAME: &str = "localhost_twenty_cycle_turn_udp_relay_reconnect";
const REGISTRATION_KEY: &str = "ephemeral-local-reconnect-registration-key";
const SUCCESS_CYCLES: usize = 20;
const RECOVERY_CASES: usize = 2;

/// Exercises ICE signaling and DataChannel teardown with one long-running Host
/// process and one long-running Terminal process. The loopback rendezvous has no
/// Direct endpoint or TURN service; no physical device or external network is used.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn localhost_twenty_cycle_ice_reconnect() {
    run_reconnect_cycles(false).await;
}

/// Exercises real authenticated TURN/UDP allocations and relay-only DataChannels
/// against an explicitly started loopback TURN fixture. This remains opt-in so
/// the offline workspace suite does not depend on an external TURN process.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated loopback TURN/UDP fixture configured by environment"]
async fn localhost_twenty_cycle_turn_udp_relay_reconnect() {
    run_reconnect_cycles(true).await;
}

async fn run_reconnect_cycles(turn_udp: bool) {
    if let Ok(role) = std::env::var("SHOCKINGVRC_ICE_CYCLE_ROLE") {
        let test_name = env("SHOCKINGVRC_ICE_CYCLE_TEST_NAME");
        assert_eq!(
            test_name,
            if turn_udp { TURN_TEST_NAME } else { ICE_TEST_NAME },
            "child mode must match the selected acceptance test"
        );
        match role.as_str() {
            "host" => run_host_child(turn_udp).await,
            "terminal" => run_terminal_child(turn_udp).await,
            _ => panic!("unknown ICE cycle child role"),
        }
        return;
    }

    let test_name = if turn_udp { TURN_TEST_NAME } else { ICE_TEST_NAME };
    let turn_fixture = if turn_udp {
        let uri = std::env::var("SHOCKINGVRC_TEST_TURN_URI").expect("loopback TURN URI");
        let realm = std::env::var("SHOCKINGVRC_TEST_TURN_REALM").expect("loopback TURN realm");
        let secret = std::env::var("SHOCKINGVRC_TEST_TURN_SHARED_SECRET").expect("test-only TURN shared secret");
        assert!(
            uri.starts_with("turn:127.0.0.1:") && uri.contains("?transport=udp"),
            "TURN relay acceptance must use an IPv4 loopback UDP fixture"
        );
        assert!(!realm.is_empty(), "TURN realm must be configured");
        assert!(!secret.is_empty(), "TURN REST shared secret must be configured");
        Some((uri, realm, secret))
    } else {
        None
    };

    let key_document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("ephemeral Host key");
    let key = Ed25519KeyPair::from_pkcs8(key_document.as_ref()).expect("parse ephemeral Host key");
    let host_id = server_id(key.public_key().as_ref());
    let host_pin = identity_fingerprint(key.public_key().as_ref());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let authorization = create_route_token(REGISTRATION_KEY.as_bytes(), &host_id, Uuid::new_v4(), now + 900);
    let private_key = URL_SAFE_NO_PAD.encode(key_document.as_ref());

    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve localhost rendezvous port");
    let address = listener.local_addr().expect("localhost address");
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
            ttl_seconds: 90,
            max_candidates_per_peer: 64,
            max_signal_frame_bytes: 64 * 1024,
        },
        turn: TurnConfig {
            enabled: turn_fixture.is_some(),
            uris: turn_fixture
                .as_ref()
                .map(|(uri, _, _)| vec![uri.clone()])
                .unwrap_or_default(),
            realm: turn_fixture
                .as_ref()
                .map(|(_, realm, _)| realm.clone())
                .unwrap_or_default(),
            secret_file: String::new(),
            credential_ttl_seconds: 600,
        },
        stun: shocking_vrc_rendezvous::config::StunConfig::default(),
        hosts: HashMap::from([(
            host_id.clone(),
            HostAuthorization {
                registration_key: REGISTRATION_KEY.into(),
            },
        )]),
        turn_secret: turn_fixture.as_ref().map(|(_, _, secret)| secret.as_bytes().to_vec()),
        config_dir: std::env::temp_dir(),
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let rendezvous = tokio::spawn(run_with_shutdown(config, async move {
        let _ = shutdown_rx.await;
    }));

    let ready_path = std::env::temp_dir().join(format!("shockingvrc-ice-cycle-{}.ready", Uuid::new_v4()));
    let _cleanup = RemoveFileOnDrop(ready_path.clone());
    let mut host = ChildGuard({
        let mut command = Command::new(std::env::current_exe().expect("integration test executable"));
        command.args(["--exact", test_name]);
        if turn_udp {
            command.arg("--ignored");
        }
        command.arg("--nocapture");
        command
            .env("SHOCKINGVRC_ICE_CYCLE_ROLE", "host")
            .env("SHOCKINGVRC_ICE_CYCLE_TEST_NAME", test_name)
            .env("SHOCKINGVRC_ICE_CYCLE_TURN_UDP", turn_udp.to_string())
            .env("SHOCKINGVRC_ICE_CYCLE_RENDEZVOUS", &rendezvous_url)
            .env("SHOCKINGVRC_ICE_CYCLE_HOST_ID", &host_id)
            .env("SHOCKINGVRC_ICE_CYCLE_HOST_PIN", &host_pin)
            .env("SHOCKINGVRC_ICE_CYCLE_PRIVATE_KEY", &private_key)
            .env("SHOCKINGVRC_ICE_CYCLE_REGISTRATION_KEY", REGISTRATION_KEY)
            .env("SHOCKINGVRC_ICE_CYCLE_READY", &ready_path)
            .envs(
                turn_fixture
                    .as_ref()
                    .map(|(uri, realm, secret)| {
                        [
                            ("SHOCKINGVRC_TEST_TURN_URI", uri.as_str()),
                            ("SHOCKINGVRC_TEST_TURN_REALM", realm.as_str()),
                            ("SHOCKINGVRC_TEST_TURN_SHARED_SECRET", secret.as_str()),
                        ]
                    })
                    .into_iter()
                    .flatten(),
            )
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start long-running Host process")
    });

    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if ready_path.exists() {
                break;
            }
            if let Some(status) = host.0.try_wait().expect("poll Host child") {
                panic!("Host child exited before registering: {status}");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Host registered with localhost rendezvous");

    let terminal = tokio::task::spawn_blocking({
        let rendezvous_url = rendezvous_url.clone();
        let host_id = host_id.clone();
        let host_pin = host_pin.clone();
        let authorization = authorization.clone();
        move || {
            let mut command = Command::new(std::env::current_exe().expect("integration test executable"));
            command.args(["--exact", test_name]);
            if turn_udp {
                command.arg("--ignored");
            }
            command.arg("--nocapture");
            command
                .env("SHOCKINGVRC_ICE_CYCLE_ROLE", "terminal")
                .env("SHOCKINGVRC_ICE_CYCLE_TEST_NAME", test_name)
                .env("SHOCKINGVRC_ICE_CYCLE_TURN_UDP", turn_udp.to_string())
                .env("SHOCKINGVRC_ICE_CYCLE_RENDEZVOUS", rendezvous_url)
                .env("SHOCKINGVRC_ICE_CYCLE_HOST_ID", host_id)
                .env("SHOCKINGVRC_ICE_CYCLE_HOST_PIN", host_pin)
                .env("SHOCKINGVRC_ICE_CYCLE_AUTHORIZATION", authorization)
                .env(
                    "SHOCKINGVRC_TEST_TURN_URI",
                    turn_fixture
                        .as_ref()
                        .map(|(uri, _, _)| uri.as_str())
                        .unwrap_or_default(),
                )
                .env(
                    "SHOCKINGVRC_TEST_TURN_REALM",
                    turn_fixture
                        .as_ref()
                        .map(|(_, realm, _)| realm.as_str())
                        .unwrap_or_default(),
                )
                .env(
                    "SHOCKINGVRC_TEST_TURN_SHARED_SECRET",
                    turn_fixture
                        .as_ref()
                        .map(|(_, _, secret)| secret.as_str())
                        .unwrap_or_default(),
                )
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
        }
    });
    let terminal_status = tokio::time::timeout(Duration::from_secs(300), terminal)
        .await
        .expect("Terminal process completed all local ICE cycles")
        .expect("join Terminal child task")
        .expect("run Terminal child process");
    assert!(terminal_status.success(), "Terminal child failed: {terminal_status}");

    let host_status = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(status) = host.0.try_wait().expect("poll Host child") {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Host processed the final disconnect");
    assert!(host_status.success(), "Host child failed: {host_status}");

    let _ = shutdown_tx.send(());
    rendezvous
        .await
        .expect("rendezvous task join")
        .expect("localhost rendezvous shutdown");
    println!(
        "PASS: {SUCCESS_CYCLES} sequential {} reconnect cycles completed with two deterministic room-recovery cases",
        if turn_udp { "TURN/UDP relay-only" } else { "ICE-only" }
    );
}

async fn run_host_child(turn_udp: bool) {
    let rendezvous_url = env("SHOCKINGVRC_ICE_CYCLE_RENDEZVOUS");
    let key_bytes = URL_SAFE_NO_PAD
        .decode(env("SHOCKINGVRC_ICE_CYCLE_PRIVATE_KEY"))
        .expect("decode ephemeral Host key");
    let signer = TestSigner::new(&key_bytes);
    let mut signaling = register_host(
        &rendezvous_url,
        &signer,
        Zeroizing::new(env("SHOCKINGVRC_ICE_CYCLE_REGISTRATION_KEY")),
        Uuid::new_v4(),
    )
    .await
    .expect("register long-running Host");
    fs::write(env("SHOCKINGVRC_ICE_CYCLE_READY"), "registered").expect("write Host ready marker");

    for attempt in 0..(RECOVERY_CASES + SUCCESS_CYCLES) {
        let result = tokio::time::timeout(
            Duration::from_secs(25),
            accept_host_room(&mut signaling, &signer, &[], vec!["127.0.0.1:0".to_owned()]),
        )
        .await
        .expect("Host accepted or rejected the next local room");
        if attempt < RECOVERY_CASES {
            let error = match result {
                Err(error) => error,
                Ok(session) => {
                    session.channels.close(&session.peer).await;
                    panic!("the first two rooms deliberately fail before API session");
                }
            };
            assert!(
                error.is_room_local(),
                "failed room should leave Host registration reusable: {error}"
            );
            if attempt == 0 {
                assert!(
                    error.to_string().to_ascii_lowercase().contains("sdp"),
                    "the early offer must survive Host TURN response ordering and reach SDP parsing: {error}"
                );
            }
            continue;
        }

        let mut session = result.expect("Host established the next ICE room");
        let cycle = attempt - RECOVERY_CASES;
        if turn_udp {
            assert_relay_udp_session(&session, "Host", cycle);
        }
        let (mut inbound, _outbound) = session.channels.take().expect("Host API channel adapter");
        let request = tokio::time::timeout(Duration::from_secs(5), inbound.recv())
            .await
            .expect("Terminal DataChannel request arrived")
            .expect("receive Terminal request");
        let request: serde_json::Value = serde_json::from_slice(&request).expect("Terminal request JSON");
        assert_eq!(request["type"], "snapshot.request");
        assert_eq!(request["cycle"], cycle);
        session
            .peer
            .send_json(
                format!(
                    r#"{{"type":"snapshot","cycle":{cycle},"state":{{"sequence":{cycle},"source":"localhost-host"}}}}"#
                )
                .as_bytes(),
            )
            .await
            .expect("send per-cycle snapshot");
        let room_id = session.room_id;
        let request_id = session.request_id;
        tokio::time::timeout(Duration::from_secs(5), signaling.wait_for_room_close(room_id))
            .await
            .expect("Rendezvous room close ended the active Host room")
            .expect("Host received the matching room-close notification");
        session.channels.close(&session.peer).await;
        close_host_room(&mut signaling, request_id, room_id)
            .await
            .expect("acknowledge the already closed signaling room");
    }
    signaling.socket.close().await;
}

async fn run_terminal_child(turn_udp: bool) {
    let rendezvous_url = env("SHOCKINGVRC_ICE_CYCLE_RENDEZVOUS");
    let host_id = env("SHOCKINGVRC_ICE_CYCLE_HOST_ID");
    let authorization = env("SHOCKINGVRC_ICE_CYCLE_AUTHORIZATION");

    send_malformed_offer(&rendezvous_url, &host_id, &authorization).await;
    close_room_before_offer(&rendezvous_url, &host_id, &authorization).await;

    let mut retired_sessions: Vec<shocking_vrc_rtc::EstablishedRtcSession> = Vec::with_capacity(1);
    for cycle in 0..SUCCESS_CYCLES {
        let config = TerminalRtcConfig {
            rendezvous_url: rendezvous_url.clone(),
            host_id: host_id.clone(),
            rendezvous_authorization: Zeroizing::new(authorization.clone()),
            expected_fingerprint: env("SHOCKINGVRC_ICE_CYCLE_HOST_PIN"),
            ice_servers: Vec::<IceServerConfig>::new(),
            udp_addrs: vec!["127.0.0.1:0".to_owned()],
            force_relay: turn_udp,
            resume_session_id: None,
        };
        let mut session = tokio::time::timeout(Duration::from_secs(20), join_terminal_room(config))
            .await
            .expect("Terminal ICE room completed")
            .expect("pinned Host identity accepted");
        if let Some(retired) = retired_sessions.pop() {
            retired.channels.close(&retired.peer).await;
        }
        if turn_udp {
            assert_relay_udp_session(&session, "Terminal", cycle);
        }
        let (mut inbound, outbound) = session.channels.take().expect("Terminal API channel adapter");
        outbound
            .send(
                serde_json::json!({"type":"snapshot.request", "cycle" : cycle})
                    .to_string()
                    .into_bytes(),
            )
            .await
            .expect("send snapshot request");
        let snapshot = tokio::time::timeout(Duration::from_secs(5), inbound.recv())
            .await
            .expect("Host snapshot arrived")
            .expect("receive Host snapshot");
        let snapshot: serde_json::Value = serde_json::from_slice(&snapshot).expect("Host snapshot JSON");
        assert_eq!(snapshot["type"], "snapshot");
        assert_eq!(snapshot["cycle"], cycle);
        assert_eq!(snapshot["state"]["sequence"], cycle);
        assert_eq!(snapshot["state"]["source"], "localhost-host");

        if let Some(signaling) = session.signaling.take() {
            signaling.close().await;
        }
        // Keep the old peer alive after signaling disconnect. This withholds
        // DataChannel OnClose while the next room is opened.
        retired_sessions.push(session);
    }
    for retired in retired_sessions {
        retired.channels.close(&retired.peer).await;
    }
}

fn assert_relay_udp_session(session: &shocking_vrc_rtc::EstablishedRtcSession, role: &str, cycle: usize) {
    assert!(
        session.ice_servers_configured,
        "{role} cycle {cycle}: Rendezvous TURN server was not configured"
    );
    assert!(
        session.turn_credentials_received,
        "{role} cycle {cycle}: TURN credentials were not received"
    );
    let pair = session
        .ice_candidate_pair
        .as_deref()
        .unwrap_or_else(|| panic!("{role} cycle {cycle}: selected ICE candidate pair was not available"));
    assert!(
        pair.contains("LOCAL RELAY/UDP ↔ REMOTE RELAY/UDP"),
        "{role} cycle {cycle}: expected a live RELAY/UDP ↔ RELAY/UDP pair, got {pair}"
    );
}

async fn send_malformed_offer(rendezvous_url: &str, host_id: &str, authorization: &str) {
    let (mut socket, request_id, room_id) = join_raw_room(rendezvous_url, host_id, authorization).await;
    socket
        .send(&SignalMessage::SignalOffer {
            protocol: SIGNAL_PROTOCOL,
            request_id,
            room_id: room_id.clone(),
            sdp: "malformed SDP for deterministic local handshake failure".to_owned(),
        })
        .await
        .expect("send malformed local offer");
    loop {
        match tokio::time::timeout(Duration::from_secs(10), socket.receive())
            .await
            .expect("Host closed malformed-offer room")
            .expect("receive Host room cleanup")
        {
            SignalMessage::RoomClose { room_id: closed, .. } if closed == room_id => break,
            _ => continue,
        }
    }
    socket.close().await;
}

async fn close_room_before_offer(rendezvous_url: &str, host_id: &str, authorization: &str) {
    let (mut socket, _, room_id) = join_raw_room(rendezvous_url, host_id, authorization).await;
    let close_request = Uuid::new_v4();
    socket
        .send(&SignalMessage::RoomClose {
            protocol: SIGNAL_PROTOCOL,
            request_id: close_request,
            room_id: room_id.clone(),
        })
        .await
        .expect("close local room before offer");
    loop {
        match tokio::time::timeout(Duration::from_secs(5), socket.receive())
            .await
            .expect("rendezvous acknowledged explicit room close")
            .expect("receive room-close acknowledgement")
        {
            SignalMessage::RoomClose {
                request_id,
                room_id: closed,
                ..
            } if request_id == close_request && closed == room_id => break,
            _ => continue,
        }
    }
    socket.close().await;
}

async fn join_raw_room(rendezvous_url: &str, host_id: &str, authorization: &str) -> (SignalSocket, Uuid, String) {
    let mut socket = SignalSocket::connect(rendezvous_url)
        .await
        .expect("open local Terminal signaling socket");
    let request = join_message(host_id.to_owned(), authorization.to_owned(), false);
    let request_id = request.request_id();
    socket.send(&request).await.expect("join local Host");
    let room_id = match tokio::time::timeout(Duration::from_secs(10), socket.receive())
        .await
        .expect("Host accepted local room")
        .expect("receive JoinAccepted")
    {
        SignalMessage::JoinAccepted {
            request_id: response_id,
            room_id,
            ..
        } if response_id == request_id => room_id,
        _ => panic!("unexpected local join response"),
    };
    (socket, request_id, room_id)
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing child environment value {name}"))
}

struct TestSigner {
    key: Ed25519KeyPair,
    public_key: Vec<u8>,
    host_id: String,
    fingerprint: String,
}

impl TestSigner {
    fn new(pkcs8: &[u8]) -> Self {
        let key = Ed25519KeyPair::from_pkcs8(pkcs8).expect("parse ephemeral Host key");
        let public_key = key.public_key().as_ref().to_vec();
        Self {
            host_id: server_id(&public_key),
            fingerprint: identity_fingerprint(&public_key),
            public_key,
            key,
        }
    }
}

impl HostIdentitySigner for TestSigner {
    fn public_key(&self) -> &[u8] {
        &self.public_key
    }
    fn server_id(&self) -> &str {
        &self.host_id
    }
    fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    fn sign(&self, transcript: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.key.sign(transcript).as_ref().to_vec())
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

struct RemoveFileOnDrop(PathBuf);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

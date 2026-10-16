use std::{
    collections::HashMap,
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{rand::SystemRandom, signature::{Ed25519KeyPair, KeyPair}};
use shocking_vrc_rendezvous::{
    config::{HostAuthorization, LoadedConfig, RoomConfig, ServerConfig, TlsConfig, TurnConfig},
    server::run_with_shutdown,
};
use shocking_vrc_rtc::{
    HostIdentitySigner, IceServerConfig, TerminalRtcConfig, accept_host_room, identity_fingerprint,
    close_host_room, join_terminal_room, register_host, server_id,
};
use tokio::sync::oneshot;
use uuid::Uuid;
use zeroize::Zeroizing;

const TEST_NAME: &str = "localhost_two_process_identity_and_datachannel";
const REGISTRATION_KEY: &str = "ephemeral-local-proof-registration-key";
const INVITE_TOKEN: &str = "ephemeral-local-proof-invite-token";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn localhost_two_process_identity_and_datachannel() {
    if let Ok(role) = std::env::var("SHOCKINGVRC_RTC_PROOF_ROLE") {
        run_child(&role).await;
        return;
    }

    let key_document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("ephemeral Host key");
    let key = Ed25519KeyPair::from_pkcs8(key_document.as_ref()).expect("parse ephemeral Host key");
    let host_id = server_id(key.public_key().as_ref());
    let fingerprint = identity_fingerprint(key.public_key().as_ref());
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
        rooms: RoomConfig { ttl_seconds: 90, max_candidates_per_peer: 64, max_signal_frame_bytes: 64 * 1024 },
        turn: TurnConfig {
            enabled: false,
            uris: Vec::new(),
            realm: String::new(),
            secret_file: String::new(),
            credential_ttl_seconds: 60,
        },
        hosts: HashMap::from([(
            host_id.clone(),
            HostAuthorization {
                registration_key: REGISTRATION_KEY.into(),
                invite_token: INVITE_TOKEN.into(),
            },
        )]),
        turn_secret: None,
        config_dir: PathBuf::from(std::env::temp_dir()),
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let rendezvous = tokio::spawn(run_with_shutdown(config, async move { let _ = shutdown_rx.await; }));
    let ready_path = std::env::temp_dir().join(format!("shockingvrc-rtc-proof-{}.ready", Uuid::new_v4()));
    let _cleanup = RemoveFileOnDrop(ready_path.clone());
    let mut host = ChildGuard(
        Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env("SHOCKINGVRC_RTC_PROOF_ROLE", "host")
            .env("SHOCKINGVRC_RTC_PROOF_RENDEZVOUS", &rendezvous_url)
            .env("SHOCKINGVRC_RTC_PROOF_HOST_ID", &host_id)
            .env("SHOCKINGVRC_RTC_PROOF_PIN", &fingerprint)
            .env("SHOCKINGVRC_RTC_PROOF_PRIVATE_KEY", private_key)
            .env("SHOCKINGVRC_RTC_PROOF_READY", &ready_path)
            .env("SHOCKINGVRC_RTC_PROOF_REGISTRATION_KEY", REGISTRATION_KEY)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start separate Host process"),
    );

    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if ready_path.exists() { break; }
            if let Some(status) = host.0.try_wait().expect("poll Host child") {
                panic!("Host child exited before registering: {status}");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Host registered with local rendezvous");

    tokio::time::sleep(Duration::from_secs(75)).await;
    assert!(
        host.0.try_wait().expect("poll idle Host child").is_none(),
        "Host registration must remain available while waiting for the first Terminal room"
    );

    run_terminal_child(&rendezvous_url, &host_id, &fingerprint, "valid").await;
    run_terminal_child(&rendezvous_url, &host_id, &"00".repeat(32), "wrong_pin").await;

    let host_status = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(status) = host.0.try_wait().expect("poll Host child") { break status; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Host process completed both local proof scenarios");
    assert!(host_status.success(), "Host child process failed: {host_status}");

    let _ = shutdown_tx.send(());
    rendezvous.await.expect("rendezvous task join").expect("local rendezvous shutdown");
}

async fn run_terminal_child(rendezvous_url: &str, host_id: &str, fingerprint: &str, mode: &str) {
    let output = tokio::task::spawn_blocking({
        let rendezvous_url = rendezvous_url.to_owned();
        let host_id = host_id.to_owned();
        let fingerprint = fingerprint.to_owned();
        let mode = mode.to_owned();
        move || {
            Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env("SHOCKINGVRC_RTC_PROOF_ROLE", "terminal")
                .env("SHOCKINGVRC_RTC_PROOF_RENDEZVOUS", rendezvous_url)
                .env("SHOCKINGVRC_RTC_PROOF_HOST_ID", host_id)
                .env("SHOCKINGVRC_RTC_PROOF_PIN", fingerprint)
                .env("SHOCKINGVRC_RTC_PROOF_MODE", mode)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
        }
    }).await.expect("wait for Terminal child")
        .expect("run separate Terminal process");
    assert!(output.success(), "Terminal child process failed: {output}");
}

async fn run_child(role: &str) {
    let rendezvous_url = std::env::var("SHOCKINGVRC_RTC_PROOF_RENDEZVOUS").expect("rendezvous URL");
    let host_id = std::env::var("SHOCKINGVRC_RTC_PROOF_HOST_ID").expect("Host ID");
    let fingerprint = std::env::var("SHOCKINGVRC_RTC_PROOF_PIN").expect("Host pin");
    let udp_addrs = vec!["127.0.0.1:0".to_owned()];
    match role {
        "host" => {
            let key_bytes = URL_SAFE_NO_PAD.decode(std::env::var("SHOCKINGVRC_RTC_PROOF_PRIVATE_KEY").expect("ephemeral test key"))
                .expect("decode ephemeral test key");
            let signer = TestSigner::new(&key_bytes);
            let server_epoch = Uuid::new_v4();
            let mut signaling = register_host(
                &rendezvous_url,
                &signer,
                Zeroizing::new(std::env::var("SHOCKINGVRC_RTC_PROOF_REGISTRATION_KEY").expect("registration key")),
                server_epoch,
            ).await.expect("Host registration");
            fs::write(std::env::var("SHOCKINGVRC_RTC_PROOF_READY").expect("ready marker"), "registered")
                .expect("write readiness marker");
            let mut session = tokio::time::timeout(Duration::from_secs(90), accept_host_room(
                &mut signaling, &signer, &[], udp_addrs.clone(),
            )).await.expect("first room negotiation").expect("first Host room");
            let (mut inbound, _outbound) = session.channels.take().expect("Host API channel adapter");
            let request_bytes = tokio::time::timeout(Duration::from_secs(10), inbound.recv())
                .await.expect("Terminal JSON frame").expect("receive Terminal JSON");
            let request: serde_json::Value = serde_json::from_slice(&request_bytes).expect("Terminal JSON");
            assert_eq!(request["type"], "proof.request");
            session.peer.send_json(br#"{"type":"proof.response","ok":true}"#).await.expect("send Host JSON response");
            let ack_bytes = tokio::time::timeout(Duration::from_secs(10), inbound.recv())
                .await.expect("Terminal JSON acknowledgement").expect("receive Terminal acknowledgement");
            let ack: serde_json::Value = serde_json::from_slice(&ack_bytes).expect("Terminal acknowledgement JSON");
            assert_eq!(ack["type"], "proof.ack");
            let room_id = session.room_id;
            let request_id = session.request_id;
            session.channels.close(&session.peer).await;
            close_host_room(&mut signaling, request_id, room_id).await.expect("close first signaling room");
            let second = tokio::time::timeout(Duration::from_secs(35), accept_host_room(
                &mut signaling, &signer, &[], udp_addrs,
            )).await.expect("second room identity rejection");
            assert!(second.is_err(), "wrong Terminal pin must not complete Host identity binding");
        }
        "terminal" => {
            let mode = std::env::var("SHOCKINGVRC_RTC_PROOF_MODE").expect("Terminal proof mode");
            let config = TerminalRtcConfig {
                rendezvous_url,
                host_id,
                invite_token: Zeroizing::new(INVITE_TOKEN.into()),
                expected_fingerprint: fingerprint,
                ice_servers: Vec::<IceServerConfig>::new(),
                udp_addrs,
                force_relay: false,
                resume_session_id: None,
            };
            if mode == "valid" {
                let mut session = tokio::time::timeout(Duration::from_secs(35), join_terminal_room(config))
                    .await.expect("Terminal WebRTC negotiation").expect("pinned Host identity verification");
                let (mut inbound, outbound) = session.channels.take().expect("Terminal API channel adapter");
                outbound.send(br#"{"type":"proof.request","value":1}"#.to_vec()).await.expect("send Terminal JSON");
                let response_bytes = tokio::time::timeout(Duration::from_secs(10), inbound.recv())
                    .await.expect("Host JSON frame").expect("receive Host JSON");
                let response: serde_json::Value = serde_json::from_slice(&response_bytes).expect("Host JSON");
                assert_eq!(response["type"], "proof.response");
                assert_eq!(response["ok"], true);
                session.peer.send_json(br#"{"type":"proof.ack"}"#).await.expect("acknowledge Host JSON");
                tokio::time::sleep(Duration::from_millis(100)).await;
                session.channels.close(&session.peer).await;
            } else {
                let error = match tokio::time::timeout(Duration::from_secs(35), join_terminal_room(config))
                    .await.expect("wrong pin rejection") {
                    Ok(session) => {
                        session.channels.close(&session.peer).await;
                        panic!("wrong Host pin must be rejected");
                    }
                    Err(error) => error,
                };
                assert!(error.to_string().contains("fingerprint does not match"), "unexpected rejection: {error}");
            }
        }
        _ => panic!("unknown proof child role"),
    }
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
    fn public_key(&self) -> &[u8] { &self.public_key }
    fn server_id(&self) -> &str { &self.host_id }
    fn fingerprint(&self) -> &str { &self.fingerprint }
    fn sign(&self, transcript: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.key.sign(transcript).as_ref().to_vec())
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) { let _ = self.0.kill(); }
}

struct RemoveFileOnDrop(PathBuf);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
}

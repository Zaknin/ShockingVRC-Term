use super::*;

use crate::{app_state::UiBridge, storage::credentials::TerminalDeviceIdentity};
use shocking_vrc_remote::{HostIdentity, api::test_support, auth::SessionManager};
use shocking_vrc_rendezvous::{
    config::{HostAuthorization, LoadedConfig, RoomConfig, ServerConfig, StunConfig, TlsConfig, TurnConfig},
    pairing::PairingInvitationV1,
    server::run_with_shutdown,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

const ICE_TEST: &str = "transport::direct_wss::rtc_process_loss::terminal_ice_supervisor_packet_loss_matrix";
const TURN_TEST: &str = "transport::direct_wss::rtc_process_loss::terminal_turn_supervisor_packet_loss_matrix";
const ROLE_ENV: &str = "SHOCKINGVRC_RTC_SUPERVISOR_PROCESS_ROLE";
const REGISTRATION_KEY: &str = "isolated-terminal-rtc-supervisor-registration-key";

fn monotonic_ms() -> u128 {
    let uptime = fs::read_to_string("/proc/uptime").expect("Linux monotonic uptime");
    (uptime
        .split_whitespace()
        .next()
        .expect("uptime seconds")
        .parse::<f64>()
        .expect("numeric uptime")
        * 1000.0) as u128
}

fn write_event(path: &Path, mut value: serde_json::Value) {
    static EVENT_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = EVENT_WRITE_LOCK.lock().expect("serialize process event append");
    value["pid"] = json!(std::process::id());
    value["monotonic_ms"] = json!(monotonic_ms());
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open process event log");
    let mut line = serde_json::to_vec(&value).expect("encode event JSON");
    line.push(b'\n');
    file.write_all(&line).expect("append process event");
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
    after: Duration,
    mut predicate: impl FnMut(&serde_json::Value) -> bool,
    description: &'static str,
) -> serde_json::Value {
    tokio::time::timeout(after, async {
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

fn atomic_json(path: &Path, value: &serde_json::Value) {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec(value).expect("serialize fixture JSON")).expect("write fixture JSON");
    fs::rename(temporary, path).expect("publish fixture JSON atomically");
}

async fn run_host_process() {
    let _ = env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .try_init();
    let root = PathBuf::from(std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_ROOT").unwrap());
    let ready = PathBuf::from(std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_HOST_READY").unwrap());
    let config_path = PathBuf::from(std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_HOST_CONFIG").unwrap());
    let stop = PathBuf::from(std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_HOST_STOP").unwrap());
    let events = PathBuf::from(std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_HOST_EVENTS").unwrap());
    let identity = Arc::new(HostIdentity::ephemeral_test_host());
    let epoch = Uuid::new_v4();
    atomic_json(
        &ready,
        &json!({
            "host_id": identity.server_id(),
            "fingerprint": identity.fingerprint(),
            "public_key": URL_SAFE_NO_PAD.encode(identity.public_key()),
            "spki_der": URL_SAFE_NO_PAD.encode(identity.spki_der()),
            "server_epoch": epoch,
        }),
    );
    write_event(&events, json!({"kind":"host_identity_ready","server_epoch":epoch}));
    tokio::select! {
        _ = async { while !config_path.exists() && !stop.exists() { tokio::time::sleep(Duration::from_millis(25)).await; } } => {},
    }
    if stop.exists() {
        return;
    }
    let config: serde_json::Value = serde_json::from_slice(&fs::read(&config_path).expect("read isolated Host config"))
        .expect("parse isolated Host config");
    let _ = fs::remove_file(&config_path);
    let rendezvous_url = config["rendezvous_url"].as_str().expect("Rendezvous URL").to_owned();
    let registration_key = config["registration_key"]
        .as_str()
        .expect("ephemeral registration key")
        .to_owned();
    let session_path = root.join("isolated-host-trust.json");
    let sessions =
        Arc::new(SessionManager::load_from_path(epoch, session_path.clone()).expect("isolated Host trust store"));

    let invitation_id = Uuid::new_v4();
    let pairing_secret = [0x53_u8; 32];
    sessions
        .create_invitation(invitation_id, Sha256::digest(pairing_secret).into())
        .expect("create one-time test invitation");
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
        direct_host: None,
        direct_port: None,
        rendezvous_url: Some(rendezvous_url.clone()),
        ice_server_urls: Vec::new(),
        invitation_id,
        pairing_secret: URL_SAFE_NO_PAD.encode(pairing_secret),
        route_token: Some(shocking_vrc_rendezvous::pairing::create_route_token(
            registration_key.as_bytes(),
            identity.server_id(),
            invitation_id,
            now + 900,
        )),
        issued_at: now,
        expires_at: now + 900,
        signature: String::new(),
    };
    invitation.signature = URL_SAFE_NO_PAD.encode(
        identity
            .sign(&invitation.signing_bytes())
            .expect("sign isolated invitation"),
    );
    let mut ready_value: serde_json::Value =
        serde_json::from_slice(&fs::read(&ready).expect("read Host identity metadata"))
            .expect("parse Host identity metadata");
    ready_value["invitation"] = json!(invitation.to_uri(now).expect("serialize signed RTC invitation"));
    ready_value["rendezvous_url"] = json!(rendezvous_url);
    atomic_json(&ready, &ready_value);

    let api_state = test_support::production_api_state(
        Arc::new(test_support::SnapshotBackend::default()),
        Arc::clone(&sessions),
        Arc::clone(&identity),
        epoch,
    );
    let monitor_stop = stop.clone();
    let monitor_events = events.clone();
    let monitor_sessions = Arc::clone(&sessions);
    let monitor = tokio::spawn(async move {
        let mut previous_phase = String::new();
        let mut previous_contact = None;
        while !monitor_stop.exists() {
            let status = monitor_sessions.status();
            let phase = status.phase.as_str().to_owned();
            if phase != previous_phase {
                write_event(
                    &monitor_events,
                    json!({
                        "kind":"host_phase", "phase":phase, "generation":status.transport_generation,
                    }),
                );
                previous_phase = phase;
            }
            if status.last_contact != previous_contact && status.last_contact.is_some() {
                write_event(&monitor_events, json!({"kind":"host_authenticated_activity"}));
                previous_contact = status.last_contact;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let mut signaling =
        shocking_vrc_rtc::register_host(&rendezvous_url, identity.as_ref(), Zeroizing::new(registration_key), epoch)
            .await
            .expect("register isolated Host with Rendezvous");
    write_event(
        &events,
        json!({"kind":"host_registered","host_id":identity.server_id(),"server_epoch":epoch}),
    );
    let stop_wait = async {
        while !stop.exists() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    tokio::pin!(stop_wait);
    loop {
        let mut session = tokio::select! {
            _ = &mut stop_wait => break,
            result = shocking_vrc_rtc::accept_host_room(
                &mut signaling, identity.as_ref(), &[], vec!["127.0.0.1:0".to_owned()],
            ) => match result {
                Ok(session) => session,
                Err(error) if error.is_room_local() => {
                    write_event(&events, json!({"kind":"host_room_setup_failed","error":error.to_string()}));
                    continue;
                }
                Err(error) => {
                    write_event(&events, json!({"kind":"host_signaling_error","class":format!("{error:?}")}));
                    break;
                }
            }
        };
        let room_id = session.room_id;
        let request_id = session.request_id;
        let room_text = room_id.to_string();
        let candidate_pair = session.ice_candidate_pair.clone().unwrap_or_default();
        let (inbound, outbound) = session.channels.take_optimized().expect("Host DataChannel API adapter");
        write_event(
            &events,
            json!({
                "kind":"host_room_established", "room_id":room_id,
                "candidate_pair":candidate_pair,
                "ice_servers_configured":session.ice_servers_configured,
                "turn_credentials_received":session.turn_credentials_received,
            }),
        );
        let serve =
            shocking_vrc_remote::api::data_channel_session_optimized(api_state.clone(), &room_text, inbound, outbound);
        tokio::pin!(serve);
        let end_reason = loop {
            tokio::select! {
                _ = &mut stop_wait => break "stop",
                _ = &mut serve => break "datachannel_closed",
                closed = signaling.wait_for_room_close(room_id) => match closed {
                    Ok(()) => break "room_close",
                    Err(shocking_vrc_rtc::SessionError::Signal(shocking_vrc_rtc::SignalError::Timeout(_))) => continue,
                    Err(error) => {
                        write_event(&events, json!({"kind":"host_room_signal_error","room_id":room_id,"class":format!("{error:?}")}));
                        break "signaling_lost";
                    }
                }
            }
        };
        drop(serve);
        session.channels.close(&session.peer).await;
        write_event(&events, json!({"kind":"host_room_ended","room_id":room_id,"reason":end_reason}));
        if stop.exists() {
            break;
        }
        if let Err(error) = shocking_vrc_rtc::close_host_room(&mut signaling, request_id, room_id).await {
            write_event(
                &events,
                json!({"kind":"host_room_cleanup_failed","room_id":room_id,"class":format!("{error:?}")}),
            );
            break;
        }
        write_event(&events, json!({"kind":"host_room_released","room_id":room_id}));
    }
    signaling.socket.close().await;
    monitor.abort();
    let _ = monitor.await;
    drop(api_state);
    drop(sessions);
    let _ = fs::remove_file(session_path);
    write_event(&events, json!({"kind":"host_stopped"}));
}

async fn run_terminal_process() {
    let _ = env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .try_init();
    let ready_path = PathBuf::from(std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_HOST_READY").unwrap());
    let stop_path = PathBuf::from(std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_TERMINAL_STOP").unwrap());
    let events = PathBuf::from(std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_TERMINAL_EVENTS").unwrap());
    let force_relay = std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_FORCE_RELAY").unwrap() == "1";
    let host: serde_json::Value = serde_json::from_slice(&fs::read(ready_path).expect("Host invitation metadata"))
        .expect("parse Host invitation metadata");
    let invitation_uri = host["invitation"].as_str().expect("signed test invitation");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_secs();
    let invitation = PairingInvitationV1::parse_uri(invitation_uri, now).expect("verify isolated signed invitation");
    let authorization = invitation
        .route_token
        .as_deref()
        .expect("signed rendezvous route authorization")
        .to_owned();
    let profile = ProfileDto {
        id: Uuid::new_v4().to_string(),
        label: "Isolated RTC process Host".into(),
        label_is_custom: true,
        host_display_name: Some("Isolated RTC process Host".into()),
        host: None,
        port: None,
        fingerprint: host["fingerprint"].as_str().unwrap().to_owned(),
        server_id: host["host_id"].as_str().unwrap().to_owned(),
        connection_mode: "traversal_only".into(),
        rendezvous_url: host["rendezvous_url"].as_str().unwrap_or_default().to_owned(),
        ice_server_urls: Vec::new(),
        force_relay,
    };
    let terminal_identity = TerminalDeviceIdentity::ephemeral_for_test();
    let (pending, _) = begin_connection(profile, Some(Zeroizing::new(authorization.clone())), None)
        .await
        .expect("production Terminal ICE traversal and signed Host identity validation");
    let initial_candidate_pair = pending.socket.ice_candidate_pair().unwrap_or_default();
    let connection = finish_device_auth(pending, &terminal_identity, Some(&invitation))
        .await
        .expect("production passwordless auth over RTC and initial authoritative snapshot");
    assert_eq!(connection.transport_name(), "webrtc_datachannel");
    let session_id = connection.secrets.session_id.clone();
    let server_epoch = connection.identity.server_epoch.clone();
    let grant = connection.device_grant.to_string();
    let initial_revision = connection.initial_snapshot.bridge_revision;
    write_event(
        &events,
        json!({
            "kind":"authenticated", "transport":"webrtc_datachannel", "session_id":session_id,
            "server_epoch":server_epoch, "generation":connection.secrets.generation,
            "room_id":connection.socket.room_id(), "candidate_pair":initial_candidate_pair,
            "snapshot_revision":initial_revision,
            "turn_credentials_received":connection.socket.ice_status().2,
        }),
    );

    let updates = Arc::new(UiBridge::default());
    let (_actions, actions) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let attempt_counter = Arc::new(AtomicU64::new(0));
    let resume_events = events.clone();
    let resume_grant = grant.clone();
    let resume_authorization = authorization.clone();
    let resume_attempts = Arc::clone(&attempt_counter);
    let resume: Box<ResumeOperation> = Box::new(move |current, cancel| {
        let event_path = resume_events.clone();
        let grant = resume_grant.clone();
        let authorization = resume_authorization.clone();
        let attempts = Arc::clone(&resume_attempts);
        let expected_session = current.secrets.session_id.clone();
        let expected_epoch = current.identity.server_epoch.clone();
        Box::pin(async move {
            let attempt_events = event_path.clone();
            let connect: Box<PendingConnector> = Box::new(move |profile, resume_session_id| {
                let attempt_events = attempt_events.clone();
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                write_event(&attempt_events, json!({"kind":"resume_attempt","attempt":attempt}));
                let device_grant = Zeroizing::new(grant.clone());
                let rendezvous_authorization = Zeroizing::new(authorization.clone());
                Box::pin(async move {
                    let result =
                        begin_connection(profile.clone(), Some(rendezvous_authorization), Some(resume_session_id))
                            .await;
                    if let Ok((pending, identity)) = &result {
                        write_event(
                            &attempt_events,
                            json!({
                                "kind":"resume_transport_established","attempt":attempt,
                                "room_id":pending.socket.room_id(),
                                "candidate_pair":pending.socket.ice_candidate_pair(),
                                "pinned_identity":identity.server_id == profile.server_id,
                                "device_grant_present":!device_grant.is_empty(),
                            }),
                        );
                    } else {
                        let failure_class = result.as_ref().err().map(ToString::to_string).unwrap_or_default();
                        write_event(
                            &attempt_events,
                            json!({
                                "kind":"resume_transport_failed","attempt":attempt,
                                "error":failure_class,
                            }),
                        );
                    }
                    result
                })
            });
            let result = reconnect_and_resume_with(current, cancel, connect).await;
            if let Ok((socket, identity, secrets, snapshot, _snapshot_seq, _capabilities)) = &result {
                write_event(
                    &event_path,
                    json!({
                        "kind":"resume_success", "session_id":secrets.session_id,
                        "same_session":secrets.session_id == expected_session,
                        "server_epoch":identity.server_epoch,
                        "same_epoch":identity.server_epoch == expected_epoch,
                        "generation":secrets.generation,
                        "room_id":socket.room_id(),
                        "candidate_pair":socket.ice_candidate_pair(),
                        "snapshot_revision":snapshot.bridge_revision,
                    }),
                );
            }
            result
        })
    });
    let mut session_task = tokio::spawn(run_session_with_resume(
        connection,
        actions,
        Arc::clone(&updates),
        cancel.clone(),
        resume,
    ));
    let mut last_phase = String::new();
    let mut last_heartbeat = None;
    let mut last_snapshot_revision = initial_revision;
    loop {
        if let Some(state) = updates.latest_connection_for_test().await {
            if state.phase != last_phase {
                write_event(&events, json!({"kind":"terminal_phase","phase":state.phase}));
                last_phase = state.phase.clone();
            }
            if state.last_heartbeat != last_heartbeat {
                if state.last_heartbeat.is_some() {
                    write_event(&events, json!({"kind":"terminal_authenticated_activity"}));
                }
                last_heartbeat = state.last_heartbeat;
            }
        }
        if let Some(snapshot) = updates.latest_snapshot_for_test().await
            && snapshot.state.bridge_revision != last_snapshot_revision
        {
            write_event(&events, json!({"kind":"snapshot","revision":snapshot.state.bridge_revision}));
            last_snapshot_revision = snapshot.state.bridge_revision;
        }
        if stop_path.exists() {
            cancel.cancel();
            break;
        }
        if session_task.is_finished() {
            write_event(&events, json!({"kind":"session_task_finished"}));
            while !stop_path.exists() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(8), &mut session_task).await;
    write_event(&events, json!({"kind":"terminal_stopped"}));
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_child(child: &mut ChildGuard, after: Duration, label: &'static str) {
    tokio::time::timeout(after, async {
        loop {
            if child.0.try_wait().expect("poll child process").is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out stopping {label} process"));
}

struct RendezvousGuard {
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), String>>>,
}

impl Drop for RendezvousGuard {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl RendezvousGuard {
    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            tokio::time::timeout(Duration::from_secs(8), task)
                .await
                .expect("Rendezvous graceful shutdown timeout")
                .expect("join isolated Rendezvous task")
                .expect("isolated Rendezvous shutdown");
        }
    }
}

struct HostTerminalPair {
    host: ChildGuard,
    terminal: ChildGuard,
    host_ready: PathBuf,
    host_config: PathBuf,
    host_stop: PathBuf,
    terminal_stop: PathBuf,
    host_events: PathBuf,
    terminal_events: PathBuf,
    controller_events: PathBuf,
    root: PathBuf,
    rendezvous: Option<RendezvousGuard>,
}

impl HostTerminalPair {
    async fn start(root: &Path, force_relay: bool) -> Self {
        let root = root.to_owned();
        fs::create_dir_all(&root).expect("create isolated RTC process folder");
        let host_ready = root.join("host-ready.json");
        let host_config = root.join("host-config.json");
        let host_stop = root.join("host-stop");
        let terminal_stop = root.join("terminal-stop");
        let host_events = root.join("host-events.jsonl");
        let terminal_events = root.join("terminal-events.jsonl");
        let controller_events = root.join("controller-events.jsonl");
        let executable = std::env::current_exe().expect("Terminal test binary");
        let host_log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("host.log"))
            .unwrap();
        let mut host_command = Command::new(&executable);
        host_command.args(["--exact", if force_relay { TURN_TEST } else { ICE_TEST }, "--nocapture"]);
        if force_relay {
            host_command.arg("--ignored");
        }
        host_command
            .env(ROLE_ENV, "host")
            .env("SHOCKINGVRC_RTC_SUPERVISOR_ROOT", &root)
            .env("SHOCKINGVRC_RTC_SUPERVISOR_HOST_READY", &host_ready)
            .env("SHOCKINGVRC_RTC_SUPERVISOR_HOST_CONFIG", &host_config)
            .env("SHOCKINGVRC_RTC_SUPERVISOR_HOST_STOP", &host_stop)
            .env("SHOCKINGVRC_RTC_SUPERVISOR_HOST_EVENTS", &host_events)
            .stdout(Stdio::from(host_log))
            .stderr(Stdio::from(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("host.log"))
                    .unwrap(),
            ));
        let host = ChildGuard(host_command.spawn().expect("spawn independent Host RTC process"));
        wait_for_event(
            &host_events,
            Duration::from_secs(10),
            |event| event["kind"] == "host_identity_ready",
            "Host test identity",
        )
        .await;
        let host_identity: serde_json::Value =
            serde_json::from_slice(&fs::read(&host_ready).expect("Host identity metadata"))
                .expect("parse Host identity metadata");

        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve isolated Rendezvous port");
        let address = listener.local_addr().expect("Rendezvous test port");
        drop(listener);
        let rendezvous_url = format!("ws://{address}/ws");
        let turn_uri = std::env::var("SHOCKINGVRC_TEST_TURN_URI").unwrap_or_default();
        let turn_realm = std::env::var("SHOCKINGVRC_TEST_TURN_REALM").unwrap_or_default();
        let turn_secret = std::env::var("SHOCKINGVRC_TEST_TURN_SHARED_SECRET").unwrap_or_default();
        if force_relay {
            assert!(
                turn_uri.starts_with("turn:127.0.0.1:") && turn_uri.contains("?transport=udp"),
                "TURN supervisor run requires the isolated UDP coturn fixture"
            );
            assert!(
                !turn_realm.is_empty() && !turn_secret.is_empty(),
                "TURN fixture credentials are required"
            );
        }
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
                enabled: force_relay,
                uris: if force_relay { vec![turn_uri] } else { Vec::new() },
                realm: if force_relay { turn_realm } else { String::new() },
                secret_file: String::new(),
                credential_ttl_seconds: 600,
            },
            stun: StunConfig::default(),
            hosts: std::collections::HashMap::from([(
                host_identity["host_id"].as_str().unwrap().to_owned(),
                HostAuthorization {
                    registration_key: REGISTRATION_KEY.to_owned(),
                },
            )]),
            turn_secret: force_relay.then(|| turn_secret.into_bytes()),
            config_dir: std::env::temp_dir(),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(run_with_shutdown(config, async move {
            let _ = shutdown_rx.await;
        }));
        let rendezvous = RendezvousGuard {
            shutdown: Some(shutdown_tx),
            task: Some(task),
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if tokio::net::TcpStream::connect(address).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("isolated Rendezvous listener startup");
        atomic_json(
            &host_config,
            &json!({
                "rendezvous_url":rendezvous_url,
                "registration_key":REGISTRATION_KEY,
            }),
        );
        wait_for_event(
            &host_events,
            Duration::from_secs(15),
            |event| event["kind"] == "host_registered",
            "Host registration",
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if serde_json::from_slice::<serde_json::Value>(&fs::read(&host_ready).unwrap_or_default())
                    .is_ok_and(|metadata| metadata["invitation"].as_str().is_some())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("Host published signed invitation metadata");

        let terminal_log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("terminal.log"))
            .unwrap();
        let mut terminal_command = Command::new(&executable);
        terminal_command.args(["--exact", if force_relay { TURN_TEST } else { ICE_TEST }, "--nocapture"]);
        if force_relay {
            terminal_command.arg("--ignored");
        }
        terminal_command
            .env(ROLE_ENV, "terminal")
            .env("SHOCKINGVRC_RTC_SUPERVISOR_HOST_READY", &host_ready)
            .env("SHOCKINGVRC_RTC_SUPERVISOR_TERMINAL_STOP", &terminal_stop)
            .env("SHOCKINGVRC_RTC_SUPERVISOR_TERMINAL_EVENTS", &terminal_events)
            .env("SHOCKINGVRC_RTC_SUPERVISOR_FORCE_RELAY", if force_relay { "1" } else { "0" })
            .stdout(Stdio::from(terminal_log))
            .stderr(Stdio::from(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("terminal.log"))
                    .unwrap(),
            ));
        let terminal = ChildGuard(
            terminal_command
                .spawn()
                .expect("spawn independent Terminal RTC process"),
        );
        let pair = Self {
            host,
            terminal,
            host_ready,
            host_config,
            host_stop,
            terminal_stop,
            host_events,
            terminal_events,
            controller_events,
            root,
            rendezvous: Some(rendezvous),
        };
        let authenticated = wait_for_event(
            &pair.terminal_events,
            Duration::from_secs(90),
            |event| event["kind"] == "authenticated",
            "Terminal RTC authentication",
        )
        .await;
        assert_eq!(authenticated["transport"], "webrtc_datachannel");
        assert_candidate_pair(
            authenticated["candidate_pair"].as_str().unwrap_or_default(),
            force_relay,
            "Terminal initial session",
        );
        wait_for_event(
            &pair.terminal_events,
            Duration::from_secs(5),
            |event| event["kind"] == "terminal_phase" && event["phase"] == "online",
            "Terminal ONLINE state after initial snapshot",
        )
        .await;
        wait_for_event(
            &pair.host_events,
            Duration::from_secs(5),
            |event| event["kind"] == "host_phase" && event["phase"] == "ACTIVE",
            "Host ACTIVE session",
        )
        .await;
        let _ = fs::remove_file(&pair.host_ready);
        let _ = fs::remove_file(&pair.host_config);
        pair
    }

    async fn stop(&mut self) {
        let _ = fs::write(&self.terminal_stop, "stop");
        let _ = fs::write(&self.host_stop, "stop");
        wait_child(&mut self.terminal, Duration::from_secs(12), "Terminal RTC").await;
        wait_child(&mut self.host, Duration::from_secs(12), "Host RTC").await;
        if let Some(rendezvous) = self.rendezvous.take() {
            rendezvous.stop().await;
        }
        for path in [
            &self.host_stop,
            &self.terminal_stop,
            &self.host_ready,
            &self.host_config,
        ] {
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for HostTerminalPair {
    fn drop(&mut self) {
        let _ = fs::write(&self.terminal_stop, "stop");
        let _ = fs::write(&self.host_stop, "stop");
        let _ = fs::remove_file(&self.host_ready);
        let _ = fs::remove_file(&self.host_config);
        let _ = &self.root;
    }
}

fn assert_candidate_pair(pair: &str, force_relay: bool, role: &str) {
    let required = if force_relay {
        "LOCAL RELAY/UDP ↔ REMOTE RELAY/UDP"
    } else {
        "LOCAL HOST/UDP ↔ REMOTE HOST/UDP"
    };
    assert!(pair.contains(required), "{role}: expected {required}, selected {pair}");
}

fn network_interfaces() -> Vec<String> {
    let output = Command::new("ip")
        .args(["-o", "link", "show"])
        .output()
        .expect("list isolated network interfaces");
    assert!(
        output.status.success(),
        "ip link failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(|name| name.trim_end_matches(':').split('@').next().unwrap_or(name).to_owned())
        .collect()
}

struct UdpLossGuard {
    active: bool,
}

impl UdpLossGuard {
    fn install() -> Self {
        let guard = Self { active: true };
        for interface in network_interfaces() {
            let existing = Command::new("tc")
                .args(["qdisc", "show", "dev", &interface])
                .output()
                .expect("inspect isolated qdisc");
            assert!(existing.status.success(), "read {interface} qdisc failed");
            assert!(
                !String::from_utf8_lossy(&existing.stdout)
                    .lines()
                    .any(|line| line.contains("qdisc prio ")),
                "isolated interface {interface} already has the test root qdisc"
            );
            run_tc(&[
                "qdisc", "add", "dev", &interface, "root", "handle", "1:", "prio", "bands", "3",
            ]);
            run_tc(&[
                "qdisc", "add", "dev", &interface, "parent", "1:3", "handle", "30:", "netem", "loss", "100%",
            ]);
            run_tc(&[
                "filter", "add", "dev", &interface, "protocol", "ip", "parent", "1:", "prio", "1", "flower",
                "ip_proto", "udp", "flowid", "1:3",
            ]);
            run_tc(&[
                "filter", "add", "dev", &interface, "protocol", "ipv6", "parent", "1:", "prio", "2", "flower",
                "ip_proto", "udp", "flowid", "1:3",
            ]);
        }
        guard
    }

    fn remove_rules() {
        for interface in network_interfaces() {
            run_tc(&["qdisc", "del", "dev", &interface, "root"]);
        }
    }

    fn restore(mut self) -> Vec<String> {
        Self::remove_rules();
        let states = network_interfaces()
            .into_iter()
            .map(|interface| {
                let output = Command::new("tc")
                    .args(["qdisc", "show", "dev", &interface])
                    .output()
                    .expect("verify restored qdisc state");
                assert!(output.status.success(), "verify qdisc state for {interface} failed");
                let state = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                assert!(
                    !state
                        .lines()
                        .any(|line| line.contains("qdisc prio ") || line.contains("qdisc netem ")),
                    "UDP loss qdisc remained installed on {interface}: {state}"
                );
                format!("{interface}: {state}")
            })
            .collect();
        self.active = false;
        states
    }
}

impl Drop for UdpLossGuard {
    fn drop(&mut self) {
        if self.active {
            for interface in network_interfaces() {
                let _ = Command::new("tc")
                    .args(["qdisc", "del", "dev", &interface, "root"])
                    .output();
            }
        }
    }
}

fn run_tc(args: &[&str]) {
    let output = Command::new("tc")
        .args(args)
        .output()
        .expect("run isolated tc fault injection");
    assert!(
        output.status.success(),
        "tc {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn tc_dropped_packets() -> usize {
    network_interfaces()
        .into_iter()
        .map(|interface| {
            let output = Command::new("tc")
                .args(["-s", "qdisc", "show", "dev", &interface])
                .output()
                .expect("read qdisc counters");
            assert!(output.status.success(), "tc counters for {interface} failed");
            let mut in_netem = false;
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if line.contains("qdisc netem ") {
                    in_netem = true;
                    continue;
                }
                if in_netem {
                    let fields = line.split_whitespace().collect::<Vec<_>>();
                    if let Some(index) = fields
                        .iter()
                        .position(|field| field.trim_start_matches('(') == "dropped")
                    {
                        return fields[index + 1]
                            .trim_end_matches(|c: char| !c.is_ascii_digit())
                            .parse::<usize>()
                            .expect("numeric netem drop count");
                    }
                }
            }
            0
        })
        .sum()
}

async fn inject_loss(pair: &HostTerminalPair, duration_seconds: u64, expect_recovery: bool, force_relay: bool) -> bool {
    let prior = read_events(&pair.terminal_events);
    let prior_resume = prior.iter().filter(|event| event["kind"] == "resume_success").count();
    let prior_attempts = prior.iter().filter(|event| event["kind"] == "resume_attempt").count();
    let prior_reconnecting = prior
        .iter()
        .filter(|event| event["kind"] == "terminal_phase" && event["phase"] == "reconnecting")
        .count();
    let last_activity = prior
        .iter()
        .filter(|event| event["kind"] == "terminal_authenticated_activity")
        .filter_map(|event| event["monotonic_ms"].as_u64())
        .max()
        .unwrap_or_default();
    let fresh_activity = wait_for_event(
        &pair.terminal_events,
        Duration::from_secs(5),
        |event| {
            event["kind"] == "terminal_authenticated_activity"
                && event["monotonic_ms"].as_u64().unwrap_or_default() > last_activity
        },
        "fresh authenticated Terminal activity before UDP loss",
    )
    .await;
    let last_authenticated_activity_ms = fresh_activity["monotonic_ms"]
        .as_u64()
        .expect("fresh authenticated activity timestamp") as u128;
    let original_authentication = prior
        .iter()
        .find(|event| event["kind"] == "authenticated")
        .expect("initial authenticated Terminal session");
    let guard = UdpLossGuard::install();
    let start = monotonic_ms();
    write_event(
        &pair.controller_events,
        json!({
            "kind":"fault_start","duration_seconds":duration_seconds,
            "transport":if force_relay {"turn_udp"} else {"ice_udp"},
            "last_authenticated_activity_ms":last_authenticated_activity_ms,
            "liveness_deadline_ms":last_authenticated_activity_ms + HEARTBEAT_TIMEOUT.as_millis(),
        }),
    );
    if duration_seconds >= 10 {
        let detection = wait_for_event(
            &pair.terminal_events,
            Duration::from_secs(15),
            |event| {
                event["kind"] == "terminal_phase"
                    && event["phase"] == "reconnecting"
                    && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= start
            },
            "Terminal ONLINE to RECONNECTING after UDP loss",
        )
        .await;
        let terminal_detect_ms = detection["monotonic_ms"].as_u64().unwrap() as u128;
        write_event(
            &pair.controller_events,
            json!({"kind":"terminal_loss_detected","duration_seconds":duration_seconds,"elapsed_ms":terminal_detect_ms - start}),
        );
        write_event(
            &pair.controller_events,
            json!({
                "kind":"liveness_detection_timing","duration_seconds":duration_seconds,
                "last_authenticated_activity_ms":last_authenticated_activity_ms,
                "detected_ms":terminal_detect_ms,
                "detected_after_activity_ms":terminal_detect_ms.saturating_sub(last_authenticated_activity_ms),
                "configured_deadline_ms":HEARTBEAT_TIMEOUT.as_millis(),
            }),
        );
        if let Some(host_reserved) = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(event) = read_events(&pair.host_events).into_iter().find(|event| {
                    event["kind"] == "host_phase"
                        && event["phase"] == "RESERVED"
                        && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= start
                }) {
                    break event;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .ok()
        {
            let reserved_ms = host_reserved["monotonic_ms"].as_u64().unwrap() as u128;
            write_event(
                &pair.controller_events,
                json!({"kind":"host_reserved","duration_seconds":duration_seconds,"elapsed_ms":reserved_ms - start}),
            );
        } else {
            write_event(
                &pair.controller_events,
                json!({"kind":"host_reserved_missing","duration_seconds":duration_seconds}),
            );
        }
    }
    tokio::time::sleep(
        Duration::from_secs(duration_seconds).saturating_sub(Duration::from_millis((monotonic_ms() - start) as u64)),
    )
    .await;
    let dropped = tc_dropped_packets();
    let qdisc_state_after_restore = guard.restore();
    let restored = monotonic_ms();
    write_event(
        &pair.controller_events,
        json!({
            "kind":"fault_restored","duration_seconds":duration_seconds,
            "actual_ms":restored - start,"qdisc_dropped_packets":dropped,
            "qdisc_state_after_restore":qdisc_state_after_restore,
        }),
    );
    assert!(
        dropped > 0,
        "qdisc counters did not prove UDP packets were dropped for {duration_seconds}s"
    );

    // At 9 seconds the 10-second liveness deadline can fall just before or after restoration.
    // Accept a healthy same-transport response or a bounded same-session resume with fresh state.
    if expect_recovery || duration_seconds == 9 {
        let boundary_case = duration_seconds == 9;
        let outcome = tokio::time::timeout(Duration::from_secs(35), async {
            loop {
                let events = read_events(&pair.terminal_events);
                if let Some(event) = events.iter().rev().find(|event| {
                    event["kind"] == "resume_success"
                        && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= start
                }) {
                    break json!({"outcome":"resume_success","event":event});
                }
                if events.iter().any(|event| event["kind"] == "session_task_finished") {
                    break json!({"outcome":"terminal_finished"});
                }
                if boundary_case {
                    let reconnecting_at = events
                        .iter()
                        .filter(|event| {
                            (event["kind"] == "resume_attempt"
                                || (event["kind"] == "terminal_phase" && event["phase"] == "reconnecting"))
                                && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= start
                        })
                        .filter_map(|event| event["monotonic_ms"].as_u64())
                        .min();
                    if let Some(activity) = events.iter().find(|event| {
                        event["kind"] == "terminal_authenticated_activity"
                            && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= restored
                            && reconnecting_at.is_none_or(|reconnecting| {
                                event["monotonic_ms"].as_u64().unwrap_or_default() < reconnecting
                            })
                    }) {
                        break json!({"outcome":"survived","activity":activity});
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        if let Ok(observed) = outcome {
            if observed["outcome"] == "survived" {
                let activity_ms = observed["activity"]["monotonic_ms"]
                    .as_u64()
                    .expect("post-restoration authenticated activity") as u128;
                write_event(
                    &pair.controller_events,
                    json!({
                        "kind":"liveness_survived_with_activity","duration_seconds":duration_seconds,
                        "last_authenticated_activity_ms":last_authenticated_activity_ms,
                        "post_restore_activity_ms":activity_ms,
                        "activity_after_restore_ms":activity_ms.saturating_sub(restored),
                        "no_reconnect_before_activity":true,
                    }),
                );
                write_event(
                    &pair.controller_events,
                    json!({
                        "kind":"short_loss_outcome","duration_seconds":duration_seconds,
                        "outcome":"survived_without_reconnect",
                        "elapsed_after_restore_ms":activity_ms.saturating_sub(restored),
                        "last_authenticated_activity_ms":last_authenticated_activity_ms,
                    }),
                );
                return true;
            }
            if observed["outcome"] != "resume_success" {
                write_event(
                    &pair.controller_events,
                    json!({"kind":"recovery_timeout","duration_seconds":duration_seconds,"outcome":observed["outcome"]}),
                );
                return false;
            }
            let resumed = observed["event"].clone();
            let detection_ms = read_events(&pair.terminal_events)
                .iter()
                .filter(|event| {
                    (event["kind"] == "resume_attempt"
                        || (event["kind"] == "terminal_phase" && event["phase"] == "reconnecting"))
                        && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= start
                })
                .filter_map(|event| event["monotonic_ms"].as_u64())
                .min()
                .map(u128::from);
            let detected_after_activity_ms =
                detection_ms.map(|detected| detected.saturating_sub(last_authenticated_activity_ms));
            let detection_timing_valid = detected_after_activity_ms.is_some_and(|elapsed| {
                elapsed + 500 >= HEARTBEAT_TIMEOUT.as_millis() && elapsed <= HEARTBEAT_TIMEOUT.as_millis() + 1_500
            });
            if let (Some(detected_ms), Some(detected_after_activity_ms)) = (detection_ms, detected_after_activity_ms) {
                write_event(
                    &pair.controller_events,
                    json!({
                        "kind":"liveness_detection_timing","duration_seconds":duration_seconds,
                        "last_authenticated_activity_ms":last_authenticated_activity_ms,
                        "detected_ms":detected_ms,
                        "detected_after_activity_ms":detected_after_activity_ms,
                        "configured_deadline_ms":HEARTBEAT_TIMEOUT.as_millis(),
                        "within_tolerance":detection_timing_valid,
                    }),
                );
            }
            let session_resumed = resumed["kind"] == "resume_success"
                && resumed["same_session"] == true
                && resumed["same_epoch"] == true
                && resumed["snapshot_revision"].as_u64().unwrap_or_default() > 0;
            let boundary_isolated = !boundary_case
                || (detection_timing_valid
                    && resumed["generation"].as_u64().unwrap_or_default()
                        > original_authentication["generation"].as_u64().unwrap_or_default()
                    && resumed["room_id"] != original_authentication["room_id"]
                    && resumed["snapshot_revision"].as_u64().unwrap_or_default()
                        > original_authentication["snapshot_revision"]
                            .as_u64()
                            .unwrap_or_default());
            let success = session_resumed && boundary_isolated;
            write_event(
                &pair.controller_events,
                json!({
                    "kind":"recovery_outcome","duration_seconds":duration_seconds,"success":success,
                    "session_id":resumed["session_id"],"generation":resumed["generation"],
                    "snapshot_revision":resumed["snapshot_revision"],
                    "elapsed_after_restore_ms":(resumed["monotonic_ms"].as_u64().unwrap_or_default() as u128).saturating_sub(restored),
                    "last_authenticated_activity_ms":last_authenticated_activity_ms,
                    "detected_after_activity_ms":detected_after_activity_ms,
                    "detection_timing_valid":detection_timing_valid,
                }),
            );
            if success {
                assert_candidate_pair(
                    resumed["candidate_pair"].as_str().unwrap_or_default(),
                    force_relay,
                    "Terminal resumed session",
                );
                if boundary_case {
                    let old_room = original_authentication["room_id"].as_str().expect("initial room id");
                    let resumed_room = resumed["room_id"].as_str().expect("resumed room id");
                    let old_room_release = wait_for_event(
                        &pair.host_events,
                        Duration::from_secs(5),
                        |event| event["kind"] == "host_room_released" && event["room_id"] == old_room,
                        "release of the original Host room before resumed room ownership",
                    )
                    .await;
                    let new_room_established = wait_for_event(
                        &pair.host_events,
                        Duration::from_secs(5),
                        |event| event["kind"] == "host_room_established" && event["room_id"] == resumed_room,
                        "Host establishment of the resumed room",
                    )
                    .await;
                    assert!(
                        old_room_release["monotonic_ms"].as_u64() < new_room_established["monotonic_ms"].as_u64(),
                        "old room must be released before the new Host room is established",
                    );
                    let reserved = wait_for_event(
                        &pair.host_events,
                        Duration::from_secs(5),
                        |event| {
                            event["kind"] == "host_phase"
                                && event["phase"] == "RESERVED"
                                && event["monotonic_ms"].as_u64().unwrap_or_default() as u128 >= start
                        },
                        "Host reservation after the old transport closed",
                    )
                    .await;
                    let active_after_resume = wait_for_event(
                        &pair.host_events,
                        Duration::from_secs(5),
                        |event| {
                            event["kind"] == "host_phase"
                                && event["phase"] == "ACTIVE"
                                && event["generation"].as_u64().unwrap_or_default()
                                    > original_authentication["generation"].as_u64().unwrap_or_default()
                        },
                        "Host ACTIVE state on the new transport generation",
                    )
                    .await;
                    assert!(
                        reserved["monotonic_ms"].as_u64() < active_after_resume["monotonic_ms"].as_u64(),
                        "Host must transition RESERVED before the resumed generation becomes ACTIVE",
                    );
                    let resumed_snapshot_revision = resumed["snapshot_revision"]
                        .as_u64()
                        .expect("authoritative resumed snapshot revision");
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
                        "fresh authoritative Snapshot from the resumed transport",
                    )
                    .await;
                }
                let online = wait_for_event(
                    &pair.terminal_events,
                    Duration::from_secs(5),
                    |event| {
                        event["kind"] == "terminal_phase"
                            && event["phase"] == "online"
                            && event["monotonic_ms"].as_u64().unwrap_or_default()
                                >= resumed["monotonic_ms"].as_u64().unwrap_or_default()
                    },
                    "Terminal ONLINE after fresh resume snapshot",
                )
                .await;
                write_event(
                    &pair.controller_events,
                    json!({"kind":"terminal_online_after_resume","duration_seconds":duration_seconds,"monotonic_ms":online["monotonic_ms"]}),
                );
            }
            return success;
        }
        write_event(
            &pair.controller_events,
            json!({"kind":"recovery_timeout","duration_seconds":duration_seconds}),
        );
        false
    } else if duration_seconds == 40 {
        let unavailable = tokio::time::timeout(Duration::from_secs(8), async {
            wait_for_event(
                &pair.terminal_events,
                Duration::from_secs(8),
                |event| event["kind"] == "terminal_phase" && event["phase"] == "host_unavailable",
                "bounded Terminal recovery failure",
            )
            .await
        })
        .await;
        let no_resume = unavailable.is_ok();
        let after = read_events(&pair.terminal_events);
        let _ = tokio::time::sleep(Duration::from_secs(2)).await;
        let resumed_after_restore = read_events(&pair.terminal_events)
            .iter()
            .filter(|event| event["kind"] == "resume_success")
            .count()
            > prior_resume;
        write_event(
            &pair.controller_events,
            json!({
                "kind":"bounded_outcome","duration_seconds":duration_seconds,
                "host_unavailable":no_resume,"resume_attempts":after.iter().filter(|event| event["kind"] == "resume_attempt").count() - prior_attempts,
                "resumed_after_restore":resumed_after_restore,
            }),
        );
        no_resume && !resumed_after_restore
    } else {
        tokio::time::sleep(Duration::from_secs(4)).await;
        let events = read_events(&pair.terminal_events);
        let reconnecting = events
            .iter()
            .filter(|event| event["kind"] == "terminal_phase" && event["phase"] == "reconnecting")
            .count()
            > prior_reconnecting;
        let attempts = events.iter().filter(|event| event["kind"] == "resume_attempt").count() > prior_attempts;
        let resumed = events.iter().filter(|event| event["kind"] == "resume_success").count() > prior_resume;
        let outcome = if resumed {
            "same_session_resume"
        } else if reconnecting || attempts {
            "reconnect_started"
        } else {
            "survived"
        };
        write_event(
            &pair.controller_events,
            json!({
                "kind":"short_loss_outcome","duration_seconds":duration_seconds,"outcome":outcome,
                "resume_attempts":events.iter().filter(|event| event["kind"] == "resume_attempt").count() - prior_attempts,
            }),
        );
        if resumed {
            wait_for_event(
                &pair.terminal_events,
                Duration::from_secs(5),
                |event| event["kind"] == "terminal_phase" && event["phase"] == "online",
                "Terminal recovered after short UDP loss",
            )
            .await;
        }
        if duration_seconds == 5 {
            !reconnecting && !attempts
        } else {
            !reconnecting && !attempts
        }
    }
}

async fn run_transport_suite(root: &Path, force_relay: bool) -> Vec<(u64, bool)> {
    let mut results = Vec::new();
    let durations = std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_MATRIX_DURATIONS")
        .map(|value| {
            let parsed = value
                .split(',')
                .map(|item| item.parse::<u64>().expect("numeric packet-loss duration"))
                .collect::<Vec<_>>();
            assert!(!parsed.is_empty(), "packet-loss duration selection must not be empty");
            assert!(parsed.iter().all(|duration| [5, 9, 15, 25, 40].contains(duration)));
            parsed
        })
        .unwrap_or_else(|_| vec![5, 9, 15, 25, 40]);
    for duration in durations {
        let case_root = root.join(format!("matrix-{duration}s"));
        let mut pair = HostTerminalPair::start(&case_root, force_relay).await;
        let expect_recovery = matches!(duration, 15 | 25);
        let success = inject_loss(&pair, duration, expect_recovery, force_relay).await;
        pair.stop().await;
        results.push((duration, success));
    }
    let repeat_cycles = std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_REPEAT_CYCLES")
        .map(|value| value.parse::<usize>().expect("numeric repeat-cycle count"))
        .unwrap_or(10);
    if repeat_cycles == 0 {
        return results;
    }
    let cycles_root = root.join("repeat-cycles");
    let mut cycles = HostTerminalPair::start(&cycles_root, force_relay).await;
    let host_pid = cycles.host.0.id();
    let terminal_pid = cycles.terminal.0.id();
    let before = read_events(&cycles.terminal_events)
        .iter()
        .filter(|event| event["kind"] == "resume_success")
        .count();
    let mut cycle_results = true;
    for cycle in 1..=repeat_cycles {
        let ok = inject_loss(&cycles, 15, true, force_relay).await;
        cycle_results &= ok;
        write_event(
            &cycles.controller_events,
            json!({"kind":"repeat_cycle","cycle":cycle,"success":ok}),
        );
        assert_eq!(
            cycles.host.0.id(),
            host_pid,
            "Host process must remain long-running through cycle {cycle}"
        );
        assert_eq!(
            cycles.terminal.0.id(),
            terminal_pid,
            "Terminal process must remain long-running through cycle {cycle}"
        );
        if !ok {
            break;
        }
    }
    let after = read_events(&cycles.terminal_events)
        .iter()
        .filter(|event| event["kind"] == "resume_success")
        .count();
    let repeat_count = after - before;
    write_event(
        &cycles.controller_events,
        json!({"kind":"repeat_summary","host_pid":host_pid,"terminal_pid":terminal_pid,"cycles":repeat_count}),
    );
    cycles.stop().await;
    results.push((10_015, cycle_results && repeat_count == repeat_cycles));
    results
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_ice_supervisor_packet_loss_matrix() {
    if let Ok(role) = std::env::var(ROLE_ENV) {
        if role == "host" {
            run_host_process().await;
        } else if role == "terminal" {
            run_terminal_process().await;
        } else {
            panic!("unknown RTC process role");
        }
        return;
    }
    if std::env::var_os("SHOCKINGVRC_TEST_RTC_SUPERVISOR_NETEM").is_none() || !cfg!(target_os = "linux") {
        eprintln!("skip independent RTC supervisor packet-loss fixture outside isolated Linux netem runner");
        return;
    }
    let root = PathBuf::from(std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_EVIDENCE").expect("evidence root"))
        .join(format!("ice-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create ICE supervisor evidence folder");
    let results = run_transport_suite(&root, false).await;
    let failed = results
        .iter()
        .filter(|(_, success)| !success)
        .map(|(duration, _)| duration.to_string())
        .collect::<Vec<_>>();
    assert!(
        failed.is_empty(),
        "ICE Terminal supervisor packet-loss acceptance failures: {failed:?}; evidence={}",
        root.display()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the isolated loopback coturn UDP fixture"]
async fn terminal_turn_supervisor_packet_loss_matrix() {
    if let Ok(role) = std::env::var(ROLE_ENV) {
        if role == "host" {
            run_host_process().await;
        } else if role == "terminal" {
            run_terminal_process().await;
        } else {
            panic!("unknown RTC process role");
        }
        return;
    }
    if std::env::var_os("SHOCKINGVRC_TEST_RTC_SUPERVISOR_NETEM").is_none() || !cfg!(target_os = "linux") {
        eprintln!("skip independent TURN supervisor packet-loss fixture outside isolated Linux netem runner");
        return;
    }
    let root = PathBuf::from(std::env::var("SHOCKINGVRC_RTC_SUPERVISOR_EVIDENCE").expect("evidence root"))
        .join(format!("turn-udp-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create TURN supervisor evidence folder");
    let results = run_transport_suite(&root, true).await;
    let failed = results
        .iter()
        .filter(|(_, success)| !success)
        .map(|(duration, _)| duration.to_string())
        .collect::<Vec<_>>();
    assert!(
        failed.is_empty(),
        "TURN/UDP Terminal supervisor packet-loss acceptance failures: {failed:?}; evidence={}",
        root.display()
    );
}

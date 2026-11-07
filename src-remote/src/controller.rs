use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use sha2::{Digest, Sha256};
use shocking_vrc_rtc::{IceServerConfig, accept_host_room, close_host_room, register_host, request_turn_credentials};
use tokio::{
    net::TcpListener,
    sync::{Mutex, OnceCell, watch},
    task::JoinHandle,
    time::{Sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroizing;

async fn wait_for_turn_refresh(timer: &mut Option<Pin<Box<Sleep>>>) {
    match timer {
        Some(timer) => timer.as_mut().await,
        None => std::future::pending().await,
    }
}

use crate::{
    RemoteBackend,
    auth::{AuthError, SessionManager, SessionPhase},
    identity::{HostIdentity, IdentityError},
    settings::RemoteAccessSettings,
    tls::{self, TlsConfigError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerState {
    Disabled,
    Starting,
    Listening,
    Degraded,
    Stopping,
    Error,
}

impl ListenerState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "DISABLED",
            Self::Starting => "STARTING",
            Self::Listening => "LISTENING",
            Self::Degraded => "DEGRADED",
            Self::Stopping => "STOPPING",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostRemoteStatus {
    pub listener: ListenerState,
    pub address: Option<SocketAddr>,
    pub fingerprint: Option<String>,
    pub server_id: Option<String>,
    pub paired_terminals: Vec<crate::auth::TrustedTerminal>,
    pub registration_key_configured: bool,
    pub session: SessionPhase,
    pub traversal: TraversalState,
    pub last_contact_seconds_ago: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalState {
    Disabled,
    Registering,
    Registered,
    Active,
    Error,
}

impl TraversalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "DISABLED",
            Self::Registering => "REGISTERING",
            Self::Registered => "REGISTERED",
            Self::Active => "ACTIVE",
            Self::Error => "ERROR",
        }
    }
}

struct RunningServer {
    generation: Uuid,
    handle: axum_server::Handle<SocketAddr>,
    task: JoinHandle<()>,
}

struct RunningTraversal {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

pub struct HostRemoteController {
    backend: Arc<dyn RemoteBackend>,
    settings: Arc<RwLock<RemoteAccessSettings>>,
    epoch: Uuid,
    identity: OnceCell<Arc<HostIdentity>>,
    sessions: OnceCell<Arc<SessionManager>>,
    api_state: OnceCell<crate::api::ApiState>,
    lifecycle: Mutex<()>,
    shutting_down: AtomicBool,
    running: Mutex<Option<RunningServer>>,
    traversal: Mutex<Option<RunningTraversal>>,
    active_invitation: Mutex<Option<Uuid>>,
    status: watch::Sender<HostRemoteStatus>,
}

impl HostRemoteController {
    pub fn new(backend: Arc<dyn RemoteBackend>, settings: RemoteAccessSettings) -> Arc<Self> {
        let initial = HostRemoteStatus {
            listener: if settings.remote_enabled {
                ListenerState::Starting
            } else {
                ListenerState::Disabled
            },
            address: None,
            fingerprint: None,
            server_id: None,
            paired_terminals: Vec::new(),
            registration_key_configured: false,
            session: SessionPhase::Free,
            traversal: TraversalState::Disabled,
            last_contact_seconds_ago: None,
            error: None,
        };
        let (status, _) = watch::channel(initial);
        Arc::new(Self {
            backend,
            settings: Arc::new(RwLock::new(settings)),
            epoch: Uuid::new_v4(),
            identity: OnceCell::new(),
            sessions: OnceCell::new(),
            api_state: OnceCell::new(),
            lifecycle: Mutex::new(()),
            shutting_down: AtomicBool::new(false),
            running: Mutex::new(None),
            traversal: Mutex::new(None),
            active_invitation: Mutex::new(None),
            status,
        })
    }

    pub async fn initialize(self: &Arc<Self>) {
        let _lifecycle = self.lifecycle.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        if let Err(error) = self.ensure_identity().await {
            self.set_error(error.to_string());
            return;
        }
        if let Err(error) = self.ensure_sessions().await {
            self.set_error(error.to_string());
            return;
        }
        let key_status = tokio::task::spawn_blocking(crate::identity::load_rendezvous_registration_key).await;
        if let Ok(Ok(key)) = key_status {
            self.set_registration_key_configured(key.is_some());
        }
        let settings = self
            .settings
            .read()
            .map(|settings| settings.clone())
            .unwrap_or_default();
        if settings.remote_enabled {
            if let Err(error) = self.start_listener(settings.clone()).await {
                self.set_error(error);
            }
        } else {
            self.set_listener_state(ListenerState::Disabled, None, None);
        }
        let traversal_settings = self.settings.read().map(|settings| settings.clone()).ok();
        if let Some(settings) = traversal_settings {
            if settings.remote_enabled && settings.traversal_enabled {
                if let Err(error) = self.start_traversal(settings).await {
                    self.set_traversal_error(error);
                }
            }
        }
        self.refresh_status();
    }

    pub fn status(&self) -> HostRemoteStatus {
        let mut status = self.status.borrow().clone();
        if let Some(sessions) = self.sessions.get() {
            let current = sessions.status();
            status.session = current.phase;
            status.last_contact_seconds_ago = current.last_contact.map(|at| at.elapsed().as_secs());
            status.paired_terminals = sessions.trusted_terminals();
        }
        status
    }

    pub async fn update_settings(self: &Arc<Self>, settings: RemoteAccessSettings) -> Result<(), String> {
        settings.validate().map_err(|error| error.to_string())?;
        let _lifecycle = self.lifecycle.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err("Remote service is shutting down".into());
        }
        let persisted = settings.clone();
        tokio::task::spawn_blocking(move || persisted.save())
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;

        let old = {
            let mut current = self
                .settings
                .write()
                .map_err(|_| "remote settings unavailable".to_owned())?;
            std::mem::replace(&mut *current, settings.clone())
        };
        if !settings.remote_enabled {
            if let Some(sessions) = self.sessions.get() {
                let _ = sessions.cancel_all_invitations();
                sessions.kick();
            }
            *self.active_invitation.lock().await = None;
            self.stop_traversal().await;
            self.stop_listener().await;
            self.set_listener_state(ListenerState::Disabled, None, None);
            return Ok(());
        }

        if !old.remote_enabled || old.listen_port != settings.listen_port || self.running.lock().await.is_none() {
            if let Some(sessions) = self.sessions.get() {
                sessions.kick();
            }
            self.stop_listener().await;
            if let Err(error) = self.start_listener(settings.clone()).await {
                self.set_error(error.clone());
                return Err(error);
            }
        }
        if old.traversal_enabled != settings.traversal_enabled
            || old.rendezvous_url != settings.rendezvous_url
            || old.ice_server_urls != settings.ice_server_urls
        {
            self.stop_traversal().await;
            if settings.traversal_enabled {
                if let Err(error) = self.start_traversal(settings.clone()).await {
                    self.set_traversal_error(error.clone());
                    return Err(error);
                }
            }
        }
        if settings.traversal_enabled && self.traversal.lock().await.is_none() {
            if let Err(error) = self.start_traversal(settings.clone()).await {
                self.set_traversal_error(error.clone());
                return Err(error);
            }
        }
        self.refresh_status();
        Ok(())
    }

    pub async fn create_pairing_invitation(&self) -> Result<String, String> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err("Remote service is shutting down".into());
        }
        let settings = self.settings.read().map_err(|_| "remote settings unavailable")?.clone();
        if !settings.remote_enabled {
            return Err("Enable Remote Access before creating an invitation".into());
        }
        let direct_host = (!settings.direct_host.trim().is_empty()).then(|| settings.direct_host.trim().to_owned());
        let rendezvous_url = (settings.traversal_enabled && !settings.rendezvous_url.trim().is_empty())
            .then(|| settings.rendezvous_url.trim().to_owned());
        if direct_host.is_none() && rendezvous_url.is_none() {
            return Err(
                "Set a public direct Host address or enable rendezvous traversal before inviting a Terminal".into(),
            );
        }
        let identity = self.ensure_identity().await.map_err(|error| error.to_string())?;
        let sessions = self.ensure_sessions().await.map_err(|error| error.to_string())?;
        let invitation_id = Uuid::new_v4();
        let secret = random::<[u8; 32]>();
        let expires_at = sessions
            .create_invitation(invitation_id, Sha256::digest(secret).into())
            .map_err(|error| error.to_string())?;
        let route_token = if rendezvous_url.is_some() {
            let registration = tokio::task::spawn_blocking(crate::identity::load_rendezvous_registration_key)
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Set the rendezvous registration key before creating an ICE invitation".to_owned())?;
            Some(shocking_vrc_rendezvous::pairing::create_route_token(
                registration.as_bytes(),
                identity.server_id(),
                invitation_id,
                expires_at,
            ))
        } else {
            None
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs();
        let mut invitation = shocking_vrc_rendezvous::pairing::PairingInvitationV1 {
            version: 1,
            host_id: identity.server_id().to_owned(),
            host_public_key: URL_SAFE_NO_PAD.encode(identity.public_key()),
            host_spki_der: URL_SAFE_NO_PAD.encode(identity.spki_der()),
            host_fingerprint: identity.fingerprint().to_owned(),
            direct_host: direct_host.clone(),
            direct_port: direct_host.as_ref().map(|_| settings.listen_port),
            rendezvous_url,
            ice_server_urls: settings
                .ice_server_urls
                .iter()
                .map(|value| value.trim())
                .filter(|value| value.starts_with("stun:") || value.starts_with("stuns:"))
                .map(str::to_owned)
                .collect(),
            invitation_id,
            pairing_secret: URL_SAFE_NO_PAD.encode(secret),
            route_token,
            issued_at: expires_at.saturating_sub(15 * 60),
            expires_at,
            signature: String::new(),
        };
        invitation.signature = URL_SAFE_NO_PAD.encode(
            identity
                .sign(&invitation.signing_bytes())
                .map_err(|error| error.to_string())?,
        );
        let uri = invitation.to_uri(now).map_err(|error| error.to_string())?;
        *self.active_invitation.lock().await = Some(invitation_id);
        Ok(uri)
    }

    pub async fn cancel_pairing_invitation(&self) -> Result<(), String> {
        if let Some(invitation_id) = self.active_invitation.lock().await.take() {
            if let Some(sessions) = self.sessions.get() {
                sessions
                    .cancel_invitation(invitation_id)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    pub async fn revoke_terminal(&self, device_id: String) -> Result<(), String> {
        let sessions = self.ensure_sessions().await.map_err(|error| error.to_string())?;
        tokio::task::spawn_blocking(move || sessions.revoke_terminal(&device_id))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        self.refresh_status();
        Ok(())
    }

    pub fn paired_terminals(&self) -> Vec<crate::auth::TrustedTerminal> {
        self.sessions
            .get()
            .map(|sessions| sessions.trusted_terminals())
            .unwrap_or_default()
    }

    pub async fn disconnect_client(&self) {
        if let Some(sessions) = self.sessions.get() {
            sessions.kick();
        }
        self.refresh_status();
    }

    pub fn report_ui_error(&self, error: impl Into<String>) {
        let mut status = self.status.borrow().clone();
        status.error = Some(error.into());
        self.status.send_replace(status);
    }

    pub fn report_startup_error(&self, error: impl Into<String>) {
        self.set_error(error);
    }

    pub async fn shutdown(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        self.shutting_down.store(true, Ordering::Release);
        if let Some(sessions) = self.sessions.get() {
            sessions.kick();
            let _ = sessions.cancel_all_invitations();
        }
        *self.active_invitation.lock().await = None;
        self.stop_traversal().await;
        self.stop_listener().await;
        self.set_listener_state(ListenerState::Disabled, None, None);
    }

    async fn ensure_identity(&self) -> Result<Arc<HostIdentity>, IdentityError> {
        self.identity
            .get_or_try_init(|| async {
                tokio::task::spawn_blocking(HostIdentity::load_or_create)
                    .await
                    .map_err(|error| IdentityError::Storage(std::io::Error::other(error.to_string())))?
                    .map(Arc::new)
            })
            .await
            .cloned()
    }

    async fn ensure_sessions(&self) -> Result<Arc<SessionManager>, AuthError> {
        self.sessions
            .get_or_try_init(|| async {
                tokio::task::spawn_blocking({
                    let epoch = self.epoch;
                    move || SessionManager::load(epoch)
                })
                .await
                .map_err(|_| AuthError::State)?
                .map(Arc::new)
            })
            .await
            .cloned()
    }

    async fn ensure_api_state(&self) -> Result<crate::api::ApiState, String> {
        self.api_state
            .get_or_try_init(|| async {
                let identity = self.ensure_identity().await.map_err(|error| error.to_string())?;
                let sessions = self.ensure_sessions().await.map_err(|error| error.to_string())?;
                Ok::<_, String>(crate::api::ApiState::new(
                    Arc::clone(&self.backend),
                    sessions,
                    identity,
                    Arc::clone(&self.settings),
                    self.epoch,
                ))
            })
            .await
            .cloned()
    }

    async fn start_traversal(self: &Arc<Self>, settings: RemoteAccessSettings) -> Result<(), String> {
        if !settings.remote_enabled {
            return Err("Enable Remote Access before traversal".to_owned());
        }
        if self.running.lock().await.is_none() {
            return Err("Remote Access listener is not ready".to_owned());
        }
        let key = tokio::task::spawn_blocking(crate::identity::load_rendezvous_registration_key)
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Set the rendezvous registration key in Host Setup before enabling traversal".to_owned())?;
        let identity = self.ensure_identity().await.map_err(|error| error.to_string())?;
        let cancel = CancellationToken::new();
        let child_cancel = cancel.clone();
        let controller = Arc::downgrade(self);
        let server_epoch = self.epoch;
        let task = tokio::spawn(async move {
            log::info!("[remote] traversal worker started");
            run_traversal(controller, settings, identity, key, server_epoch, child_cancel).await;
            log::info!("[remote] traversal worker stopped");
        });
        *self.traversal.lock().await = Some(RunningTraversal { cancel, task });
        self.set_traversal_state(TraversalState::Registering);
        Ok(())
    }

    async fn stop_traversal(&self) {
        if let Some(running) = self.traversal.lock().await.take() {
            running.cancel.cancel();
            if timeout(Duration::from_secs(3), running.task).await.is_err() {}
        }
        self.set_traversal_state(TraversalState::Disabled);
    }

    pub async fn set_traversal_registration_key(self: &Arc<Self>, key: String) -> Result<(), String> {
        let key = Zeroizing::new(key.trim().to_owned());
        let configured = !key.is_empty();
        tokio::task::spawn_blocking(move || crate::identity::save_rendezvous_registration_key(key.as_str()))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        self.set_registration_key_configured(configured);
        let _lifecycle = self.lifecycle.lock().await;
        if let Some(sessions) = self.sessions.get() {
            sessions.kick();
            let _ = sessions.cancel_all_invitations();
        }
        *self.active_invitation.lock().await = None;
        self.stop_traversal().await;
        let settings = self.settings.read().map_err(|_| "remote settings unavailable")?.clone();
        if settings.remote_enabled && settings.traversal_enabled {
            self.start_traversal(settings).await?;
        }
        Ok(())
    }

    /// Serves the existing API v1 session over a verified DataChannel.
    pub async fn serve_data_channel(
        &self,
        room_id: &str,
        inbound: tokio::sync::mpsc::Receiver<Vec<u8>>,
        outbound: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) -> Result<(), String> {
        if self.shutting_down.load(Ordering::Acquire)
            || !self
                .settings
                .read()
                .map_err(|_| "remote settings unavailable")?
                .remote_enabled
        {
            return Err("Remote Access is disabled".to_owned());
        }
        let state = self.ensure_api_state().await?;
        crate::api::data_channel_session(state, room_id, inbound, outbound).await;
        Ok(())
    }

    async fn start_listener(self: &Arc<Self>, settings: RemoteAccessSettings) -> Result<(), String> {
        self.set_listener_state(ListenerState::Starting, None, None);
        let identity = self.ensure_identity().await.map_err(|error| error.to_string())?;
        let _sessions = self.ensure_sessions().await.map_err(|error| error.to_string())?;
        let tls_config = tls::server_config(&identity).map_err(|error: TlsConfigError| error.to_string())?;
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), settings.listen_port);
        let listener = TcpListener::bind(address).await.map_err(|error| error.to_string())?;
        let bound_address = listener.local_addr().map_err(|error| error.to_string())?;
        let std_listener = listener.into_std().map_err(|error| error.to_string())?;
        let server = axum_server::from_tcp_rustls(std_listener, tls_config).map_err(|error| error.to_string())?;
        let handle = axum_server::Handle::new();
        let handle_for_task = handle.clone();
        let generation = Uuid::new_v4();
        let controller = Arc::downgrade(self);
        let api_state = self.ensure_api_state().await?;
        let router = crate::api::router(api_state);
        let task = tokio::spawn(async move {
            let result = server
                .handle(handle_for_task)
                .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                .await;
            if let Some(controller) = controller.upgrade() {
                let message = match result {
                    Ok(()) => "listener task exited unexpectedly".to_owned(),
                    Err(error) => error.to_string(),
                };
                controller.server_exited(generation, message).await;
            }
        });
        *self.running.lock().await = Some(RunningServer {
            generation,
            handle,
            task,
        });
        self.set_listener_state(ListenerState::Listening, Some(bound_address), None);
        if let Some(identity) = self.identity.get() {
            let mut status = self.status.borrow().clone();
            status.fingerprint = Some(identity.fingerprint().to_owned());
            status.server_id = Some(identity.server_id().to_owned());
            self.status.send_replace(status);
        }
        self.refresh_status();
        Ok(())
    }

    async fn stop_listener(&self) {
        self.set_listener_state(ListenerState::Stopping, None, None);
        let running = self.running.lock().await.take();
        if let Some(mut running) = running {
            running.handle.graceful_shutdown(Some(Duration::from_secs(2)));
            if timeout(Duration::from_secs(3), &mut running.task).await.is_err() {
                running.task.abort();
                let _ = running.task.await;
            }
        }
    }

    async fn server_exited(&self, generation: Uuid, error: String) {
        let mut running = self.running.lock().await;
        if running.as_ref().is_some_and(|server| server.generation == generation) {
            running.take();
            drop(running);
            self.set_error(format!("Remote listener stopped: {error}"));
        }
    }

    fn set_listener_state(&self, listener: ListenerState, address: Option<SocketAddr>, error: Option<String>) {
        let mut status = self.status.borrow().clone();
        status.listener = listener;
        status.address = address;
        status.error = error;
        self.status.send_replace(status);
    }

    fn set_error(&self, error: impl Into<String>) {
        self.set_listener_state(ListenerState::Error, None, Some(error.into()));
    }

    fn refresh_status(&self) {
        let mut status = self.status.borrow().clone();
        if let Some(identity) = self.identity.get() {
            status.fingerprint = Some(identity.fingerprint().to_owned());
            status.server_id = Some(identity.server_id().to_owned());
        }
        if let Some(sessions) = self.sessions.get() {
            status.paired_terminals = sessions.trusted_terminals();
            let session = sessions.status();
            status.session = session.phase;
            status.last_contact_seconds_ago = session.last_contact.map(|at| at.elapsed().as_secs());
        }
        status.error = None;
        self.status.send_replace(status);
    }

    fn set_traversal_state(&self, traversal: TraversalState) {
        let mut status = self.status.borrow().clone();
        status.traversal = traversal;
        self.status.send_replace(status);
    }

    fn set_registration_key_configured(&self, configured: bool) {
        let mut status = self.status.borrow().clone();
        status.registration_key_configured = configured;
        self.status.send_replace(status);
    }

    fn set_traversal_error(&self, error: String) {
        let mut status = self.status.borrow().clone();
        status.traversal = TraversalState::Error;
        status.error = Some(error);
        self.status.send_replace(status);
    }

}

async fn run_traversal(
    controller: std::sync::Weak<HostRemoteController>,
    settings: RemoteAccessSettings,
    identity: Arc<HostIdentity>,
    registration_key: Zeroizing<String>,
    server_epoch: Uuid,
    cancel: CancellationToken,
) {
    log::info!("[remote] connecting to rendezvous for Host registration");
    let ice_servers: Vec<IceServerConfig> = settings
        .ice_server_urls
        .iter()
        .map(|url| IceServerConfig {
            urls: vec![url.clone()],
            username: None,
            credential: None,
        })
        .collect();
    let udp_addrs = vec!["0.0.0.0:0".to_owned()];
    let mut delay = Duration::from_secs(1);
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let registration = tokio::select! {
            _ = cancel.cancelled() => return,
            result = register_host(&settings.rendezvous_url, identity.as_ref(), Zeroizing::new(registration_key.to_string()), server_epoch) => result,
        };
        let mut signaling = match registration {
            Ok(signaling) => {
                log::info!("[remote] rendezvous accepted Host registration");
                delay = Duration::from_secs(1);
                signaling
            }
            Err(error) => {
                log::error!("[remote] rendezvous Host registration failed: {error}");
                if let Some(owner) = controller.upgrade() {
                    owner.set_traversal_error(error.to_string());
                }
                tokio::select! { _ = cancel.cancelled() => return, _ = tokio::time::sleep(delay) => {} }
                delay = (delay * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        if let Some(owner) = controller.upgrade() {
            owner.set_traversal_state(TraversalState::Registered);
        }
        loop {
            log::debug!("[remote] waiting for Terminal signaling room");
            let room = tokio::select! {
                _ = cancel.cancelled() => { signaling.socket.close().await; return; }
                result = accept_host_room(&mut signaling, identity.as_ref(), &ice_servers, udp_addrs.clone()) => result,
            };
            match room {
                Ok(mut session) => {
                    log::info!(
                        "[remote] ICE DataChannel session established room={} request={}",
                        session.room_id,
                        session.request_id
                    );
                    if let Some(owner) = controller.upgrade() {
                        owner.set_traversal_state(TraversalState::Active);
                    }
                    let (inbound, outbound) = match session.channels.take() {
                        Ok(channels) => channels,
                        Err(error) => {
                            if let Some(owner) = controller.upgrade() {
                                owner.set_traversal_error(error.to_string());
                            }
                            session.channels.close(&session.peer).await;
                            continue;
                        }
                    };
                    if let Some(owner) = controller.upgrade() {
                        let room_text = session.room_id.to_string();
                        log::info!("[remote] starting Host API session over DataChannel room={room_text}");
                        let mut serve = Box::pin(owner.serve_data_channel(&room_text, inbound, outbound));
                        let mut turn_refresh_timer = session.turn_refresh_at.map(|at| {
                            Box::pin(tokio::time::sleep(at.saturating_duration_since(tokio::time::Instant::now())))
                        });
                        loop {
                            tokio::select! {
                                _ = cancel.cancelled() => break,
                                _ = &mut serve => break,
                                _ = wait_for_turn_refresh(&mut turn_refresh_timer), if turn_refresh_timer.is_some() => {
                                    match request_turn_credentials(&mut signaling.socket, &room_text).await {
                                        Ok(Some(credentials)) => {
                                            match session.peer.update_turn_credentials(credentials.server).await {
                                                Ok(()) => {
                                                    let delay = credentials.refresh_at.saturating_duration_since(tokio::time::Instant::now());
                                                    log::info!("[remote] renewed TURN credentials for active room={room_text}; next_refresh_in_seconds={}", delay.as_secs());
                                                    turn_refresh_timer = Some(Box::pin(tokio::time::sleep(delay)));
                                                }
                                                Err(error) => {
                                                    log::error!("[remote] could not apply renewed TURN credentials for room={room_text}: {error}");
                                                    turn_refresh_timer = Some(Box::pin(tokio::time::sleep(Duration::from_secs(15))));
                                                }
                                            }
                                        }
                                        Ok(None) => {
                                            log::warn!("[remote] TURN renewal returned no configured TURN service for room={room_text}");
                                            turn_refresh_timer = None;
                                        }
                                        Err(error) => {
                                            log::error!("[remote] TURN credential renewal request failed for room={room_text}: {error}");
                                            turn_refresh_timer = Some(Box::pin(tokio::time::sleep(Duration::from_secs(15))));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let room_id = session.room_id;
                    let request_id = session.request_id;
                    session.channels.close(&session.peer).await;
                    if let Err(error) = close_host_room(&mut signaling, request_id, room_id).await {
                        if let Some(owner) = controller.upgrade() {
                            owner.set_traversal_error(error.to_string());
                        }
                        signaling.socket.close().await;
                        break;
                    }
                    log::info!("[remote] signaling room closed after DataChannel session ended");
                    if let Some(owner) = controller.upgrade() {
                        owner.set_traversal_state(TraversalState::Registered);
                    }
                }
                Err(error) => {
                    log::error!(
                        "[remote] Host ICE room setup failed; closing this signaling registration before retry: {error}"
                    );
                    if let Some(owner) = controller.upgrade() {
                        owner.set_traversal_error(error.to_string());
                    }
                    signaling.socket.close().await;
                    break;
                }
            }
        }
        tokio::select! { _ = cancel.cancelled() => return, _ = tokio::time::sleep(delay) => {} }
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

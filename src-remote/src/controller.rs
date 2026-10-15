use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{
    net::TcpListener,
    sync::{Mutex, OnceCell, watch},
    task::JoinHandle,
    time::timeout,
};
use uuid::Uuid;

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
    pub credentials_configured: bool,
    pub session: SessionPhase,
    pub last_contact_seconds_ago: Option<u64>,
    pub error: Option<String>,
}

struct RunningServer {
    generation: Uuid,
    handle: axum_server::Handle<SocketAddr>,
    task: JoinHandle<()>,
}

pub struct HostRemoteController {
    backend: Arc<dyn RemoteBackend>,
    settings: Arc<RwLock<RemoteAccessSettings>>,
    epoch: Uuid,
    identity: OnceCell<Arc<HostIdentity>>,
    sessions: OnceCell<Arc<SessionManager>>,
    lifecycle: Mutex<()>,
    shutting_down: AtomicBool,
    running: Mutex<Option<RunningServer>>,
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
            credentials_configured: false,
            session: SessionPhase::Free,
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
            lifecycle: Mutex::new(()),
            shutting_down: AtomicBool::new(false),
            running: Mutex::new(None),
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
        let settings = self
            .settings
            .read()
            .map(|settings| settings.clone())
            .unwrap_or_default();
        if settings.remote_enabled {
            if let Err(error) = self.start_listener(settings).await {
                self.set_error(error);
            }
        } else {
            self.set_listener_state(ListenerState::Disabled, None, None);
        }
        self.refresh_status();
    }

    pub fn status(&self) -> HostRemoteStatus {
        let mut status = self.status.borrow().clone();
        if let Some(sessions) = self.sessions.get() {
            let current = sessions.status();
            status.session = current.phase;
            status.last_contact_seconds_ago = current.last_contact.map(|at| at.elapsed().as_secs());
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
                sessions.kick();
            }
            self.stop_listener().await;
            self.set_listener_state(ListenerState::Disabled, None, None);
            return Ok(());
        }

        if !old.remote_enabled || old.listen_port != settings.listen_port || self.running.lock().await.is_none() {
            if let Some(sessions) = self.sessions.get() {
                sessions.kick();
            }
            self.stop_listener().await;
            if let Err(error) = self.start_listener(settings).await {
                self.set_error(error.clone());
                return Err(error);
            }
        }
        self.refresh_status();
        Ok(())
    }

    pub async fn set_credentials(self: &Arc<Self>, username: String, password: String) -> Result<(), String> {
        let _lifecycle = self.lifecycle.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err("Remote service is shutting down".into());
        }
        let sessions = self.ensure_sessions().await.map_err(|error| error.to_string())?;
        tokio::task::spawn_blocking(move || sessions.set_credentials(&username, &password))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        self.refresh_status();
        let settings = self
            .settings
            .read()
            .map_err(|_| "remote settings unavailable".to_owned())?
            .clone();
        if settings.remote_enabled && self.running.lock().await.is_none() {
            if let Err(error) = self.start_listener(settings).await {
                self.set_error(error.clone());
                return Err(error);
            }
        }
        self.clear_error();
        Ok(())
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
        }
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

    async fn start_listener(self: &Arc<Self>, settings: RemoteAccessSettings) -> Result<(), String> {
        self.set_listener_state(ListenerState::Starting, None, None);
        let identity = self.ensure_identity().await.map_err(|error| error.to_string())?;
        let sessions = self.ensure_sessions().await.map_err(|error| error.to_string())?;
        if !sessions.has_credentials() {
            return Err("Set a Host username and password before enabling Remote Access.".into());
        }
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
        let api_state = crate::api::ApiState::new(
            Arc::clone(&self.backend),
            sessions,
            identity.clone(),
            Arc::clone(&self.settings),
            self.epoch,
        );
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
            status.credentials_configured = sessions.has_credentials();
            let session = sessions.status();
            status.session = session.phase;
            status.last_contact_seconds_ago = session.last_contact.map(|at| at.elapsed().as_secs());
        }
        status.error = None;
        self.status.send_replace(status);
    }

    fn clear_error(&self) {
        let mut status = self.status.borrow().clone();
        status.error = None;
        self.status.send_replace(status);
    }
}

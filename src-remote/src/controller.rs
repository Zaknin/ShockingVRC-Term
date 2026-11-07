use std::{
    collections::HashMap,
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
use shocking_vrc_rtc::{
    IceServerConfig, PeerCloseReason, SessionError, SignalError, accept_host_room, close_host_room, register_host,
    request_turn_credentials,
};
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

enum TurnRefreshWait<T> {
    ApiEnded,
    RefreshCompleted(T),
}

async fn wait_for_turn_refresh_or_api_end<S, R>(serve: &mut S, refresh: R) -> TurnRefreshWait<R::Output>
where
    S: std::future::Future + Unpin,
    R: std::future::Future,
{
    tokio::select! {
        _ = serve => TurnRefreshWait::ApiEnded,
        result = refresh => TurnRefreshWait::RefreshCompleted(result),
    }
}

#[cfg(test)]
mod turn_refresh_cancellation_tests {
    use super::{TurnRefreshWait, wait_for_turn_refresh_or_api_end};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::sync::oneshot;
    use tokio::time::{Duration, timeout};

    struct RefreshDrop(Arc<AtomicBool>);

    impl Drop for RefreshDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn host_kick_ends_api_while_turn_renewal_is_pending() {
        let (api_end_tx, api_end_rx) = oneshot::channel();
        let mut serve = Box::pin(async move {
            let _ = api_end_rx.await;
        });
        let (renewal_started_tx, renewal_started_rx) = oneshot::channel();
        let refresh_dropped = Arc::new(AtomicBool::new(false));
        let refresh_guard = RefreshDrop(Arc::clone(&refresh_dropped));
        let refresh = async move {
            let _guard = refresh_guard;
            let _ = renewal_started_tx.send(());
            std::future::pending::<()>().await;
        };
        let kick = tokio::spawn(async move {
            renewal_started_rx.await.expect("renewal started");
            api_end_tx.send(()).expect("Host API ended after kick");
        });

        let result = timeout(Duration::from_secs(1), wait_for_turn_refresh_or_api_end(&mut serve, refresh))
            .await
            .expect("Host Kick must interrupt a blocked TURN renewal");
        assert!(matches!(result, TurnRefreshWait::ApiEnded));
        assert!(
            refresh_dropped.load(Ordering::SeqCst),
            "the renewal future must be dropped with its owning session"
        );
        kick.await.expect("kick signal task");
    }
}

use crate::{
    RemoteBackend,
    auth::{AuthError, InvitationHistoryRecord, SessionEndReason, SessionManager, SessionPhase, TrustedTerminal},
    identity::{HostIdentity, IdentityError},
    settings::{InvitationMode, RemoteAccessSettings, is_valid_direct_host},
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
    pub host_display_name: String,
    pub paired_terminals: Vec<TrustedTerminal>,
    pub active_invitations: Vec<InvitationHistoryRecord>,
    pub invitation_history: Vec<InvitationHistoryRecord>,
    pub registration_key_configured: bool,
    pub session: SessionPhase,
    pub traversal: TraversalState,
    pub ice_servers_configured: Option<bool>,
    pub rendezvous_stun_received: Option<bool>,
    pub turn_credentials_received: Option<bool>,
    pub local_candidates_gathered: Option<bool>,
    pub ice_candidate_pair: Option<String>,
    pub last_contact_seconds_ago: Option<u64>,
    pub error: Option<String>,
    pub notice: Option<String>,
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
    invitation_uris: Mutex<HashMap<Uuid, Zeroizing<String>>>,
    status: watch::Sender<HostRemoteStatus>,
}

struct PreparedInvitation {
    id: Uuid,
    secret_hash: [u8; 32],
    created_at: u64,
    expires_at: u64,
    uri: String,
}

/// Returns whether the selected invitation route is ready, with a message that
/// tells the user which setting or service state needs attention.
pub fn invitation_route_readiness(settings: &RemoteAccessSettings, status: &HostRemoteStatus) -> Result<(), String> {
    if !settings.remote_enabled {
        return Err("Enable Remote Access before creating an invitation".to_owned());
    }
    if status.listener != ListenerState::Listening {
        return Err(format!(
            "Remote listener is {}; wait until it reports LISTENING before creating an invitation",
            status.listener.as_str()
        ));
    }
    if status.server_id.is_none() {
        return Err("Host identity is still loading; wait for the Host ID to appear".to_owned());
    }

    let direct_ready = || {
        if is_valid_direct_host(&settings.direct_host) {
            Ok(())
        } else {
            Err("Enter a valid Direct address (IP or domain) for LAN/Direct invitations".to_owned())
        }
    };
    let traversal_ready = || {
        if !settings.traversal_enabled || settings.rendezvous_url.trim().is_empty() {
            return Err("Enable ICE traversal and enter a Rendezvous URL for Internet invitations".to_owned());
        }
        if !status.registration_key_configured {
            return Err("Save the Rendezvous registration key before creating an Internet invitation".to_owned());
        }
        match status.traversal {
            TraversalState::Registered | TraversalState::Active => Ok(()),
            TraversalState::Registering => {
                Err("Rendezvous is REGISTERING; wait until Host registration completes".to_owned())
            }
            TraversalState::Error => {
                Err("Rendezvous reports ERROR; check the Host policy entry and service status".to_owned())
            }
            TraversalState::Disabled => {
                Err("Rendezvous traversal is DISABLED; enable it for Internet invitations".to_owned())
            }
        }
    };

    match settings.invitation_mode {
        InvitationMode::Direct => direct_ready(),
        InvitationMode::Internet => traversal_ready(),
        InvitationMode::Hybrid => {
            direct_ready()?;
            traversal_ready()
        }
    }
}

fn prepare_signed_invitation(
    identity: &HostIdentity,
    settings: &RemoteAccessSettings,
    invitation_id: Uuid,
    secret: &[u8; 32],
    created_at: u64,
    registration_key: Option<&str>,
) -> Result<PreparedInvitation, String> {
    let includes_direct = matches!(settings.invitation_mode, InvitationMode::Direct | InvitationMode::Hybrid);
    let includes_traversal = matches!(settings.invitation_mode, InvitationMode::Internet | InvitationMode::Hybrid);
    let direct_host =
        (includes_direct && !settings.direct_host.trim().is_empty()).then(|| settings.direct_host.trim().to_owned());
    let rendezvous_url =
        (includes_traversal && settings.traversal_enabled && !settings.rendezvous_url.trim().is_empty())
            .then(|| settings.rendezvous_url.trim().to_owned());
    match settings.invitation_mode {
        InvitationMode::Direct if direct_host.is_none() => {
            return Err("Direct invitation requires a valid Direct Host address".into());
        }
        InvitationMode::Internet if rendezvous_url.is_none() => {
            return Err("Internet invitation requires enabled Rendezvous traversal".into());
        }
        InvitationMode::Hybrid if direct_host.is_none() || rendezvous_url.is_none() => {
            return Err(
                "Hybrid invitation requires both a Direct Host address and enabled Rendezvous traversal".into(),
            );
        }
        _ => {}
    }
    let has_rendezvous_route = rendezvous_url.is_some();
    let expires_at = created_at.saturating_add(15 * 60);
    let route_token = if rendezvous_url.is_some() {
        let registration_key = registration_key
            .ok_or_else(|| "Set the rendezvous registration key before creating an ICE invitation".to_owned())?;
        Some(shocking_vrc_rendezvous::pairing::create_route_token(
            registration_key.as_bytes(),
            identity.server_id(),
            invitation_id,
            expires_at,
        ))
    } else {
        None
    };
    let mut invitation = shocking_vrc_rendezvous::pairing::PairingInvitationV1 {
        version: 1,
        host_id: identity.server_id().to_owned(),
        host_public_key: URL_SAFE_NO_PAD.encode(identity.public_key()),
        host_spki_der: URL_SAFE_NO_PAD.encode(identity.spki_der()),
        host_fingerprint: identity.fingerprint().to_owned(),
        direct_host: direct_host.clone(),
        direct_port: direct_host.as_ref().map(|_| settings.listen_port),
        rendezvous_url,
        ice_server_urls: if has_rendezvous_route {
            settings.ice_server_urls.clone()
        } else {
            Vec::new()
        },
        invitation_id,
        pairing_secret: URL_SAFE_NO_PAD.encode(secret),
        route_token,
        issued_at: created_at,
        expires_at,
        signature: String::new(),
    };
    invitation.signature = URL_SAFE_NO_PAD.encode(
        identity
            .sign(&invitation.signing_bytes())
            .map_err(|error| error.to_string())?,
    );
    let uri = invitation.to_uri(created_at).map_err(|error| error.to_string())?;
    Ok(PreparedInvitation {
        id: invitation_id,
        secret_hash: Sha256::digest(secret).into(),
        created_at,
        expires_at,
        uri,
    })
}

impl HostRemoteController {
    pub fn new(backend: Arc<dyn RemoteBackend>, settings: RemoteAccessSettings) -> Arc<Self> {
        Self::new_with_epoch(backend, settings, Uuid::new_v4())
    }

    fn new_with_epoch(backend: Arc<dyn RemoteBackend>, settings: RemoteAccessSettings, epoch: Uuid) -> Arc<Self> {
        let initial = HostRemoteStatus {
            listener: if settings.remote_enabled {
                ListenerState::Starting
            } else {
                ListenerState::Disabled
            },
            address: None,
            fingerprint: None,
            server_id: None,
            host_display_name: settings.host_display_name.clone(),
            paired_terminals: Vec::new(),
            active_invitations: Vec::new(),
            invitation_history: Vec::new(),
            registration_key_configured: false,
            session: SessionPhase::Free,
            traversal: TraversalState::Disabled,
            ice_servers_configured: None,
            rendezvous_stun_received: None,
            turn_credentials_received: None,
            local_candidates_gathered: None,
            ice_candidate_pair: None,
            last_contact_seconds_ago: None,
            error: None,
            notice: None,
        };
        let (status, _) = watch::channel(initial);
        Arc::new(Self {
            backend,
            settings: Arc::new(RwLock::new(settings)),
            epoch,
            identity: OnceCell::new(),
            sessions: OnceCell::new(),
            api_state: OnceCell::new(),
            lifecycle: Mutex::new(()),
            shutting_down: AtomicBool::new(false),
            running: Mutex::new(None),
            traversal: Mutex::new(None),
            invitation_uris: Mutex::new(HashMap::new()),
            status,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        backend: Arc<dyn RemoteBackend>,
        settings: RemoteAccessSettings,
        epoch: Uuid,
    ) -> Arc<Self> {
        Self::new_with_epoch(backend, settings, epoch)
    }

    #[cfg(test)]
    pub(crate) fn configure_test_runtime(&self, identity: Arc<HostIdentity>, sessions: Arc<SessionManager>) {
        assert!(self.identity.set(Arc::clone(&identity)).is_ok());
        assert!(self.sessions.set(Arc::clone(&sessions)).is_ok());
        let api_state = crate::api::ApiState::new(
            Arc::clone(&self.backend),
            sessions,
            identity,
            Arc::clone(&self.settings),
            self.epoch,
        );
        assert!(self.api_state.set(api_state).is_ok());
    }

    async fn spawn_traversal_worker<F>(&self, spawn: F) -> bool
    where
        F: FnOnce(CancellationToken) -> JoinHandle<()>,
    {
        let mut traversal = self.traversal.lock().await;
        if traversal.as_ref().is_some_and(|running| !running.task.is_finished()) {
            return false;
        }
        if let Some(finished) = traversal.take() {
            let _ = finished.task.await;
        }
        let cancel = CancellationToken::new();
        let task = spawn(cancel.clone());
        *traversal = Some(RunningTraversal { cancel, task });
        true
    }

    /// Starts the production traversal worker with the test's ephemeral registration key.
    #[cfg(test)]
    pub(crate) async fn start_traversal_for_test(self: &Arc<Self>, registration_key: Zeroizing<String>) {
        let settings = self.settings.read().expect("test Host settings lock").clone();
        let identity = self.identity.get().expect("test Host identity configured").clone();
        let controller = Arc::downgrade(self);
        let server_epoch = self.epoch;
        let started = self
            .spawn_traversal_worker(move |cancel| {
                tokio::spawn(run_traversal(
                    controller,
                    settings,
                    identity,
                    registration_key,
                    server_epoch,
                    cancel,
                ))
            })
            .await;
        if started {
            self.set_traversal_state(TraversalState::Registering);
        }
    }

    #[cfg(test)]
    pub(crate) async fn stop_traversal_for_test(&self) {
        self.stop_traversal().await;
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
        if let Ok(settings) = self.settings.read() {
            status.host_display_name = settings.host_display_name.clone();
        }
        if let Some(sessions) = self.sessions.get() {
            let current = sessions.status();
            status.session = current.phase;
            status.last_contact_seconds_ago = current.last_contact.map(|at| at.elapsed().as_secs());
            status.paired_terminals = sessions.trusted_terminals();
            status.active_invitations = sessions.active_invitations();
            status.invitation_history = sessions.invitation_history();
        }
        if let Ok(mut uris) = self.invitation_uris.try_lock() {
            let active = status
                .active_invitations
                .iter()
                .map(|entry| entry.id)
                .collect::<std::collections::HashSet<_>>();
            uris.retain(|id, _| active.contains(id));
        }
        status
    }

    pub async fn update_settings(self: &Arc<Self>, mut settings: RemoteAccessSettings) -> Result<(), String> {
        settings.ice_server_urls =
            shocking_vrc_rendezvous::protocol::normalize_ice_server_uris(&settings.ice_server_urls).map_err(|_| {
                "ICE server URLs must use valid STUN/TURN schemes without embedded credentials".to_owned()
            })?;
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
                sessions.terminate_current(SessionEndReason::HostUnavailable);
            }
            self.clear_invitation_uris().await;
            self.stop_traversal().await;
            self.stop_listener().await;
            self.set_listener_state(ListenerState::Disabled, None, None);
            return Ok(());
        }

        if !old.remote_enabled || old.listen_port != settings.listen_port || self.running.lock().await.is_none() {
            if let Some(sessions) = self.sessions.get() {
                sessions.terminate_current(SessionEndReason::HostUnavailable);
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

    pub async fn create_pairing_invitation(&self, description: Option<String>) -> Result<Uuid, String> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err("Remote service is shutting down".into());
        }
        let settings = self.settings.read().map_err(|_| "remote settings unavailable")?.clone();
        invitation_route_readiness(&settings, &self.status())?;
        let identity = self.ensure_identity().await.map_err(|error| error.to_string())?;
        let sessions = self.ensure_sessions().await.map_err(|error| error.to_string())?;
        let invitation_id = Uuid::new_v4();
        let secret = Zeroizing::new(random::<[u8; 32]>());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs();
        let registration_key = if matches!(settings.invitation_mode, InvitationMode::Internet | InvitationMode::Hybrid)
        {
            tokio::task::spawn_blocking(crate::identity::load_rendezvous_registration_key)
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?
        } else {
            None
        };
        let prepared = prepare_signed_invitation(
            identity.as_ref(),
            &settings,
            invitation_id,
            &*secret,
            now,
            registration_key.as_deref().map(String::as_str),
        )?;
        sessions
            .activate_invitation(
                prepared.id,
                prepared.secret_hash,
                prepared.created_at,
                prepared.expires_at,
                description,
            )
            .map_err(|error| error.to_string())?;
        self.invitation_uris
            .lock()
            .await
            .insert(prepared.id, Zeroizing::new(prepared.uri));
        self.refresh_status();
        Ok(prepared.id)
    }

    pub async fn invitation_uri(&self, invitation_id: Uuid) -> Option<String> {
        let active = self.sessions.get().is_some_and(|sessions| {
            sessions
                .active_invitations()
                .iter()
                .any(|record| record.id == invitation_id)
        });
        if !active {
            return None;
        }
        self.invitation_uris
            .lock()
            .await
            .get(&invitation_id)
            .map(|value| value.to_string())
    }

    pub async fn revoke_pairing_invitation(&self, invitation_id: Uuid) -> Result<(), String> {
        if let Some(sessions) = self.sessions.get() {
            sessions
                .cancel_invitation(invitation_id)
                .map_err(|error| error.to_string())?;
        }
        self.invitation_uris.lock().await.remove(&invitation_id);
        self.refresh_status();
        Ok(())
    }

    pub async fn set_terminal_name(&self, device_id: String, name: Option<String>) -> Result<(), String> {
        let sessions = self.ensure_sessions().await.map_err(|error| error.to_string())?;
        tokio::task::spawn_blocking(move || sessions.set_terminal_name(&device_id, name))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        self.refresh_status();
        Ok(())
    }

    pub async fn set_host_display_name(self: &Arc<Self>, name: String) -> Result<(), String> {
        let mut settings = self.settings.read().map_err(|_| "remote settings unavailable")?.clone();
        settings.host_display_name = name;
        self.update_settings(settings).await
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

    pub fn paired_terminals(&self) -> Vec<TrustedTerminal> {
        self.sessions
            .get()
            .map(|sessions| sessions.trusted_terminals())
            .unwrap_or_default()
    }

    pub async fn disconnect_client(&self) {
        if let Some(sessions) = self.sessions.get() {
            sessions.terminate_current(SessionEndReason::HostKick);
        }
        self.refresh_status();
    }

    pub fn report_ui_error(&self, error: impl Into<String>) {
        let mut status = self.status.borrow().clone();
        status.error = Some(error.into());
        self.status.send_replace(status);
    }

    pub fn report_ui_notice(&self, notice: impl Into<String>) {
        let mut status = self.status.borrow().clone();
        status.notice = Some(notice.into());
        self.status.send_replace(status);
    }

    pub fn report_startup_error(&self, error: impl Into<String>) {
        self.set_error(error);
    }

    pub async fn shutdown(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        self.shutting_down.store(true, Ordering::Release);
        if let Some(sessions) = self.sessions.get() {
            sessions.terminate_current(SessionEndReason::HostUnavailable);
            let _ = sessions.cancel_all_invitations();
        }
        self.clear_invitation_uris().await;
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
        let controller = Arc::downgrade(self);
        let server_epoch = self.epoch;
        let started = self
            .spawn_traversal_worker(move |cancel| {
                tokio::spawn(async move {
                    log::info!("[remote] traversal worker started");
                    run_traversal(controller, settings, identity, key, server_epoch, cancel).await;
                    log::info!("[remote] traversal worker stopped");
                })
            })
            .await;
        if started {
            self.set_traversal_state(TraversalState::Registering);
        }
        Ok(())
    }

    async fn stop_traversal(&self) {
        if let Some(running) = self.traversal.lock().await.take() {
            running.cancel.cancel();
            let mut task = running.task;
            if timeout(Duration::from_secs(20), &mut task).await.is_err() {
                log::warn!("[remote] Host traversal task did not stop within 20 seconds; aborting the cancelled task");
                task.abort();
                let _ = task.await;
            }
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
            sessions.terminate_current(SessionEndReason::HostUnavailable);
            let _ = sessions.cancel_all_invitations();
        }
        self.clear_invitation_uris().await;
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
        if let Ok(settings) = self.settings.read() {
            status.host_display_name = settings.host_display_name.clone();
        }
        if let Some(identity) = self.identity.get() {
            status.fingerprint = Some(identity.fingerprint().to_owned());
            status.server_id = Some(identity.server_id().to_owned());
        }
        if let Some(sessions) = self.sessions.get() {
            status.paired_terminals = sessions.trusted_terminals();
            status.active_invitations = sessions.active_invitations();
            status.invitation_history = sessions.invitation_history();
            let session = sessions.status();
            status.session = session.phase;
            status.last_contact_seconds_ago = session.last_contact.map(|at| at.elapsed().as_secs());
        }
        status.error = None;
        self.status.send_replace(status);
    }

    async fn clear_invitation_uris(&self) {
        self.invitation_uris.lock().await.clear();
    }

    fn set_traversal_state(&self, traversal: TraversalState) {
        let mut status = self.status.borrow().clone();
        status.traversal = traversal;
        if traversal != TraversalState::Active {
            status.ice_servers_configured = None;
            status.rendezvous_stun_received = None;
            status.turn_credentials_received = None;
            status.local_candidates_gathered = None;
            status.ice_candidate_pair = None;
            if status
                .notice
                .as_deref()
                .is_some_and(|notice| notice.starts_with("Signaling degraded:"))
            {
                status.notice = None;
            }
        }
        self.status.send_replace(status);
    }

    fn set_ice_status(
        &self,
        ice_servers_configured: bool,
        rendezvous_stun_received: bool,
        turn_credentials_received: bool,
        local_candidates_gathered: bool,
        ice_candidate_pair: Option<String>,
    ) {
        let mut status = self.status.borrow().clone();
        status.ice_servers_configured = Some(ice_servers_configured);
        status.rendezvous_stun_received = Some(rendezvous_stun_received);
        status.turn_credentials_received = Some(turn_credentials_received);
        status.local_candidates_gathered = Some(local_candidates_gathered);
        status.ice_candidate_pair = ice_candidate_pair;
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

#[cfg(test)]
mod invitation_preparation_tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::path::PathBuf;

    fn test_sessions() -> (SessionManager, PathBuf) {
        let path = std::env::temp_dir().join(format!("shockingvrc-controller-invite-{}.json", Uuid::new_v4()));
        (SessionManager::load_from_path(Uuid::new_v4(), path.clone()).unwrap(), path)
    }

    fn ready_status() -> HostRemoteStatus {
        HostRemoteStatus {
            listener: ListenerState::Listening,
            address: Some("127.0.0.1:4318".parse().unwrap()),
            fingerprint: Some("test-fingerprint".to_owned()),
            server_id: Some("test-host-id".to_owned()),
            host_display_name: "Test Host".to_owned(),
            paired_terminals: Vec::new(),
            active_invitations: Vec::new(),
            invitation_history: Vec::new(),
            registration_key_configured: true,
            session: SessionPhase::Free,
            traversal: TraversalState::Registered,
            ice_servers_configured: None,
            rendezvous_stun_received: None,
            turn_credentials_received: None,
            local_candidates_gathered: None,
            ice_candidate_pair: None,
            last_contact_seconds_ago: None,
            error: None,
            notice: None,
        }
    }

    #[test]
    fn missing_registration_credential_cannot_leave_a_ghost_invitation() {
        let identity = HostIdentity::ephemeral_for_test();
        let mut settings = RemoteAccessSettings::default();
        settings.invitation_mode = InvitationMode::Internet;
        settings.traversal_enabled = true;
        settings.rendezvous_url = "wss://rendezvous.example.com/api/signal/v1".to_owned();
        let (sessions, path) = test_sessions();

        let result = prepare_signed_invitation(&identity, &settings, Uuid::new_v4(), &[7; 32], 1_700_000_000, None);

        match result {
            Err(error) => assert!(error.contains("registration key")),
            Ok(_) => panic!("missing registration key unexpectedly prepared an invitation"),
        }
        assert!(sessions.active_invitations().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn serialization_failure_cannot_leave_a_ghost_invitation() {
        let identity = HostIdentity::ephemeral_for_test();
        let mut settings = RemoteAccessSettings::default();
        settings.invitation_mode = InvitationMode::Direct;
        settings.direct_host = "bad host".to_owned();
        let (sessions, path) = test_sessions();

        assert!(
            prepare_signed_invitation(&identity, &settings, Uuid::new_v4(), &[8; 32], 1_700_000_000, None).is_err()
        );
        assert!(sessions.active_invitations().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn prepared_direct_invitation_is_signed_and_has_exact_fifteen_minute_ttl() {
        let identity = HostIdentity::ephemeral_for_test();
        let mut settings = RemoteAccessSettings::default();
        settings.invitation_mode = InvitationMode::Direct;
        settings.direct_host = "192.168.1.25".to_owned();
        let now = 1_700_000_000;
        let prepared = prepare_signed_invitation(&identity, &settings, Uuid::new_v4(), &[9; 32], now, None).unwrap();
        let invitation = shocking_vrc_rendezvous::pairing::PairingInvitationV1::parse_uri(&prepared.uri, now).unwrap();

        assert_eq!(prepared.created_at, now);
        assert_eq!(prepared.expires_at - prepared.created_at, 15 * 60);
        assert_eq!(invitation.expires_at - invitation.issued_at, 15 * 60);
        let expected_hash: [u8; 32] = Sha256::digest([9; 32]).into();
        assert_eq!(prepared.secret_hash, expected_hash);
    }

    #[test]
    fn internet_invitation_omits_even_a_saved_direct_endpoint() {
        let identity = HostIdentity::ephemeral_for_test();
        let mut settings = RemoteAccessSettings::default();
        settings.invitation_mode = crate::InvitationMode::Internet;
        settings.traversal_enabled = true;
        settings.rendezvous_url = "wss://rendezvous.example.test/api/signal/v1".to_owned();
        settings.direct_host = "192.168.1.25".to_owned();
        settings.ice_server_urls = vec![
            "stun:stun.custom.test".to_owned(),
            "turn:relay.custom.test:3478?transport=udp".to_owned(),
        ];

        let prepared = prepare_signed_invitation(
            &identity,
            &settings,
            Uuid::new_v4(),
            &[10; 32],
            1_700_000_000,
            Some("test-registration-key"),
        )
        .unwrap();
        let invitation =
            shocking_vrc_rendezvous::pairing::PairingInvitationV1::parse_uri(&prepared.uri, 1_700_000_000).unwrap();

        assert_eq!(invitation.direct_host, None);
        assert_eq!(invitation.direct_port, None);
        assert_eq!(
            invitation.rendezvous_url.as_deref(),
            Some("wss://rendezvous.example.test/api/signal/v1")
        );
        assert_eq!(
            invitation.ice_server_urls,
            ["stun:stun.custom.test", "turn:relay.custom.test:3478?transport=udp",]
        );
    }

    #[test]
    fn direct_invitation_does_not_add_configured_rendezvous_routes() {
        let identity = HostIdentity::ephemeral_for_test();
        let mut settings = RemoteAccessSettings::default();
        settings.invitation_mode = crate::InvitationMode::Direct;
        settings.traversal_enabled = true;
        settings.rendezvous_url = "wss://rendezvous.example.test/api/signal/v1".to_owned();
        settings.direct_host = "192.168.1.25".to_owned();

        let prepared = prepare_signed_invitation(
            &identity,
            &settings,
            Uuid::new_v4(),
            &[11; 32],
            1_700_000_000,
            Some("test-registration-key"),
        )
        .unwrap();
        let invitation =
            shocking_vrc_rendezvous::pairing::PairingInvitationV1::parse_uri(&prepared.uri, 1_700_000_000).unwrap();

        assert_eq!(invitation.direct_host.as_deref(), Some("192.168.1.25"));
        assert_eq!(invitation.rendezvous_url, None);
        assert_eq!(invitation.route_token, None);
    }

    #[test]
    fn internet_invitation_omits_absent_direct_fields_from_json() {
        let identity = HostIdentity::ephemeral_for_test();
        let mut settings = RemoteAccessSettings::default();
        settings.invitation_mode = InvitationMode::Internet;
        settings.traversal_enabled = true;
        settings.rendezvous_url = "wss://rendezvous.example.test/api/signal/v1".to_owned();

        let prepared = prepare_signed_invitation(
            &identity,
            &settings,
            Uuid::new_v4(),
            &[12; 32],
            1_700_000_000,
            Some("test-registration-key"),
        )
        .unwrap();
        let encoded = prepared.uri.strip_prefix("shockingvrc://pair/").unwrap();
        let bytes = URL_SAFE_NO_PAD.decode(encoded).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert!(json.get("direct_host").is_none());
        assert!(json.get("direct_port").is_none());
    }

    #[test]
    fn invitation_readiness_uses_only_the_selected_routes() {
        let mut settings = RemoteAccessSettings::default();
        settings.remote_enabled = true;
        settings.direct_host = "192.168.1.25".to_owned();
        let mut status = ready_status();

        settings.invitation_mode = InvitationMode::Direct;
        status.traversal = TraversalState::Disabled;
        status.registration_key_configured = false;
        assert!(invitation_route_readiness(&settings, &status).is_ok());

        settings.invitation_mode = InvitationMode::Internet;
        settings.traversal_enabled = true;
        settings.rendezvous_url = "wss://rendezvous.example.test/api/signal/v1".to_owned();
        status.traversal = TraversalState::Registering;
        status.registration_key_configured = true;
        let error = invitation_route_readiness(&settings, &status).unwrap_err();
        assert!(error.contains("REGISTERING"));

        status.traversal = TraversalState::Registered;
        assert!(invitation_route_readiness(&settings, &status).is_ok());
    }

    #[test]
    fn hybrid_invitation_readiness_requires_both_routes() {
        let mut settings = RemoteAccessSettings::default();
        settings.remote_enabled = true;
        settings.invitation_mode = InvitationMode::Hybrid;
        settings.direct_host = "192.168.1.25".to_owned();
        settings.traversal_enabled = true;
        settings.rendezvous_url = "wss://rendezvous.example.test/api/signal/v1".to_owned();
        let mut status = ready_status();
        status.traversal = TraversalState::Registering;

        assert!(
            invitation_route_readiness(&settings, &status)
                .unwrap_err()
                .contains("REGISTERING")
        );
        status.traversal = TraversalState::Registered;
        assert!(invitation_route_readiness(&settings, &status).is_ok());
    }
}

#[cfg(test)]
mod stale_room_regression_tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use shocking_vrc_rendezvous::{
        config::{HostAuthorization, LoadedConfig, RoomConfig, ServerConfig, StunConfig, TlsConfig, TurnConfig},
        pairing::create_route_token,
        server::run_with_shutdown,
    };
    use shocking_vrc_rtc::TerminalRtcConfig;
    use std::{
        fs,
        path::PathBuf,
        process::{Child, Command, Stdio},
    };
    use tokio::{net::TcpListener, sync::oneshot};

    const REGISTRATION_KEY: &str = "ephemeral-stale-room-test-key";
    const TEST_NAME: &str = "controller::stale_room_regression_tests::terminal_signaling_close_preserves_direct_datachannel_then_releases_after_channel_close";

    struct NoopBackend;

    impl RemoteBackend for NoopBackend {
        fn snapshot(&self) -> Result<crate::SnapshotDto, crate::BackendError> {
            Err(crate::BackendError::new("TEST", "snapshot is unused"))
        }
        fn presets(&self) -> Result<crate::protocol::PresetCatalogDto, crate::BackendError> {
            Err(crate::BackendError::new("TEST", "presets are unused"))
        }
        fn zone_presets(&self) -> Result<crate::protocol::ZonePresetCatalogDto, crate::BackendError> {
            Err(crate::BackendError::new("TEST", "zone presets are unused"))
        }
        fn logs(&self, _cursor: Option<u64>, _limit: u16) -> Result<crate::protocol::LogPageDto, crate::BackendError> {
            Err(crate::BackendError::new("TEST", "logs are unused"))
        }
        fn enqueue(&self, _command: crate::RemoteCommand) -> Result<(), crate::BackendError> {
            Err(crate::BackendError::new("TEST", "commands are unused"))
        }
    }

    #[tokio::test]
    async fn traversal_worker_retries_when_rendezvous_becomes_available() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);

        let identity = Arc::new(HostIdentity::ephemeral_for_test());
        let host_id = identity.server_id().to_owned();
        let settings = RemoteAccessSettings {
            remote_enabled: true,
            traversal_enabled: true,
            rendezvous_url: format!("ws://{address}/ws"),
            ..RemoteAccessSettings::default()
        };
        let controller = Arc::new(HostRemoteController::new(Arc::new(NoopBackend), settings));
        assert!(controller.identity.set(Arc::clone(&identity)).is_ok());
        controller
            .start_traversal_for_test(Zeroizing::new(REGISTRATION_KEY.to_owned()))
            .await;

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_ne!(controller.status.borrow().traversal, TraversalState::Registered);

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
            turn: TurnConfig::default(),
            stun: StunConfig::default(),
            hosts: HashMap::from([(
                host_id,
                HostAuthorization { registration_key: REGISTRATION_KEY.into() },
            )]),
            turn_secret: None,
            config_dir: std::env::temp_dir(),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(run_with_shutdown(config, async move {
            let _ = shutdown_rx.await;
        }));
        let mut status = controller.status.subscribe();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if status.borrow().traversal == TraversalState::Registered {
                    break;
                }
                status.changed().await.expect("Host status channel");
            }
        })
        .await
        .expect("Host traversal worker should reconnect after a transient unavailable Rendezvous");

        controller.stop_traversal_for_test().await;
        let _ = shutdown_tx.send(());
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn traversal_worker_reregisters_after_registered_signaling_socket_closes() {
        use futures_util::{SinkExt, StreamExt};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio_tungstenite::{accept_async, tungstenite::Message};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let registrations = Arc::new(AtomicUsize::new(0));
        let server_cancel = CancellationToken::new();
        let task_cancel = server_cancel.clone();
        let registration_count = Arc::clone(&registrations);
        let identity = Arc::new(HostIdentity::ephemeral_for_test());
        let host_id = identity.server_id().to_owned();
        let server = tokio::spawn(async move {
            let (stream, _) = tokio::select! {
                _ = task_cancel.cancelled() => return,
                accepted = listener.accept() => accepted.unwrap(),
            };
            let mut socket = accept_async(stream).await.unwrap();
            let request: serde_json::Value = match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => serde_json::from_str(&text).unwrap(),
                other => panic!("expected Host registration request, got {other:?}"),
            };
            assert_eq!(request["type"], "register");
            let response = serde_json::json!({
                "protocol": 1,
                "type": "registered",
                "request_id": request["request_id"],
                "host_id": host_id,
                "registration_epoch": Uuid::new_v4(),
            });
            socket.send(Message::Text(response.to_string().into())).await.unwrap();
            registration_count.fetch_add(1, Ordering::SeqCst);
            socket.send(Message::Close(None)).await.unwrap();
            drop(socket);

            let (stream, _) = tokio::select! {
                _ = task_cancel.cancelled() => return,
                accepted = listener.accept() => accepted.unwrap(),
            };
            let mut socket = accept_async(stream).await.unwrap();
            let request: serde_json::Value = match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => serde_json::from_str(&text).unwrap(),
                other => panic!("expected retried Host registration request, got {other:?}"),
            };
            assert_eq!(request["type"], "register");
            let response = serde_json::json!({
                "protocol": 1,
                "type": "registered",
                "request_id": request["request_id"],
                "host_id": host_id,
                "registration_epoch": Uuid::new_v4(),
            });
            socket.send(Message::Text(response.to_string().into())).await.unwrap();
            registration_count.fetch_add(1, Ordering::SeqCst);
            task_cancel.cancelled().await;
        });

        let settings = RemoteAccessSettings {
            remote_enabled: true,
            traversal_enabled: true,
            rendezvous_url: format!("ws://{address}/ws"),
            ..RemoteAccessSettings::default()
        };
        let controller = Arc::new(HostRemoteController::new(Arc::new(NoopBackend), settings));
        assert!(controller.identity.set(identity).is_ok());
        controller
            .start_traversal_for_test(Zeroizing::new(REGISTRATION_KEY.to_owned()))
            .await;

        tokio::time::timeout(Duration::from_secs(8), async {
            while registrations.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Host worker should establish a fresh registration after socket loss");
        assert_eq!(controller.status.borrow().traversal, TraversalState::Registered);

        controller.stop_traversal_for_test().await;
        server_cancel.cancel();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn traversal_worker_start_is_deduplicated_and_cancellable() {
        let controller = HostRemoteController::new(
            Arc::new(NoopBackend),
            RemoteAccessSettings::default(),
        );
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let first_counter = Arc::clone(&starts);
        assert!(controller.spawn_traversal_worker(move |cancel| {
            tokio::spawn(async move {
                first_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = ready_tx.send(());
                cancel.cancelled().await;
            })
        }).await);
        ready_rx.await.expect("first worker started");

        let duplicate_counter = Arc::clone(&starts);
        assert!(!controller.spawn_traversal_worker(move |cancel| {
            duplicate_counter.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
            tokio::spawn(async move { cancel.cancelled().await; })
        }).await);
        assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 1);

        controller.stop_traversal().await;
        assert!(controller.traversal.lock().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn terminal_signaling_close_preserves_direct_datachannel_then_releases_after_channel_close() {
        if std::env::var("SHOCKINGVRC_STALE_ROOM_ROLE").as_deref() == Ok("host") {
            run_host_child().await;
            return;
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let rendezvous_url = format!("ws://{address}/ws");
        let identity = HostIdentity::ephemeral_for_test();
        let host_id = identity.server_id().to_owned();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let authorization = create_route_token(REGISTRATION_KEY.as_bytes(), &host_id, Uuid::new_v4(), now + 900);
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
            turn: TurnConfig::default(),
            stun: StunConfig::default(),
            hosts: HashMap::from([(
                host_id.clone(),
                HostAuthorization {
                    registration_key: REGISTRATION_KEY.into(),
                },
            )]),
            turn_secret: None,
            config_dir: std::env::temp_dir(),
        };
        let (rendezvous_shutdown_tx, rendezvous_shutdown_rx) = oneshot::channel::<()>();
        let rendezvous = tokio::spawn(run_with_shutdown(config.clone(), async move {
            let _ = rendezvous_shutdown_rx.await;
        }));

        let ready_path = std::env::temp_dir().join(format!("shockingvrc-stale-room-{}.ready", Uuid::new_v4()));
        let _ready_cleanup = RemoveFileOnDrop(ready_path.clone());
        let mut host = ChildGuard(
            Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env("SHOCKINGVRC_STALE_ROOM_ROLE", "host")
                .env("SHOCKINGVRC_STALE_ROOM_RENDEZVOUS", &rendezvous_url)
                .env(
                    "SHOCKINGVRC_STALE_ROOM_PRIVATE_KEY",
                    URL_SAFE_NO_PAD.encode(identity.private_key_bytes_for_test()),
                )
                .env("SHOCKINGVRC_STALE_ROOM_READY", &ready_path)
                .env("SHOCKINGVRC_STALE_ROOM_REGISTRATION_KEY", REGISTRATION_KEY)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("start long-running Host traversal worker"),
        );
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
        .expect("Host traversal worker registered");

        let terminal_config = || TerminalRtcConfig {
            rendezvous_url: rendezvous_url.clone(),
            host_id: host_id.clone(),
            rendezvous_authorization: Zeroizing::new(authorization.clone()),
            expected_fingerprint: identity.fingerprint().to_owned(),
            ice_servers: Vec::new(),
            udp_addrs: vec!["127.0.0.1:0".to_owned()],
            force_relay: false,
            resume_session_id: None,
        };
        let first_session =
            tokio::time::timeout(Duration::from_secs(20), shocking_vrc_rtc::join_terminal_room(terminal_config()))
                .await;
        let mut old_session = first_session
            .expect("first Terminal room negotiation completed")
            .expect("first Host identity proof passed");
        let (mut old_inbound, old_outbound) = old_session.channels.take().expect("old Terminal API channel adapter");
        old_outbound
            .send(
                serde_json::to_vec(&serde_json::json!({
                    "type": "hello",
                    "protocol": 1,
                    "request_id": Uuid::new_v4(),
                    "client": "stale-room-regression",
                    "client_version": "test",
                    "client_nonce": URL_SAFE_NO_PAD.encode([3_u8; 32]),
                    "transport": "webrtc-datachannel",
                }))
                .unwrap(),
            )
            .await
            .expect("send API hello on the old DataChannel");
        let hello_response = tokio::time::timeout(Duration::from_secs(5), old_inbound.recv())
            .await
            .expect("receive Host API hello response")
            .expect("Host replied on the active API DataChannel");
        let hello_response: serde_json::Value = serde_json::from_slice(&hello_response).unwrap();
        assert_eq!(hello_response["type"], "hello.ok");

        // Drop only Terminal signaling. A direct P2P DataChannel is an independent
        // authenticated transport, so the Host must keep servicing it until it closes.
        old_session.signaling.take().unwrap().close().await;
        tokio::time::sleep(Duration::from_millis(250)).await;

        old_outbound
            .send(
                serde_json::to_vec(&serde_json::json!({
                    "type": "state.get",
                    "protocol": 1,
                    "request_id": Uuid::new_v4(),
                }))
                .unwrap(),
            )
            .await
            .expect("send API frame after signaling loss");
        let resumed_api_response = tokio::time::timeout(Duration::from_secs(5), old_inbound.recv())
            .await
            .expect("DataChannel API remains responsive after signaling loss")
            .expect("Host API response remains on the direct DataChannel");
        let resumed_api_response: serde_json::Value = serde_json::from_slice(&resumed_api_response).unwrap();
        assert_eq!(resumed_api_response["type"], "error");
        assert_eq!(resumed_api_response["code"], "AUTH_REQUIRED");

        // Closing the actual DataChannel ends the authenticated API lease. Host may then
        // register again and accept the next room without restarting its process.
        old_session.channels.close(&old_session.peer).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let next_session = tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                match shocking_vrc_rtc::join_terminal_room(terminal_config()).await {
                    Ok(session) => break Ok(session),
                    Err(error) if error.is_room_local() => tokio::time::sleep(Duration::from_millis(250)).await,
                    Err(error) => {
                        eprintln!("next room join failed after old DataChannel close: {error}");
                        break Err(error);
                    }
                }
            }
        })
        .await;
        let next_session_established = matches!(&next_session, Ok(Ok(_)));

        if let Ok(Ok(mut session)) = next_session {
            if let Some(signaling) = session.signaling.take() {
                signaling.close().await;
            }
            session.channels.close(&session.peer).await;
        }
        assert!(
            next_session_established,
            "Host must accept a new room after Terminal signaling closes while the previous DataChannel remains open"
        );

        let _ = host.0.kill();
        let _ = host.0.wait();
        let _ = rendezvous_shutdown_tx.send(());
        rendezvous.await.unwrap().unwrap();
    }

    async fn run_host_child() {
        let rendezvous_url = std::env::var("SHOCKINGVRC_STALE_ROOM_RENDEZVOUS").unwrap();
        let private_key = URL_SAFE_NO_PAD
            .decode(std::env::var("SHOCKINGVRC_STALE_ROOM_PRIVATE_KEY").unwrap())
            .unwrap();
        let identity = Arc::new(HostIdentity::from_private_key_for_test(private_key).unwrap());
        let mut settings = RemoteAccessSettings::default();
        settings.remote_enabled = true;
        settings.traversal_enabled = true;
        settings.rendezvous_url = rendezvous_url;
        let controller = HostRemoteController::new(Arc::new(NoopBackend), settings.clone());
        let server_epoch = controller.epoch;
        assert!(controller.identity.set(Arc::clone(&identity)).is_ok());
        let trust_path = std::env::temp_dir().join(format!("shockingvrc-stale-room-{}.json", Uuid::new_v4()));
        let sessions = Arc::new(SessionManager::load_from_path(server_epoch, trust_path).unwrap());
        assert!(controller.sessions.set(Arc::clone(&sessions)).is_ok());
        assert!(
            controller
                .api_state
                .set(crate::api::ApiState::new(
                    Arc::clone(&controller.backend),
                    sessions,
                    Arc::clone(&identity),
                    Arc::clone(&controller.settings),
                    server_epoch,
                ))
                .is_ok()
        );

        let cancel = CancellationToken::new();
        tokio::spawn(run_traversal(
            Arc::downgrade(&controller),
            settings,
            identity,
            Zeroizing::new(std::env::var("SHOCKINGVRC_STALE_ROOM_REGISTRATION_KEY").unwrap()),
            server_epoch,
            cancel,
        ));
        let mut status = controller.status.subscribe();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if status.borrow().traversal == TraversalState::Registered {
                    break;
                }
                status.changed().await.unwrap();
            }
        })
        .await
        .expect("Host registered with local Rendezvous");
        fs::write(std::env::var("SHOCKINGVRC_STALE_ROOM_READY").unwrap(), "registered").unwrap();
        std::future::pending::<()>().await;
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
    #[cfg(test)]
    let udp_addrs = vec!["127.0.0.1:0".to_owned()];
    #[cfg(not(test))]
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
                        owner.set_ice_status(
                            session.ice_servers_configured,
                            session.rendezvous_stun_received,
                            session.turn_credentials_received,
                            session.local_candidates_gathered,
                            session.ice_candidate_pair.clone(),
                        );
                    }
                    let (inbound, outbound) = match session.channels.take() {
                        Ok(channels) => channels,
                        Err(error) => {
                            if let Some(owner) = controller.upgrade() {
                                owner.set_traversal_error(error.to_string());
                            }
                            session
                                .channels
                                .close_with_reason(&session.peer, PeerCloseReason::ApiSessionEnded)
                                .await;
                            match close_host_room(&mut signaling, session.request_id, session.room_id).await {
                                Ok(()) => {
                                    if let Some(owner) = controller.upgrade() {
                                        owner.set_traversal_state(TraversalState::Registered);
                                    }
                                }
                                Err(close_error) => {
                                    if let Some(owner) = controller.upgrade() {
                                        owner.set_traversal_error(close_error.to_string());
                                    }
                                    signaling.socket.close().await;
                                    break;
                                }
                            }
                            continue;
                        }
                    };
                    let mut signaling_lost = false;
                    let mut signaling_available = true;
                    let mut peer_close_reason = PeerCloseReason::ApiSessionEnded;
                    if let Some(owner) = controller.upgrade() {
                        let room_text = session.room_id.to_string();
                        log::info!("[remote] starting Host API session over DataChannel room={room_text}");
                        let mut serve = Box::pin(owner.serve_data_channel(&room_text, inbound, outbound));
                        let mut turn_refresh_timer = session.turn_refresh_at.map(|at| {
                            Box::pin(tokio::time::sleep(at.saturating_duration_since(tokio::time::Instant::now())))
                        });
                        let mut cancelled = false;
                        'active_room: loop {
                            enum ActiveRoomEvent {
                                Cancelled,
                                ServiceEnded,
                                TurnRefreshDue,
                                RoomSignal(Result<(), SessionError>),
                            }
                            let event = tokio::select! {
                                _ = cancel.cancelled() => ActiveRoomEvent::Cancelled,
                                _ = &mut serve => ActiveRoomEvent::ServiceEnded,
                                _ = wait_for_turn_refresh(&mut turn_refresh_timer), if turn_refresh_timer.is_some() => ActiveRoomEvent::TurnRefreshDue,
                                result = signaling.wait_for_room_close(session.room_id), if signaling_available => ActiveRoomEvent::RoomSignal(result),
                            };
                            match event {
                                ActiveRoomEvent::Cancelled => {
                                    cancelled = true;
                                    peer_close_reason = PeerCloseReason::LocalShutdown;
                                    break;
                                }
                                ActiveRoomEvent::ServiceEnded => {
                                    peer_close_reason = PeerCloseReason::ApiSessionEnded;
                                    break;
                                }
                                ActiveRoomEvent::RoomSignal(Ok(())) => {
                                    log::info!("[remote] Rendezvous closed the active signaling room room={room_text}");
                                    if session.peer.selected_pair_uses_relay().await == Some(false) {
                                        log::warn!(
                                            "[remote] Rendezvous signaling room closed on a direct P2P path; preserving the authenticated DataChannel until it closes room={room_text}"
                                        );
                                        signaling_available = false;
                                        turn_refresh_timer = None;
                                        if let Some(owner) = controller.upgrade() {
                                            owner.report_ui_notice("Signaling degraded: Rendezvous closed this room while the direct P2P DataChannel remains active. It will accept new rooms after this DataChannel closes.");
                                        }
                                    } else {
                                        peer_close_reason = PeerCloseReason::RoomClosed;
                                        break;
                                    }
                                }
                                ActiveRoomEvent::RoomSignal(Err(SessionError::Signal(SignalError::Timeout(_)))) => {
                                    // Receive timeouts do not end a healthy DataChannel session.
                                }
                                ActiveRoomEvent::RoomSignal(Err(error)) => {
                                    log::warn!("[remote] Host signaling ended during active room={room_text}: {error}");
                                    if session.peer.selected_pair_uses_relay().await == Some(false) {
                                        log::warn!(
                                            "[remote] auxiliary signaling failed on a direct P2P path; preserving the authenticated DataChannel and disabling signaling-dependent renewal room={room_text}"
                                        );
                                        signaling_lost = true;
                                        signaling_available = false;
                                        turn_refresh_timer = None;
                                        if let Some(owner) = controller.upgrade() {
                                            owner.report_ui_notice("Signaling degraded: Rendezvous is unavailable, but the authenticated direct P2P DataChannel remains active until it closes.");
                                        }
                                    } else {
                                        signaling_lost = true;
                                        peer_close_reason = PeerCloseReason::RoomClosed;
                                        break;
                                    }
                                }
                                ActiveRoomEvent::TurnRefreshDue => {
                                    let relay_path = shocking_vrc_rtc::turn_refresh_failure_disposition(
                                        session.peer.selected_pair_uses_relay().await,
                                    )
                                        == shocking_vrc_rtc::TurnRefreshFailureDisposition::ReconnectRelaySession;
                                    let refresh_wait = tokio::select! {
                                        _ = cancel.cancelled() => { cancelled = true; break 'active_room; },
                                        result = wait_for_turn_refresh_or_api_end(
                                            &mut serve,
                                            request_turn_credentials(&mut signaling, &room_text),
                                        ) => result,
                                    };
                                    let refresh = match refresh_wait {
                                        TurnRefreshWait::ApiEnded => break 'active_room,
                                        TurnRefreshWait::RefreshCompleted(result) => result,
                                    };
                                    match refresh {
                                        Ok(Some(credentials)) => {
                                            match session.peer.update_turn_credentials(credentials.server).await {
                                                Ok(()) => {
                                                    let delay = credentials
                                                        .refresh_at
                                                        .saturating_duration_since(tokio::time::Instant::now());
                                                    if relay_path {
                                                        log::info!(
                                                            "[remote] staged fresh TURN credentials for a future ICE gathering; automatic TURN refresh keeps the active relay allocation and DataChannel room={room_text}; next_refresh_in_seconds={}",
                                                            delay.as_secs()
                                                        );
                                                    } else {
                                                        log::info!(
                                                            "[remote] staged fresh TURN credentials for a future ICE gathering while preserving the active direct path room={room_text}; next_refresh_in_seconds={}",
                                                            delay.as_secs()
                                                        );
                                                    }
                                                    turn_refresh_timer = Some(Box::pin(tokio::time::sleep(delay)));
                                                }
                                                Err(error @ shocking_vrc_rtc::RtcError::Configuration(_))
                                                    if relay_path =>
                                                {
                                                    log::error!(
                                                        "[remote] TURN configuration changed or no longer matches the relay room={room_text}; rebuilding the authenticated transport instead of retrying: {error}"
                                                    );
                                                    peer_close_reason = PeerCloseReason::TurnRotation;
                                                    break 'active_room;
                                                }
                                                Err(error @ shocking_vrc_rtc::RtcError::Configuration(_)) => {
                                                    log::error!(
                                                        "[remote] TURN configuration changed for a direct active path room={room_text}; renewal stopped until the next ICE room: {error}"
                                                    );
                                                    turn_refresh_timer = None;
                                                }
                                                Err(error) => {
                                                    log::error!(
                                                        "[remote] could not stage renewed TURN credentials for room={room_text}: {error}"
                                                    );
                                                    turn_refresh_timer =
                                                        Some(Box::pin(tokio::time::sleep(Duration::from_secs(15))));
                                                }
                                            }
                                        }
                                        Ok(None) => {
                                            if relay_path {
                                                log::error!(
                                                    "[remote] TURN renewal returned no service for the active relay room={room_text}; releasing the room before its allocation expires"
                                                );
                                                peer_close_reason = PeerCloseReason::TurnRotation;
                                                break 'active_room;
                                            }
                                            log::warn!(
                                                "[remote] TURN renewal returned no service on the direct active path room={room_text}; renewal stopped"
                                            );
                                            turn_refresh_timer = None;
                                        }
                                        Err(SessionError::Closed) => {
                                            if relay_path {
                                                log::info!(
                                                    "[remote] TURN renewal confirmed that relay room={room_text} is closed; releasing the old transport before same-session resume"
                                                );
                                                peer_close_reason = PeerCloseReason::RoomClosed;
                                                break 'active_room;
                                            }
                                            log::warn!(
                                                "[remote] TURN renewal confirmed that signaling room={room_text} is closed on the direct path; preserving the active DataChannel and stopping renewal"
                                            );
                                            turn_refresh_timer = None;
                                        }
                                        Err(SessionError::Signal(
                                            SignalError::Closed | SignalError::Transport(_) | SignalError::Protocol(_),
                                        )) => {
                                            log::warn!(
                                                "[remote] Host signaling was lost during TURN renewal for room={room_text}"
                                            );
                                            if !relay_path {
                                                signaling_lost = true;
                                                signaling_available = false;
                                                turn_refresh_timer = None;
                                                log::warn!(
                                                    "[remote] preserving direct P2P DataChannel after signaling loss and disabling future TURN renewal room={room_text}"
                                                );
                                                if let Some(owner) = controller.upgrade() {
                                                    owner.report_ui_notice("Signaling degraded: Rendezvous is unavailable, but the authenticated direct P2P DataChannel remains active until it closes.");
                                                }
                                            } else {
                                                signaling_lost = true;
                                                peer_close_reason = PeerCloseReason::RoomClosed;
                                                break 'active_room;
                                            }
                                        }
                                        Err(
                                            error @ (SessionError::Rendezvous(_) | SessionError::UnexpectedSignal(_)),
                                        ) => {
                                            if relay_path {
                                                log::error!(
                                                    "[remote] TURN renewal returned a permanent protocol/configuration error for relay room={room_text}; rebuilding the authenticated transport: {error}"
                                                );
                                                peer_close_reason = PeerCloseReason::TurnRotation;
                                                break 'active_room;
                                            }
                                            log::error!(
                                                "[remote] TURN renewal returned a permanent protocol/configuration error on a direct active path room={room_text}; renewal stopped until the next ICE room: {error}"
                                            );
                                            turn_refresh_timer = None;
                                        }
                                        Err(error) => {
                                            log::warn!(
                                                "[remote] TURN credential renewal failed for room={room_text}; keeping the active DataChannel and retrying: {error}"
                                            );
                                            turn_refresh_timer =
                                                Some(Box::pin(tokio::time::sleep(Duration::from_secs(15))));
                                        }
                                    }
                                }
                            }
                        }
                        // Release the API receiver before closing its RTC channel pumps.
                        drop(serve);
                        if cancelled {
                            if let Some(owner) = controller.upgrade()
                                && let Some(sessions) = owner.sessions.get()
                            {
                                sessions.terminate_current(SessionEndReason::HostUnavailable);
                            }
                        }
                    }
                    let room_id = session.room_id;
                    let request_id = session.request_id;
                    session
                        .channels
                        .close_with_reason(&session.peer, peer_close_reason)
                        .await;
                    if cancel.is_cancelled() {
                        signaling.socket.close().await;
                        return;
                    }
                    if signaling_lost {
                        signaling.socket.close().await;
                        break;
                    }
                    let close_result = tokio::select! {
                        _ = cancel.cancelled() => {
                            signaling.socket.close().await;
                            return;
                        }
                        result = close_host_room(&mut signaling, request_id, room_id) => result,
                    };
                    if let Err(error) = close_result {
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
                    log::error!("[remote] Host ICE room setup failed: {error}");
                    if let Some(owner) = controller.upgrade() {
                        owner.set_traversal_error(error.to_string());
                    }
                    if error.is_room_local() {
                        if let Some(owner) = controller.upgrade() {
                            owner.set_traversal_state(TraversalState::Registered);
                        }
                        continue;
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

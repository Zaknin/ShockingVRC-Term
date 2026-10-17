use tauri::{State, ipc::Channel};
use std::{
    future::Future,
    io::{Read, Seek, SeekFrom},
    pin::Pin,
};
use tokio::{sync::{mpsc, oneshot}, time::{Duration, timeout}};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    allowed_operation::AllowedOperation,
    app_state::{AppState, ConnectionHandle},
    protocol::{CommandAckDto, ConnectionStateDto, HostIdentityDto, ProfileDto, ProfileInputDto, TerminalDiagnosticsDto, UiEvent},
    storage::credentials,
    transport::{SessionAction, begin_connection, finish_device_auth, run_session},
};

#[tauri::command]
pub async fn list_profiles(state: State<'_, AppState>) -> Result<Vec<ProfileDto>, String> {
    Ok(state.profiles.lock().await.list())
}

#[tauri::command]
pub async fn delete_profile(state: State<'_, AppState>, profile_id: String) -> Result<(), String> {
    if state.connection.lock().await.as_ref().is_some_and(|connection| connection.profile_id == profile_id) {
        return Err("Disconnect from this Host before removing its profile".to_owned());
    }

    let saved_profiles = state.profiles.lock().await.list();
    let pending_profile = state.pending_pairing_profile.lock().await.clone();
    let plan = plan_profile_forget(&profile_id, &saved_profiles, pending_profile.as_ref())
        .ok_or_else(|| "Host profile was not found".to_owned())?;
    credentials::delete(&plan.profile.id)?;
    if plan.clear_pending_pairing {
        *state.pending_pairing.lock().await = None;
        *state.pending_pairing_profile.lock().await = None;
        state.updates.clear_pairing_profile(&plan.profile.id).await;
    }
    if plan.delete_saved_profile {
        state.profiles.lock().await.delete(&plan.profile.id)
    } else {
        Ok(())
    }
}

struct ProfileForgetPlan {
    profile: ProfileDto,
    delete_saved_profile: bool,
    clear_pending_pairing: bool,
}

fn plan_profile_forget(
    profile_id: &str,
    saved_profiles: &[ProfileDto],
    pending_profile: Option<&ProfileDto>,
) -> Option<ProfileForgetPlan> {
    let saved = saved_profiles.iter().find(|profile| profile.id == profile_id);
    let pending = pending_profile.filter(|profile| profile.id == profile_id);
    let profile = saved.or(pending)?.clone();
    Some(ProfileForgetPlan {
        profile,
        delete_saved_profile: saved.is_some(),
        clear_pending_pairing: pending.is_some(),
    })
}

#[tauri::command]
pub async fn subscribe_state(state: State<'_, AppState>, channel: Channel<UiEvent>) -> Result<(), String> {
    state.updates.attach(channel).await;
    Ok(())
}

#[tauri::command]
pub async fn open_pairing_invitation(state: State<'_, AppState>, invitation: String) -> Result<ProfileDto, String> {
    let invitation = Zeroizing::new(invitation);
    accept_pairing_invitation(&state, invitation.as_str(), false).await
}

pub async fn accept_pairing_invitation(state: &AppState, invitation_uri: &str, publish_to_ui: bool) -> Result<ProfileDto, String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?.as_secs();
    let invitation = shocking_vrc_rendezvous::pairing::PairingInvitationV1::parse_uri(invitation_uri, now)
        .map_err(|error| error.to_string())?;
    let direct_host = invitation.direct_host.clone();
    let existing_id = state.profiles.lock().await.list().into_iter()
        .find(|profile| profile.server_id == invitation.host_id)
        .map(|profile| profile.id)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let profile = crate::storage::normalize_profile(ProfileInputDto {
        id: Some(existing_id),
        label: format!("Host {}", invitation.host_id.chars().take(8).collect::<String>()),
        host: direct_host.clone().unwrap_or_else(|| "localhost".to_owned()),
        port: invitation.direct_port.unwrap_or(4318),
        fingerprint: invitation.host_fingerprint.clone(),
        server_id: invitation.host_id.clone(),
        connection_mode: if direct_host.is_none() { "traversal_only".to_owned() }
            else if invitation.rendezvous_url.is_none() { "direct_only".to_owned() }
            else { "auto".to_owned() },
        rendezvous_url: invitation.rendezvous_url.clone().unwrap_or_default(),
        ice_server_urls: invitation.ice_server_urls.clone(),
        force_relay: false,
    })?;
    *state.pending_pairing.lock().await = Some(invitation);
    *state.pending_pairing_profile.lock().await = Some(profile.clone());
    if publish_to_ui {
        state.updates.publish(UiEvent::PairingInvitation(profile.clone())).await;
    }
    Ok(profile)
}

#[tauri::command]
pub async fn connect_profile(state: State<'_, AppState>, profile_id: String) -> Result<HostIdentityDto, String> {
    // Tauri dispatches async commands from a WebView2 protocol callback. Keep the
    // future it spawns small; run the large connection state machine on a worker.
    let connection: Pin<Box<dyn Future<Output = Result<HostIdentityDto, String>> + Send + '_>> =
        Box::pin(connect_profile_inner(state, profile_id));
    connection.await
}

async fn connect_profile_inner(state: State<'_, AppState>, profile_id: String) -> Result<HostIdentityDto, String> {
    if state.connection.lock().await.is_some() {
        return Err("Disconnect from the current Host before selecting another profile".to_owned());
    }
    let pending_profile = state.pending_pairing_profile.lock().await.clone()
        .filter(|profile| profile.id == profile_id);
    let mut profile = match pending_profile {
        Some(profile) => profile,
        None => state.profiles.lock().await.get(&profile_id)?,
    };
    log::info!("[terminal] verifying Host profile mode={}", profile.connection_mode);
    let invitation = state.pending_pairing.lock().await.clone()
        .filter(|invitation| invitation.host_id == profile.server_id);
    let rendezvous_authorization = if let Some(invitation) = invitation.as_ref() {
        invitation.route_token.as_ref().map(|token| Zeroizing::new(token.clone()))
    } else {
        credentials::read_device_grant(&profile.id, &profile.fingerprint)?
    };
    state
        .updates
        .publish(UiEvent::Connection(ConnectionStateDto {
            phase: "connecting".to_owned(),
            transport: None,
            ice_candidate_pair: None,
            profile_id: Some(profile.id.clone()),
            host_label: Some(profile.label.clone()),
            fingerprint: Some(profile.fingerprint.clone()),
            server_id: None,
            last_heartbeat: None,
            message: Some("Checking the signed Host identity before device authentication".to_owned()),
        }))
        .await;
    let (pending, identity) = match begin_connection(profile.clone(), rendezvous_authorization, None).await {
        Ok(value) => value,
        Err(error) => {
            publish_connection_error(&state, &profile, error.clone()).await;
            return Err(error);
        }
    };
    if !profile.server_id.is_empty() && identity.server_id != profile.server_id {
        publish_connection_error(&state, &profile, "Host ID does not match the invitation or saved profile".to_owned()).await;
        return Err("Host ID does not match the invitation or saved profile".to_owned());
    }
    let terminal_identity = credentials::TerminalDeviceIdentity::load_or_create()?;
    let (connection, identity) = match finish_device_auth(pending, &terminal_identity, invitation.as_ref()).await {
        Ok(connection) => (connection, identity),
        Err(pairing_error) if invitation.is_some() => {
            log::warn!("[terminal] pairing handshake failed; retrying once with the trusted device key: {pairing_error}");
            let recovery_authorization = invitation.as_ref()
                .and_then(|invitation| invitation.route_token.as_ref())
                .map(|token| Zeroizing::new(token.clone()));
            let (recovery_pending, recovery_identity) = match begin_connection(
                profile.clone(), recovery_authorization, None,
            ).await {
                Ok(value) => value,
                Err(recovery_error) => {
                    let error = format!(
                        "Pairing attempt failed ({pairing_error}); trusted-device recovery could not reconnect: {recovery_error}"
                    );
                    log::error!("[terminal] passwordless Host recovery failed: {error}");
                    publish_connection_error(&state, &profile, error.clone()).await;
                    return Err(error);
                }
            };
            if recovery_identity.server_id != identity.server_id
                || recovery_identity.fingerprint != identity.fingerprint
            {
                let error = "Trusted-device recovery connected to a different Host identity".to_owned();
                publish_connection_error(&state, &profile, error.clone()).await;
                return Err(error);
            }
            match finish_device_auth(recovery_pending, &terminal_identity, None).await {
                Ok(connection) => (connection, recovery_identity),
                Err(recovery_error) => {
                    let error = format!(
                        "Pairing attempt failed ({pairing_error}); trusted-device recovery failed: {recovery_error}"
                    );
                    log::error!("[terminal] passwordless Host recovery failed: {error}");
                    publish_connection_error(&state, &profile, error.clone()).await;
                    return Err(error);
                }
            }
        }
        Err(error) => {
            log::error!("[terminal] passwordless Host authentication failed: {error}");
            publish_connection_error(&state, &profile, error.clone()).await;
            return Err(error);
        }
    };
    credentials::save_device_grant(&profile.id, &profile.fingerprint, &connection.device_grant)?;
    let was_pending_pairing = invitation.is_some();
    let should_save_profile = was_pending_pairing || profile.server_id.is_empty();
    profile.server_id = identity.server_id.clone();
    if should_save_profile {
        state.profiles.lock().await.save(ProfileInputDto {
            id: Some(profile.id.clone()),
            label: profile.label.clone(),
            host: profile.host.clone(),
            port: profile.port,
            fingerprint: profile.fingerprint.clone(),
            server_id: profile.server_id.clone(),
            connection_mode: profile.connection_mode.clone(),
            rendezvous_url: profile.rendezvous_url.clone(),
            ice_server_urls: profile.ice_server_urls.clone(),
            force_relay: profile.force_relay,
        })?;
    }
    if invitation.is_some() {
        *state.pending_pairing.lock().await = None;
        *state.pending_pairing_profile.lock().await = None;
        state.updates.clear_pairing_profile(&profile.id).await;
    }
    log::info!("[terminal] paired device session established profile={} transport={}", profile_id, connection.transport_name());
    let (actions, receiver) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    let connection_id = Uuid::new_v4();
    let handle = ConnectionHandle {
        id: connection_id,
        profile_id,
        actions,
        cancel: cancel.clone(),
    };
    {
        let mut current = state.connection.lock().await;
        if current.is_some() {
            return Err("A Host session is already active".to_owned());
        }
        *current = Some(handle);
    }
    let updates = std::sync::Arc::clone(&state.updates);
    let registry = std::sync::Arc::clone(&state.connection);
    tauri::async_runtime::spawn(async move {
        run_session(connection, receiver, updates, cancel).await;
        let mut current = registry.lock().await;
        if current.as_ref().is_some_and(|handle| handle.id == connection_id) {
            *current = None;
        }
    });
    Ok(identity)
}

async fn publish_connection_error(state: &AppState, profile: &ProfileDto, error: String) {
    state.updates.publish(UiEvent::Connection(ConnectionStateDto {
        phase: "disconnected".to_owned(), transport: None, ice_candidate_pair: None,
        profile_id: Some(profile.id.clone()), host_label: Some(profile.label.clone()),
        fingerprint: Some(profile.fingerprint.clone()), server_id: Some(profile.server_id.clone()),
        last_heartbeat: None, message: Some(error),
    })).await;
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(handle) = state.connection.lock().await.take() {
        log::info!("[terminal] user requested session disconnect profile={}", handle.profile_id);
        handle.cancel.cancel();
        let _ = handle.actions.send(SessionAction::Disconnect).await;
    } else {
        state
            .updates
            .publish(UiEvent::Connection(ConnectionStateDto {
                phase: "disconnected".to_owned(),
                transport: None,
                ice_candidate_pair: None,
                profile_id: None,
                host_label: None,
                fingerprint: None,
                server_id: None,
                last_heartbeat: None,
                message: None,
            }))
            .await;
    }
    Ok(())
}

#[tauri::command]
pub async fn fetch_state(state: State<'_, AppState>) -> Result<(), String> {
    send_action(&state, SessionAction::FetchState).await
}

#[tauri::command]
pub async fn fetch_capabilities(state: State<'_, AppState>) -> Result<(), String> {
    send_action(&state, SessionAction::FetchCapabilities).await
}

#[tauri::command]
pub async fn fetch_catalog(state: State<'_, AppState>, kind: String) -> Result<(), String> {
    if !matches!(kind.as_str(), "presets" | "zone_presets") {
        return Err("Unsupported Host catalog".to_owned());
    }
    send_action(&state, SessionAction::FetchCatalog(kind)).await
}

#[tauri::command]
pub async fn fetch_logs(state: State<'_, AppState>, cursor: Option<String>, limit: Option<u16>) -> Result<(), String> {
    send_action(&state, SessionAction::FetchLogs(cursor, limit.unwrap_or(100).clamp(1, 200))).await
}

#[tauri::command]
pub fn get_terminal_diagnostics(state: State<'_, AppState>) -> Result<TerminalDiagnosticsDto, String> {
    const MAX_TAIL_BYTES: u64 = 96 * 1024;
    const MAX_LINES: usize = 300;
    let mut file = match std::fs::File::open(&state.terminal_log_path) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Could not read Terminal diagnostic log: {error}")),
    };
    let lines = if let Some(file) = file.as_mut() {
        let length = file.metadata().map_err(|error| format!("Could not inspect Terminal diagnostic log: {error}"))?.len();
        let offset = length.saturating_sub(MAX_TAIL_BYTES);
        file.seek(SeekFrom::Start(offset)).map_err(|error| format!("Could not seek Terminal diagnostic log: {error}"))?;
        let mut bytes = Vec::with_capacity(length.saturating_sub(offset) as usize);
        file.read_to_end(&mut bytes).map_err(|error| format!("Could not read Terminal diagnostic log: {error}"))?;
        let text = String::from_utf8_lossy(&bytes);
        let tail_lines: Vec<&str> = text.lines().skip(if offset > 0 { 1 } else { 0 }).collect();
        let mut lines: Vec<String> = tail_lines.into_iter().rev().take(MAX_LINES).map(str::to_owned).collect();
        lines.reverse();
        lines
    } else {
        Vec::new()
    };
    let detailed_ice_enabled = std::env::var("SHOCKINGVRC_ICE_DIAGNOSTICS")
        .is_ok_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on"));
    Ok(TerminalDiagnosticsDto { lines, detailed_ice_enabled })
}

#[tauri::command]
pub fn clear_terminal_diagnostics(state: State<'_, AppState>) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&state.terminal_log_path)
        .map(|_| ())
        .map_err(|error| format!("Could not clear Terminal diagnostic log: {error}"))
}

#[tauri::command]
pub async fn submit_operation(state: State<'_, AppState>, operation: AllowedOperation) -> Result<CommandAckDto, String> {
    let request_id = Uuid::new_v4();
    let frame = operation.into_wire_frame(request_id)?;
    let (response, receiver) = oneshot::channel();
    send_action(&state, SessionAction::Command { request_id, frame, response }).await?;
    match timeout(Duration::from_secs(10), receiver).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("Host session ended before command acknowledgement; outcome is unknown. The command was not replayed.".to_owned()),
        Err(_) => Err("Timed out waiting for Host command acknowledgement; outcome is unknown. The command was not replayed.".to_owned()),
    }
}

async fn send_action(state: &AppState, action: SessionAction) -> Result<(), String> {
    let current = state.connection.lock().await;
    let handle = current.as_ref().ok_or_else(|| "Connect to a Host first".to_owned())?;
    handle
        .actions
        .send(action)
        .await
        .map_err(|_| "The Host session is no longer available".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_pairing_profile_is_forgettable_before_it_is_saved() {
        let pending = ProfileDto {
            id: "pending-host-id".to_owned(),
            label: "Pending Host".to_owned(),
            host: "localhost".to_owned(),
            port: 4318,
            fingerprint: "00".repeat(32),
            server_id: "host-under-test".to_owned(),
            connection_mode: "traversal_only".to_owned(),
            rendezvous_url: "ws://127.0.0.1:4319/ws".to_owned(),
            ice_server_urls: Vec::new(),
            force_relay: false,
        };

        let plan = plan_profile_forget("pending-host-id", &[], Some(&pending))
            .expect("the pending invitation should be a forgettable Host entry");

        assert_eq!(plan.profile.id, "pending-host-id");
        assert!(!plan.delete_saved_profile);
        assert!(plan.clear_pending_pairing);
    }

    #[test]
    fn forgetting_saved_host_also_clears_its_pending_invitation() {
        let saved = ProfileDto {
            id: "saved-host-id".to_owned(),
            label: "Saved Host".to_owned(),
            host: "localhost".to_owned(),
            port: 4318,
            fingerprint: "11".repeat(32),
            server_id: "host-under-test".to_owned(),
            connection_mode: "traversal_only".to_owned(),
            rendezvous_url: "ws://127.0.0.1:4319/ws".to_owned(),
            ice_server_urls: Vec::new(),
            force_relay: false,
        };
        let pending = ProfileDto { label: "New invitation".to_owned(), ..saved.clone() };

        let plan = plan_profile_forget("saved-host-id", &[saved], Some(&pending)).unwrap();

        assert_eq!(plan.profile.id, "saved-host-id");
        assert!(plan.delete_saved_profile);
        assert!(plan.clear_pending_pairing);
    }
}

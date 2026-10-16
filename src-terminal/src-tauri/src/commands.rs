use tauri::{State, ipc::Channel};
use std::io::{Read, Seek, SeekFrom};
use tokio::{sync::{mpsc, oneshot}, time::{Duration, timeout}};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    allowed_operation::AllowedOperation,
    app_state::{AppState, ConnectionHandle},
    protocol::{CommandAckDto, ConnectionStateDto, HostIdentityDto, ProfileDto, ProfileInputDto, TerminalDiagnosticsDto, UiEvent},
    storage::credentials,
    transport::{SessionAction, begin_connection, finish_login, run_session},
};

#[tauri::command]
pub async fn list_profiles(state: State<'_, AppState>) -> Result<Vec<ProfileDto>, String> {
    Ok(state.profiles.lock().await.list())
}

#[tauri::command]
pub async fn save_profile(state: State<'_, AppState>, profile: ProfileInputDto) -> Result<ProfileDto, String> {
    state.profiles.lock().await.save(profile)
}

#[tauri::command]
pub async fn delete_profile(state: State<'_, AppState>, profile_id: String) -> Result<(), String> {
    if state.connection.lock().await.as_ref().is_some_and(|connection| connection.profile_id == profile_id) {
        return Err("Disconnect from this Host before removing its profile".to_owned());
    }
    state.profiles.lock().await.get(&profile_id)?;
    credentials::delete(&profile_id)?;
    if state
        .pending
        .lock()
        .await
        .as_ref()
        .is_some_and(|pending| pending.profile_id() == profile_id)
    {
        *state.pending.lock().await = None;
    }
    state.profiles.lock().await.delete(&profile_id)
}

#[tauri::command]
pub async fn subscribe_state(state: State<'_, AppState>, channel: Channel<UiEvent>) -> Result<(), String> {
    state.updates.attach(channel).await;
    Ok(())
}

#[tauri::command]
pub async fn connect_profile(state: State<'_, AppState>, profile_id: String) -> Result<HostIdentityDto, String> {
    if state.connection.lock().await.is_some() {
        return Err("Disconnect from the current Host before selecting another profile".to_owned());
    }
    *state.pending.lock().await = None;
    let profile = state.profiles.lock().await.get(&profile_id)?;
    log::info!("[terminal] verifying Host profile mode={}", profile.connection_mode);
    let invite_token = credentials::read_invite_token(&profile.id, &profile.fingerprint)?;
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
            message: Some("Checking the saved Host fingerprint over HTTPS and WSS".to_owned()),
        }))
        .await;
    match begin_connection(profile.clone(), invite_token, None).await {
        Ok((pending, identity)) => {
            let transport = pending.transport_name().to_owned();
            if profile.server_id != identity.server_id {
                state.profiles.lock().await.set_server_id(&profile.id, identity.server_id.clone())?;
            }
            *state.pending.lock().await = Some(pending);
            state
                .updates
                .publish(UiEvent::Connection(ConnectionStateDto {
                    phase: "identity_verified".to_owned(),
                    transport: Some(transport),
                    ice_candidate_pair: None,
                    profile_id: Some(profile.id),
                    host_label: Some(profile.label),
                    fingerprint: Some(identity.fingerprint.clone()),
                    server_id: Some(identity.server_id.clone()),
                    last_heartbeat: None,
                    message: Some("Host identity verified. Enter credentials to sign in.".to_owned()),
                }))
                .await;
            Ok(identity)
        }
        Err(error) => {
            log::error!("[terminal] Host verification/connection failed: {error}");
            state
                .updates
                .publish(UiEvent::Connection(ConnectionStateDto {
                    phase: "disconnected".to_owned(),
                    transport: None,
                    ice_candidate_pair: None,
                    profile_id: Some(profile.id),
                    host_label: Some(profile.label),
                    fingerprint: Some(profile.fingerprint),
                    server_id: None,
                    last_heartbeat: None,
                    message: Some(error.clone()),
                }))
                .await;
            Err(error)
        }
    }
}

#[tauri::command]
pub async fn save_invite_token(state: State<'_, AppState>, profile_id: String, token: String) -> Result<(), String> {
    let profile = state.profiles.lock().await.get(&profile_id)?;
    credentials::save_invite_token(&profile.id, &profile.fingerprint, &token)
}

#[tauri::command]
pub async fn has_saved_invite_token(state: State<'_, AppState>, profile_id: String) -> Result<bool, String> {
    let profile = state.profiles.lock().await.get(&profile_id)?;
    credentials::has_invite_token(&profile.id, &profile.fingerprint)
}

#[tauri::command]
pub async fn forget_invite_token(state: State<'_, AppState>, profile_id: String) -> Result<(), String> {
    let profile = state.profiles.lock().await.get(&profile_id)?;
    credentials::delete_invite_token(&profile.id)
}

#[tauri::command]
pub async fn login(
    state: State<'_, AppState>,
    profile_id: String,
    username: String,
    password: String,
    remember_password: Option<bool>,
) -> Result<(), String> {
    login_with_credentials(&state, profile_id, username, Zeroizing::new(password), remember_password.unwrap_or(false)).await
}

#[tauri::command]
pub async fn login_saved(state: State<'_, AppState>, profile_id: String) -> Result<(), String> {
    let profile = state.profiles.lock().await.get(&profile_id)?;
    let (username, password) = credentials::read(&profile_id, &profile.fingerprint)?
        .ok_or_else(|| "No saved Windows Credential Manager entry exists for this Host".to_owned())?;
    login_with_credentials(&state, profile_id, username, password, false).await
}

#[tauri::command]
pub async fn has_saved_credentials(state: State<'_, AppState>, profile_id: String) -> Result<bool, String> {
    let profile = state.profiles.lock().await.get(&profile_id)?;
    credentials::has_saved(&profile_id, &profile.fingerprint)
}

#[tauri::command]
pub async fn forget_credentials(profile_id: String) -> Result<(), String> {
    credentials::delete(&profile_id)
}

async fn login_with_credentials(
    state: &AppState,
    profile_id: String,
    username: String,
    password: Zeroizing<String>,
    remember_password: bool,
) -> Result<(), String> {
    log::info!("[terminal] signing in to Host profile={profile_id}");
    if state.connection.lock().await.is_some() {
        return Err("A Host session is already active".to_owned());
    }
    let expected_fingerprint = state.profiles.lock().await.get(&profile_id)?.fingerprint;
    let pending = {
        let mut pending = state.pending.lock().await;
        let connection = pending
            .as_ref()
            .ok_or_else(|| "Verify the Host identity before signing in".to_owned())?;
        if connection.profile_id() != profile_id || connection.fingerprint() != expected_fingerprint {
            return Err("Verified Host profile changed. Verify its fingerprint again before signing in.".to_owned());
        }
        pending.take().expect("pending connection was checked above")
    };
    let profile_id = pending.profile_id().to_owned();
    let connection = match finish_login(pending, username.clone(), password.to_string()).await {
        Ok(connection) => connection,
        Err(error) => {
            log::error!("[terminal] Host login failed for profile={profile_id}: {error}");
            state
                .updates
                .publish(UiEvent::Connection(ConnectionStateDto {
                    phase: "disconnected".to_owned(),
                    transport: None,
                    ice_candidate_pair: None,
                    profile_id: Some(profile_id),
                    host_label: None,
                    fingerprint: None,
                    server_id: None,
                    last_heartbeat: None,
                    message: Some(error.clone()),
                }))
                .await;
            return Err(error);
        }
    };
    let credential_warning = if remember_password {
        credentials::save(&profile_id, &expected_fingerprint, &username, &password).err()
    } else {
        None
    };
    log::info!("[terminal] authenticated Host session established profile={profile_id} transport={}", connection.transport_name());
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
    if let Some(error) = credential_warning {
        state
            .updates
            .publish(UiEvent::Error(format!("Host sign-in succeeded, but the password was not saved: {error}")))
            .await;
    }
    Ok(())
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    *state.pending.lock().await = None;
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

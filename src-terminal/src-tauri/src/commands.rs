use std::{future::Future, pin::Pin};
use tauri::{State, ipc::Channel};
use tokio::{
    sync::{mpsc, oneshot},
    time::{Duration, Instant, sleep, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    allowed_operation::AllowedOperation,
    app_state::{AppState, ConnectionHandle},
    graph_workspace,
    protocol::{
        CommandAckDto, ConnectionStateDto, GraphApplyResultDto, GraphApplyStatusDto, GraphDocumentDto,
        GraphDocumentTransferDto, GraphPresetCatalogDto, GraphPresetSummaryDto, GraphPresetTransferDto, GraphUploadDto,
        HostIdentityDto, MAX_FRAME_BYTES, PROTOCOL_VERSION, ProfileDto, ProfileInputDto, TerminalDiagnosticsDto,
        UiEvent,
    },
    storage::credentials,
    transport::{SessionAction, begin_connection, finish_device_auth, run_session},
};
use shocking_vrc_core::graph::{GraphPreset, parse_graph_preset, preset::GRAPH_PRESET_VERSION};

const GRAPH_DOCUMENT_MAX_BYTES: usize = 4 * 1024 * 1024;
const GRAPH_DOCUMENT_CHUNK_BYTES: usize = 48 * 1024;
const GRAPH_DOCUMENT_TOTAL_TIMEOUT: Duration = Duration::from_secs(110);
const GRAPH_API_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone)]
struct GraphApiSession {
    connection_id: Uuid,
    actions: mpsc::Sender<SessionAction>,
}

#[tauri::command]
pub async fn list_profiles(state: State<'_, AppState>) -> Result<Vec<ProfileDto>, String> {
    Ok(state.profiles.lock().await.list())
}

#[tauri::command]
pub async fn rename_profile(
    state: State<'_, AppState>,
    profile_id: String,
    label: String,
) -> Result<ProfileDto, String> {
    if state
        .connection
        .lock()
        .await
        .as_ref()
        .is_some_and(|connection| connection.profile_id == profile_id)
    {
        return Err("Disconnect from this Host before renaming its connection".to_owned());
    }
    state.profiles.lock().await.rename(&profile_id, &label)
}

#[tauri::command]
pub async fn cache_host_display_name(
    state: State<'_, AppState>,
    profile_id: String,
    host_display_name: Option<String>,
) -> Result<(), String> {
    let connection = state.connection.lock().await;
    if !connection
        .as_ref()
        .is_some_and(|active| active.profile_id == profile_id)
    {
        return Err("A Host display name can only be cached from its authenticated active session".to_owned());
    }
    let display_name = normalize_host_display_name(host_display_name)?;
    state
        .profiles
        .lock()
        .await
        .set_host_display_name(&profile_id, display_name)?;
    Ok(())
}

fn normalize_host_display_name(host_display_name: Option<String>) -> Result<Option<String>, String> {
    let display_name = host_display_name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    if display_name.as_ref().is_some_and(|name| name.chars().count() > 80) {
        return Err("Host display name exceeds 80 characters".to_owned());
    }
    Ok(display_name)
}

#[tauri::command]
pub async fn save_profile_connection_settings(
    state: State<'_, AppState>,
    profile_id: String,
    host: String,
    port: u16,
    connection_mode: String,
    rendezvous_url: String,
    ice_server_urls: Vec<String>,
    force_relay: bool,
) -> Result<ProfileDto, String> {
    if state
        .connection
        .lock()
        .await
        .as_ref()
        .is_some_and(|connection| connection.profile_id == profile_id)
    {
        return Err("Disconnect from this Host before changing connection settings".to_owned());
    }
    let mut profiles = state.profiles.lock().await;
    let current = profiles.get(&profile_id)?;
    profiles.save(ProfileInputDto {
        id: Some(current.id),
        label: current.label,
        host,
        port,
        fingerprint: current.fingerprint,
        server_id: current.server_id,
        connection_mode,
        rendezvous_url,
        ice_server_urls,
        force_relay,
    })
}

#[tauri::command]
pub async fn delete_profile(state: State<'_, AppState>, profile_id: String) -> Result<(), String> {
    let _lifecycle = state.connection_lifecycle.lock().await;
    if state
        .connection
        .lock()
        .await
        .as_ref()
        .is_some_and(|connection| connection.profile_id == profile_id)
    {
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
        state.profiles.lock().await.delete(&plan.profile.id)?;
    }
    state.updates.clear_connection_profile(&plan.profile.id).await;
    Ok(())
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

pub async fn accept_pairing_invitation(
    state: &AppState,
    invitation_uri: &str,
    publish_to_ui: bool,
) -> Result<ProfileDto, String> {
    let _lifecycle = state.connection_lifecycle.lock().await;
    if state.connection.lock().await.is_some() {
        return Err("Disconnect from the current Host before opening a pairing invitation".to_owned());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    let invitation = shocking_vrc_rendezvous::pairing::PairingInvitationV1::parse_uri(invitation_uri, now)
        .map_err(|error| error.to_string())?;
    let direct_host = invitation.direct_host.clone();
    let existing_profile = state
        .profiles
        .lock()
        .await
        .list()
        .into_iter()
        .find(|profile| profile.server_id == invitation.host_id);
    let fingerprint = pairing_fingerprint(existing_profile.as_ref(), &invitation.host_fingerprint)?;
    let existing_id = existing_profile
        .as_ref()
        .map(|profile| profile.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let label = pairing_profile_label(&invitation.host_id, existing_profile.as_ref());
    let profile = crate::storage::normalize_profile(ProfileInputDto {
        id: Some(existing_id),
        label,
        host: direct_host.clone().unwrap_or_default(),
        port: invitation.direct_port.unwrap_or_default(),
        fingerprint,
        server_id: invitation.host_id.clone(),
        connection_mode: if direct_host.is_none() {
            "traversal_only".to_owned()
        } else if invitation.rendezvous_url.is_none() {
            "direct_only".to_owned()
        } else {
            "auto".to_owned()
        },
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

fn pairing_fingerprint(existing: Option<&ProfileDto>, invited_fingerprint: &str) -> Result<String, String> {
    if let Some(existing) = existing {
        if !existing.fingerprint.eq_ignore_ascii_case(invited_fingerprint) {
            return Err("The invitation Host fingerprint does not match the saved identity pin".to_owned());
        }
        return Ok(existing.fingerprint.clone());
    }
    Ok(invited_fingerprint.to_owned())
}

fn pairing_profile_label(host_id: &str, existing: Option<&ProfileDto>) -> String {
    existing
        .map(|profile| profile.label.clone())
        .unwrap_or_else(|| format!("Host {}", host_id.chars().take(8).collect::<String>()))
}

#[tauri::command]
pub async fn connect_profile(state: State<'_, AppState>, profile_id: String) -> Result<HostIdentityDto, String> {
    let _lifecycle = state.connection_lifecycle.lock().await;
    // Tauri dispatches async commands from a WebView2 protocol callback. Keep the
    // future it spawns small; run the large connection state machine on a worker.
    let connection: Pin<Box<dyn Future<Output = Result<HostIdentityDto, String>> + Send + '_>> =
        Box::pin(connect_profile_inner(&state, profile_id));
    connection.await
}

async fn connect_profile_inner(state: &AppState, profile_id: String) -> Result<HostIdentityDto, String> {
    if state.connection.lock().await.is_some() {
        return Err("Disconnect from the current Host before selecting another profile".to_owned());
    }
    let session_generation = state.updates.begin_session();
    state.updates.diagnostics.record(
        crate::diagnostics::DiagnosticKind::ConnectionAttempt,
        None,
        None,
        None,
        None,
        None,
    );
    let pending_profile = state
        .pending_pairing_profile
        .lock()
        .await
        .clone()
        .filter(|profile| profile.id == profile_id);
    let mut profile = match pending_profile {
        Some(profile) => profile,
        None => state.profiles.lock().await.get(&profile_id)?,
    };
    log::info!("[terminal] verifying Host profile mode={}", profile.connection_mode);
    let invitation = state
        .pending_pairing
        .lock()
        .await
        .clone()
        .filter(|invitation| invitation.host_id == profile.server_id);
    let rendezvous_authorization = if let Some(invitation) = invitation.as_ref() {
        invitation
            .route_token
            .as_ref()
            .map(|token| Zeroizing::new(token.clone()))
    } else {
        credentials::read_device_grant(&profile.id, &profile.fingerprint)?
    };
    state
        .updates
        .publish(UiEvent::Connection(ConnectionStateDto {
            phase: "connecting".to_owned(),
            transport: None,
            ice_candidate_pair: None,
            ice_servers_configured: None,
            rendezvous_stun_received: None,
            turn_credentials_received: None,
            local_candidates_gathered: None,
            profile_id: Some(profile.id.clone()),
            host_label: Some(profile.label.clone()),
            fingerprint: Some(profile.fingerprint.clone()),
            server_id: None,
            last_heartbeat: None,
            message: Some("Checking the signed Host identity before device authentication".to_owned()),
            session_generation: Some(session_generation),
            server_epoch: None,
            transport_generation: None,
        }))
        .await;
    let (pending, identity) = match begin_connection(profile.clone(), rendezvous_authorization, None).await {
        Ok(value) => value,
        Err(error) => {
            publish_connection_error(&state, &profile, session_generation, error.clone()).await;
            return Err(error);
        }
    };
    if !profile.server_id.is_empty() && identity.server_id != profile.server_id {
        publish_connection_error(
            &state,
            &profile,
            session_generation,
            "Host ID does not match the invitation or saved profile".to_owned(),
        )
        .await;
        return Err("Host ID does not match the invitation or saved profile".to_owned());
    }
    let terminal_identity = credentials::TerminalDeviceIdentity::load_or_create()?;
    let (connection, identity) = match finish_device_auth(pending, &terminal_identity, invitation.as_ref()).await {
        Ok(connection) => (connection, identity),
        Err(pairing_error) if invitation.is_some() => {
            log::warn!(
                "[terminal] pairing handshake failed; retrying once with the trusted device key: {pairing_error}"
            );
            let recovery_authorization = invitation
                .as_ref()
                .and_then(|invitation| invitation.route_token.as_ref())
                .map(|token| Zeroizing::new(token.clone()));
            let (recovery_pending, recovery_identity) = match begin_connection(
                profile.clone(),
                recovery_authorization,
                None,
            )
            .await
            {
                Ok(value) => value,
                Err(recovery_error) => {
                    let error = format!(
                        "Pairing attempt failed ({pairing_error}); trusted-device recovery could not reconnect: {recovery_error}"
                    );
                    log::error!("[terminal] passwordless Host recovery failed: {error}");
                    publish_connection_error(&state, &profile, session_generation, error.clone()).await;
                    return Err(error);
                }
            };
            if recovery_identity.server_id != identity.server_id
                || recovery_identity.fingerprint != identity.fingerprint
            {
                let error = "Trusted-device recovery connected to a different Host identity".to_owned();
                publish_connection_error(&state, &profile, session_generation, error.clone()).await;
                return Err(error);
            }
            match finish_device_auth(recovery_pending, &terminal_identity, None).await {
                Ok(connection) => (connection, recovery_identity),
                Err(recovery_error) => {
                    let error = format!(
                        "Pairing attempt failed ({pairing_error}); trusted-device recovery failed: {recovery_error}"
                    );
                    log::error!("[terminal] passwordless Host recovery failed: {error}");
                    publish_connection_error(&state, &profile, session_generation, error.clone()).await;
                    return Err(error);
                }
            }
        }
        Err(error) => {
            log::error!("[terminal] passwordless Host authentication failed: {error}");
            publish_connection_error(&state, &profile, session_generation, error.clone()).await;
            return Err(error);
        }
    };
    let mut connection = connection;
    connection.session_generation = session_generation;
    credentials::save_device_grant(&profile.id, &profile.fingerprint, &connection.device_grant)?;
    let was_pending_pairing = invitation.is_some();
    let should_save_profile = was_pending_pairing || profile.server_id.is_empty();
    profile.server_id = identity.server_id.clone();
    if should_save_profile {
        state.profiles.lock().await.save(ProfileInputDto {
            id: Some(profile.id.clone()),
            label: profile.label.clone(),
            host: profile.host.clone().unwrap_or_default(),
            port: profile.port.unwrap_or_default(),
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
    log::info!(
        "[terminal] paired device session established profile={} transport={}",
        profile_id,
        connection.transport_name()
    );
    let (actions, receiver) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    let connection_id = Uuid::new_v4();
    let (closed_tx, closed_rx) = oneshot::channel();
    let handle = ConnectionHandle {
        id: connection_id,
        profile_id,
        actions,
        cancel: cancel.clone(),
        closed: Some(closed_rx),
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
        drop(current);
        let _ = closed_tx.send(());
    });
    Ok(identity)
}

async fn publish_connection_error(state: &AppState, profile: &ProfileDto, session_generation: u64, error: String) {
    state.updates.diagnostics.record(
        crate::diagnostics::DiagnosticKind::ConnectionFailed,
        None,
        None,
        None,
        None,
        None,
    );
    state
        .updates
        .publish(UiEvent::Connection(ConnectionStateDto {
            phase: "disconnected".to_owned(),
            transport: None,
            ice_candidate_pair: None,
            ice_servers_configured: None,
            rendezvous_stun_received: None,
            turn_credentials_received: None,
            local_candidates_gathered: None,
            profile_id: Some(profile.id.clone()),
            host_label: Some(profile.label.clone()),
            fingerprint: Some(profile.fingerprint.clone()),
            server_id: Some(profile.server_id.clone()),
            last_heartbeat: None,
            message: Some(error),
            session_generation: Some(session_generation),
            server_epoch: None,
            transport_generation: None,
        }))
        .await;
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, host_display_name: Option<String>) -> Result<(), String> {
    let _lifecycle = state.connection_lifecycle.lock().await;
    let mut current = state.connection.lock().await;
    if let Some(active) = current.as_ref() {
        match normalize_host_display_name(host_display_name) {
            Ok(Some(display_name)) => {
                if let Err(error) = state
                    .profiles
                    .lock()
                    .await
                    .set_host_display_name(&active.profile_id, Some(display_name))
                {
                    log::warn!("[terminal] could not cache authenticated Host display name before disconnect: {error}");
                }
            }
            Ok(None) => {}
            Err(error) => log::warn!("[terminal] skipped invalid Host display name during disconnect: {error}"),
        }
    }
    let handle = current.take();
    drop(current);
    if let Some(handle) = handle {
        state.updates.diagnostics.record(
            crate::diagnostics::DiagnosticKind::DisconnectRequested,
            None,
            None,
            None,
            None,
            None,
        );
        log::info!("[terminal] user requested session disconnect profile={}", handle.profile_id);
        handle.cancel.cancel();
        let _ = handle.actions.try_send(SessionAction::Disconnect);
        if let Some(mut closed) = handle.closed {
            if timeout(Duration::from_secs(6), &mut closed).await.is_err() {
                return Err("Terminal session cleanup did not finish within 6 seconds".to_owned());
            }
        }
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
pub async fn fetch_logs(
    state: State<'_, AppState>,
    cursor: Option<String>,
    limit: Option<u16>,
) -> Result<crate::protocol::LogsDto, String> {
    let (response, receiver) = oneshot::channel();
    send_action(
        &state,
        SessionAction::FetchLogs(cursor, limit.unwrap_or(100).clamp(1, 200), response),
    )
    .await?;
    let response = timeout(Duration::from_secs(30), receiver)
        .await
        .map_err(|_| "Timed out waiting for Host log response".to_owned())?
        .map_err(|_| "Host session ended before the log response arrived".to_owned())??;
    serde_json::from_value::<crate::protocol::LogsDto>(response)
        .map_err(|_| "Host returned an invalid log response".to_owned())
}

#[tauri::command]
pub async fn fetch_graph_document(state: State<'_, AppState>) -> Result<GraphDocumentDto, String> {
    timeout(GRAPH_DOCUMENT_TOTAL_TIMEOUT, fetch_graph_document_inner(&state))
        .await
        .map_err(|_| "Timed out downloading the Host Graph document".to_owned())?
}

async fn fetch_graph_document_inner(state: &AppState) -> Result<GraphDocumentDto, String> {
    let session = current_graph_api_session(state).await?;
    fetch_graph_document_for_session(state, &session).await
}

async fn fetch_graph_document_for_session(
    state: &AppState,
    session: &GraphApiSession,
) -> Result<GraphDocumentDto, String> {
    let request_id = Uuid::new_v4();
    let start_response = graph_api_request_for_session(
        state,
        session,
        "graph.document.get",
        "graph.document",
        request_id,
        serde_json::json!({}),
    )
    .await?;
    let transfer = validate_graph_transfer_start(&start_response, request_id)?;
    let mut bytes = Vec::with_capacity(transfer.total_bytes);
    let mut offset = 0_usize;

    while offset < transfer.total_bytes {
        let request_id = Uuid::new_v4();
        let response = graph_api_request_for_session(
            state,
            session,
            "graph.document.chunk",
            "graph.document.chunk",
            request_id,
            serde_json::json!({
                "transfer_id": transfer.transfer_id,
                "offset": offset,
                "max_bytes": GRAPH_DOCUMENT_CHUNK_BYTES,
                "sha256": transfer.sha256,
            }),
        )
        .await?;
        let chunk = validate_graph_chunk_response(&response, request_id, &transfer, offset)?;
        offset = offset
            .checked_add(chunk.len())
            .ok_or_else(|| "Host Graph document length overflowed".to_owned())?;
        bytes.extend_from_slice(&chunk);
    }

    validate_graph_document_payload(&transfer, &bytes)
}

#[tauri::command]
pub async fn fetch_graph_presets(state: State<'_, AppState>) -> Result<GraphPresetCatalogDto, String> {
    let request_id = Uuid::new_v4();
    let response =
        graph_api_request(&state, "graph.presets.get", "graph.presets", request_id, serde_json::json!({})).await?;
    let entries = response
        .get("entries")
        .cloned()
        .ok_or_else(|| "Host returned an invalid Graph preset catalog".to_owned())?;
    serde_json::from_value::<Vec<GraphPresetSummaryDto>>(entries)
        .map(|entries| GraphPresetCatalogDto { entries })
        .map_err(|_| "Host returned an invalid Graph preset catalog".to_owned())
}

#[tauri::command]
pub async fn fetch_graph_preset(state: State<'_, AppState>, preset_id: String) -> Result<GraphPreset, String> {
    let deadline = Instant::now() + GRAPH_DOCUMENT_TOTAL_TIMEOUT;
    fetch_graph_preset_inner(&state, &preset_id, deadline).await
}

async fn fetch_graph_preset_inner(state: &AppState, preset_id: &str, deadline: Instant) -> Result<GraphPreset, String> {
    if preset_id.trim().is_empty() || preset_id.len() > 128 || preset_id.chars().any(char::is_control) {
        return Err("Graph preset ID is invalid".to_owned());
    }
    let request_id = Uuid::new_v4();
    let response = graph_api_request_until(
        state,
        "graph.preset.get",
        "graph.preset",
        request_id,
        serde_json::json!({ "preset_id": preset_id }),
        deadline,
    )
    .await?;
    let transfer = validate_graph_preset_transfer_start(&response, request_id, preset_id)?;
    let mut bytes = Vec::with_capacity(transfer.total_bytes);
    let mut offset = 0_usize;
    while offset < transfer.total_bytes {
        let request_id = Uuid::new_v4();
        let response = graph_api_request_until(
            state,
            "graph.preset.chunk",
            "graph.preset.chunk",
            request_id,
            serde_json::json!({
                "transfer_id": transfer.transfer_id,
                "offset": offset,
                "max_bytes": transfer.chunk_bytes,
                "sha256": transfer.sha256,
            }),
            deadline,
        )
        .await?;
        let chunk = validate_graph_preset_chunk_response(&response, request_id, &transfer, offset)?;
        offset = offset
            .checked_add(chunk.len())
            .ok_or_else(|| "Graph preset length overflowed".to_owned())?;
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != transfer.total_bytes || graph_sha256_hex(&bytes) != transfer.sha256.to_ascii_lowercase() {
        return Err("Host Graph preset checksum or length did not match".to_owned());
    }
    let json = std::str::from_utf8(&bytes).map_err(|_| "Host Graph preset is not UTF-8 JSON".to_owned())?;
    let preset = parse_graph_preset(json)?;
    if preset.version != GRAPH_PRESET_VERSION {
        return Err(format!(
            "Unsupported Graph preset version {}; expected {GRAPH_PRESET_VERSION}",
            preset.version
        ));
    }
    let graph_json =
        serde_json::to_string(&preset.graph).map_err(|_| "Host Graph preset could not be normalized".to_owned())?;
    let validation = graph_workspace::validate_graph_json(&graph_json)?;
    if !validation.missing_types.is_empty() {
        return Err("Host Graph preset contains node types unsupported by this Terminal version".to_owned());
    }
    Ok(preset)
}

#[tauri::command]
pub async fn apply_graph_draft(
    state: State<'_, AppState>,
    base_revision: u64,
    graph_json: String,
) -> Result<GraphApplyResultDto, String> {
    apply_graph_draft_inner(&state, base_revision, &graph_json).await
}

async fn apply_graph_draft_inner(
    state: &AppState,
    base_revision: u64,
    graph_json: &str,
) -> Result<GraphApplyResultDto, String> {
    use base64::Engine;

    let validation = graph_workspace::validate_graph_json(graph_json)?;
    if !validation.valid {
        let first_issue = validation
            .issues
            .iter()
            .find(|issue| issue.severity == "error")
            .map(|issue| issue.message.as_str())
            .unwrap_or("Graph draft validation failed");
        return Err(format!("Graph draft is invalid: {first_issue}"));
    }
    let bytes = serde_json::to_vec(&validation.graph).map_err(|_| "Graph draft could not be serialized".to_owned())?;
    if bytes.is_empty() || bytes.len() > GRAPH_DOCUMENT_MAX_BYTES {
        return Err("Graph draft must be between 1 byte and 4 MiB".to_owned());
    }

    let session = current_graph_api_session(state).await?;
    let deadline = Instant::now() + GRAPH_DOCUMENT_TOTAL_TIMEOUT;
    let upload_request_id = Uuid::new_v4();
    let upload_response = graph_api_request_until_session(
        state,
        &session,
        "graph.apply.start",
        "graph.apply.upload",
        upload_request_id,
        serde_json::json!({
            "base_revision": base_revision,
            "total_bytes": bytes.len(),
            "sha256": graph_sha256_hex(&bytes),
        }),
        deadline,
    )
    .await?;
    let upload = validate_graph_upload_start(&upload_response, upload_request_id, base_revision, bytes.len())?;

    let mut offset = 0_usize;
    for chunk in bytes.chunks(upload.chunk_bytes) {
        let request_id = Uuid::new_v4();
        let response = graph_api_request_until_session(
            state,
            &session,
            "graph.apply.chunk",
            "graph.apply.chunk",
            request_id,
            serde_json::json!({
                "upload_id": upload.upload_id,
                "offset": offset,
                "max_bytes": chunk.len(),
                "sha256": graph_sha256_hex(chunk),
                "data": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(chunk),
            }),
            deadline,
        )
        .await?;
        validate_graph_apply_chunk_response(&response, request_id, &upload, offset, chunk.len())?;
        offset = offset
            .checked_add(chunk.len())
            .ok_or_else(|| "Graph upload offset overflowed".to_owned())?;
    }
    if offset != upload.total_bytes {
        return Err("Graph upload ended before all bytes were acknowledged".to_owned());
    }

    let commit_request_id = Uuid::new_v4();
    let commit = graph_commit_once(
        || {
            graph_api_request_until_session(
                state,
                &session,
                "graph.apply.commit",
                "graph.apply.status",
                commit_request_id,
                serde_json::json!({ "upload_id": upload.upload_id }),
                deadline,
            )
        },
        base_revision,
    )
    .await;
    let commit_response = match commit {
        Ok(response) => response,
        Err(mut unknown) => {
            unknown.operation_id = Some(upload.upload_id.clone());
            return Ok(unknown);
        }
    };
    let upload_id = match Uuid::parse_str(&upload.upload_id) {
        Ok(id) => id,
        Err(_) => {
            return Ok(graph_apply_unknown_for_operation(
                base_revision,
                &upload.upload_id,
                "Host returned an invalid upload identifier.",
            ));
        }
    };
    let initial_status = match validate_graph_apply_status_response(&commit_response, commit_request_id, upload_id) {
        Ok(status) => status,
        Err(_) => {
            return Ok(graph_apply_unknown_for_operation(
                base_revision,
                &upload.upload_id,
                "Host received the apply request but returned an invalid receipt.",
            ));
        }
    };
    observe_graph_apply(state, &session, &upload, base_revision, initial_status, deadline).await
}

async fn observe_graph_apply(
    state: &AppState,
    session: &GraphApiSession,
    upload: &GraphUploadDto,
    base_revision: u64,
    mut status: GraphApplyStatusDto,
    deadline: Instant,
) -> Result<GraphApplyResultDto, String> {
    let upload_id = match Uuid::parse_str(&upload.upload_id) {
        Ok(id) => id,
        Err(_) => {
            return Ok(graph_apply_unknown_for_operation(
                base_revision,
                &upload.upload_id,
                "Host returned an invalid upload identifier.",
            ));
        }
    };
    let mut next_poll_at = Instant::now();
    loop {
        match status.state.as_str() {
            "applied" => {
                let Some(applied_revision) = status.graph_revision else {
                    return Ok(graph_apply_unknown_for_operation(
                        base_revision,
                        &upload.upload_id,
                        "Host reported apply without a graph revision.",
                    ));
                };
                if applied_revision <= base_revision {
                    return Ok(graph_apply_unknown_for_operation(
                        base_revision,
                        &upload.upload_id,
                        "Host reported an apply revision that did not advance the submitted base revision.",
                    ));
                }
                if status
                    .graph_sha256
                    .as_deref()
                    .is_some_and(|identity| !valid_graph_sha256(identity))
                {
                    return Ok(graph_apply_unknown_for_operation(
                        base_revision,
                        &upload.upload_id,
                        "Host returned an invalid committed Graph identity.",
                    ));
                }
                return Ok(confirm_graph_apply_readback(
                    || fetch_graph_document_for_session(state, session),
                    base_revision,
                    &upload.upload_id,
                    applied_revision,
                    status.graph_sha256.as_deref(),
                    deadline,
                )
                .await);
            }
            "conflict" => {
                return Ok(GraphApplyResultDto {
                    state: "conflict".to_owned(),
                    base_revision,
                    operation_id: Some(upload.upload_id.clone()),
                    graph_revision: None,
                    current_revision: status.current_revision,
                    message: status
                        .message
                        .or_else(|| Some("Host Graph changed since this draft was loaded.".to_owned())),
                });
            }
            "rejected" => {
                return Ok(GraphApplyResultDto {
                    state: "rejected".to_owned(),
                    base_revision,
                    operation_id: Some(upload.upload_id.clone()),
                    graph_revision: None,
                    current_revision: status.current_revision,
                    message: status
                        .message
                        .or_else(|| status.code.map(|code| format!("Host rejected Graph apply ({code})."))),
                });
            }
            "unknown" => {
                let details = status
                    .message
                    .or_else(|| {
                        status
                            .code
                            .map(|code| format!("Host apply status is unknown ({code})."))
                    })
                    .unwrap_or_else(|| "Host could not determine whether the apply executed.".to_owned());
                return Ok(graph_apply_unknown_for_operation(base_revision, &upload.upload_id, &details));
            }
            "pending" | "uploading" => {}
            _ => {
                return Ok(graph_apply_unknown_for_operation(
                    base_revision,
                    &upload.upload_id,
                    "Host returned an unrecognized Graph apply state.",
                ));
            }
        }

        if Instant::now() >= deadline {
            return Ok(graph_apply_unknown_for_operation(
                base_revision,
                &upload.upload_id,
                "Timed out waiting for Host apply confirmation. The draft was not replayed.",
            ));
        }
        if next_poll_at > Instant::now() {
            let wait = next_poll_at.saturating_duration_since(Instant::now());
            if timeout_at(deadline, sleep(wait)).await.is_err() {
                return Ok(graph_apply_unknown_for_operation(
                    base_revision,
                    &upload.upload_id,
                    "Timed out waiting for Host apply confirmation. The draft was not replayed.",
                ));
            }
        }
        let request_id = Uuid::new_v4();
        let response = match graph_api_request_until_session(
            state,
            session,
            "graph.apply.status",
            "graph.apply.status",
            request_id,
            serde_json::json!({ "upload_id": upload_id }),
            deadline,
        )
        .await
        {
            Ok(response) => response,
            Err(_) => {
                return Ok(graph_apply_unknown_for_operation(
                    base_revision,
                    &upload.upload_id,
                    "Host accepted the apply request, but its status could not be confirmed. The draft was not replayed.",
                ));
            }
        };
        status = match validate_graph_apply_status_response(&response, request_id, upload_id) {
            Ok(status) => status,
            Err(_) => {
                return Ok(graph_apply_unknown_for_operation(
                    base_revision,
                    &upload.upload_id,
                    "Host returned an invalid Graph apply status. The draft was not replayed.",
                ));
            }
        };
        next_poll_at = Instant::now() + Duration::from_millis(250);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyReadback {
    Applied,
    HostChanged { current_revision: u64 },
    AwaitingPublication,
    InconsistentDocument,
}

fn classify_apply_readback(
    applied_revision: u64,
    committed_graph_sha256: Option<&str>,
    fresh: &GraphDocumentDto,
) -> ApplyReadback {
    if fresh.graph_revision < applied_revision {
        return ApplyReadback::AwaitingPublication;
    }
    if let Some(expected) = committed_graph_sha256 {
        if !valid_graph_sha256(expected) {
            return ApplyReadback::InconsistentDocument;
        }
        if fresh.graph_revision == applied_revision {
            let Ok(bytes) = serde_json::to_vec(&fresh.graph) else {
                return ApplyReadback::InconsistentDocument;
            };
            if !graph_sha256_hex(&bytes).eq_ignore_ascii_case(expected) {
                return ApplyReadback::InconsistentDocument;
            }
        }
    }
    if fresh.graph_revision > applied_revision {
        ApplyReadback::HostChanged {
            current_revision: fresh.graph_revision,
        }
    } else {
        ApplyReadback::Applied
    }
}

async fn confirm_graph_apply_readback<F, Fut>(
    mut fetch_document: F,
    base_revision: u64,
    operation_id: &str,
    applied_revision: u64,
    committed_graph_sha256: Option<&str>,
    deadline: Instant,
) -> GraphApplyResultDto
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<GraphDocumentDto, String>>,
{
    let mut last_revision = None;
    loop {
        let fresh = match timeout_at(deadline, fetch_document()).await {
            Ok(Ok(document)) => document,
            _ => {
                return graph_apply_unreconciled(
                    base_revision,
                    operation_id,
                    applied_revision,
                    last_revision,
                    "Apply was confirmed, but the Host Graph document could not be refreshed.",
                );
            }
        };
        last_revision = Some(fresh.graph_revision);
        match classify_apply_readback(applied_revision, committed_graph_sha256, &fresh) {
            ApplyReadback::Applied => {
                return GraphApplyResultDto {
                    state: "applied".to_owned(),
                    base_revision,
                    operation_id: Some(operation_id.to_owned()),
                    graph_revision: Some(applied_revision),
                    current_revision: Some(fresh.graph_revision),
                    message: None,
                };
            }
            ApplyReadback::HostChanged { current_revision } => {
                return GraphApplyResultDto {
                    state: "host_changed".to_owned(),
                    base_revision,
                    operation_id: Some(operation_id.to_owned()),
                    graph_revision: Some(applied_revision),
                    current_revision: Some(current_revision),
                    message: Some("Host Graph changed after the confirmed Apply.".to_owned()),
                };
            }
            ApplyReadback::InconsistentDocument => {
                return graph_apply_unreconciled(
                    base_revision,
                    operation_id,
                    applied_revision,
                    Some(fresh.graph_revision),
                    "Apply was confirmed, but the Host Graph document did not match its committed identity.",
                );
            }
            ApplyReadback::AwaitingPublication => {
                if Instant::now() >= deadline || timeout_at(deadline, sleep(Duration::from_millis(250))).await.is_err()
                {
                    return graph_apply_unreconciled(
                        base_revision,
                        operation_id,
                        applied_revision,
                        last_revision,
                        "Apply was confirmed, but Host Graph publication did not reach its committed revision.",
                    );
                }
            }
        }
    }
}

fn valid_graph_sha256(identity: &str) -> bool {
    identity.len() == 64 && identity.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn graph_apply_unknown(base_revision: u64, message: &str) -> GraphApplyResultDto {
    GraphApplyResultDto {
        state: "unknown".to_owned(),
        base_revision,
        operation_id: None,
        graph_revision: None,
        current_revision: None,
        message: Some(message.to_owned()),
    }
}

fn graph_apply_unknown_for_operation(base_revision: u64, operation_id: &str, message: &str) -> GraphApplyResultDto {
    GraphApplyResultDto {
        operation_id: Some(operation_id.to_owned()),
        ..graph_apply_unknown(base_revision, message)
    }
}

fn graph_apply_unreconciled(
    base_revision: u64,
    operation_id: &str,
    applied_revision: u64,
    current_revision: Option<u64>,
    message: &str,
) -> GraphApplyResultDto {
    GraphApplyResultDto {
        state: "applied_unreconciled".to_owned(),
        base_revision,
        operation_id: Some(operation_id.to_owned()),
        graph_revision: Some(applied_revision),
        current_revision,
        message: Some(message.to_owned()),
    }
}

#[tauri::command]
pub async fn set_engine_mode(state: State<'_, AppState>, mode: String) -> Result<CommandAckDto, String> {
    let actions = current_action_sender(&state).await?;
    submit_engine_mode(&actions, &mode).await
}

async fn submit_engine_mode(actions: &mpsc::Sender<SessionAction>, mode: &str) -> Result<CommandAckDto, String> {
    if !matches!(mode, "classic" | "graph") {
        return Err("Engine mode must be classic or graph".to_owned());
    }
    let request_id = Uuid::new_v4();
    let frame = serde_json::json!({
        "type": "command",
        "protocol": PROTOCOL_VERSION,
        "request_id": request_id,
        "op": "engine.set_mode",
        "args": { "mode": mode },
    });
    let (response, receiver) = oneshot::channel();
    actions
        .send(SessionAction::Command {
            request_id,
            frame,
            response,
        })
        .await
        .map_err(|_| "The Host session is no longer available".to_owned())?;
    match timeout(Duration::from_secs(10), receiver).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(
            "Host session ended before command acknowledgement; outcome is unknown. The command was not replayed."
                .to_owned(),
        ),
        Err(_) => Err(
            "Timed out waiting for Host command acknowledgement; outcome is unknown. The command was not replayed."
                .to_owned(),
        ),
    }
}

async fn graph_api_request(
    state: &AppState,
    request_type: &str,
    expected_type: &str,
    request_id: Uuid,
    fields: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let actions = current_action_sender(state).await?;
    graph_api_request_with_sender(&actions, request_type, expected_type, request_id, fields).await
}

async fn graph_api_request_for_session(
    state: &AppState,
    session: &GraphApiSession,
    request_type: &str,
    expected_type: &str,
    request_id: Uuid,
    fields: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let current = state.connection.lock().await;
    if current.as_ref().map(|connection| connection.id) != Some(session.connection_id) {
        return Err("The Host session changed during Graph apply confirmation".to_owned());
    }
    drop(current);
    graph_api_request_with_sender(&session.actions, request_type, expected_type, request_id, fields).await
}

async fn graph_api_request_with_sender(
    actions: &mpsc::Sender<SessionAction>,
    request_type: &str,
    expected_type: &str,
    request_id: Uuid,
    fields: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut frame = serde_json::json!({
        "type": request_type,
        "protocol": PROTOCOL_VERSION,
        "request_id": request_id,
    });
    let extras = fields
        .as_object()
        .ok_or_else(|| "Graph request fields must be an object".to_owned())?;
    let frame_object = frame
        .as_object_mut()
        .ok_or_else(|| "Graph request could not be created".to_owned())?;
    frame_object.extend(extras.iter().map(|(key, value)| (key.clone(), value.clone())));

    let (response, receiver) = oneshot::channel();
    actions
        .send(SessionAction::GraphRequest { frame, response })
        .await
        .map_err(|_| "The Host session is no longer available".to_owned())?;
    let response = timeout(GRAPH_API_REQUEST_TIMEOUT, receiver)
        .await
        .map_err(|_| "Timed out waiting for a Host Graph response".to_owned())?
        .map_err(|_| "Host session ended before the Graph response arrived".to_owned())??;
    let request_id_text = request_id.to_string();
    if response.get("type").and_then(serde_json::Value::as_str) != Some(expected_type)
        || response.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id_text.as_str())
        || response.get("protocol").and_then(serde_json::Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
    {
        return Err("Host returned a mismatched Graph response".to_owned());
    }
    let response_bytes = serde_json::to_vec(&response).map_err(|_| "Host returned invalid Graph JSON".to_owned())?;
    if response_bytes.len() > MAX_FRAME_BYTES {
        return Err("Host Graph response exceeds the protocol frame limit".to_owned());
    }
    Ok(response)
}

async fn graph_api_request_until(
    state: &AppState,
    request_type: &str,
    expected_type: &str,
    request_id: Uuid,
    fields: serde_json::Value,
    deadline: Instant,
) -> Result<serde_json::Value, String> {
    timeout_at(
        deadline,
        graph_api_request(state, request_type, expected_type, request_id, fields),
    )
    .await
    .map_err(|_| "Graph transfer exceeded its 110 second time limit".to_owned())?
}

async fn graph_api_request_until_session(
    state: &AppState,
    session: &GraphApiSession,
    request_type: &str,
    expected_type: &str,
    request_id: Uuid,
    fields: serde_json::Value,
    deadline: Instant,
) -> Result<serde_json::Value, String> {
    timeout_at(
        deadline,
        graph_api_request_for_session(state, session, request_type, expected_type, request_id, fields),
    )
    .await
    .map_err(|_| "Graph transfer exceeded its 110 second time limit".to_owned())?
}

async fn graph_commit_once<F, Fut>(request: F, base_revision: u64) -> Result<serde_json::Value, GraphApplyResultDto>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<serde_json::Value, String>>,
{
    request().await.map_err(|_| {
        graph_apply_unknown(
            base_revision,
            "Host may have received the apply request, but confirmation timed out. The draft was not replayed.",
        )
    })
}

fn validate_graph_preset_transfer_start(
    response: &serde_json::Value,
    request_id: Uuid,
    preset_id: &str,
) -> Result<GraphPresetTransferDto, String> {
    let request_id_text = request_id.to_string();
    if response.get("type").and_then(serde_json::Value::as_str) != Some("graph.preset")
        || response.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id_text.as_str())
        || response.get("protocol").and_then(serde_json::Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
    {
        return Err("Host returned mismatched Graph preset transfer metadata".to_owned());
    }
    let transfer: GraphPresetTransferDto = serde_json::from_value(response.clone())
        .map_err(|_| "Host returned invalid Graph preset transfer metadata".to_owned())?;
    Uuid::parse_str(&transfer.transfer_id)
        .map_err(|_| "Host returned an invalid Graph preset transfer ID".to_owned())?;
    if transfer.preset_id != preset_id
        || transfer.total_bytes == 0
        || transfer.total_bytes > GRAPH_DOCUMENT_MAX_BYTES
        || transfer.chunk_bytes != GRAPH_DOCUMENT_CHUNK_BYTES
        || transfer.sha256.len() != 64
        || !transfer.sha256.chars().all(|character| character.is_ascii_hexdigit())
    {
        return Err("Host Graph preset metadata is outside the supported bounds".to_owned());
    }
    Ok(transfer)
}

fn validate_graph_preset_chunk_response(
    response: &serde_json::Value,
    request_id: Uuid,
    transfer: &GraphPresetTransferDto,
    offset: usize,
) -> Result<Vec<u8>, String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    if offset >= transfer.total_bytes {
        return Err("Graph preset chunk offset is outside the preset".to_owned());
    }
    let expected_len = (transfer.total_bytes - offset).min(transfer.chunk_bytes);
    let request_id_text = request_id.to_string();
    if response.get("type").and_then(serde_json::Value::as_str) != Some("graph.preset.chunk")
        || response.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id_text.as_str())
        || response.get("protocol").and_then(serde_json::Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
        || response.get("transfer_id").and_then(serde_json::Value::as_str) != Some(transfer.transfer_id.as_str())
        || response.get("preset_id").and_then(serde_json::Value::as_str) != Some(transfer.preset_id.as_str())
        || response.get("offset").and_then(serde_json::Value::as_u64) != Some(offset as u64)
        || response.get("total_bytes").and_then(serde_json::Value::as_u64) != Some(transfer.total_bytes as u64)
        || response.get("sha256").and_then(serde_json::Value::as_str) != Some(transfer.sha256.as_str())
    {
        return Err("Host returned a mismatched Graph preset chunk".to_owned());
    }
    let next_offset = offset
        .checked_add(expected_len)
        .ok_or_else(|| "Graph preset chunk offset overflowed".to_owned())?;
    if response.get("next_offset").and_then(serde_json::Value::as_u64) != Some(next_offset as u64)
        || response.get("done").and_then(serde_json::Value::as_bool) != Some(next_offset == transfer.total_bytes)
    {
        return Err("Host returned a Graph preset chunk with an invalid length".to_owned());
    }
    let data = response
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Host returned a Graph preset chunk without data".to_owned())?;
    let decoded = URL_SAFE_NO_PAD
        .decode(data)
        .map_err(|_| "Host returned invalid Graph preset chunk encoding".to_owned())?;
    if decoded.len() != expected_len {
        return Err("Host returned a Graph preset chunk with an invalid byte length".to_owned());
    }
    Ok(decoded)
}

fn validate_graph_upload_start(
    response: &serde_json::Value,
    request_id: Uuid,
    base_revision: u64,
    total_bytes: usize,
) -> Result<GraphUploadDto, String> {
    let request_id_text = request_id.to_string();
    if response.get("type").and_then(serde_json::Value::as_str) != Some("graph.apply.upload")
        || response.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id_text.as_str())
        || response.get("protocol").and_then(serde_json::Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
    {
        return Err("Host returned mismatched Graph apply upload metadata".to_owned());
    }
    let upload: GraphUploadDto = serde_json::from_value(response.clone())
        .map_err(|_| "Host returned invalid Graph apply upload metadata".to_owned())?;
    Uuid::parse_str(&upload.upload_id).map_err(|_| "Host returned an invalid Graph upload ID".to_owned())?;
    if upload.base_revision != base_revision
        || upload.total_bytes != total_bytes
        || upload.chunk_bytes != GRAPH_DOCUMENT_CHUNK_BYTES
        || upload.next_offset != 0
    {
        return Err("Host Graph upload metadata does not match the submitted draft".to_owned());
    }
    Ok(upload)
}

fn validate_graph_apply_chunk_response(
    response: &serde_json::Value,
    request_id: Uuid,
    upload: &GraphUploadDto,
    offset: usize,
    chunk_len: usize,
) -> Result<(), String> {
    let request_id_text = request_id.to_string();
    let next_offset = offset
        .checked_add(chunk_len)
        .ok_or_else(|| "Graph apply chunk offset overflowed".to_owned())?;
    if response.get("type").and_then(serde_json::Value::as_str) != Some("graph.apply.chunk")
        || response.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id_text.as_str())
        || response.get("protocol").and_then(serde_json::Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
        || response.get("upload_id").and_then(serde_json::Value::as_str) != Some(upload.upload_id.as_str())
        || response.get("next_offset").and_then(serde_json::Value::as_u64) != Some(next_offset as u64)
        || response.get("total_bytes").and_then(serde_json::Value::as_u64) != Some(upload.total_bytes as u64)
        || response.get("done").and_then(serde_json::Value::as_bool) != Some(next_offset == upload.total_bytes)
    {
        return Err("Host returned an invalid Graph apply chunk acknowledgement".to_owned());
    }
    Ok(())
}

fn validate_graph_apply_status_response(
    response: &serde_json::Value,
    request_id: Uuid,
    upload_id: Uuid,
) -> Result<GraphApplyStatusDto, String> {
    let request_id_text = request_id.to_string();
    let upload_id_text = upload_id.to_string();
    if response.get("type").and_then(serde_json::Value::as_str) != Some("graph.apply.status")
        || response.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id_text.as_str())
        || response.get("protocol").and_then(serde_json::Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
        || response.get("upload_id").and_then(serde_json::Value::as_str) != Some(upload_id_text.as_str())
    {
        return Err("Host returned a mismatched Graph apply status".to_owned());
    }
    let status: GraphApplyStatusDto =
        serde_json::from_value(response.clone()).map_err(|_| "Host returned invalid Graph apply status".to_owned())?;
    if !matches!(
        status.state.as_str(),
        "uploading" | "pending" | "applied" | "conflict" | "rejected" | "unknown"
    ) || (status.state == "applied" && status.graph_revision.is_none())
        || (status.state == "conflict" && status.current_revision.is_none())
        || (status.state == "rejected" && (status.code.is_none() || status.message.is_none()))
        || (status.state == "unknown" && (status.code.is_none() || status.message.is_none()))
    {
        return Err("Host Graph apply status is incomplete".to_owned());
    }
    Ok(status)
}

fn validate_graph_transfer_start(
    response: &serde_json::Value,
    request_id: Uuid,
) -> Result<GraphDocumentTransferDto, String> {
    let request_id_text = request_id.to_string();
    if response.get("type").and_then(serde_json::Value::as_str) != Some("graph.document")
        || response.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id_text.as_str())
        || response.get("protocol").and_then(serde_json::Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
    {
        return Err("Host returned mismatched Graph transfer metadata".to_owned());
    }
    let transfer: GraphDocumentTransferDto = serde_json::from_value(response.clone())
        .map_err(|_| "Host returned invalid Graph transfer metadata".to_owned())?;
    uuid::Uuid::parse_str(&transfer.transfer_id)
        .map_err(|_| "Host returned an invalid Graph transfer ID".to_owned())?;
    if transfer.total_bytes == 0
        || transfer.total_bytes > GRAPH_DOCUMENT_MAX_BYTES
        || transfer.chunk_bytes != GRAPH_DOCUMENT_CHUNK_BYTES
        || transfer.sha256.len() != 64
        || !transfer.sha256.chars().all(|character| character.is_ascii_hexdigit())
        || !matches!(transfer.editor.mode.as_str(), "classic" | "graph")
    {
        return Err("Host Graph transfer metadata is outside the supported bounds".to_owned());
    }
    Ok(transfer)
}

fn validate_graph_chunk_response(
    response: &serde_json::Value,
    request_id: Uuid,
    transfer: &GraphDocumentTransferDto,
    offset: usize,
) -> Result<Vec<u8>, String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    if offset >= transfer.total_bytes {
        return Err("Graph chunk offset is outside the document".to_owned());
    }
    let expected_len = (transfer.total_bytes - offset).min(transfer.chunk_bytes);
    let request_id_text = request_id.to_string();
    if response.get("type").and_then(serde_json::Value::as_str) != Some("graph.document.chunk")
        || response.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id_text.as_str())
        || response.get("protocol").and_then(serde_json::Value::as_u64) != Some(u64::from(PROTOCOL_VERSION))
        || response.get("transfer_id").and_then(serde_json::Value::as_str) != Some(transfer.transfer_id.as_str())
        || response.get("graph_revision").and_then(serde_json::Value::as_u64) != Some(transfer.graph_revision)
        || response.get("offset").and_then(serde_json::Value::as_u64) != Some(offset as u64)
        || response.get("total_bytes").and_then(serde_json::Value::as_u64) != Some(transfer.total_bytes as u64)
        || response.get("sha256").and_then(serde_json::Value::as_str) != Some(transfer.sha256.as_str())
    {
        return Err("Host returned a mismatched Graph chunk".to_owned());
    }
    let next_offset = offset
        .checked_add(expected_len)
        .ok_or_else(|| "Graph chunk offset overflowed".to_owned())?;
    if response.get("next_offset").and_then(serde_json::Value::as_u64) != Some(next_offset as u64)
        || response.get("done").and_then(serde_json::Value::as_bool) != Some(next_offset == transfer.total_bytes)
    {
        return Err("Host returned a Graph chunk with an invalid length".to_owned());
    }
    let data = response
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Host returned a Graph chunk without data".to_owned())?;
    let decoded = URL_SAFE_NO_PAD
        .decode(data)
        .map_err(|_| "Host returned invalid Graph chunk encoding".to_owned())?;
    if decoded.len() != expected_len {
        return Err("Host returned a Graph chunk with an invalid byte length".to_owned());
    }
    Ok(decoded)
}

fn validate_graph_document_payload(
    transfer: &GraphDocumentTransferDto,
    bytes: &[u8],
) -> Result<GraphDocumentDto, String> {
    if bytes.len() != transfer.total_bytes || graph_sha256_hex(bytes) != transfer.sha256.to_ascii_lowercase() {
        return Err("Host Graph document checksum or length did not match".to_owned());
    }
    let graph: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "Host returned invalid Graph document JSON".to_owned())?;
    if !graph.is_object() {
        return Err("Host returned an invalid Graph document root".to_owned());
    }
    graph_workspace::validate_graph_node_id_precision_value(&graph)?;
    Ok(GraphDocumentDto {
        graph_revision: transfer.graph_revision,
        graph,
        valid: transfer.valid,
        issues: transfer.issues.clone(),
        editor: transfer.editor.clone(),
    })
}

fn graph_sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = sha2::Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[tauri::command]
pub fn get_terminal_diagnostics(state: State<'_, AppState>) -> Result<TerminalDiagnosticsDto, String> {
    const MAX_LINES: usize = 300;
    let records = state
        .diagnostic_store
        .tail_records(MAX_LINES)
        .map_err(|error| format!("Could not read retained Terminal diagnostics: {error}"))?;
    let lines = records
        .into_iter()
        .map(|record| {
            format!(
                "[{}] {} [{}] {}",
                record.level, record.timestamp_local, record.target, record.message
            )
        })
        .collect();
    let detailed_ice_enabled = std::env::var("SHOCKINGVRC_ICE_DIAGNOSTICS")
        .is_ok_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on"));
    Ok(TerminalDiagnosticsDto {
        lines,
        detailed_ice_enabled,
        runtime: state.updates.diagnostics.snapshot(),
        heartbeat_incidents: state.updates.diagnostics.heartbeat_incidents().snapshot(),
    })
}

#[tauri::command]
pub fn record_telemetry_render_metrics(
    state: State<'_, AppState>,
    samples: u64,
    total_ms: u64,
    max_ms: u64,
) -> Result<(), String> {
    if samples > 5_000 || total_ms > 60_000_u64.saturating_mul(samples) || max_ms > 60_000 {
        return Err("Telemetry render metrics exceed the bounded diagnostic limits".to_owned());
    }
    state
        .updates
        .diagnostics
        .record_frontend_render_batch(samples, total_ms, max_ms);
    Ok(())
}

#[tauri::command]
pub fn clear_terminal_diagnostics(state: State<'_, AppState>) -> Result<(), String> {
    state.updates.diagnostics.clear_events_and_counters();
    Ok(())
}

#[tauri::command]
pub async fn submit_operation(
    state: State<'_, AppState>,
    operation: AllowedOperation,
) -> Result<CommandAckDto, String> {
    let request_id = Uuid::new_v4();
    let frame = operation.into_wire_frame(request_id)?;
    let (response, receiver) = oneshot::channel();
    send_action(
        &state,
        SessionAction::Command {
            request_id,
            frame,
            response,
        },
    )
    .await?;
    match timeout(Duration::from_secs(10), receiver).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(
            "Host session ended before command acknowledgement; outcome is unknown. The command was not replayed."
                .to_owned(),
        ),
        Err(_) => Err(
            "Timed out waiting for Host command acknowledgement; outcome is unknown. The command was not replayed."
                .to_owned(),
        ),
    }
}

async fn send_action(state: &AppState, action: SessionAction) -> Result<(), String> {
    let actions = current_action_sender(state).await?;
    actions
        .send(action)
        .await
        .map_err(|_| "The Host session is no longer available".to_owned())
}

async fn current_action_sender(state: &AppState) -> Result<mpsc::Sender<SessionAction>, String> {
    let current = state.connection.lock().await;
    current
        .as_ref()
        .map(|connection| connection.actions.clone())
        .ok_or_else(|| "Connect to a Host first".to_owned())
}

async fn current_graph_api_session(state: &AppState) -> Result<GraphApiSession, String> {
    let current = state.connection.lock().await;
    let connection = current.as_ref().ok_or_else(|| "Connect to a Host first".to_owned())?;
    Ok(GraphApiSession {
        connection_id: connection.id,
        actions: connection.actions.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disconnect_completes_while_action_send_waits_for_capacity() {
        struct TemporaryProfileDir(std::path::PathBuf);

        impl Drop for TemporaryProfileDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let profile_dir =
            TemporaryProfileDir(std::env::temp_dir().join(format!("shockingvrc-queue-test-{}", Uuid::new_v4())));
        let diagnostic_store =
            shocking_vrc_diagnostics::PersistentLogStore::open(profile_dir.0.join("diagnostics").join("logs"))
                .expect("open isolated Terminal diagnostic store");
        let state = AppState::open(profile_dir.0.clone(), diagnostic_store).expect("open isolated Terminal test state");
        let (actions, _receiver) = mpsc::channel(32);
        let capacity = actions.max_capacity();
        assert_eq!(capacity, 32);
        for _ in 0..capacity {
            assert!(actions.try_send(SessionAction::FetchState).is_ok());
        }

        let cancel = CancellationToken::new();
        *state.connection.lock().await = Some(ConnectionHandle {
            id: Uuid::new_v4(),
            profile_id: "host-under-test".to_owned(),
            actions: actions.clone(),
            cancel: cancel.clone(),
            closed: None,
        });

        let send = send_action(&state, SessionAction::FetchCapabilities);
        tokio::pin!(send);
        assert!(futures_util::poll!(&mut send).is_pending());

        let mut current = state
            .connection
            .try_lock()
            .expect("a full action queue must not hold the connection mutex needed by disconnect");
        let handle = current.take().expect("active test connection");
        drop(current);

        handle.cancel.cancel();
        assert!(cancel.is_cancelled());
        assert!(
            handle.actions.try_send(SessionAction::Disconnect).is_err(),
            "disconnect cancellation must still work when the bounded action queue is full"
        );

        drop(_receiver);
        assert!(send.await.is_err());
    }

    #[tokio::test]
    async fn graph_confirmation_does_not_switch_to_a_new_host_session_mid_operation() {
        struct TemporaryProfileDir(std::path::PathBuf);

        impl Drop for TemporaryProfileDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let profile_dir =
            TemporaryProfileDir(std::env::temp_dir().join(format!("shockingvrc-session-pin-{}", Uuid::new_v4())));
        let diagnostic_store =
            shocking_vrc_diagnostics::PersistentLogStore::open(profile_dir.0.join("diagnostics").join("logs"))
                .expect("open isolated Terminal diagnostic store");
        let state = AppState::open(profile_dir.0.clone(), diagnostic_store).expect("open isolated Terminal test state");
        let old_connection_id = Uuid::new_v4();
        let (old_actions, mut old_receiver) = mpsc::channel(4);
        let (new_actions, _new_receiver) = mpsc::channel(4);
        *state.connection.lock().await = Some(ConnectionHandle {
            id: Uuid::new_v4(),
            profile_id: "new-host".to_owned(),
            actions: new_actions,
            cancel: CancellationToken::new(),
            closed: None,
        });
        let original_session = GraphApiSession {
            connection_id: old_connection_id,
            actions: old_actions,
        };

        let result = graph_api_request_for_session(
            &state,
            &original_session,
            "graph.apply.status",
            "graph.apply.status",
            Uuid::new_v4(),
            serde_json::json!({ "upload_id": Uuid::new_v4() }),
        )
        .await;

        assert!(result.unwrap_err().contains("session changed"));
        assert!(
            old_receiver.try_recv().is_err(),
            "no confirmation request may cross into the new session"
        );
    }

    #[test]
    fn re_pair_preserves_a_custom_local_connection_alias() {
        let existing = ProfileDto {
            id: "stable-profile-id".to_owned(),
            label: "Nightmare — Main".to_owned(),
            label_is_custom: true,
            host_display_name: None,
            host: Some("localhost".to_owned()),
            port: Some(4318),
            fingerprint: "00".repeat(32),
            server_id: "12345678-host-id".to_owned(),
            connection_mode: "direct_only".to_owned(),
            rendezvous_url: String::new(),
            ice_server_urls: Vec::new(),
            force_relay: false,
        };

        assert_eq!(pairing_profile_label(&existing.server_id, Some(&existing)), "Nightmare — Main");
        assert_eq!(pairing_profile_label(&existing.server_id, None), "Host 12345678");
    }

    #[test]
    fn re_pair_keeps_the_saved_identity_pin_and_rejects_an_invitation_with_a_new_pin() {
        let existing = ProfileDto {
            id: "stable-profile-id".to_owned(),
            label: "Saved Host".to_owned(),
            label_is_custom: true,
            host_display_name: None,
            host: None,
            port: None,
            fingerprint: "ab".repeat(32),
            server_id: "host-under-test".to_owned(),
            connection_mode: "traversal_only".to_owned(),
            rendezvous_url: String::new(),
            ice_server_urls: Vec::new(),
            force_relay: false,
        };

        assert_eq!(
            pairing_fingerprint(Some(&existing), &"AB".repeat(32)).unwrap(),
            existing.fingerprint
        );
        assert_eq!(
            pairing_fingerprint(Some(&existing), &"cd".repeat(32)).unwrap_err(),
            "The invitation Host fingerprint does not match the saved identity pin",
        );
        assert_eq!(pairing_fingerprint(None, &"cd".repeat(32)).unwrap(), "cd".repeat(32));
    }

    #[test]
    fn deleted_saved_profile_can_be_replaced_from_a_fresh_invitation() {
        let existing = ProfileDto {
            id: "host-profile".to_owned(),
            label: "Saved Host".to_owned(),
            label_is_custom: false,
            host_display_name: None,
            host: None,
            port: None,
            fingerprint: "ab".repeat(32),
            server_id: "host-under-test".to_owned(),
            connection_mode: "traversal_only".to_owned(),
            rendezvous_url: String::new(),
            ice_server_urls: Vec::new(),
            force_relay: false,
        };
        let deletion = plan_profile_forget("host-profile", std::slice::from_ref(&existing), None)
            .expect("saved profile can be deleted");
        assert!(deletion.delete_saved_profile);
        assert!(!deletion.clear_pending_pairing);

        // Once the saved profile is gone, invitation identity establishes the new local pin.
        let fresh_invitation_pin = "cd".repeat(32);
        assert_eq!(pairing_fingerprint(None, &fresh_invitation_pin).unwrap(), fresh_invitation_pin);
    }

    #[test]
    fn pending_pairing_profile_is_forgettable_before_it_is_saved() {
        let pending = ProfileDto {
            id: "pending-host-id".to_owned(),
            label: "Pending Host".to_owned(),
            label_is_custom: false,
            host_display_name: None,
            host: None,
            port: None,
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
            label_is_custom: false,
            host_display_name: None,
            host: None,
            port: None,
            fingerprint: "11".repeat(32),
            server_id: "host-under-test".to_owned(),
            connection_mode: "traversal_only".to_owned(),
            rendezvous_url: "ws://127.0.0.1:4319/ws".to_owned(),
            ice_server_urls: Vec::new(),
            force_relay: false,
        };
        let pending = ProfileDto {
            label: "New invitation".to_owned(),
            ..saved.clone()
        };

        let plan = plan_profile_forget("saved-host-id", &[saved], Some(&pending)).unwrap();

        assert_eq!(plan.profile.id, "saved-host-id");
        assert!(plan.delete_saved_profile);
        assert!(plan.clear_pending_pairing);
    }

    #[test]
    fn disconnect_host_name_cache_normalizes_empty_and_rejects_oversized_values() {
        assert_eq!(
            normalize_host_display_name(Some("  Host name  ".to_owned())).unwrap(),
            Some("Host name".to_owned())
        );
        assert_eq!(normalize_host_display_name(Some("  ".to_owned())).unwrap(), None);
        assert!(normalize_host_display_name(Some("x".repeat(81))).is_err());
    }

    #[test]
    fn diagnostic_log_ingestion_filters_credentials_and_preserves_safe_lifecycle_text() {
        let safe = shocking_vrc_rtc::sanitize_diagnostic_log_line(
            "[INFO] authenticated session started transport=webrtc_datachannel",
        );
        assert!(safe.unwrap().contains("authenticated session"));
        for unsafe_line in [
            "invite_token=do-not-export",
            "Authorization: Bearer do-not-export",
            "signing_key=do-not-export",
            "TURN credentials: username password",
            "v=0\\r\\na=sdp",
        ] {
            assert_eq!(shocking_vrc_rtc::sanitize_diagnostic_log_line(unsafe_line), None);
        }
    }

    #[test]
    fn graph_transfer_verifies_request_ids_chunk_lengths_and_final_checksum() {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        use serde_json::json;

        let request_id = Uuid::new_v4();
        let transfer_id = Uuid::new_v4();
        let bytes = br#"{"nodes":[]}"#;
        let sha256 = "acf2fa576acb702442f9d0101673354c398db67315c066ca48be8db8e0d2c75b";
        let start = validate_graph_transfer_start(
            &json!({
                "type": "graph.document",
                "protocol": 1,
                "request_id": request_id,
                "transfer_id": transfer_id,
                "graph_revision": 7,
                "total_bytes": bytes.len(),
                "chunk_bytes": GRAPH_DOCUMENT_CHUNK_BYTES,
                "sha256": sha256,
                "valid": true,
                "issues": [],
                "editor": {
                    "mode": "graph", "panic": false, "ui_panic": false,
                    "caps": [50, 50], "tick": 11
                }
            }),
            request_id,
        )
        .expect("valid transfer metadata");
        let chunk = json!({
            "type": "graph.document.chunk",
            "protocol": 1,
            "request_id": request_id,
            "transfer_id": transfer_id,
            "graph_revision": 7,
            "offset": 0,
            "next_offset": bytes.len(),
            "total_bytes": bytes.len(),
            "done": true,
            "sha256": sha256,
            "data": URL_SAFE_NO_PAD.encode(bytes),
        });
        let decoded = validate_graph_chunk_response(&chunk, request_id, &start, 0).expect("valid full chunk");
        let document = validate_graph_document_payload(&start, &decoded).expect("valid final graph hash");
        assert_eq!(document.graph_revision, 7);
        assert_eq!(document.graph, json!({ "nodes": [] }));
        assert_eq!(document.editor.mode, "graph");

        let wrong_request = validate_graph_chunk_response(&chunk, Uuid::new_v4(), &start, 0);
        assert!(wrong_request.is_err());
        let wrong_length = {
            let mut corrupt = chunk.clone();
            corrupt["next_offset"] = json!(0);
            validate_graph_chunk_response(&corrupt, request_id, &start, 0)
        };
        assert!(wrong_length.is_err());
        let corrupt_document = br#"{"nodes":[1]}"#;
        assert!(validate_graph_document_payload(&start, corrupt_document).is_err());

        let unsafe_document =
            br#"{"version":1,"nodes":[{"id":9007199254740992,"kind":{"type":"Constant","value":0.5}}],"edges":[]}"#;
        let mut unsafe_start = start;
        unsafe_start.total_bytes = unsafe_document.len();
        unsafe_start.sha256 = graph_sha256_hex(unsafe_document);
        assert!(
            validate_graph_document_payload(&unsafe_start, unsafe_document)
                .unwrap_err()
                .contains("JavaScript")
        );
    }

    #[test]
    fn graph_preset_transfer_checks_identity_bounds_and_final_checksum() {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        use serde_json::json;

        let request_id = Uuid::new_v4();
        let transfer_id = Uuid::new_v4();
        let preset_id = "builtin:sample";
        let bytes = br#"{"version":1,"name":"Sample"}"#;
        let checksum = graph_sha256_hex(bytes);
        let transfer = validate_graph_preset_transfer_start(
            &json!({
                "type":"graph.preset", "protocol":1, "request_id":request_id,
                "transfer_id":transfer_id, "preset_id":preset_id,
                "total_bytes":bytes.len(), "chunk_bytes":GRAPH_DOCUMENT_CHUNK_BYTES, "sha256":checksum,
            }),
            request_id,
            preset_id,
        )
        .expect("valid preset transfer metadata");
        let chunk = validate_graph_preset_chunk_response(
            &json!({
                "type":"graph.preset.chunk", "protocol":1, "request_id":request_id,
                "transfer_id":transfer_id, "preset_id":preset_id, "offset":0,
                "next_offset":bytes.len(), "total_bytes":bytes.len(), "done":true,
                "sha256":checksum, "data":URL_SAFE_NO_PAD.encode(bytes),
            }),
            request_id,
            &transfer,
            0,
        )
        .expect("valid preset bytes");
        assert_eq!(chunk, bytes);

        let mut wrong_identity = json!({
            "type":"graph.preset", "protocol":1, "request_id":request_id,
            "transfer_id":transfer_id, "preset_id":"other",
            "total_bytes":bytes.len(), "chunk_bytes":GRAPH_DOCUMENT_CHUNK_BYTES, "sha256":checksum,
        });
        assert!(validate_graph_preset_transfer_start(&wrong_identity, request_id, preset_id).is_err());
        wrong_identity["total_bytes"] = json!(GRAPH_DOCUMENT_MAX_BYTES + 1);
        assert!(validate_graph_preset_transfer_start(&wrong_identity, request_id, preset_id).is_err());
    }

    #[test]
    fn graph_apply_receipts_validate_ids_and_distinguish_pending_from_confirmed() {
        use serde_json::json;

        let request_id = Uuid::new_v4();
        let upload_id = Uuid::new_v4();
        let upload = validate_graph_upload_start(
            &json!({
                "type":"graph.apply.upload", "protocol":1, "request_id":request_id,
                "upload_id":upload_id, "base_revision":8, "total_bytes":73,
                "chunk_bytes":GRAPH_DOCUMENT_CHUNK_BYTES, "next_offset":0,
            }),
            request_id,
            8,
            73,
        )
        .expect("valid upload response");
        assert!(
            validate_graph_apply_chunk_response(
                &json!({
                    "type":"graph.apply.chunk", "protocol":1, "request_id":request_id,
                    "upload_id":upload_id, "next_offset":73, "total_bytes":73, "done":true,
                }),
                request_id,
                &upload,
                0,
                73,
            )
            .is_ok()
        );

        let pending = validate_graph_apply_status_response(
            &json!({
                "type":"graph.apply.status", "protocol":1, "request_id":request_id,
                "upload_id":upload_id, "state":"pending",
            }),
            request_id,
            upload_id,
        )
        .unwrap();
        assert_eq!(pending.state, "pending");
        let applied = validate_graph_apply_status_response(
            &json!({
                "type":"graph.apply.status", "protocol":1, "request_id":request_id,
                "upload_id":upload_id, "state":"applied", "graph_revision":9,
            }),
            request_id,
            upload_id,
        )
        .unwrap();
        assert_eq!(applied.graph_revision, Some(9));
        assert_eq!(applied.graph_sha256, None, "older Hosts omit the optional committed identity");
        let identity = "ab".repeat(32);
        let bound = validate_graph_apply_status_response(
            &json!({
                "type":"graph.apply.status", "protocol":1, "request_id":request_id,
                "upload_id":upload_id, "state":"applied", "graph_revision":9,
                "graph_sha256":identity,
            }),
            request_id,
            upload_id,
        )
        .expect("new Host identity receipt");
        assert_eq!(bound.graph_sha256.as_deref(), Some(identity.as_str()));
        let unresolved = validate_graph_apply_status_response(
            &json!({
                "type":"graph.apply.status", "protocol":1, "request_id":request_id,
                "upload_id":upload_id, "state":"unknown", "code":"STATUS_UNAVAILABLE",
                "message":"The Host could not recover its worker result",
            }),
            request_id,
            upload_id,
        )
        .expect("Host must distinguish unconfirmed execution from rejection");
        assert_eq!(unresolved.state, "unknown");
        assert_eq!(unresolved.code.as_deref(), Some("STATUS_UNAVAILABLE"));
        assert!(
            validate_graph_apply_status_response(
                &json!({
                    "type":"graph.apply.status", "protocol":1, "request_id":request_id,
                    "upload_id":upload_id, "state":"unknown", "code":"STATUS_UNAVAILABLE",
                }),
                request_id,
                upload_id,
            )
            .is_err()
        );
        assert!(
            validate_graph_apply_status_response(
                &json!({
                    "type":"graph.apply.status", "protocol":1, "request_id":request_id,
                    "upload_id":Uuid::new_v4(), "state":"applied", "graph_revision":9,
                }),
                request_id,
                upload_id,
            )
            .is_err()
        );
    }

    #[test]
    fn apply_readback_distinguishes_later_host_device_discovery_from_apply_failure() {
        use serde_json::json;

        let committed_graph = json!({"version":1,"nodes":[],"edges":[]});
        let mut host_graph = committed_graph.clone();
        host_graph["nodes"] = json!([{
            "id": 21,
            "kind": {"type":"Device", "mac":"AA:BB"},
            "pos": [40.0, 60.0]
        }]);
        let document = test_graph_document(14, host_graph);
        let receipt_identity = graph_identity_for_test(&committed_graph);

        let observation = classify_apply_readback(13, Some(&receipt_identity), &document);

        assert_eq!(observation, ApplyReadback::HostChanged { current_revision: 14 });
    }

    #[test]
    fn apply_readback_accepts_host_normalization_when_receipt_matches_committed_document() {
        use serde_json::json;

        let submitted_graph = json!({"version":1,"nodes":[],"edges":[]});
        let committed_graph = json!({
            "version":1,
            "nodes":[{"id":21,"kind":{"type":"Device","mac":"AA:BB"},"pos":[40.0,60.0]}],
            "edges":[]
        });
        assert_ne!(
            submitted_graph, committed_graph,
            "Host normalization changes the authoritative Graph"
        );
        let document = test_graph_document(13, committed_graph.clone());
        let receipt_identity = graph_identity_for_test(&committed_graph);

        let observation = classify_apply_readback(13, Some(&receipt_identity), &document);

        assert_eq!(observation, ApplyReadback::Applied);
    }

    #[test]
    fn apply_readback_waits_when_host_document_publication_lags_the_receipt() {
        use serde_json::json;

        let document = test_graph_document(12, json!({"version":1,"nodes":[],"edges":[]}));

        let observation = classify_apply_readback(13, None, &document);

        assert_eq!(observation, ApplyReadback::AwaitingPublication);
    }

    #[test]
    fn apply_readback_rejects_a_same_revision_document_that_breaks_the_receipt_identity() {
        use serde_json::json;

        let committed_graph = json!({"version":1,"nodes":[],"edges":[]});
        let other_graph = json!({"version":1,"nodes":[{"id":22}],"edges":[]});
        let document = test_graph_document(13, other_graph);
        let receipt_identity = graph_identity_for_test(&committed_graph);

        let observation = classify_apply_readback(13, Some(&receipt_identity), &document);

        assert_eq!(observation, ApplyReadback::InconsistentDocument);
    }

    #[test]
    fn legacy_apply_receipt_without_graph_identity_remains_revision_bound() {
        use serde_json::json;

        let document = test_graph_document(13, json!({"version":1,"nodes":[],"edges":[]}));

        let observation = classify_apply_readback(13, None, &document);

        assert_eq!(observation, ApplyReadback::Applied);
    }

    #[tokio::test(start_paused = true)]
    async fn confirmed_apply_waits_for_a_delayed_graph_document_publication() {
        use serde_json::json;
        use std::collections::VecDeque;

        let committed = json!({"version":1,"nodes":[],"edges":[]});
        let mut documents = VecDeque::from([
            test_graph_document(12, committed.clone()),
            test_graph_document(13, committed.clone()),
        ]);
        let operation_id = Uuid::new_v4().to_string();
        let receipt_identity = graph_identity_for_test(&committed);

        let result = confirm_graph_apply_readback(
            || {
                let document = documents.pop_front().expect("two read-only document fetches");
                async move { Ok(document) }
            },
            12,
            &operation_id,
            13,
            Some(&receipt_identity),
            Instant::now() + Duration::from_secs(2),
        )
        .await;

        assert_eq!(result.state, "applied");
        assert_eq!(result.graph_revision, Some(13));
        assert_eq!(result.current_revision, Some(13));
        assert_eq!(result.operation_id.as_deref(), Some(operation_id.as_str()));
    }

    #[tokio::test]
    async fn confirmed_apply_followed_by_device_discovery_reports_the_later_host_revision() {
        use serde_json::json;

        let committed = json!({"version":1,"nodes":[],"edges":[]});
        let after_device_discovery = json!({
            "version":1,
            "nodes":[{"id":21,"kind":{"type":"Device","mac":"AA:BB"},"pos":[40.0,60.0]}],
            "edges":[]
        });
        let document = test_graph_document(14, after_device_discovery);
        let operation_id = Uuid::new_v4().to_string();
        let receipt_identity = graph_identity_for_test(&committed);

        let result = confirm_graph_apply_readback(
            || {
                let document = document.clone();
                async move { Ok(document) }
            },
            12,
            &operation_id,
            13,
            Some(&receipt_identity),
            Instant::now() + Duration::from_secs(2),
        )
        .await;

        assert_eq!(result.state, "host_changed");
        assert_eq!(result.graph_revision, Some(13));
        assert_eq!(result.current_revision, Some(14));
    }

    #[tokio::test]
    async fn confirmed_apply_followed_by_a_host_local_edit_reports_host_changed() {
        use serde_json::json;

        let committed = json!({"version":1,"nodes":[],"edges":[]});
        let host_edit = json!({
            "version":1,
            "nodes":[{"id":22,"kind":{"type":"Time"},"pos":[80.0,100.0]}],
            "edges":[]
        });
        let document = test_graph_document(14, host_edit);
        let receipt_identity = graph_identity_for_test(&committed);

        let result = confirm_graph_apply_readback(
            || {
                let document = document.clone();
                async move { Ok(document) }
            },
            12,
            "host-local-edit-fixture",
            13,
            Some(&receipt_identity),
            Instant::now() + Duration::from_secs(2),
        )
        .await;

        assert_eq!(result.state, "host_changed");
        assert_eq!(result.current_revision, Some(14));
    }

    #[tokio::test]
    async fn confirmed_apply_with_document_transfer_failure_keeps_apply_receipt_and_operation_id() {
        let result = confirm_graph_apply_readback(
            || async { Err("synthetic session interruption".to_owned()) },
            12,
            "apply-operation-fixture",
            13,
            None,
            Instant::now() + Duration::from_secs(2),
        )
        .await;

        assert_eq!(result.state, "applied_unreconciled");
        assert_eq!(result.graph_revision, Some(13));
        assert_eq!(result.operation_id.as_deref(), Some("apply-operation-fixture"));
        assert!(result.message.unwrap().contains("could not be refreshed"));
    }

    fn test_graph_document(graph_revision: u64, graph: serde_json::Value) -> GraphDocumentDto {
        GraphDocumentDto {
            graph_revision,
            graph,
            valid: true,
            issues: Vec::new(),
            editor: crate::protocol::GraphEditorMetadataDto {
                mode: "graph".to_owned(),
                panic: false,
                ui_panic: false,
                caps: [40, 40],
                tick: 0,
            },
        }
    }

    fn graph_identity_for_test(graph: &serde_json::Value) -> String {
        graph_sha256_hex(&serde_json::to_vec(graph).expect("canonical Graph JSON"))
    }

    #[tokio::test]
    async fn graph_commit_timeout_is_reported_unknown_and_is_never_replayed() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let calls = Arc::new(AtomicUsize::new(0));
        let call_counter = Arc::clone(&calls);
        let result = graph_commit_once(
            move || async move {
                call_counter.fetch_add(1, Ordering::SeqCst);
                Err("simulated lost response".to_owned())
            },
            12,
        )
        .await;

        let unknown = result.expect_err("lost commit response has an unknown outcome");
        assert_eq!(unknown.state, "unknown");
        assert_eq!(unknown.base_revision, 12);
        assert!(unknown.message.unwrap().contains("not replayed"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn legacy_capabilities_parse_without_graph_features() {
        use serde_json::json;

        let capabilities: crate::protocol::CapabilitiesDto = serde_json::from_value(json!({
            "operations": [], "features": ["session.resume"]
        }))
        .expect("older Host capability lists remain valid");
        assert!(
            !capabilities
                .features
                .iter()
                .any(|feature| feature.starts_with("graph."))
        );
        assert!(
            !capabilities
                .operations
                .iter()
                .any(|operation| operation == "engine.set_mode")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn engine_mode_ack_timeout_reports_unknown_and_does_not_replay() {
        let (actions, mut receiver) = mpsc::channel(4);
        let action_sender = actions.clone();
        let request = tokio::spawn(async move { submit_engine_mode(&action_sender, "graph").await });
        let action = receiver.recv().await.expect("mode command is queued once");

        if let SessionAction::Command { frame, .. } = action {
            assert_eq!(frame["op"], "engine.set_mode");
            assert_eq!(frame["args"]["mode"], "graph");
        } else {
            panic!("expected mode command");
        }

        tokio::time::advance(Duration::from_secs(10)).await;
        let error = request
            .await
            .expect("command task completes")
            .expect_err("ack times out");
        assert!(error.contains("outcome is unknown"));
        assert!(error.contains("not replayed"));
        assert!(receiver.try_recv().is_err(), "timeout must not queue another mode command");
    }
}

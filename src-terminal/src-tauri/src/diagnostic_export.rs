use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU8, Ordering},
    },
    process::Command,
    time::Duration,
};

use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use serde::{
    Deserialize, Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shocking_vrc_diagnostics::{
    DiagnosticLogRecord, LogExportChunk, LogSnapshot, LogSnapshotMetadata, LogTimeRange, MAX_LOG_CHUNK_BYTES,
};
use shocking_vrc_rtc::sanitize_diagnostic_log_line;
use tauri::State;
use tokio::{sync::oneshot, time::timeout};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    app_state::{AppState, DiagnosticExportControl},
    protocol::ConnectionStateDto,
    transport::SessionAction,
};

const HOST_RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticExportRequest {
    pub source: String,
    pub time_range: String,
    pub terminal_timezone: String,
    #[serde(default)]
    pub start_local: Option<String>,
    #[serde(default)]
    pub end_local: Option<String>,
    #[serde(default)]
    pub start_utc: Option<String>,
    #[serde(default)]
    pub end_utc: Option<String>,
    pub open_folder_after_export: bool,
}

#[derive(Clone, Debug, Serialize)]
struct RequestedTimeRange {
    kind: String,
    start_utc: Option<String>,
    end_utc: Option<String>,
    terminal_timezone: String,
    start_local: Option<String>,
    end_local: Option<String>,
    boundary: &'static str,
}

#[derive(Clone, Debug)]
struct ResolvedTimeRange {
    range: Option<LogTimeRange>,
    metadata: RequestedTimeRange,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticExportResult {
    pub output_path: String,
    pub explorer_opened: bool,
    pub export_file_selected: bool,
    pub explorer_error: Option<String>,
}

#[derive(Serialize)]
pub struct DiagnosticExportStatus {
    pub running: bool,
    pub stage: String,
    pub progress_percent: u8,
}

#[tauri::command]
pub async fn diagnostic_export_progress(state: State<'_, AppState>) -> Result<DiagnosticExportStatus, String> {
    let control = state.diagnostic_export.lock().await.clone();
    Ok(match control {
        Some(control) => DiagnosticExportStatus {
            running: true,
            stage: control
                .stage
                .lock()
                .map(|value| value.clone())
                .unwrap_or_else(|_| "Preparing export".to_owned()),
            progress_percent: control.progress.load(Ordering::Relaxed).min(100),
        },
        None => DiagnosticExportStatus {
            running: false,
            stage: String::new(),
            progress_percent: 0,
        },
    })
}

#[tauri::command]
pub async fn cancel_diagnostic_export(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(control) = state.diagnostic_export.lock().await.clone() {
        control.cancel.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn export_diagnostics(
    state: State<'_, AppState>,
    request: DiagnosticExportRequest,
) -> Result<DiagnosticExportResult, String> {
    requested_sources(&request.source)?;
    let resolved_range = resolve_time_range(&request, Utc::now())?;
    let control = DiagnosticExportControl {
        id: Uuid::new_v4(),
        cancel: CancellationToken::new(),
        progress: Arc::new(AtomicU8::new(0)),
        stage: Arc::new(StdMutex::new("Preparing diagnostic snapshot".to_owned())),
    };
    {
        let mut active = state.diagnostic_export.lock().await;
        if active.is_some() {
            return Err("A diagnostic export is already running".to_owned());
        }
        *active = Some(control.clone());
    }
    let result = run_export(&state, &request, &resolved_range, &control).await;
    let mut active = state.diagnostic_export.lock().await;
    if active.as_ref().is_some_and(|current| current.id == control.id) {
        *active = None;
    }
    let output_path = result?;
    let mut launch = |target: &Path, select_file: bool| launch_explorer(target, select_file);
    Ok(finalize_export_result(output_path, request.open_folder_after_export, &mut launch))
}

async fn run_export(
    state: &AppState,
    request: &DiagnosticExportRequest,
    time_range: &ResolvedTimeRange,
    control: &DiagnosticExportControl,
) -> Result<String, String> {
    let mut host_snapshot_id = None;
    let result = run_export_inner(state, request, time_range, control, &mut host_snapshot_id).await;
    if let Some(snapshot_id) = host_snapshot_id {
        let _ = request_host(state, &CancellationToken::new(), "logs.export.cancel", Some(snapshot_id)).await;
    }
    result
}

async fn run_export_inner(
    state: &AppState,
    request: &DiagnosticExportRequest,
    time_range: &ResolvedTimeRange,
    control: &DiagnosticExportControl,
    host_snapshot_id: &mut Option<Uuid>,
) -> Result<String, String> {
    let data_dir = state
        .terminal_log_path
        .parent()
        .ok_or_else(|| "Terminal data folder is unavailable".to_owned())?;
    let directory = data_dir.join("diagnostics");
    fs::create_dir_all(&directory).map_err(|error| format!("Could not create local diagnostics folder: {error}"))?;
    let host_raw_path = directory.join(format!(".host-{}.jsonl.part", control.id));
    let terminal_raw_path = directory.join(format!(".terminal-{}.jsonl.part", control.id));
    let mut temps = TempFiles::new(vec![host_raw_path.clone(), terminal_raw_path.clone()]);
    let started_at_utc = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let connection_at_start = state.updates.connection_snapshot().await;
    let mut runtime = state.updates.diagnostics.snapshot();
    filter_runtime_snapshot(&mut runtime, time_range.range.as_ref());
    let incident_store = state.updates.diagnostics.heartbeat_incidents();
    let (mut heartbeat_incidents, heartbeat_incident_persistence_errors) = tokio::task::spawn_blocking(move || {
        let flush_result = incident_store.flush();
        (
            incident_store.snapshot(),
            incident_store
                .persistence_errors()
                .saturating_add(if flush_result.is_err() { 1 } else { 0 }),
        )
    })
    .await
    .map_err(|error| format!("Heartbeat incident snapshot worker failed: {error}"))?;
    filter_heartbeat_incidents(&mut heartbeat_incidents, time_range.range.as_ref());
    let terminal_session = terminal_session_metadata(connection_at_start.as_ref(), runtime.current_room_id);
    let (want_terminal, want_host) = requested_sources(&request.source)?;

    let terminal_source = if want_terminal {
        set_stage(control, "Snapshotting Terminal logs", 3);
        let store = state.diagnostic_store.clone();
        let range = time_range.range.clone();
        let snapshot = tokio::task::spawn_blocking(move || store.snapshot_filtered(range.as_ref()))
            .await
            .map_err(|error| format!("Terminal log snapshot worker failed: {error}"))?
            .map_err(|error| format!("Could not snapshot Terminal logs: {error}"))?;
        if snapshot.metadata().total_bytes > MAX_SOURCE_BYTES {
            return Err("Terminal diagnostic snapshot exceeds the 256 MiB export limit".to_owned());
        }
        let snapshot_meta = snapshot.metadata().clone();
        let path = terminal_raw_path.clone();
        let cancel = control.cancel.clone();
        let progress = Arc::clone(&control.progress);
        let stage = Arc::clone(&control.stage);
        let (record_count, sha256) = tokio::task::spawn_blocking(move || {
            spool_snapshot(snapshot, &path, &cancel, |done, total| {
                progress.store(
                    if total == 0 {
                        18
                    } else {
                        3 + (15 * done / total).min(15) as u8
                    },
                    Ordering::Relaxed,
                );
                if let Ok(mut value) = stage.lock() {
                    *value = "Snapshotting Terminal logs".to_owned();
                }
            })
        })
        .await
        .map_err(|error| format!("Terminal log spool worker failed: {error}"))??;
        Some(StreamSource {
            application: ApplicationInfo {
                name: "ShockingVRC Terminal".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                git_commit: option_env!("SHOCKINGVRC_GIT_COMMIT").unwrap_or("unknown").to_owned(),
            },
            snapshot: snapshot_meta,
            session: terminal_session.clone(),
            record_count,
            sha256,
            records_path: terminal_raw_path.clone(),
        })
    } else {
        None
    };

    let host_source = if want_host {
        if connection_at_start
            .as_ref()
            .is_none_or(|connection| connection.phase != "online")
        {
            return Err("Connect to the Host before exporting Host diagnostics".to_owned());
        }
        set_stage(control, "Requesting authenticated Host snapshot", 20);
        let mut start_frame = json!({"type":"logs.export.start","protocol":1,"request_id":Uuid::new_v4()});
        if let Some(range) = &time_range.range {
            start_frame["start_utc"] = json!(range.start_utc);
            start_frame["end_utc"] = json!(range.end_utc);
        }
        let started = request_host_frame(state, &control.cancel, start_frame).await?;
        let meta: LogSnapshotMetadata = serde_json::from_value(
            started
                .get("snapshot")
                .cloned()
                .ok_or_else(|| "Host omitted diagnostic snapshot metadata".to_owned())?,
        )
        .map_err(|_| "Host returned invalid diagnostic snapshot metadata".to_owned())?;
        *host_snapshot_id = Some(meta.snapshot_id);
        if meta.total_bytes > MAX_SOURCE_BYTES {
            return Err("Host diagnostic snapshot exceeds the 256 MiB export limit".to_owned());
        }
        let app = started
            .get("application")
            .cloned()
            .filter(Value::is_object)
            .ok_or_else(|| "Host omitted diagnostic build provenance".to_owned())?;
        let host_session = started
            .get("session")
            .cloned()
            .filter(Value::is_object)
            .ok_or_else(|| "Host omitted diagnostic session metadata".to_owned())?;
        validate_host_session(connection_at_start.as_ref(), &host_session)?;
        let mut host_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&host_raw_path)
            .map_err(|error| format!("Could not create Host diagnostics temporary file: {error}"))?;
        let mut hasher = Sha256::new();
        let mut offset = 0_u64;
        let mut count = 0_u64;
        loop {
            ensure_not_cancelled(control)?;
            validate_connection_unchanged(state, connection_at_start.as_ref()).await?;
            let mut response = None;
            let mut last_error = "Host diagnostics chunk request failed".to_owned();
            for _attempt in 0..=2 {
                let chunk_frame = json!({
                    "type":"logs.export.chunk", "protocol":1, "request_id":Uuid::new_v4(),
                    "snapshot_id":meta.snapshot_id, "offset":offset, "max_bytes":MAX_LOG_CHUNK_BYTES
                });
                match request_host_frame(state, &control.cancel, chunk_frame).await {
                    Ok(value) => {
                        response = Some(value);
                        break;
                    }
                    Err(error) if error.starts_with("Timed out waiting for the Host diagnostics response") => {
                        last_error = error
                    }
                    Err(error) => return Err(error),
                }
            }
            let response = response.ok_or(last_error)?;
            let chunk: LogExportChunk = serde_json::from_value(response)
                .map_err(|_| "Host returned an invalid diagnostics chunk".to_owned())?;
            if chunk.snapshot_id != meta.snapshot_id
                || chunk.offset != offset
                || chunk.next_offset != offset.saturating_add(chunk.bytes.len() as u64)
                || chunk.bytes.len() > MAX_LOG_CHUNK_BYTES
            {
                return Err("Host diagnostics chunk did not match the expected snapshot offset".to_owned());
            }
            host_file
                .write_all(&chunk.bytes)
                .map_err(|error| format!("Could not spool Host diagnostics: {error}"))?;
            hasher.update(&chunk.bytes);
            count = count.saturating_add(chunk.bytes.iter().filter(|byte| **byte == b'\n').count() as u64);
            offset = chunk.next_offset;
            control.progress.store(
                if meta.total_bytes == 0 {
                    95
                } else {
                    20 + (70 * offset / meta.total_bytes).min(70) as u8
                },
                Ordering::Relaxed,
            );
            if chunk.eof {
                let local_hash = hex(&hasher.clone().finalize());
                if chunk.sha256.as_deref() != Some(local_hash.as_str()) || offset != meta.total_bytes {
                    return Err("Host diagnostics SHA-256 verification failed".to_owned());
                }
                break;
            }
            if offset >= meta.total_bytes {
                return Err("Host diagnostics ended without an EOF integrity response".to_owned());
            }
        }
        host_file
            .flush()
            .map_err(|error| format!("Could not flush Host diagnostics spool: {error}"))?;
        host_file
            .sync_all()
            .map_err(|error| format!("Could not sync Host diagnostics spool: {error}"))?;
        validate_jsonl(&host_raw_path, count)?;
        Some(StreamSource {
            application: application_from_value(&app),
            snapshot: meta,
            session: host_session,
            record_count: count,
            sha256: hex(&hasher.clone().finalize()),
            records_path: host_raw_path.clone(),
        })
    } else {
        None
    };

    ensure_not_cancelled(control)?;
    if want_host {
        validate_connection_unchanged(state, connection_at_start.as_ref()).await?;
    }
    set_stage(control, "Writing verified JSON package", 96);

    let package = DiagnosticPackage {
        schema_version: 1,
        exported_at_utc: started_at_utc,
        selected_sources: request.source.clone(),
        requested_time_range: time_range.metadata.clone(),
        complete: true,
        terminal_runtime: runtime,
        terminal_heartbeat_incidents: heartbeat_incidents,
        terminal_heartbeat_incident_persistence_errors: heartbeat_incident_persistence_errors,
        terminal_session,
        sources: Sources {
            terminal: terminal_source,
            host: host_source,
        },
    };
    let output_path = unique_output_path(&directory)?;
    let output_final_part = output_path.with_extension("json.part");
    temps.add(output_final_part.clone());
    let package = package;
    let part = output_final_part.clone();
    tokio::task::spawn_blocking(move || {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .map_err(|error| format!("Could not create export package: {error}"))?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, &package)
            .map_err(|error| format!("Could not serialize export package: {error}"))?;
        writer
            .flush()
            .map_err(|error| format!("Could not flush export package: {error}"))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("Could not sync export package: {error}"))?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|error| format!("Export writer failed: {error}"))??;
    ensure_not_cancelled(control)?;
    fs::rename(&output_final_part, &output_path)
        .map_err(|error| format!("Could not publish complete diagnostics package: {error}"))?;
    temps.disarm(&output_final_part);
    control.progress.store(100, Ordering::Relaxed);
    set_stage(control, "Export complete", 100);
    Ok(output_path.display().to_string())
}

async fn request_host(
    state: &AppState,
    cancel: &CancellationToken,
    request_type: &str,
    snapshot_id: Option<Uuid>,
) -> Result<Value, String> {
    let request_id = Uuid::new_v4();
    let mut frame = json!({"type":request_type,"protocol":1,"request_id":request_id});
    if let Some(snapshot_id) = snapshot_id {
        frame["snapshot_id"] = json!(snapshot_id);
    }
    request_host_frame(state, cancel, frame).await
}

async fn request_host_frame(state: &AppState, cancel: &CancellationToken, frame: Value) -> Result<Value, String> {
    ensure_not_cancelled_token(cancel)?;
    let actions = {
        let connection = state.connection.lock().await;
        connection
            .as_ref()
            .ok_or_else(|| "Host session is no longer available".to_owned())?
            .actions
            .clone()
    };
    let (response, receiver) = oneshot::channel();
    actions
        .send(SessionAction::LogExportRequest { frame, response })
        .await
        .map_err(|_| "Host session is no longer available".to_owned())?;
    tokio::select! {
        _ = cancel.cancelled() => Err("Diagnostic export cancelled".to_owned()),
        result = timeout(HOST_RESPONSE_TIMEOUT, receiver) => match result {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => Err("Host session ended before the diagnostics response arrived".to_owned()),
            Err(_) => Err("Timed out waiting for the Host diagnostics response".to_owned()),
        }
    }
}

async fn validate_connection_unchanged(state: &AppState, before: Option<&ConnectionStateDto>) -> Result<(), String> {
    let current = state.updates.connection_snapshot().await;
    let unchanged = match (before, current.as_ref()) {
        (Some(before), Some(current)) => {
            before.phase == "online"
                && current.phase == "online"
                && before.profile_id == current.profile_id
                && before.server_epoch == current.server_epoch
                && before.session_generation == current.session_generation
                && before.transport_generation == current.transport_generation
        }
        _ => false,
    };
    if unchanged {
        Ok(())
    } else {
        Err("Host session or transport generation changed during diagnostic export".to_owned())
    }
}

fn validate_host_session(before: Option<&ConnectionStateDto>, host: &Value) -> Result<(), String> {
    let before = before.ok_or_else(|| "Host session is unavailable".to_owned())?;
    if host.get("server_epoch").and_then(Value::as_str) != before.server_epoch.as_deref()
        || host.get("transport_generation").and_then(Value::as_u64) != before.transport_generation
    {
        return Err("Host diagnostic snapshot belongs to a different session generation".to_owned());
    }
    Ok(())
}

fn terminal_session_metadata(connection: Option<&ConnectionStateDto>, room_id: Option<Uuid>) -> Value {
    json!({
        "phase": connection.map(|value| value.phase.as_str()),
        "transport": connection.and_then(|value| value.transport.as_deref()),
        "room_id": room_id,
        "server_epoch": connection.and_then(|value| value.server_epoch.as_deref()),
        "session_generation": connection.and_then(|value| value.session_generation),
        "transport_generation": connection.and_then(|value| value.transport_generation),
    })
}

fn application_from_value(value: &Value) -> ApplicationInfo {
    ApplicationInfo {
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("ShockingVRC Host")
            .to_owned(),
        version: value
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        git_commit: value
            .get("git_commit")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
    }
}

#[derive(Serialize)]
struct ApplicationInfo {
    name: String,
    version: String,
    git_commit: String,
}

struct StreamSource {
    application: ApplicationInfo,
    snapshot: LogSnapshotMetadata,
    session: Value,
    record_count: u64,
    sha256: String,
    records_path: PathBuf,
}

impl Serialize for StreamSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(6))?;
        map.serialize_entry("application", &self.application)?;
        map.serialize_entry("snapshot", &self.snapshot)?;
        map.serialize_entry("session", &self.session)?;
        map.serialize_entry("record_count", &self.record_count)?;
        map.serialize_entry("sha256", &self.sha256)?;
        map.serialize_entry("records", &StreamRecords(&self.records_path))?;
        map.end()
    }
}

struct StreamRecords<'a>(&'a Path);

impl Serialize for StreamRecords<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let file = File::open(self.0).map_err(serde::ser::Error::custom)?;
        let mut seq = serializer.serialize_seq(None)?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(serde::ser::Error::custom)?;
            if line.is_empty() {
                continue;
            }
            let record: DiagnosticLogRecord = serde_json::from_str(&line).map_err(serde::ser::Error::custom)?;
            if sanitize_diagnostic_log_line(&record.message).is_none() {
                continue;
            }
            seq.serialize_element(&record)?;
        }
        seq.end()
    }
}

#[derive(Serialize)]
struct Sources {
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal: Option<StreamSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<StreamSource>,
}

#[derive(Serialize)]
struct DiagnosticPackage {
    schema_version: u8,
    exported_at_utc: String,
    selected_sources: String,
    requested_time_range: RequestedTimeRange,
    complete: bool,
    terminal_runtime: crate::diagnostics::DiagnosticSnapshot,
    terminal_heartbeat_incidents: Vec<crate::heartbeat_incidents::HeartbeatIncident>,
    terminal_heartbeat_incident_persistence_errors: u64,
    terminal_session: Value,
    sources: Sources,
}

fn spool_snapshot(
    mut snapshot: LogSnapshot,
    path: &Path,
    cancel: &CancellationToken,
    mut progress: impl FnMut(u64, u64),
) -> Result<(u64, String), String> {
    let metadata = snapshot.metadata().clone();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("Could not create Terminal diagnostics temporary file: {error}"))?;
    let mut hasher = Sha256::new();
    let mut offset = 0;
    let mut count = 0_u64;
    loop {
        ensure_not_cancelled_token(cancel)?;
        let chunk = snapshot
            .next_chunk(offset, MAX_LOG_CHUNK_BYTES)
            .map_err(|error| format!("Could not read Terminal diagnostics snapshot: {error}"))?;
        file.write_all(&chunk.bytes)
            .map_err(|error| format!("Could not spool Terminal diagnostics: {error}"))?;
        hasher.update(&chunk.bytes);
        count = count.saturating_add(chunk.bytes.iter().filter(|byte| **byte == b'\n').count() as u64);
        offset = chunk.next_offset;
        progress(offset, metadata.total_bytes);
        if chunk.eof {
            let digest = hex(&hasher.clone().finalize());
            if chunk.sha256.as_deref() != Some(digest.as_str()) || offset != metadata.total_bytes {
                return Err("Terminal diagnostics SHA-256 verification failed".to_owned());
            }
            break;
        }
    }
    file.flush()
        .map_err(|error| format!("Could not flush Terminal diagnostics: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("Could not sync Terminal diagnostics: {error}"))?;
    validate_jsonl(path, count)?;
    Ok((count, hex(&hasher.clone().finalize())))
}

fn validate_jsonl(path: &Path, expected_count: u64) -> Result<(), String> {
    let file = File::open(path).map_err(|error| format!("Could not validate diagnostics spool: {error}"))?;
    let mut count = 0_u64;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| format!("Could not read diagnostics spool: {error}"))?;
        if line.is_empty() {
            continue;
        }
        let record: DiagnosticLogRecord = serde_json::from_str(&line)
            .map_err(|_| "Diagnostic log snapshot contains an invalid structured record".to_owned())?;
        if sanitize_diagnostic_log_line(&record.message).is_none() {
            return Err("Diagnostic log snapshot contains a record excluded by the privacy filter".to_owned());
        }
        count += 1;
    }
    if count == expected_count {
        Ok(())
    } else {
        Err("Diagnostic log record count did not match the captured stream".to_owned())
    }
}

fn unique_output_path(directory: &Path) -> Result<PathBuf, String> {
    for index in 0..100 {
        let name = if index == 0 {
            format!(
                "shockingvrc-diagnostics-{}.json",
                chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f")
            )
        } else {
            format!(
                "shockingvrc-diagnostics-{}-{index}.json",
                chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f")
            )
        };
        let path = directory.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("Could not allocate a unique diagnostics filename".to_owned())
}

fn ensure_not_cancelled(control: &DiagnosticExportControl) -> Result<(), String> {
    ensure_not_cancelled_token(&control.cancel)
}

fn ensure_not_cancelled_token(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("Diagnostic export cancelled".to_owned())
    } else {
        Ok(())
    }
}

fn set_stage(control: &DiagnosticExportControl, stage: &str, progress: u8) {
    if let Ok(mut value) = control.stage.lock() {
        *value = stage.to_owned();
    }
    control.progress.store(progress.min(100), Ordering::Relaxed);
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn requested_sources(source: &str) -> Result<(bool, bool), String> {
    match source {
        "terminal" => Ok((true, false)),
        "host" => Ok((false, true)),
        "both" => Ok((true, true)),
        _ => Err("Select Terminal only, Host only, or Both".to_owned()),
    }
}

fn filter_runtime_snapshot(
    runtime: &mut crate::diagnostics::DiagnosticSnapshot,
    range: Option<&LogTimeRange>,
) {
    let Some(range) = range else {
        runtime.exported_event_count = runtime.events.len() as u64;
        return;
    };
    let start = DateTime::parse_from_rfc3339(&range.start_utc)
        .expect("validated diagnostic time range")
        .with_timezone(&Utc);
    let end = DateTime::parse_from_rfc3339(&range.end_utc)
        .expect("validated diagnostic time range")
        .with_timezone(&Utc);
    let retained = runtime.events.len() as u64;
    let mut unknown = 0_u64;
    let mut excluded = 0_u64;
    runtime.events.retain(|event| {
        let Some(timestamp) = DateTime::parse_from_rfc3339(&event.timestamp_utc)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc))
        else {
            unknown = unknown.saturating_add(1);
            return false;
        };
        let included = start <= timestamp && timestamp < end;
        if !included {
            excluded = excluded.saturating_add(1);
        }
        included
    });
    runtime.exported_event_count = runtime.events.len() as u64;
    runtime.events_excluded_by_time_range = excluded;
    runtime.events_unknown_timestamp = unknown;
    debug_assert_eq!(retained, runtime.exported_event_count + excluded + unknown);
}

fn filter_heartbeat_incidents(
    incidents: &mut Vec<crate::heartbeat_incidents::HeartbeatIncident>,
    range: Option<&LogTimeRange>,
) {
    let Some(range) = range else {
        return;
    };
    let start = DateTime::parse_from_rfc3339(&range.start_utc)
        .expect("validated diagnostic time range")
        .with_timezone(&Utc);
    let end = DateTime::parse_from_rfc3339(&range.end_utc)
        .expect("validated diagnostic time range")
        .with_timezone(&Utc);
    incidents.retain(|incident| {
        let Some(incident_start) = DateTime::parse_from_rfc3339(&incident.started_utc)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc))
        else {
            return false;
        };
        let incident_end = incident
            .completed_utc
            .as_deref()
            .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
            .map(|timestamp| timestamp.with_timezone(&Utc))
            .unwrap_or_else(|| end.clone());
        incident_start < end && incident_end >= start
    });
}

fn resolve_time_range(request: &DiagnosticExportRequest, now: DateTime<Utc>) -> Result<ResolvedTimeRange, String> {
    let normalized = |time: String| time;
    let (range, start_local, end_local) = match request.time_range.as_str() {
        "all" => (None, None, None),
        "last_1_hour" | "last_6_hours" | "last_24_hours" => {
            let hours = match request.time_range.as_str() {
                "last_1_hour" => 1,
                "last_6_hours" => 6,
                _ => 24,
            };
            let end = now;
            let start = end - ChronoDuration::hours(hours);
            let start = start.to_rfc3339_opts(SecondsFormat::Millis, true);
            let end = end.to_rfc3339_opts(SecondsFormat::Millis, true);
            (Some(LogTimeRange::new(&start, &end)?), None, None)
        }
        "custom" => {
            if request.terminal_timezone.trim().is_empty() {
                return Err("Could not determine the Terminal local timezone".to_owned());
            }
            let start = request
                .start_utc
                .as_deref()
                .ok_or_else(|| "Choose a valid custom From date and time".to_owned())?;
            let end = request
                .end_utc
                .as_deref()
                .ok_or_else(|| "Choose a valid custom To date and time".to_owned())?;
            if request.start_local.is_none() || request.end_local.is_none() {
                return Err("Custom local From and To date/time values are required for export metadata".to_owned());
            }
            (
                Some(LogTimeRange::new(start, end)?),
                request.start_local.clone(),
                request.end_local.clone(),
            )
        }
        _ => return Err("Select a supported diagnostic time range".to_owned()),
    };
    let metadata = RequestedTimeRange {
        kind: request.time_range.clone(),
        start_utc: range.as_ref().map(|range| normalized(range.start_utc.clone())),
        end_utc: range.as_ref().map(|range| normalized(range.end_utc.clone())),
        terminal_timezone: request.terminal_timezone.clone(),
        start_local,
        end_local,
        boundary: "[start_utc,end_utc)",
    };
    Ok(ResolvedTimeRange { range, metadata })
}

fn finalize_export_result(
    output_path: String,
    open_folder: bool,
    launch: &mut impl FnMut(&Path, bool) -> Result<(), String>,
) -> DiagnosticExportResult {
    let (explorer_opened, export_file_selected, explorer_error) =
        match maybe_reveal_export_path(Path::new(&output_path), open_folder, launch) {
            Ok(ExplorerReveal::NotRequested) => (false, false, None),
            Ok(ExplorerReveal::FileSelected) => (true, true, None),
            Ok(ExplorerReveal::FolderOpened) => (true, false, None),
            Err(error) => (false, false, Some(error)),
        };
    DiagnosticExportResult {
        output_path,
        explorer_opened,
        export_file_selected,
        explorer_error,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ExplorerReveal {
    NotRequested,
    FileSelected,
    FolderOpened,
}

fn maybe_reveal_export_path(
    path: &Path,
    enabled: bool,
    launch: &mut impl FnMut(&Path, bool) -> Result<(), String>,
) -> Result<ExplorerReveal, String> {
    if !enabled {
        return Ok(ExplorerReveal::NotRequested);
    }
    match launch(path, true) {
        Ok(()) => Ok(ExplorerReveal::FileSelected),
        Err(select_error) => {
            let directory = path
                .parent()
                .ok_or_else(|| format!("Could not select the export and no parent folder exists: {select_error}"))?;
            launch(directory, false).map_err(|folder_error| {
                format!("Could not select the export ({select_error}) or open its folder ({folder_error})")
            })?;
            Ok(ExplorerReveal::FolderOpened)
        }
    }
}

#[cfg(windows)]
fn launch_explorer(path: &Path, select_file: bool) -> Result<(), String> {
    use std::ffi::OsString;
    let mut command = Command::new("explorer.exe");
    if select_file {
        let mut argument = OsString::from("/select,");
        argument.push(path.as_os_str());
        command.arg(argument);
    } else {
        command.arg(path.as_os_str());
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Windows Explorer could not be started: {error}"))
}

#[cfg(not(windows))]
fn launch_explorer(_path: &Path, _select_file: bool) -> Result<(), String> {
    Err("Opening the export in Explorer is only available on Windows".to_owned())
}

struct TempFiles {
    paths: Vec<PathBuf>,
}

impl TempFiles {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }
    fn add(&mut self, path: PathBuf) {
        self.paths.push(path);
    }
    fn disarm(&mut self, path: &Path) {
        self.paths.retain(|value| value != path);
    }
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(contents: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("shockingvrc-export-test-{}.jsonl", Uuid::new_v4()));
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn source_selection_is_explicit_and_both_is_supported() {
        assert_eq!(requested_sources("terminal").unwrap(), (true, false));
        assert_eq!(requested_sources("host").unwrap(), (false, true));
        assert_eq!(requested_sources("both").unwrap(), (true, true));
        assert!(requested_sources("unknown").is_err());
    }

    fn request(time_range: &str) -> DiagnosticExportRequest {
        DiagnosticExportRequest {
            source: "both".to_owned(),
            time_range: time_range.to_owned(),
            terminal_timezone: "America/New_York".to_owned(),
            start_local: None,
            end_local: None,
            start_utc: None,
            end_utc: None,
            open_folder_after_export: true,
        }
    }

    #[test]
    fn relative_ranges_capture_one_end_instant_and_default_to_all_retained() {
        let now = DateTime::parse_from_rfc3339("2026-10-24T12:34:56.789Z")
            .unwrap()
            .with_timezone(&Utc);
        let all = resolve_time_range(&request("all"), now).unwrap();
        assert!(all.range.is_none());
        assert_eq!(all.metadata.kind, "all");
        assert_eq!(all.metadata.start_utc, None);

        let last_hour = resolve_time_range(&request("last_1_hour"), now).unwrap();
        assert_eq!(last_hour.range.as_ref().unwrap().start_utc, "2026-10-24T11:34:56.789Z");
        assert_eq!(last_hour.range.as_ref().unwrap().end_utc, "2026-10-24T12:34:56.789Z");
        assert_eq!(last_hour.metadata.boundary, "[start_utc,end_utc)");

        for (preset, hours) in [("last_6_hours", 6_i64), ("last_24_hours", 24_i64)] {
            let resolved = resolve_time_range(&request(preset), now).unwrap();
            let start = DateTime::parse_from_rfc3339(&resolved.range.as_ref().unwrap().start_utc)
                .unwrap()
                .with_timezone(&Utc);
            let end = DateTime::parse_from_rfc3339(&resolved.range.as_ref().unwrap().end_utc)
                .unwrap()
                .with_timezone(&Utc);
            assert_eq!(end - start, ChronoDuration::hours(hours));
            assert_eq!(resolved.range.as_ref().unwrap().end_utc, last_hour.range.as_ref().unwrap().end_utc);
        }
    }

    #[test]
    fn custom_time_range_requires_valid_utc_and_preserves_local_zone_metadata() {
        let now = Utc::now();
        let mut custom = request("custom");
        custom.start_local = Some("2026-10-24T23:30".to_owned());
        custom.end_local = Some("2026-10-25T00:15".to_owned());
        custom.start_utc = Some("2026-10-24T19:30:00.000Z".to_owned());
        custom.end_utc = Some("2026-10-24T20:15:00.000Z".to_owned());
        let resolved = resolve_time_range(&custom, now).unwrap();
        assert_eq!(resolved.metadata.start_local.as_deref(), Some("2026-10-24T23:30"));
        assert_eq!(resolved.metadata.terminal_timezone, "America/New_York");

        custom.end_utc = Some("2026-10-24T19:30:00Z".to_owned());
        assert!(resolve_time_range(&custom, now).unwrap_err().contains("earlier"));
    }

    #[test]
    fn explorer_helper_respects_opt_out_and_falls_back_to_unicode_folder() {
        let path = PathBuf::from(r"C:\Temp\Shocking VRC\данные\export.json");
        let mut should_not_launch = |_: &Path, _: bool| panic!("Explorer must not launch when disabled");
        assert_eq!(
            maybe_reveal_export_path(&path, false, &mut should_not_launch).unwrap(),
            ExplorerReveal::NotRequested
        );
        let mut calls = Vec::new();
        let mut launch = |target: &Path, select: bool| {
            calls.push((target.to_path_buf(), select));
            if select { Err("selection unsupported".to_owned()) } else { Ok(()) }
        };
        assert_eq!(
            maybe_reveal_export_path(&path, true, &mut launch).unwrap(),
            ExplorerReveal::FolderOpened
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], (path.clone(), true));
        assert_eq!(calls[1], (path.parent().unwrap().to_path_buf(), false));
    }

    #[test]
    fn explorer_helper_selects_the_completed_export_when_supported() {
        let path = PathBuf::from(r"C:\Temp\Shocking VRC\данные\export.json");
        let mut calls = Vec::new();
        let mut launch = |target: &Path, select: bool| {
            calls.push((target.to_path_buf(), select));
            Ok(())
        };
        assert_eq!(
            maybe_reveal_export_path(&path, true, &mut launch).unwrap(),
            ExplorerReveal::FileSelected
        );
        assert_eq!(calls, [(path, true)]);
    }

    #[test]
    fn explorer_failure_is_reported_without_invalidating_the_written_json() {
        let path = r"C:\Temp\Shocking VRC\данные\export.json".to_owned();
        let mut launch = |_: &Path, _: bool| Err("Explorer unavailable".to_owned());
        let result = finalize_export_result(path.clone(), true, &mut launch);
        assert_eq!(result.output_path, path);
        assert!(!result.explorer_opened);
        assert!(result.explorer_error.as_deref().unwrap().contains("Explorer unavailable"));
    }

    #[test]
    fn runtime_range_filters_events_but_keeps_since_reset_counters() {
        let recorder = crate::diagnostics::DiagnosticRecorder::default();
        recorder.record(crate::diagnostics::DiagnosticKind::HeartbeatTx, None, None, None, None, None);
        recorder.record(crate::diagnostics::DiagnosticKind::HeartbeatAck, None, None, None, None, None);
        let mut runtime = recorder.snapshot();
        let lifetime_heartbeat_count = runtime.counters.heartbeat_ack;
        let start = runtime.events[0].timestamp_utc.clone();
        let end = runtime.events[1].timestamp_utc.clone();
        let end_exclusive = DateTime::parse_from_rfc3339(&end).unwrap().with_timezone(&Utc)
            + ChronoDuration::milliseconds(1);
        let range = LogTimeRange::new(&start, &end_exclusive.to_rfc3339()).unwrap();
        filter_runtime_snapshot(&mut runtime, Some(&range));
        assert_eq!(runtime.events.len(), 2);
        assert_eq!(runtime.exported_event_count, 2);
        assert_eq!(runtime.events_excluded_by_time_range, 0);
        assert_eq!(runtime.counters.heartbeat_ack, lifetime_heartbeat_count);
        assert_eq!(runtime.counters_scope.as_deref(), Some("since_last_reset"));
    }

    #[test]
    fn unified_export_keeps_an_active_heartbeat_incident_that_overlaps_the_selected_range() {
        let incident = crate::heartbeat_incidents::HeartbeatIncident {
            id: Uuid::new_v4(),
            started_utc: "2026-10-23T23:59:00.000Z".to_owned(),
            completed_utc: None,
            outcome: crate::heartbeat_incidents::HeartbeatIncidentOutcome::InProgress,
            room_id: Some(Uuid::new_v4()),
            generation: Some(4),
            transport: Some("webrtc_datachannel".to_owned()),
            candidate_pair: Some("RELAY/UDP ↔ RELAY/UDP".to_owned()),
            queue_metrics: crate::heartbeat_incidents::HeartbeatIncidentQueueMetrics::default(),
            prelude_events: Vec::new(),
            recovery_events: Vec::new(),
            recovery_events_dropped: 0,
        };
        let range = LogTimeRange::new("2026-10-24T00:00:00.000Z", "2026-10-24T01:00:00.000Z").unwrap();
        let mut incidents = vec![incident.clone()];
        filter_heartbeat_incidents(&mut incidents, Some(&range));
        assert_eq!(incidents, [incident.clone()]);

        let package = DiagnosticPackage {
            schema_version: 1,
            exported_at_utc: "2026-10-24T00:30:00.000Z".to_owned(),
            selected_sources: "terminal".to_owned(),
            requested_time_range: resolve_time_range(&request("all"), Utc::now()).unwrap().metadata,
            complete: true,
            terminal_runtime: crate::diagnostics::DiagnosticRecorder::default().snapshot(),
            terminal_heartbeat_incidents: incidents,
            terminal_heartbeat_incident_persistence_errors: 0,
            terminal_session: serde_json::json!({}),
            sources: Sources { terminal: None, host: None },
        };
        let encoded = serde_json::to_vec(&package).unwrap();
        let encoded: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(encoded["schema_version"], 1);
        assert!(encoded.get("terminal_runtime").is_some());
        assert!(encoded.get("sources").is_some());
        assert_eq!(encoded["terminal_heartbeat_incidents"][0]["id"], incident.id.to_string());
        assert_eq!(encoded["terminal_heartbeat_incidents"][0]["outcome"], "in_progress");
    }

    #[test]
    fn export_refuses_unsafe_records_and_preserves_source_timestamp_on_safe_records() {
        let safe = DiagnosticLogRecord::imported_legacy(
            "WARN",
            "heartbeat deadline elapsed",
            Some("2026-10-22 01:02:03.004".to_owned()),
        );
        let safe_bytes = [serde_json::to_vec(&safe).unwrap(), b"\n".to_vec()].concat();
        let safe_path = temp_file(&safe_bytes);
        validate_jsonl(&safe_path, 1).unwrap();
        let streamed = serde_json::to_string(&StreamRecords(&safe_path)).unwrap();
        assert!(streamed.contains("2026-10-22 01:02:03.004"));
        let _ = fs::remove_file(safe_path);

        let unsafe_record = DiagnosticLogRecord::now("WARN", "test", "access_token=private-value");
        let unsafe_bytes = [serde_json::to_vec(&unsafe_record).unwrap(), b"\n".to_vec()].concat();
        let unsafe_path = temp_file(&unsafe_bytes);
        assert!(validate_jsonl(&unsafe_path, 1).unwrap_err().contains("privacy filter"));
        let _ = fs::remove_file(unsafe_path);
    }

    #[test]
    fn canceled_export_is_detected_before_publishing() {
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(ensure_not_cancelled_token(&token).unwrap_err(), "Diagnostic export cancelled");
    }
}

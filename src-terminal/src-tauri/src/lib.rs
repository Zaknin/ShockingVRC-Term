mod allowed_operation;
mod app_state;
mod commands;
mod diagnostic_export;
mod diagnostics;
mod heartbeat_incidents;
mod protocol;
mod storage;
mod transport;

use shocking_vrc_diagnostics::{DiagnosticLogRecord, PersistentLogStore};
use shocking_vrc_rtc::sanitize_diagnostic_log_line;
use std::{io::Write, path::Path};
use tauri::{Manager, Runtime};
use tauri_plugin_deep_link::DeepLinkExt;

fn process_pairing_urls<R: Runtime>(app: &tauri::AppHandle<R>, urls: Vec<url::Url>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for url in urls {
            let state = app.state::<app_state::AppState>();
            if let Err(error) = commands::accept_pairing_invitation(&state, url.as_str(), true).await {
                log::warn!("[terminal] rejected pairing deep link: {error}");
                state.updates.publish(crate::protocol::UiEvent::Error(error)).await;
            }
        }
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
        }
    });
}

struct TerminalLogger {
    store: PersistentLogStore,
    level: log::LevelFilter,
    ice_diagnostics: bool,
}

impl log::Log for TerminalLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        let target = metadata.target();
        let selected_target = ["shockingvrc_terminal", "shocking_vrc_rtc", "shocking_vrc_rendezvous"]
            .iter()
            .any(|prefix| target.starts_with(prefix));
        (selected_target && metadata.level() <= self.level)
            || (self.ice_diagnostics
                && metadata.level() == log::Level::Debug
                && (target.starts_with("webrtc::peer_connection::transport::turn_relayer")
                    || target.starts_with("rtc_turn::client::relay")))
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        if let Some(message) = sanitize_diagnostic_log_line(&message) {
            let _ = self
                .store
                .append(DiagnosticLogRecord::now(record.level().as_str(), record.target(), message));
        }
    }

    fn flush(&self) {
        let _ = self.store.flush();
    }
}

fn initialize_terminal_logging(data_dir: &Path) -> Result<PersistentLogStore, String> {
    let store = PersistentLogStore::open(data_dir.join("diagnostics").join("logs"))
        .map_err(|error| format!("Could not open persistent Terminal diagnostics: {error}"))?;
    import_legacy_terminal_logs(data_dir, &store)
        .map_err(|error| format!("Could not preserve earlier Terminal log history: {error}"))?;
    let diagnostics = std::env::var("SHOCKINGVRC_ICE_DIAGNOSTICS")
        .is_ok_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on"));
    let level = if diagnostics {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    log::set_boxed_logger(Box::new(TerminalLogger {
        store: store.clone(),
        level,
        ice_diagnostics: diagnostics,
    }))
    .map_err(|error| format!("Could not initialize Terminal logger: {error}"))?;
    log::set_max_level(if diagnostics { level } else { level });
    log::info!("Terminal transport diagnostics initialized; detailed ICE diagnostics={diagnostics}");
    Ok(store)
}

fn import_legacy_terminal_logs(data_dir: &Path, store: &PersistentLogStore) -> std::io::Result<()> {
    let marker = store.root().join("legacy-transport-import-v1.done");
    if marker.exists() {
        return Ok(());
    }
    let active = data_dir.join("transport.log");
    let previous = diagnostics::previous_transport_log_path(&active);
    let mut imported = 0usize;
    for path in [previous, active] {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines() {
            if let Some(safe_line) = sanitize_diagnostic_log_line(line) {
                let level = line
                    .strip_prefix('[')
                    .and_then(|value| value.split(']').next())
                    .unwrap_or("INFO")
                    .trim();
                let source_timestamp = legacy_line_timestamp(line);
                let record = DiagnosticLogRecord::imported_legacy(level, safe_line, source_timestamp);
                if !store.append(record.clone()) {
                    store.flush()?;
                    if !store.append(record) {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            "legacy log import queue remained full",
                        ));
                    }
                }
                imported += 1;
                if imported % 1_000 == 0 {
                    store.flush()?;
                }
            }
        }
    }
    store.flush()?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)?
        .write_all(b"legacy transport logs imported once\n")?;
    Ok(())
}

fn legacy_line_timestamp(line: &str) -> Option<String> {
    let after_level = line.split_once("] ")?.1;
    let (timestamp, _) = after_level.split_once(" [")?;
    Some(timestamp.to_owned())
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .plugin(tauri_plugin_deep_link::init());
    builder
        .setup(|app| {
            let data_dir = terminal_data_dir(app)?;
            std::fs::create_dir_all(&data_dir)?;
            let diagnostic_store = initialize_terminal_logging(&data_dir).map_err(std::io::Error::other)?;
            let state = app_state::AppState::open(data_dir, diagnostic_store).map_err(std::io::Error::other)?;
            app.manage(state);
            let deep_link = app.deep_link();
            if let Some(urls) = deep_link.get_current()? {
                process_pairing_urls(app.handle(), urls);
            }
            deep_link.on_open_url({
                let app = app.handle().clone();
                move |event| process_pairing_urls(&app, event.urls())
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                if let Some(state) = window.try_state::<app_state::AppState>() {
                    if let Ok(connection) = state.connection.try_lock() {
                        if let Some(connection) = connection.as_ref() {
                            connection.cancel.cancel();
                        }
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_profiles,
            commands::rename_profile,
            commands::cache_host_display_name,
            commands::save_profile_connection_settings,
            commands::delete_profile,
            commands::subscribe_state,
            commands::open_pairing_invitation,
            commands::connect_profile,
            commands::disconnect,
            commands::fetch_state,
            commands::fetch_capabilities,
            commands::fetch_catalog,
            commands::fetch_logs,
            commands::fetch_graph_document,
            commands::fetch_graph_presets,
            commands::set_engine_mode,
            commands::get_terminal_diagnostics,
            commands::record_telemetry_render_metrics,
            commands::clear_terminal_diagnostics,
            diagnostic_export::export_diagnostics,
            diagnostic_export::diagnostic_export_progress,
            diagnostic_export::cancel_diagnostic_export,
            commands::submit_operation,
        ])
        .build(tauri::generate_context!())
        .expect("ShockingVRC Terminal failed to start")
        .run(|_app_handle, _event| {});
    log::logger().flush();
}

fn terminal_data_dir(app: &tauri::App) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    #[cfg(feature = "diagnostics-test-profile")]
    if let Some(path) = std::env::var_os("SHOCKINGVRC_TEST_DATA_DIR") {
        return Ok(std::path::PathBuf::from(path));
    }
    Ok(app.path().app_data_dir()?)
}

#[cfg(test)]
mod persistent_log_tests {
    use super::*;

    #[test]
    fn legacy_transport_logs_are_imported_once_without_losing_source_clock_or_files() {
        let root = std::env::temp_dir().join(format!("shockingvrc-terminal-legacy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let active = root.join("transport.log");
        let previous = diagnostics::previous_transport_log_path(&active);
        std::fs::write(
            &previous,
            "[WARN ] 2026-10-22 01:02:03.004 [terminal] heartbeat deadline elapsed\n",
        )
        .unwrap();
        std::fs::write(&active, "[INFO ] 2026-10-22 01:02:04.005 [terminal] access_token=must-not-import\n[INFO ] 2026-10-22 01:02:05.006 [terminal] session resumed\n").unwrap();
        let store = PersistentLogStore::open(root.join("diagnostics").join("logs")).unwrap();

        import_legacy_terminal_logs(&root, &store).unwrap();
        import_legacy_terminal_logs(&root, &store).unwrap();
        let records = store.tail_records(10).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].source_timestamp.as_deref(), Some("2026-10-22 01:02:03.004"));
        assert!(records[0].message.contains("heartbeat deadline elapsed"));
        assert!(records[1].message.contains("session resumed"));
        assert!(records.iter().all(|record| !record.message.contains("must-not-import")));
        assert!(
            active.exists() && previous.exists(),
            "migration must preserve original profile files"
        );

        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}

mod app_state;
mod allowed_operation;
mod commands;
mod protocol;
mod storage;
mod transport;

use std::{fs::OpenOptions, io, io::Write, path::Path};
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

fn initialize_terminal_logging(data_dir: &Path) -> io::Result<()> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("transport.log"))?;
    let diagnostics = std::env::var("SHOCKINGVRC_ICE_DIAGNOSTICS")
        .is_ok_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on"));
    let level = if diagnostics { log::LevelFilter::Debug } else { log::LevelFilter::Info };
    let mut builder = env_logger::Builder::new();
    builder
        .filter_level(log::LevelFilter::Off)
        .filter_module("shockingvrc_terminal", level)
        .filter_module("shocking_vrc_rtc", level)
        .filter_module("shocking_vrc_rendezvous", level)
        .target(env_logger::Target::Pipe(Box::new(file)))
        .format(|buffer, record| {
            let tag = match record.level() {
                log::Level::Error => "ERROR",
                log::Level::Warn => "WARN ",
                log::Level::Info => "INFO ",
                log::Level::Debug => "DEBUG",
                log::Level::Trace => "TRACE",
            };
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            writeln!(buffer, "[{tag}] {timestamp} [{}] {}", record.target(), record.args())
        });
    if diagnostics {
        builder
            .filter_module("webrtc::peer_connection::transport::turn_relayer", log::LevelFilter::Debug)
            .filter_module("rtc_turn::client::relay", log::LevelFilter::Debug);
    }
    builder.try_init().map_err(io::Error::other)?;
    log::info!("Terminal transport diagnostics initialized; detailed ICE diagnostics={diagnostics}");
    Ok(())
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .plugin(tauri_plugin_deep_link::init());
    builder
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            if let Err(error) = initialize_terminal_logging(&data_dir) {
                eprintln!("Terminal diagnostic log initialization failed: {error}");
            }
            let state = app_state::AppState::open(data_dir).map_err(std::io::Error::other)?;
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
            commands::delete_profile,
            commands::subscribe_state,
            commands::open_pairing_invitation,
            commands::connect_profile,
            commands::disconnect,
            commands::fetch_state,
            commands::fetch_capabilities,
            commands::fetch_catalog,
            commands::fetch_logs,
            commands::get_terminal_diagnostics,
            commands::clear_terminal_diagnostics,
            commands::submit_operation,
        ])
        .run(tauri::generate_context!())
        .expect("ShockingVRC Terminal failed to start");
}

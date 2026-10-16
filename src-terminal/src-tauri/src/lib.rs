mod app_state;
mod allowed_operation;
mod commands;
mod protocol;
mod storage;
mod transport;

use std::{fs::OpenOptions, io, io::Write, path::Path};
use tauri::Manager;

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
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            if let Err(error) = initialize_terminal_logging(&data_dir) {
                eprintln!("Terminal diagnostic log initialization failed: {error}");
            }
            let state = app_state::AppState::open(data_dir).map_err(std::io::Error::other)?;
            app.manage(state);
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
                    if let Ok(mut pending) = state.pending.try_lock() {
                        *pending = None;
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_profiles,
            commands::save_profile,
            commands::delete_profile,
            commands::subscribe_state,
            commands::connect_profile,
            commands::save_invite_token,
            commands::has_saved_invite_token,
            commands::forget_invite_token,
            commands::login,
            commands::login_saved,
            commands::has_saved_credentials,
            commands::forget_credentials,
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

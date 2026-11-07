mod app_state;
mod allowed_operation;
mod commands;
mod protocol;
mod storage;
mod transport;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
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
            commands::submit_operation,
        ])
        .run(tauri::generate_context!())
        .expect("ShockingVRC Terminal failed to start");
}

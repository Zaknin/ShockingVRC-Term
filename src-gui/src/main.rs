#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use shocking_vrc_app::runtime::{self, SessionOptions};
use shocking_vrc_gui::{GuiApp, bridge, remote_adapter::BridgeRemoteBackend};
use shocking_vrc_remote::{HostRemoteController, RemoteAccessSettings};

fn main() -> eframe::Result<()> {
    shocking_vrc_core::update::cleanup_staging_files();

    let log_buffer = shocking_vrc_app::logger::init("info");
    let opts = SessionOptions::from_args(
        "Usage: shockingvrc [--port <n>] [--scan-timeout <secs>] [--skip-update-check]",
    );

    log::info!(
        "ShockingVRC v{} — Hello DG-Lab Users",
        shocking_vrc_core::VERSION
    );

    let runtime_handle = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the async runtime");

    let state = runtime_handle.block_on(runtime::start_session(&opts));
    log::info!("[gui] Ready.");

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([900.0, 620.0])
            .with_title(format!("ShockingVRC {}", shocking_vrc_core::VERSION))
            .with_app_id("shockingvrc"),
        ..Default::default()
    };

    let session = Arc::clone(&state);
    let skip_update_check = opts.skip_update_check;
    let remote_slot: Arc<Mutex<Option<Arc<HostRemoteController>>>> = Arc::new(Mutex::new(None));
    let remote_slot_for_ui = Arc::clone(&remote_slot);

    let result = {
        let _enter = runtime_handle.enter();
        eframe::run_native(
            "ShockingVRC",
            native_options,
            Box::new(move |cc| {
                let ctx = cc.egui_ctx.clone();
                let bridge = bridge::spawn(session, move || ctx.request_repaint());
                let (remote_settings, settings_error) = match RemoteAccessSettings::load() {
                    Ok(settings) => (settings, None),
                    Err(error) => (RemoteAccessSettings::default(), Some(error.to_string())),
                };
                let remote_backend = BridgeRemoteBackend::new(bridge.clone(), Arc::clone(&log_buffer));
                let remote = HostRemoteController::new(remote_backend, remote_settings);
                let remote_init = Arc::clone(&remote);
                tokio::spawn(async move {
                    remote_init.initialize().await;
                    if let Some(error) = settings_error {
                        remote_init.report_startup_error(format!("Remote settings could not be loaded: {error}"));
                    }
                });
                if let Ok(mut slot) = remote_slot_for_ui.lock() {
                    *slot = Some(Arc::clone(&remote));
                }
                if !skip_update_check {
                    bridge.send(bridge::Command::CheckUpdate);
                }
                Ok(Box::new(GuiApp::new(cc, bridge, log_buffer, remote)))
            }),
        )
    };

    if let Some(remote) = remote_slot.lock().ok().and_then(|mut slot| slot.take()) {
        runtime_handle.block_on(remote.shutdown());
    }
    runtime_handle.block_on(runtime::shutdown_session(&state));

    if let Some((downloaded, restart_args)) = bridge::pending_update() {
        match shocking_vrc_core::update::apply_self_update(&downloaded, &restart_args) {
            Ok(()) => {
                let _ = std::fs::remove_file(&downloaded);
                return result;
            }
            Err(e) => eprintln!("[update] install failed: {e}"),
        }
    }

    result
}


use std::{env, process::ExitCode};

use shocking_vrc_rendezvous::{config::LoadedConfig, server};

#[tokio::main]
async fn main() -> ExitCode {
    let ice_diagnostics = env::var("SHOCKINGVRC_ICE_DIAGNOSTICS")
        .is_ok_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on"));
    let mut logger = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if ice_diagnostics {
        logger.filter_module("shocking_vrc_rendezvous", log::LevelFilter::Debug);
    }
    let _ = logger.try_init();
    log::info!("Rendezvous diagnostics initialized; detailed ICE diagnostics={ice_diagnostics}");
    let config_path = match config_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let config = match LoadedConfig::load(&config_path) {
        Ok(config) => config,
        Err(error) => {
            log::error!("{error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = server::run(config).await {
        log::error!("{error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn config_path() -> Result<String, &'static str> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("--config") => args.next().ok_or("missing path after --config"),
        Some("--help" | "-h") => Err("Usage: shocking-vrc-rendezvous --config <path> (or set SVRC_CONFIG)"),
        Some(_) => Err("Usage: shocking-vrc-rendezvous --config <path> (or set SVRC_CONFIG)"),
        None => env::var("SVRC_CONFIG").map_err(|_| "Provide --config <path> or set SVRC_CONFIG"),
    }
}

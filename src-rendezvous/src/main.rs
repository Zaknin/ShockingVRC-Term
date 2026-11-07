use std::{env, process::ExitCode};

use shocking_vrc_rendezvous::{config::LoadedConfig, server};

#[tokio::main]
async fn main() -> ExitCode {
    env_logger::init();
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

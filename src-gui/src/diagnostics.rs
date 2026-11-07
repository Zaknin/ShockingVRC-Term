use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

use shocking_vrc_rtc::sanitize_diagnostic_log_line;

pub fn export_host_diagnostics(lines: &[String]) -> Result<PathBuf, String> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "Windows LOCALAPPDATA is unavailable; no diagnostics were written".to_owned())?;
    let directory = local_app_data.join("ShockingVRC").join("diagnostics");
    fs::create_dir_all(&directory).map_err(|error| format!("Could not create local diagnostics folder: {error}"))?;

    let exported_at = chrono::Utc::now().to_rfc3339();
    let safe_lines: Vec<String> = lines
        .iter()
        .filter_map(|line| sanitize_diagnostic_log_line(line))
        .collect();
    let package = serde_json::json!({
        "schema_version": 1,
        "application": "ShockingVRC Host",
        "version": env!("CARGO_PKG_VERSION"),
        "git_commit": option_env!("SHOCKINGVRC_GIT_COMMIT").unwrap_or("unknown"),
        "exported_at": exported_at,
        "events": safe_lines,
        "crash_dumps_included": false,
        "uploaded": false,
    });
    write_unique_json(&directory, "host-diagnostics", &package)
}

fn write_unique_json(directory: &std::path::Path, prefix: &str, value: &serde_json::Value) -> Result<PathBuf, String> {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f");
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| format!("Could not encode diagnostics: {error}"))?;
    for suffix in 0..100u8 {
        let name = if suffix == 0 {
            format!("{prefix}-{stamp}.json")
        } else {
            format!("{prefix}-{stamp}-{suffix}.json")
        };
        let path = directory.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(&bytes) {
                    let _ = fs::remove_file(&path);
                    return Err(format!("Could not write diagnostics package: {error}"));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Could not create diagnostics package: {error}")),
        }
    }
    Err("Could not allocate a unique diagnostics filename".to_owned())
}

#[cfg(test)]
mod tests {
    use super::sanitize_diagnostic_log_line;

    #[test]
    fn export_filter_keeps_lifecycle_but_omits_secret_values() {
        assert!(sanitize_diagnostic_log_line("TURN credentials received valid_for_seconds=600").is_some());
        assert!(sanitize_diagnostic_log_line("TURN credentials username=private password=secret").is_none());
    }
}

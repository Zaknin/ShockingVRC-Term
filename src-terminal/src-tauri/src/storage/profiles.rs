use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use crate::protocol::{ProfileDto, ProfileInputDto};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const PROFILE_FILE: &str = "profiles-v1.json";

#[derive(Debug, Serialize, Deserialize)]
struct ProfileFile {
    version: u8,
    profiles: Vec<ProfileDto>,
}

pub struct ProfileStore {
    path: PathBuf,
    profiles: Vec<ProfileDto>,
}

impl ProfileStore {
    pub fn open(app_data_dir: PathBuf) -> Result<Self, String> {
        let path = app_data_dir.join(PROFILE_FILE);
        let profiles = match fs::read(&path) {
            Ok(bytes) => {
                let file: ProfileFile = serde_json::from_slice(&bytes)
                    .map_err(|error| format!("Could not read Terminal profiles: {error}"))?;
                if file.version != 1 {
                    return Err("Terminal profile file version is not supported".to_owned());
                }
                file.profiles
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("Could not read Terminal profiles: {error}")),
        };
        Ok(Self { path, profiles })
    }

    pub fn list(&self) -> Vec<ProfileDto> {
        self.profiles.clone()
    }

    pub fn get(&self, id: &str) -> Result<ProfileDto, String> {
        self.profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| "Host profile was not found".to_owned())
    }

    pub fn save(&mut self, input: ProfileInputDto) -> Result<ProfileDto, String> {
        let profile = normalize_profile(input)?;
        let mut profiles = self.profiles.clone();
        if let Some(existing) = profiles.iter_mut().find(|item| item.id == profile.id) {
            *existing = profile.clone();
        } else {
            profiles.push(profile.clone());
        }
        self.persist_profiles(&profiles)?;
        self.profiles = profiles;
        Ok(profile)
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let mut profiles = self.profiles.clone();
        let previous_len = profiles.len();
        profiles.retain(|profile| profile.id != id);
        if profiles.len() == previous_len {
            return Err("Host profile was not found".to_owned());
        }
        self.persist_profiles(&profiles)?;
        self.profiles = profiles;
        Ok(())
    }

    pub fn rename(&mut self, id: &str, label: &str) -> Result<ProfileDto, String> {
        let mut profiles = self.profiles.clone();
        let profile = profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or_else(|| "Host profile was not found".to_owned())?;
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 80 {
            return Err("Connection name must contain between 1 and 80 characters".to_owned());
        }
        profile.label = label.to_owned();
        let renamed = profile.clone();
        self.persist_profiles(&profiles)?;
        self.profiles = profiles;
        Ok(renamed)
    }

    fn persist_profiles(&self, profiles: &[ProfileDto]) -> Result<(), String> {
        let parent = self.path.parent().ok_or("Profile path is invalid")?;
        fs::create_dir_all(parent).map_err(|error| format!("Could not create Terminal data folder: {error}"))?;
        let bytes = serde_json::to_vec_pretty(&ProfileFile {
            version: 1,
            profiles: profiles.to_vec(),
        })
        .map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let write_result = (|| {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Could not save Host profiles: {error}"));
        }
        if let Err(error) = replace_file(&temporary, &self.path) {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Could not replace Host profiles: {error}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "shockingvrc-profile-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ))
    }

    fn sample_profile(id: &str) -> ProfileInputDto {
        ProfileInputDto {
            id: Some(id.to_owned()),
            label: "Host 12345678".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 4318,
            fingerprint: "ab".repeat(32),
            server_id: "12345678-host-id".to_owned(),
            connection_mode: "direct_only".to_owned(),
            rendezvous_url: String::new(),
            ice_server_urls: Vec::new(),
            force_relay: false,
        }
    }

    #[test]
    fn rename_changes_only_label_and_preserves_profile_id_and_saved_record() {
        let directory = test_dir();
        let mut store = ProfileStore::open(directory.clone()).unwrap();
        let id = Uuid::new_v4().to_string();
        let original = store.save(sample_profile(&id)).unwrap();

        let renamed = store.rename(&id, "  Nightmare — Main  ").unwrap();

        assert_eq!(renamed.id, original.id);
        assert_eq!(renamed.label, "Nightmare — Main");
        assert_eq!(renamed.fingerprint, original.fingerprint);
        assert_eq!(renamed.server_id, original.server_id);
        let reopened = ProfileStore::open(directory.clone()).unwrap();
        let loaded = reopened.get(&id).unwrap();
        assert_eq!(loaded.id, renamed.id);
        assert_eq!(loaded.label, renamed.label);
        assert_eq!(loaded.fingerprint, renamed.fingerprint);
        assert_eq!(loaded.server_id, renamed.server_id);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn rename_rejects_empty_or_oversized_alias_without_mutating_profile() {
        let directory = test_dir();
        let mut store = ProfileStore::open(directory.clone()).unwrap();
        let id = Uuid::new_v4().to_string();
        let original = store.save(sample_profile(&id)).unwrap();

        assert!(store.rename(&id, "  ").is_err());
        assert!(store.rename(&id, &"x".repeat(81)).is_err());
        assert_eq!(store.get(&id).unwrap().label, original.label);
        let _ = fs::remove_dir_all(directory);
    }
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both paths are valid, NUL-terminated UTF-16 buffers that remain alive for this call.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

pub fn normalize_profile(input: ProfileInputDto) -> Result<ProfileDto, String> {
    let label = input.label.trim().to_owned();
    let host = input.host.trim().trim_matches(['[', ']']).to_owned();
    if label.is_empty() || label.chars().count() > 80 {
        return Err("Profile name must contain between 1 and 80 characters".to_owned());
    }
    if host.is_empty()
        || host.chars().any(char::is_whitespace)
        || host.contains('/')
        || host.contains('@')
        || host.contains('#')
        || host.contains('?')
    {
        return Err("Enter a host name or IP address without a URL or path".to_owned());
    }
    if input.port == 0 {
        return Err("Port must be between 1 and 65535".to_owned());
    }
    let connection_mode = input.connection_mode.trim().to_ascii_lowercase();
    if !matches!(connection_mode.as_str(), "auto" | "direct_only" | "traversal_only") {
        return Err("Connection mode must be auto, direct_only, or traversal_only".to_owned());
    }
    let rendezvous_url = input.rendezvous_url.trim().to_owned();
    if !rendezvous_url.is_empty() {
        shocking_vrc_rtc::validate_signal_url(&rendezvous_url).map_err(|error| error.to_string())?;
    }
    if connection_mode == "traversal_only" && (rendezvous_url.is_empty() || input.server_id.trim().is_empty()) {
        return Err("Traversal-only profiles require a rendezvous URL and Host ID".to_owned());
    }
    for uri in &input.ice_server_urls {
        let uri = uri.trim();
        if uri.is_empty() || !(uri.starts_with("stun:") || uri.starts_with("stuns:") || uri.starts_with("turn:") || uri.starts_with("turns:")) {
            return Err("ICE server URIs must use stun:, stuns:, turn:, or turns:".to_owned());
        }
    }
    let fingerprint = crate::transport::canonical_fingerprint(&input.fingerprint)?;
    let id = input
        .id
        .filter(|id| Uuid::parse_str(id).is_ok())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    Ok(ProfileDto {
        id,
        label,
        host,
        port: input.port,
        fingerprint,
        server_id: input.server_id.trim().to_owned(),
        connection_mode,
        rendezvous_url,
        ice_server_urls: input.ice_server_urls.into_iter().map(|value| value.trim().to_owned()).filter(|value| !value.is_empty()).collect(),
        force_relay: input.force_relay,
    })
}

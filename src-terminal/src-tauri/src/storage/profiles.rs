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
        let mut profiles = match fs::read(&path) {
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
        let mut migrated_legacy_endpoint = false;
        for profile in &mut profiles {
            let normalized_ice_servers = shocking_vrc_rendezvous::protocol::normalize_ice_server_uris(&profile.ice_server_urls)
                .map_err(|_| format!("Could not read Terminal profiles: ICE server URIs are invalid in profile {}", profile.id))?;
            if normalized_ice_servers != profile.ice_server_urls {
                profile.ice_server_urls = normalized_ice_servers;
                migrated_legacy_endpoint = true;
            }
            if profile.connection_mode == "traversal_only"
                && profile.host.as_deref().is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
            {
                profile.host = None;
                profile.port = None;
                migrated_legacy_endpoint = true;
            }
            let generated = format!("Host {}", profile.server_id.chars().take(8).collect::<String>());
            if profile.label != generated {
                profile.label_is_custom = true;
            }
        }
        let store = Self { path, profiles };
        if migrated_legacy_endpoint {
            store.persist_profiles(&store.profiles)?;
        }
        Ok(store)
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
        let mut profile = normalize_profile(input)?;
        let mut profiles = self.profiles.clone();
        if let Some(existing) = profiles.iter_mut().find(|item| item.id == profile.id) {
            profile.label_is_custom = existing.label_is_custom;
            profile.host_display_name = existing.host_display_name.clone();
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

    pub fn set_host_display_name(&mut self, id: &str, display_name: Option<String>) -> Result<ProfileDto, String> {
        let mut profiles = self.profiles.clone();
        let profile = profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or_else(|| "Host profile was not found".to_owned())?;
        profile.host_display_name = display_name;
        let updated = profile.clone();
        self.persist_profiles(&profiles)?;
        self.profiles = profiles;
        Ok(updated)
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
        profile.label_is_custom = true;
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

    #[test]
    fn opening_legacy_profiles_distinguishes_generated_labels_from_user_aliases() {
        let directory = test_dir();
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(PROFILE_FILE);
        let profiles = serde_json::json!({
            "version": 1,
            "profiles": [
                {
                    "id": Uuid::new_v4().to_string(), "label": "Host 12345678", "host": "127.0.0.1", "port": 4318,
                    "fingerprint": "ab".repeat(32), "server_id": "12345678-host-id", "connection_mode": "direct_only",
                    "rendezvous_url": "", "ice_server_urls": [], "force_relay": false
                },
                {
                    "id": Uuid::new_v4().to_string(), "label": "Nightmare — Main", "host": "127.0.0.1", "port": 4318,
                    "fingerprint": "cd".repeat(32), "server_id": "abcdefgh-host-id", "connection_mode": "direct_only",
                    "rendezvous_url": "", "ice_server_urls": [], "force_relay": false
                }
            ]
        });
        fs::write(&path, serde_json::to_vec(&profiles).unwrap()).unwrap();

        let store = ProfileStore::open(directory.clone()).unwrap();
        let profiles = store.list();
        assert!(!profiles[0].label_is_custom);
        assert!(profiles[1].label_is_custom);
        assert_eq!(profiles[0].host_display_name, None);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn traversal_only_profile_migrates_legacy_localhost_placeholder_without_changing_identity() {
        let directory = test_dir();
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(PROFILE_FILE);
        let id = Uuid::new_v4().to_string();
        let legacy = serde_json::json!({
            "version": 1,
            "profiles": [{
                "id": id,
                "label": "Host abcdef12",
                "host": "localhost",
                "port": 4318,
                "fingerprint": "ab".repeat(32),
                "server_id": "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwxyz23",
                "connection_mode": "traversal_only",
                "rendezvous_url": "wss://rendezvous.example.test/api/signal/v1",
                "ice_server_urls": [],
                "force_relay": false
            }]
        });
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let store = ProfileStore::open(directory.clone()).unwrap();
        let migrated = store.get(&id).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();

        assert_ne!(saved["profiles"][0].get("host"), Some(&serde_json::json!("localhost")));
        assert_eq!(migrated.id, id);
        assert_eq!(migrated.server_id, "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwxyz23");
        assert_eq!(migrated.fingerprint, "ab".repeat(32));
        assert_eq!(migrated.connection_mode, "traversal_only");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn traversal_only_profile_can_be_normalized_without_a_direct_endpoint() {
        let mut input = sample_profile("ice-only");
        input.host.clear();
        input.port = 0;
        input.connection_mode = "traversal_only".to_owned();
        input.rendezvous_url = "wss://rendezvous.example.test/api/signal/v1".to_owned();

        let profile = normalize_profile(input).expect("ICE-only profile should not require a direct endpoint");
        let serialized = serde_json::to_value(profile).unwrap();

        assert!(serialized.get("host").is_none());
        assert!(serialized.get("port").is_none());
    }

    #[test]
    fn profile_custom_ice_servers_are_normalized_deduplicated_and_support_ipv6() {
        let mut input = sample_profile("ice-custom");
        input.ice_server_urls = vec![
            " STUN:Example.test ".to_owned(),
            "stun:example.test:3478".to_owned(),
            "turn:[2001:db8::1]?transport=udp".to_owned(),
        ];
        let profile = normalize_profile(input).expect("valid IPv4/IPv6 endpoint forms");
        assert_eq!(profile.ice_server_urls, [
            "stun:example.test:3478",
            "turn:[2001:db8::1]:3478?transport=udp",
        ]);

        let mut input = sample_profile("ice-userinfo");
        input.ice_server_urls = vec!["turn:user:password@relay.example.test".to_owned()];
        assert!(normalize_profile(input).is_err(), "credentials must not be persisted in a URI");
    }

    #[test]
    fn pairing_profile_save_and_host_name_cache_preserve_a_local_alias() {
        let directory = test_dir();
        let mut store = ProfileStore::open(directory.clone()).unwrap();
        let id = Uuid::new_v4().to_string();
        let original = store.save(sample_profile(&id)).unwrap();
        let alias = store.rename(&id, "  Host 12345678  ").unwrap();
        assert!(alias.label_is_custom, "an explicit alias can match the generated fallback");

        let updated = store.save(ProfileInputDto {
            id: Some(id.clone()), label: alias.label.clone(), host: "example.test".to_owned(), port: 4318,
            fingerprint: alias.fingerprint.clone(), server_id: alias.server_id.clone(),
            connection_mode: alias.connection_mode.clone(), rendezvous_url: alias.rendezvous_url.clone(),
            ice_server_urls: alias.ice_server_urls.clone(), force_relay: alias.force_relay,
        }).unwrap();
        let cached = store.set_host_display_name(&id, Some("My Coyote".to_owned())).unwrap();
        assert_eq!(cached.label, "Host 12345678");
        assert!(cached.label_is_custom);
        assert_eq!(cached.host_display_name.as_deref(), Some("My Coyote"));
        assert_eq!(cached.id, original.id);
        assert_eq!(cached.fingerprint, original.fingerprint);
        assert_eq!(cached.server_id, original.server_id);

        let reopened = ProfileStore::open(directory.clone()).unwrap();
        let loaded = reopened.get(&id).unwrap();
        assert!(loaded.label_is_custom);
        assert_eq!(loaded.host_display_name.as_deref(), Some("My Coyote"));
        assert_eq!(updated.host.as_deref(), Some("example.test"));
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
    let connection_mode = input.connection_mode.trim().to_ascii_lowercase();
    if !matches!(connection_mode.as_str(), "auto" | "direct_only" | "traversal_only") {
        return Err("Connection mode must be auto, direct_only, or traversal_only".to_owned());
    }
    if connection_mode != "traversal_only"
        && (host.is_empty()
            || host.chars().any(char::is_whitespace)
            || host.contains('/')
            || host.contains('@')
            || host.contains('#')
            || host.contains('?'))
    {
        return Err("Enter a host name or IP address without a URL or path".to_owned());
    }
    if connection_mode != "traversal_only" && input.port == 0 {
        return Err("Port must be between 1 and 65535".to_owned());
    }
    let rendezvous_url = input.rendezvous_url.trim().to_owned();
    if !rendezvous_url.is_empty() {
        shocking_vrc_rtc::validate_signal_url(&rendezvous_url).map_err(|error| error.to_string())?;
    }
    if connection_mode == "traversal_only" && (rendezvous_url.is_empty() || input.server_id.trim().is_empty()) {
        return Err("Traversal-only profiles require a rendezvous URL and Host ID".to_owned());
    }
    if connection_mode == "auto" && host.is_empty() && (rendezvous_url.is_empty() || input.server_id.trim().is_empty()) {
        return Err("Profiles without a Direct endpoint require a rendezvous URL and Host ID".to_owned());
    }
    if connection_mode == "direct_only" && host.is_empty() {
        return Err("Direct-only profiles require a Direct Host endpoint".to_owned());
    }
    let ice_server_urls = shocking_vrc_rendezvous::protocol::normalize_ice_server_uris(&input.ice_server_urls)
        .map_err(|_| "ICE server URIs must use valid stun:, stuns:, turn:, or turns: URIs without embedded credentials".to_owned())?;
    let fingerprint = crate::transport::canonical_fingerprint(&input.fingerprint)?;
    let id = input
        .id
        .filter(|id| Uuid::parse_str(id).is_ok())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    Ok(ProfileDto {
        id,
        label,
        label_is_custom: false,
        host_display_name: None,
        host: (connection_mode != "traversal_only" && !host.is_empty()).then_some(host),
        port: (connection_mode != "traversal_only" && input.port > 0).then_some(input.port),
        fingerprint,
        server_id: input.server_id.trim().to_owned(),
        connection_mode,
        rendezvous_url,
        ice_server_urls,
        force_relay: input.force_relay,
    })
}

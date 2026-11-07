use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::{fs, fs::OpenOptions};

use egui_kittest::Harness;
use shocking_vrc_gui::app::{Ctx, UiState};
use shocking_vrc_gui::bridge::{Bridge, Snapshot};
use shocking_vrc_gui::fixtures;
use shocking_vrc_gui::model::Section;
use shocking_vrc_gui::remote_adapter::BridgeRemoteBackend;
use shocking_vrc_gui::theme::{self, Palette, ThemeState};
use shocking_vrc_gui::views;
use shocking_vrc_remote::{HostRemoteController, RemoteAccessSettings};

const W: f32 = 1280.0;
const H: f32 = 900.0;
const RENDER_CHILD_ENV: &str = "SHOCKINGVRC_ISOLATED_RENDER_CHILD";
const RENDER_APPDATA_ENV: &str = "SHOCKINGVRC_RENDER_APPDATA";
const RENDER_ROOT_ENV: &str = "SHOCKINGVRC_RENDER_ROOT";
const RENDER_TEMP_ENV: &str = "SHOCKINGVRC_RENDER_TEMP_ROOT";
const RENDER_NONCE_ENV: &str = "SHOCKINGVRC_RENDER_NONCE";
const RENDER_ORIGINAL_APPDATA_ENV: &str = "SHOCKINGVRC_RENDER_ORIGINAL_APPDATA";
const ISOLATION_MARKER: &str = ".shockingvrc-ux22-render-fixture";
const UNSET_APPDATA: &str = "__SHOCKINGVRC_APPDATA_UNSET__";

struct Scene {
    bridge: Bridge,
    _rx: tokio::sync::mpsc::UnboundedReceiver<shocking_vrc_gui::bridge::Command>,
    snap: Snapshot,
    log: shocking_vrc_app::logger::LogBuffer,
    remote: Arc<HostRemoteController>,
}

impl Scene {
    fn new(snap: Snapshot) -> Self {
        let (bridge, rx) = fixtures::detached_bridge();
        bridge.set_snapshot(snap.clone());
        fixtures::fill_shared(&bridge.shared);

        let log: shocking_vrc_app::logger::LogBuffer = Default::default();
        if let Ok(mut b) = log.lock() {
            for line in [
                "[INFO ] [cli] Ready.",
                "[INFO ] [ble] Device slot created for AA:BB:CC:DD:EE:FF",
                "[WARN ] [ble] Device 11:22:33:44:55:66 disconnected — coordinator will reconnect",
                "[INFO ] [osc] Port 33777 selected (OSC UDP + OSCQuery HTTP).",
                "[ERROR] [config] Save failed: permission denied",
                "[DEBUG] [cli] Skipped 3 zone events (lagged)",
            ] {
                b.push_back(line.to_string());
            }
        }

        let remote_backend = BridgeRemoteBackend::new(bridge.clone(), Arc::clone(&log), None);
        let remote = HostRemoteController::new(remote_backend, RemoteAccessSettings::default());

        Self {
            bridge,
            _rx: rx,
            snap,
            log,
            remote,
        }
    }

    fn render_section(&self, section: Section, theme_id: &str) -> image::RgbaImage {
        self.render_section_sized(section, theme_id, egui::vec2(W, H))
    }

    fn render_section_sized(&self, section: Section, theme_id: &str, size: egui::Vec2) -> image::RgbaImage {
        let theme = ThemeState::new(theme_id);
        let pal: Palette = *theme.palette();
        let mut state = UiState::default_for(section);

        let mut harness = Harness::builder().with_size(size).wgpu().build_ui(|ui| {
            theme::install_style(ui.ctx(), &pal);
            ui.painter()
                .rect_filled(ui.max_rect(), theme::widgets::SQUARE, pal.base);
            let mut cx = Ctx {
                pal: &pal,
                snap: &self.snap,
                bridge: &self.bridge,
                ui_state: &mut state,
                log: &self.log,
                remote: &self.remote,
            };
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match section {
                    Section::Status => views::status::show(ui, &mut cx),
                    Section::Zones => views::zones::show(ui, &mut cx),
                    Section::Channels => views::channels::show(ui, &mut cx),
                    Section::Tuning => views::tuning::show(ui, &mut cx),
                    Section::Modulation => views::modulation::show(ui, &mut cx),
                    Section::Log => views::log::show(ui, &mut cx),
                    Section::Presets => views::presets::show(ui, &mut cx),
                    Section::Setup => {
                        views::setup::show(ui, &mut cx);
                    }
                    Section::Alarm => views::alarm::show(ui, &mut cx),
                });
        });

        harness.run();
        harness.render().expect("render")
    }
}

#[test]
fn ux21_capture_host_setup_review_screenshots() {
    if std::env::var_os(RENDER_CHILD_ENV).is_some() {
        with_validated_child(&ChildIsolationInputs::from_process(), |isolation| {
            // This is deliberately the first operation in child mode: malformed or
            // incomplete child configuration must fail before Scene constructs a
            // HostRemoteController, and before initialize can load identity/trust.
            isolation.revalidate_current_process()?;
            let scene = Scene::new(Snapshot::empty_for_test());
            let size = egui::vec2(1280.0, 1400.0);

            let setup = scene.render_section_sized(Section::Setup, "terminal", size);
            assert_not_blank(&setup, "UX-2.1 Host Setup");
            save(&setup, "ux22-host-setup-limits");

            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("isolated screenshot runtime");
            isolation.revalidate_current_process()?;
            runtime.block_on(scene.remote.initialize());
            if scene.remote.status().paired_terminals.len() != 1
                || scene.remote.status().paired_terminals[0].friendly_name.as_deref() != Some("Review Laptop")
            {
                return Err("isolated screenshot Host did not load the synthetic Terminal fixture");
            }

            // Initialization is expected to create its DPAPI blob beside the
            // fixture, under the already-validated test APPDATA only.
            let host_dir = isolation.app_data.join("ShockingVRC");
            if !host_dir.join("host_identity.dpapi").is_file() {
                return Err("isolated Host identity was not created under test APPDATA");
            }
            if fs::read(host_dir.join("trusted_terminals.json"))
                .map_err(|_| "could not verify isolated trust fixture")?
                != synthetic_trust_fixture()
            {
                return Err("isolated trust fixture changed during screenshot initialization");
            }
            if host_dir.join("rendezvous_registration.dpapi").exists() {
                return Err("screenshot test unexpectedly found a rendezvous credential in test APPDATA");
            }

            let paired = scene.render_section_sized(Section::Setup, "terminal", size);
            assert_not_blank(&paired, "UX-2.1 Host paired devices");
            save(&paired, "ux22-host-paired-device-description");
            runtime.block_on(scene.remote.shutdown());
            Ok(())
        })
        .unwrap_or_else(|error| panic!("refusing isolated screenshot child: {error}"));
        return;
    }

    let original_app_data = std::env::var_os("APPDATA").map(PathBuf::from);
    let cleanup = TestAppData::create(original_app_data).expect("create safe isolated Host test APPDATA");
    let child_inputs = cleanup.child_inputs();
    validate_child_isolation(&child_inputs).expect("parent-created screenshot fixture must validate");
    let shots = cleanup.root.join("screenshots");
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("ux21_capture_host_setup_review_screenshots")
        .arg("--nocapture")
        .env("APPDATA", &cleanup.app_data)
        .env(RENDER_APPDATA_ENV, &cleanup.app_data)
        .env(RENDER_ROOT_ENV, &cleanup.root)
        .env(RENDER_TEMP_ENV, &cleanup.temp_root)
        .env(RENDER_NONCE_ENV, &cleanup.nonce)
        .env(RENDER_ORIGINAL_APPDATA_ENV, &cleanup.original_app_data_token)
        .env("SHOCKINGVRC_SHOTS", &shots)
        .env(RENDER_CHILD_ENV, "1")
        .status()
        .expect("launch isolated Host screenshot child");
    assert!(status.success(), "isolated Host screenshot child failed: {status}");
}

#[derive(Clone, Debug, Default)]
struct ChildIsolationInputs {
    child_flag: Option<OsString>,
    app_data: Option<PathBuf>,
    explicit_app_data: Option<PathBuf>,
    root: Option<PathBuf>,
    temp_root: Option<PathBuf>,
    nonce: Option<OsString>,
    original_app_data: Option<OsString>,
}

impl ChildIsolationInputs {
    fn from_process() -> Self {
        let path = |name: &str| std::env::var_os(name).map(PathBuf::from);
        Self {
            child_flag: std::env::var_os(RENDER_CHILD_ENV),
            app_data: path("APPDATA"),
            explicit_app_data: path(RENDER_APPDATA_ENV),
            root: path(RENDER_ROOT_ENV),
            temp_root: path(RENDER_TEMP_ENV),
            nonce: std::env::var_os(RENDER_NONCE_ENV),
            original_app_data: std::env::var_os(RENDER_ORIGINAL_APPDATA_ENV),
        }
    }
}

struct ValidatedIsolation {
    root: PathBuf,
    app_data: PathBuf,
    temp_root: PathBuf,
    nonce: String,
    original_app_data: Option<PathBuf>,
}

impl ValidatedIsolation {
    fn revalidate_current_process(&self) -> Result<(), &'static str> {
        let current = validate_child_isolation(&ChildIsolationInputs::from_process())?;
        if !same_path(&self.root, &current.root)
            || !same_path(&self.app_data, &current.app_data)
            || !same_path(&self.temp_root, &current.temp_root)
            || self.nonce != current.nonce
            || self.original_app_data.as_deref() != current.original_app_data.as_deref()
        {
            return Err("screenshot isolation changed during the test");
        }
        Ok(())
    }
}

fn with_validated_child<T>(
    inputs: &ChildIsolationInputs,
    action: impl FnOnce(&ValidatedIsolation) -> Result<T, &'static str>,
) -> Result<T, &'static str> {
    let isolation = validate_child_isolation(inputs)?;
    action(&isolation)
}

fn validate_child_isolation(inputs: &ChildIsolationInputs) -> Result<ValidatedIsolation, &'static str> {
    if inputs.child_flag.as_deref() != Some(OsStr::new("1")) {
        return Err("child mode is not explicitly enabled");
    }
    let app_data = inputs.app_data.as_deref().ok_or("APPDATA is missing")?;
    let explicit_app_data = inputs
        .explicit_app_data
        .as_deref()
        .ok_or("explicit test APPDATA is missing")?;
    let root = inputs.root.as_deref().ok_or("test root is missing")?;
    let configured_temp = inputs.temp_root.as_deref().ok_or("test temporary root is missing")?;
    let nonce = inputs
        .nonce
        .as_deref()
        .and_then(OsStr::to_str)
        .ok_or("test fixture nonce is missing or invalid")?;
    let parsed_nonce = uuid::Uuid::parse_str(nonce).map_err(|_| "test fixture nonce is invalid")?;
    if parsed_nonce.to_string() != nonce {
        return Err("test fixture nonce is not canonical");
    }
    let original_token = inputs
        .original_app_data
        .as_deref()
        .ok_or("original APPDATA separation marker is missing")?;
    let original_app_data = if original_token == OsStr::new(UNSET_APPDATA) {
        None
    } else {
        Some(canonical_existing_directory(Path::new(original_token))?)
    };

    let runtime_temp = canonical_existing_directory(&std::env::temp_dir())?;
    let configured_temp = canonical_existing_directory(configured_temp)?;
    if !same_path(&runtime_temp, &configured_temp) {
        return Err("test temporary directory configuration changed");
    }
    let root = canonical_existing_directory(root)?;
    if !same_path(root.parent().ok_or("test root has no parent")?, &configured_temp) {
        return Err("test root is not a direct child of the canonical temporary directory");
    }
    if !path_is_within(&root, &configured_temp) {
        return Err("test root is not contained by the canonical temporary directory");
    }
    if let Some(original) = &original_app_data {
        if paths_overlap(&root, original) || paths_overlap(&configured_temp, original) {
            return Err("test storage overlaps the original APPDATA directory");
        }
    }

    let expected_app_data = root.join("appdata");
    let app_data = canonical_existing_directory(app_data)?;
    let explicit_app_data = canonical_existing_directory(explicit_app_data)?;
    let expected_app_data = canonical_existing_directory(&expected_app_data)?;
    if !same_path(&app_data, &explicit_app_data) || !same_path(&app_data, &expected_app_data) {
        return Err("APPDATA does not match the explicit test-owned directory");
    }
    let host_dir = canonical_existing_directory(&app_data.join("ShockingVRC"))?;
    if !same_path(&host_dir, &root.join("appdata").join("ShockingVRC")) {
        return Err("Host storage is outside the validated test directory");
    }

    let marker_path = root.join(ISOLATION_MARKER);
    if read_checked_file(&marker_path)? != marker_bytes(nonce) {
        return Err("test ownership marker is absent or invalid");
    }
    if read_checked_file(&host_dir.join("trusted_terminals.json"))? != synthetic_trust_fixture() {
        return Err("synthetic trusted Terminal fixture is absent or invalid");
    }

    Ok(ValidatedIsolation {
        root,
        app_data,
        temp_root: configured_temp,
        nonce: nonce.to_owned(),
        original_app_data,
    })
}

struct TestAppData {
    root: PathBuf,
    app_data: PathBuf,
    temp_root: PathBuf,
    nonce: String,
    original_app_data: Option<PathBuf>,
    original_app_data_token: OsString,
    armed: bool,
}

impl TestAppData {
    fn create(original_app_data: Option<PathBuf>) -> Result<Self, &'static str> {
        let temp_root = canonical_existing_directory(&std::env::temp_dir())?;
        let original_app_data = original_app_data
            .as_deref()
            .map(canonical_existing_directory)
            .transpose()?;
        if original_app_data
            .as_ref()
            .is_some_and(|original| paths_overlap(&temp_root, original))
        {
            return Err("the configured temporary directory overlaps original APPDATA");
        }

        let mut created = None;
        for _ in 0..4 {
            let nonce = uuid::Uuid::new_v4().to_string();
            let candidate = temp_root.join(format!("shockingvrc-ux22-render-{nonce}"));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    created = Some((candidate, nonce));
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err("could not create unique screenshot test directory"),
            }
        }
        let (candidate, nonce) = created.ok_or("could not allocate a unique screenshot test directory")?;
        let root = canonical_existing_directory(&candidate)?;
        if !same_path(root.parent().ok_or("test root has no parent")?, &temp_root) {
            let _ = fs::remove_dir(&candidate);
            return Err("created test root escaped the canonical temporary directory");
        }

        let marker_path = root.join(ISOLATION_MARKER);
        if write_new_file(&marker_path, &marker_bytes(&nonce)).is_err() {
            let _ = fs::remove_file(&marker_path);
            let _ = fs::remove_dir(&root);
            return Err("could not create screenshot fixture ownership marker");
        }
        let original_app_data_token = original_app_data
            .as_ref()
            .map(|path| path.as_os_str().to_owned())
            .unwrap_or_else(|| OsString::from(UNSET_APPDATA));
        let owner = Self {
            app_data: root.join("appdata"),
            root,
            temp_root,
            nonce,
            original_app_data,
            original_app_data_token,
            armed: true,
        };
        fs::create_dir(&owner.app_data).map_err(|_| "could not create isolated test APPDATA")?;
        let host_dir = owner.app_data.join("ShockingVRC");
        fs::create_dir(&host_dir).map_err(|_| "could not create isolated Host data directory")?;
        write_new_file(&host_dir.join("trusted_terminals.json"), &synthetic_trust_fixture())?;
        Ok(owner)
    }

    fn child_inputs(&self) -> ChildIsolationInputs {
        ChildIsolationInputs {
            child_flag: Some(OsString::from("1")),
            app_data: Some(self.app_data.clone()),
            explicit_app_data: Some(self.app_data.clone()),
            root: Some(self.root.clone()),
            temp_root: Some(self.temp_root.clone()),
            nonce: Some(OsString::from(&self.nonce)),
            original_app_data: Some(self.original_app_data_token.clone()),
        }
    }
}

impl Drop for TestAppData {
    fn drop(&mut self) {
        if !self.armed
            || !same_path(self.root.parent().unwrap_or(Path::new("")), &self.temp_root)
            || self
                .original_app_data
                .as_ref()
                .is_some_and(|original| paths_overlap(&self.root, original))
        {
            return;
        }
        let Ok(root) = canonical_existing_directory(&self.root) else {
            return;
        };
        if !same_path(&root, &self.root)
            || read_checked_file(&root.join(ISOLATION_MARKER)).ok().as_deref()
                != Some(marker_bytes(&self.nonce).as_slice())
        {
            return;
        }
        let _ = fs::remove_dir_all(&root);
    }
}

fn synthetic_trust_fixture() -> Vec<u8> {
    let public_key = [7_u8; 32];
    let device_id = shocking_vrc_rendezvous::pairing::device_id_for_public_key(&public_key)
        .expect("derive deterministic screenshot Terminal id");
    let mut terminals = serde_json::Map::new();
    terminals.insert(
        device_id.clone(),
        serde_json::json!({
            "device_id": device_id,
            "public_key": shocking_vrc_rendezvous::pairing::encode(&public_key),
            "paired_at": 1792224000,
            "friendly_name": "Review Laptop"
        }),
    );
    serde_json::to_vec(&serde_json::json!({ "version": 2, "terminals": terminals, "invitations": {} }))
        .expect("serialize isolated trusted Terminal fixture")
}

fn marker_bytes(nonce: &str) -> Vec<u8> {
    format!("SHOCKINGVRC-UX22-RENDER-FIXTURE-V1\n{nonce}\n").into_bytes()
}

fn write_new_file(path: &Path, contents: &[u8]) -> Result<(), &'static str> {
    let parent = path.parent().ok_or("fixture path has no parent")?;
    check_no_reparse_points(parent)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "could not create test fixture file")?;
    file.write_all(contents)
        .map_err(|_| "could not write test fixture file")?;
    file.sync_all().map_err(|_| "could not flush test fixture file")
}

fn read_checked_file(path: &Path) -> Result<Vec<u8>, &'static str> {
    check_no_reparse_points(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "test fixture file is missing")?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err("test fixture path is not a regular file");
    }
    fs::read(path).map_err(|_| "could not read test fixture file")
}

fn canonical_existing_directory(path: &Path) -> Result<PathBuf, &'static str> {
    validate_absolute_path(path)?;
    check_no_reparse_points(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "test directory is missing")?;
    if !metadata.is_dir() || is_reparse_point(&metadata) {
        return Err("test path is not a plain directory");
    }
    let canonical = fs::canonicalize(path).map_err(|_| "could not canonicalize test directory")?;
    validate_absolute_path(&canonical)?;
    check_no_reparse_points(&canonical)?;
    Ok(canonical)
}

fn validate_absolute_path(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("test path must be absolute and normalized");
    }
    Ok(())
}

fn check_no_reparse_points(path: &Path) -> Result<(), &'static str> {
    validate_absolute_path(path)?;
    for ancestor in path.ancestors().take_while(|ancestor| !ancestor.as_os_str().is_empty()) {
        let metadata = fs::symlink_metadata(ancestor).map_err(|_| "test path component is missing")?;
        if is_reparse_point(&metadata) {
            return Err("test path contains a symbolic link or reparse point");
        }
    }
    Ok(())
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    path_is_within(left, right) || path_is_within(right, left)
}

fn path_is_within(path: &Path, parent: &Path) -> bool {
    let path = path_key(path);
    let parent = path_key(parent);
    path == parent
        || path
            .strip_prefix(&(parent.trim_end_matches('/').to_owned() + "/"))
            .is_some()
}

fn path_key(path: &Path) -> String {
    let mut key = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        key = key.to_lowercase();
    }
    while key.len() > 1 && key.ends_with('/') {
        key.pop();
    }
    key
}

#[cfg(test)]
mod isolation_tests {
    use super::*;
    use std::cell::Cell;

    fn assert_rejected_before_action(inputs: &ChildIsolationInputs) {
        let action_called = Cell::new(false);
        let result = with_validated_child(inputs, |_| {
            action_called.set(true);
            Ok(())
        });
        assert!(result.is_err(), "invalid child configuration must be rejected");
        assert!(
            !action_called.get(),
            "identity/trust initialization boundary ran after rejection"
        );
    }

    #[test]
    fn render_isolation_accepts_parent_created_child_fixture() {
        let owner = TestAppData::create(None).expect("safe synthetic test tree");
        let proof = with_validated_child(&owner.child_inputs(), |proof| {
            assert!(same_path(&proof.root, &owner.root));
            assert!(same_path(&proof.app_data, &owner.app_data));
            assert!(same_path(&proof.temp_root, &owner.temp_root));
            assert!(proof.original_app_data.is_none());
            Ok(())
        });
        assert!(proof.is_ok(), "valid synthetic parent/child environment should pass");
    }

    #[test]
    fn forced_child_without_isolated_environment_fails_before_initialization() {
        let inputs = ChildIsolationInputs {
            child_flag: Some(OsString::from("1")),
            ..ChildIsolationInputs::default()
        };
        assert_rejected_before_action(&inputs);
    }

    #[test]
    fn missing_fixture_fails_before_initialization() {
        let owner = TestAppData::create(None).expect("safe synthetic test tree");
        fs::remove_file(owner.app_data.join("ShockingVRC").join("trusted_terminals.json"))
            .expect("remove synthetic fixture in test-only directory");
        assert_rejected_before_action(&owner.child_inputs());
    }

    #[test]
    fn wrong_fixture_fails_before_initialization() {
        let owner = TestAppData::create(None).expect("safe synthetic test tree");
        fs::write(owner.app_data.join("ShockingVRC").join("trusted_terminals.json"), b"{}")
            .expect("replace fixture in test-only directory");
        assert_rejected_before_action(&owner.child_inputs());
    }

    #[test]
    fn mismatched_explicit_appdata_fails_before_initialization() {
        let owner = TestAppData::create(None).expect("safe synthetic test tree");
        let other = TestAppData::create(None).expect("second safe synthetic test tree");
        let mut inputs = owner.child_inputs();
        inputs.explicit_app_data = Some(other.app_data.clone());
        assert_rejected_before_action(&inputs);
    }

    #[test]
    fn relative_appdata_fails_before_initialization() {
        let owner = TestAppData::create(None).expect("safe synthetic test tree");
        let mut inputs = owner.child_inputs();
        inputs.app_data = Some(PathBuf::from("Roaming"));
        inputs.explicit_app_data = Some(PathBuf::from("Roaming"));
        assert_rejected_before_action(&inputs);
    }
}

impl Scene {
    fn render_overlay(&self, open: impl Fn(&mut UiState)) -> image::RgbaImage {
        let theme = ThemeState::new("terminal");
        let pal: Palette = *theme.palette();
        let mut state = UiState::default_for(Section::Status);
        open(&mut state);

        let mut harness = Harness::builder().with_size(egui::vec2(W, H)).wgpu().build_ui(|ui| {
            theme::install_style(ui.ctx(), &pal);
            ui.painter()
                .rect_filled(ui.max_rect(), theme::widgets::SQUARE, pal.base);
            let ctx = ui.ctx().clone();
            let mut cx = Ctx {
                pal: &pal,
                snap: &self.snap,
                bridge: &self.bridge,
                ui_state: &mut state,
                log: &self.log,
                remote: &self.remote,
            };
            views::status::show(ui, &mut cx);

            if cx.ui_state.show_zone_presets {
                shocking_vrc_gui::overlays::zone_presets::show(&ctx, &mut cx);
            }
            if cx.ui_state.show_function_picker {
                shocking_vrc_gui::overlays::function_picker::show(&ctx, &mut cx);
            }
            if cx.ui_state.preset_save.is_some() {
                shocking_vrc_gui::overlays::preset_save::show(&ctx, &mut cx);
            }
            shocking_vrc_gui::overlays::confirm::show(&ctx, &mut cx);
            shocking_vrc_gui::overlays::update::show(&ctx, &mut cx);
            if cx.snap.alarm_status.phase.is_ringing() {
                shocking_vrc_gui::overlays::alarm_ring::show(&ctx, &mut cx);
            }
        });

        harness.run();
        harness.render().expect("render")
    }
}

fn out_dir() -> Option<std::path::PathBuf> {
    let dir = std::env::var("SHOCKINGVRC_SHOTS").ok()?;
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn save(img: &image::RgbaImage, name: &str) {
    if let Some(dir) = out_dir() {
        let path = dir.join(format!("{name}.png"));
        if let Err(e) = img.save(&path) {
            eprintln!("could not write {}: {e}", path.display());
        }
    }
}

fn assert_not_blank(img: &image::RgbaImage, what: &str) {
    let first = img.get_pixel(0, 0);
    let distinct = img.pixels().any(|p| p != first);
    assert!(distinct, "{what} rendered as a flat, empty frame");
}

#[test]
fn every_section_renders() {
    let scene = Scene::new(fixtures::demo_snapshot());
    for section in Section::ALL {
        let img = scene.render_section(section, "terminal");
        assert_not_blank(&img, section.title());
        save(&img, &format!("section-{}", section.title().to_lowercase()));
    }
}

#[test]
fn every_section_renders_with_nothing_connected() {
    let mut snap = Snapshot::empty_for_test();
    snap.osc_port = 9001;
    let scene = Scene::new(snap);
    for section in Section::ALL {
        let img = scene.render_section(section, "terminal");
        assert_not_blank(&img, section.title());
    }
    save(&scene.render_section(Section::Status, "terminal"), "empty-status");
}

#[test]
fn every_palette_renders() {
    let scene = Scene::new(fixtures::demo_snapshot());
    for t in theme::THEMES {
        let img = scene.render_section(Section::Status, t.id);
        assert_not_blank(&img, t.name);
        save(&img, &format!("palette-{}", t.id));
    }
}

#[test]
fn a_ringing_alarm_renders() {
    let scene = Scene::new(fixtures::ringing_snapshot());
    let img = scene.render_section(Section::Alarm, "terminal");
    assert_not_blank(&img, "ringing alarm");
    save(&img, "alarm-ringing");
}

type OverlayCase = (&'static str, fn(&mut UiState));

#[test]
fn every_overlay_renders() {
    let scene = Scene::new(fixtures::demo_snapshot());
    let cases: [OverlayCase; 5] = [
        ("zone-presets", |s| s.show_zone_presets = true),
        ("function-picker", |s| s.show_function_picker = true),
        ("preset-save", |s| {
            s.preset_save = Some(shocking_vrc_gui::app::PresetSaveDialog {
                channel: shocking_vrc_gui::model::Channel::A,
                name: String::new(),
                author: "Amigo_FPS".into(),
            })
        }),
        ("confirm-preset-delete", |s| s.preset_delete_confirm = Some(0)),
        ("confirm-zoneset-delete", |s| s.zone_preset_delete_confirm = Some(0)),
    ];

    for (name, open) in cases {
        let img = scene.render_overlay(open);
        assert_not_blank(&img, name);
        save(&img, &format!("overlay-{name}"));
    }
}

#[test]
fn the_alarm_ring_renders() {
    let scene = Scene::new(fixtures::ringing_snapshot());
    let img = scene.render_overlay(|_| {});
    assert_not_blank(&img, "alarm ring");
    save(&img, "overlay-alarm-ring");
}

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use shocking_vrc_app::app_state::{AppState, DeviceSlotInfo};
use shocking_vrc_app::{device_config, prefs, presets as preset_store, zone_presets};
use shocking_vrc_core::cli::{
    AggregationMode, AlarmChannels, AlarmConfig, AlarmStatus, ChannelConfig, CliConfig, CliStatus,
    ContactMode, MotionNorms, PowerLimits, UkfConfig, ZoneEntry, ZoneId,
};
use shocking_vrc_core::modulation::config::ModulationConfig;
use shocking_vrc_core::presets::{ChannelPreset, PresetEntry, ZonePresetEntry};
use shocking_vrc_core::{DownloadProgress, ReleaseInfo, ZoneEvent};
use shocking_vrc_remote::{RemoteChannel, RemoteStrengthPolicy};

use crate::model::{Channel, ModKind};

#[derive(Clone)]
pub struct Snapshot {
    pub config: CliConfig,
    pub staged_preview_only: bool,
    pub status: CliStatus,
    pub alarm: AlarmConfig,
    pub alarm_status: AlarmStatus,
    pub avatar_zones: Vec<ZoneEvent>,
    pub vrchat_found: bool,
    pub osc_port: u16,
    pub oscquery_port: Option<u16>,
    pub devices: Vec<DeviceSlotInfo>,
    pub active_device: usize,
    pub revision: u64,
    pub published_at: Instant,
}

impl Snapshot {
    pub fn empty_for_test() -> Self {
        Self::empty()
    }

    fn empty() -> Self {
        Self {
            config: CliConfig::default(),
            staged_preview_only: true,
            status: empty_status(),
            alarm: AlarmConfig::default(),
            alarm_status: AlarmStatus::default(),
            avatar_zones: Vec::new(),
            vrchat_found: false,
            osc_port: 0,
            oscquery_port: None,
            devices: Vec::new(),
            active_device: 0,
            revision: 0,
            published_at: Instant::now(),
        }
    }

    pub fn channel(&self, ch: Channel) -> &ChannelConfig {
        match ch {
            Channel::A => &self.config.channel_a,
            Channel::B => &self.config.channel_b,
        }
    }

    pub fn channel_status(&self, ch: Channel) -> &shocking_vrc_core::cli::ChannelStatus {
        match ch {
            Channel::A => &self.status.channel_a,
            Channel::B => &self.status.channel_b,
        }
    }

    pub fn active(&self) -> Option<&DeviceSlotInfo> {
        self.devices.get(self.active_device)
    }

    pub fn has_device(&self) -> bool {
        !self.devices.is_empty()
    }

    pub fn mod_slot(&self, ch: Channel, kind: ModKind, seg: usize) -> &Option<ModulationConfig> {
        let c = self.channel(ch);
        match kind {
            ModKind::Freq => &c.freq_modulation[seg],
            ModKind::Intensity => &c.intensity_modulation[seg],
        }
    }
}

pub fn empty_status() -> CliStatus {
    let ch = || shocking_vrc_core::cli::ChannelStatus {
        raw_level: 0.0,
        strength: 0,
        frequency: [0; 4],
        active_zones: Vec::new(),
        kinematics: Default::default(),
    };
    CliStatus {
        channel_a: ch(),
        channel_b: ch(),
        device_connected: false,
    }
}

#[derive(Clone, Default)]
pub struct PresetState {
    pub entries: Vec<PresetEntry>,
    pub loading: bool,
    pub error: Option<String>,
    pub source: String,
}

#[derive(Clone, Default)]
pub struct ZonePresetState {
    pub entries: Vec<ZonePresetEntry>,
}

#[derive(Clone, Default)]
pub struct UpdateState {
    pub available: Option<ReleaseInfo>,
    pub checking: bool,
    pub downloading: bool,
    pub progress: Option<DownloadProgress>,
    pub error: Option<String>,
    pub pending: Option<(std::path::PathBuf, Vec<String>)>,
    pub dismissed: bool,
    last_progress_at: Option<Instant>,
}

pub struct Shared {
    pub snapshot: RwLock<Snapshot>,
    pub presets: RwLock<PresetState>,
    pub zone_presets: RwLock<ZonePresetState>,
    pub update: RwLock<UpdateState>,
    pub toast: RwLock<Option<Toast>>,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

impl Shared {
    fn new() -> Self {
        Self {
            snapshot: RwLock::new(Snapshot::empty()),
            presets: RwLock::new(PresetState::default()),
            zone_presets: RwLock::new(ZonePresetState::default()),
            update: RwLock::new(UpdateState::default()),
            toast: RwLock::new(None),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|_| Snapshot::empty())
    }

    pub fn notify(&self, kind: ToastKind, text: impl Into<String>) {
        if let Ok(mut slot) = self.toast.write() {
            *slot = Some(Toast {
                text: text.into(),
                kind,
                at: Instant::now(),
            });
        }
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    SwitchDevice(usize),

    AddZone(Channel, ZoneId),
    AddAllAvatarZones(Channel),
    RemoveZone(Channel, usize),
    SetZoneMode(Channel, usize, ContactMode),
    SetZoneScale(Channel, usize, u8),
    SetZoneThresholdMin(Channel, usize, u8),
    SetZoneThresholdMax(Channel, usize, u8),

    SetFrequency(Channel, usize, u8),
    SetIntensity(Channel, usize, u8),
    SetLimitMin(Channel, u8),
    SetLimitMax(Channel, u8),
    SetPowerLimitPolicy(RemoteStrengthPolicy),
    SetAggregation(Channel, AggregationMode),
    SaveConfig,
    LoadConfig,

    SetUkf(UkfConfig),
    ResetUkf,
    SetNorms(MotionNorms),
    ResetNorms,

    ApplyModulation(Channel, ModKind, usize, Box<ModulationConfig>),
    ClearModulation(Channel, ModKind, usize),
    ClearChannelModulation(Channel),

    RefreshPresets,
    ApplyPreset(Channel, usize),
    SaveUserPreset {
        channel: Channel,
        name: String,
        author: String,
    },
    DeleteUserPreset(usize),

    ReloadZonePresets,
    ApplyZonePreset(usize),
    SaveZonePreset(String),
    DeleteZonePreset(usize),

    SetAlarmConfig(Box<AlarmConfig>),
    SetAlarmChannels(AlarmChannels),
    AlarmTest,
    AlarmStop,
    AlarmSnooze,

    SetAutoSave(bool),
    SetAlarmTabVisible(bool),

    CheckUpdate,
    StartUpdateDownload,
    SkipUpdateVersion,
    DismissUpdate,
    #[cfg(feature = "host-perf-harness")]
    StopPerformanceHarness,
}

impl Command {
    fn persists(&self) -> bool {
        matches!(
            self,
            Command::AddZone(..)
                | Command::AddAllAvatarZones(..)
                | Command::RemoveZone(..)
                | Command::SetZoneMode(..)
                | Command::SetZoneScale(..)
                | Command::SetZoneThresholdMin(..)
                | Command::SetZoneThresholdMax(..)
                | Command::SetFrequency(..)
                | Command::SetIntensity(..)
                | Command::SetLimitMin(..)
                | Command::SetLimitMax(..)
                | Command::SetAggregation(..)
                | Command::SetUkf(..)
                | Command::ResetUkf
                | Command::SetNorms(..)
                | Command::ResetNorms
                | Command::ApplyModulation(..)
                | Command::ClearModulation(..)
                | Command::ClearChannelModulation(..)
                | Command::ApplyPreset(..)
                | Command::ApplyZonePreset(..)
        )
    }
}

#[derive(Clone)]
pub struct Bridge {
    tx: UnboundedSender<Command>,
    pub shared: Arc<Shared>,
}

fn clamp_limits_to_ceiling(limits: &mut PowerLimits, cap: u8) -> bool {
    let cap = cap.min(200);
    let max = limits.max.min(cap);
    let min = limits.min.min(cap).min(max);
    if (min, max) == (limits.min, limits.max) {
        return false;
    }
    *limits = PowerLimits::new(min, max);
    true
}

fn bounded_limit_request(value: u8, cap: Option<u8>) -> u8 {
    value.min(cap.unwrap_or(200).min(200))
}

fn set_limit_min(limits: &mut PowerLimits, requested: u8, cap: Option<u8>) {
    let ceiling = cap.unwrap_or(200).min(200);
    let min = requested.min(ceiling);
    let max = limits.max.min(ceiling).max(min);
    *limits = PowerLimits::new(min, max);
}

fn set_limit_max(limits: &mut PowerLimits, requested: u8, cap: Option<u8>) {
    let max = bounded_limit_request(requested, cap);
    let min = if cap.is_some() {
        limits.min.min(max)
    } else {
        limits.min
    };
    *limits = PowerLimits::new(min, max);
}

impl Bridge {
    pub fn detached() -> (Self, UnboundedReceiver<Command>) {
        let (tx, rx) = unbounded_channel();
        (
            Self {
                tx,
                shared: Arc::new(Shared::new()),
            },
            rx,
        )
    }

    pub fn set_snapshot(&self, snap: Snapshot) {
        if let Ok(mut slot) = self.shared.snapshot.write() {
            *slot = snap;
        }
    }

    pub fn send(&self, cmd: Command) {
        if self.try_send(cmd).is_err() {
            log::error!("[gui] command channel closed");
        }
    }

    /// Enqueue a command and report whether the existing Worker channel accepted it.
    /// This does not report whether the Worker later applied the command.
    pub fn try_send(&self, cmd: Command) -> Result<(), tokio::sync::mpsc::error::SendError<Command>> {
        self.tx.send(cmd)
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.snapshot()
    }

    #[cfg(feature = "host-perf-harness")]
    pub fn stop_performance_harness(&self) {
        self.send(Command::StopPerformanceHarness);
    }
}

pub fn spawn(state: Arc<AppState>, repaint: impl Fn() + Send + Sync + 'static) -> Bridge {
    spawn_with_power_limit_policy(state, RemoteStrengthPolicy::default(), repaint)
}

pub fn spawn_with_power_limit_policy(
    state: Arc<AppState>,
    power_limit_policy: RemoteStrengthPolicy,
    repaint: impl Fn() + Send + Sync + 'static,
) -> Bridge {
    let (tx, rx) = unbounded_channel();
    let shared = Arc::new(Shared::new());
    let bridge = Bridge {
        tx,
        shared: Arc::clone(&shared),
    };

    let worker = Worker {
        state,
        shared: Arc::clone(&shared),
        active_device: 0,
        staged_config: CliConfig::default(),
        power_limit_policy,
        auto_save: prefs::load_ui_prefs().auto_save,
        repaint: Box::new(repaint),
        #[cfg(feature = "host-perf-harness")]
        perf_metrics: None,
        #[cfg(feature = "host-perf-harness")]
        timer_wakeups: 0,
        #[cfg(feature = "host-perf-harness")]
        command_wakeups: 0,
    };
    tokio::spawn(worker.run(rx, true));

    bridge
}

/// Start the real GUI Worker with synthetic AppState while omitting the remote
/// preset refresh. This is available only to the local native profiling example.
#[cfg(feature = "host-perf-harness")]
pub fn spawn_for_perf_harness(
    state: Arc<AppState>,
    repaint: impl Fn() + Send + Sync + 'static,
    metrics_path: std::path::PathBuf,
) -> (Bridge, tokio::task::JoinHandle<()>) {
    let (tx, rx) = unbounded_channel();
    let shared = Arc::new(Shared::new());
    let bridge = Bridge {
        tx,
        shared: Arc::clone(&shared),
    };
    let worker = Worker {
        state,
        shared,
        active_device: 0,
        staged_config: CliConfig::default(),
        power_limit_policy: RemoteStrengthPolicy::default(),
        auto_save: prefs::load_ui_prefs().auto_save,
        repaint: Box::new(repaint),
        perf_metrics: Some(WorkerMetrics::new(metrics_path)),
        timer_wakeups: 0,
        command_wakeups: 0,
    };
    let task = tokio::spawn(worker.run(rx, false));
    (bridge, task)
}

struct Worker {
    state: Arc<AppState>,
    shared: Arc<Shared>,
    active_device: usize,
    staged_config: CliConfig,
    power_limit_policy: RemoteStrengthPolicy,
    auto_save: bool,
    repaint: Box<dyn Fn() + Send + Sync>,
    #[cfg(feature = "host-perf-harness")]
    perf_metrics: Option<WorkerMetrics>,
    #[cfg(feature = "host-perf-harness")]
    timer_wakeups: u64,
    #[cfg(feature = "host-perf-harness")]
    command_wakeups: u64,
}

#[cfg(feature = "host-perf-harness")]
struct WorkerMetrics {
    output: std::path::PathBuf,
    started: Instant,
    samples: Vec<WorkerSample>,
}

#[cfg(feature = "host-perf-harness")]
struct WorkerSample {
    utc_ms: u128,
    elapsed_us: u128,
    snapshot_build_us: u128,
    snapshot_read_lock_us: u128,
    snapshot_write_lock_us: u128,
    publish_total_us: u128,
    timer_wakeups: u64,
    command_wakeups: u64,
}

#[cfg(feature = "host-perf-harness")]
impl WorkerMetrics {
    fn new(output: std::path::PathBuf) -> Self {
        Self {
            output,
            started: Instant::now(),
            samples: Vec::with_capacity(20_000),
        }
    }

    fn push(
        &mut self,
        build: Duration,
        read_lock: Duration,
        write_lock: Duration,
        total: Duration,
        timer_wakeups: u64,
        command_wakeups: u64,
    ) {
        self.samples.push(WorkerSample {
            utc_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            elapsed_us: self.started.elapsed().as_micros(),
            snapshot_build_us: build.as_micros(),
            snapshot_read_lock_us: read_lock.as_micros(),
            snapshot_write_lock_us: write_lock.as_micros(),
            publish_total_us: total.as_micros(),
            timer_wakeups,
            command_wakeups,
        });
    }

    fn write(&self) {
        let result = (|| -> std::io::Result<()> {
            use std::io::Write;

            let mut file = std::fs::File::create(&self.output)?;
            writeln!(file, "utc_ms,elapsed_us,snapshot_build_us,snapshot_read_lock_us,snapshot_write_lock_us,publish_total_us,timer_wakeups,command_wakeups")?;
            for sample in &self.samples {
                writeln!(
                    file,
                    "{},{},{},{},{},{},{},{}",
                    sample.utc_ms,
                    sample.elapsed_us,
                    sample.snapshot_build_us,
                    sample.snapshot_read_lock_us,
                    sample.snapshot_write_lock_us,
                    sample.publish_total_us,
                    sample.timer_wakeups,
                    sample.command_wakeups
                )?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("could not write profiling Worker metrics: {error}");
        }
    }
}

impl Worker {
    fn publish_interval(has_engine: bool) -> Duration {
        if has_engine {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(250)
        }
    }

    async fn run(mut self, mut rx: UnboundedReceiver<Command>, refresh_remote_presets: bool) {
        self.reload_zone_presets();
        if refresh_remote_presets {
            self.start_preset_refresh();
        }
        self.publish().await;

        let mut next_tick = tokio::time::Instant::now() + Self::publish_interval(self.engine().is_some());

        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    #[cfg(feature = "host-perf-harness")]
                    if matches!(&cmd, Command::StopPerformanceHarness) { break; }
                    #[cfg(feature = "host-perf-harness")]
                    { self.command_wakeups = self.command_wakeups.saturating_add(1); }
                    self.apply(cmd).await;
                    self.publish().await;
                    (self.repaint)();
                }
                _ = tokio::time::sleep_until(next_tick) => {
                    #[cfg(feature = "host-perf-harness")]
                    { self.timer_wakeups = self.timer_wakeups.saturating_add(1); }
                    self.publish().await;
                    self.expire_toast();
                    self.check_download_stall();
                    next_tick = tokio::time::Instant::now() + Self::publish_interval(self.engine().is_some());
                }
            }
        }

        #[cfg(feature = "host-perf-harness")]
        if let Some(metrics) = self.perf_metrics.take() {
            metrics.write();
        }
    }

    fn engine(&self) -> Option<shocking_vrc_core::CliEngine> {
        self.state.engine_at(self.active_device)
    }

    fn device_connected(&self) -> bool {
        self.state.slot_infos().get(self.active_device).is_some_and(|device| device.connected)
    }

    fn connected_engine(&self) -> Option<shocking_vrc_core::CliEngine> {
        self.device_connected().then(|| self.engine()).flatten()
    }

    async fn current_config(&self) -> CliConfig {
        match self.connected_engine() {
            Some(engine) => engine.config().await,
            None => self.staged_config.clone(),
        }
    }

    fn power_limit_cap(&self, channel: Channel) -> Option<u8> {
        if !self.power_limit_policy.enabled {
            return None;
        }
        let remote_channel = match channel {
            Channel::A => RemoteChannel::A,
            Channel::B => RemoteChannel::B,
        };
        // Persisted settings validation rejects missing caps. Treat an invalid in-memory
        // enabled policy as zero so a stale/partial settings update fails closed.
        Some(
            self.power_limit_policy
                .cap_for(remote_channel)
                .unwrap_or(0)
                .min(200),
        )
    }

    fn clamp_config_to_power_policy(&self, config: &mut CliConfig) -> bool {
        let (Some(cap_a), Some(cap_b)) = (
            self.power_limit_cap(Channel::A),
            self.power_limit_cap(Channel::B),
        ) else {
            return false;
        };
        let a_changed = clamp_limits_to_ceiling(&mut config.channel_a.limits, cap_a);
        let b_changed = clamp_limits_to_ceiling(&mut config.channel_b.limits, cap_b);
        a_changed || b_changed
    }

    fn notify_preview_only(&self) {
        self.notify(ToastKind::Info, "DEVICE OFFLINE · PREVIEW ONLY · NOT APPLIED TO HARDWARE");
    }

    fn active_mac(&self) -> Option<String> {
        self.state
            .slot_infos()
            .get(self.active_device)
            .map(|d| d.mac.clone())
    }

    async fn publish(&mut self) {
        #[cfg(feature = "host-perf-harness")]
        let publish_started = Instant::now();
        #[cfg(feature = "host-perf-harness")]
        let build_started = Instant::now();
        let devices = self.state.slot_infos();
        if !devices.is_empty() && self.active_device >= devices.len() {
            self.active_device = devices.len() - 1;
        }

        let connected_engine = self.connected_engine();
        let staged_preview_only = connected_engine.is_none();
        let (mut config, status) = match connected_engine.as_ref() {
            Some(engine) => (engine.config().await, engine.current_status().await),
            None => (self.staged_config.clone(), empty_status()),
        };

        // Reconcile the authoritative Worker config before publishing a snapshot.
        // The prior Shared snapshot is never used as a source of limit values.
        if self.clamp_config_to_power_policy(&mut config) {
            if let Some(engine) = connected_engine.as_ref() {
                engine.set_config(config.clone()).await;
                engine.sync_hardware_limits().await;
            } else {
                self.staged_config = config.clone();
                self.notify_preview_only();
            }
            self.persist_config().await;
        }

        #[cfg(feature = "host-perf-harness")]
        let read_lock_started = Instant::now();
        let revision = self
            .shared
            .snapshot
            .read()
            .map(|snapshot| snapshot.revision.wrapping_add(1))
            .unwrap_or(0);
        #[cfg(feature = "host-perf-harness")]
        let snapshot_read_lock = read_lock_started.elapsed();
        let snap = Snapshot {
            config,
            staged_preview_only,
            status,
            alarm: self.state.alarm.config(),
            alarm_status: self.state.alarm.status(),
            avatar_zones: self.state.scanner.zones().await,
            vrchat_found: self.state.scanner.vrchat_connected().await,
            osc_port: self.state.scanner.port().await,
            oscquery_port: self.state.scanner.oscquery_port().await,
            devices,
            active_device: self.active_device,
            revision,
            published_at: Instant::now(),
        };
        #[cfg(feature = "host-perf-harness")]
        let snapshot_build = build_started.elapsed();
        #[cfg(feature = "host-perf-harness")]
        let write_lock_started = Instant::now();
        if let Ok(mut slot) = self.shared.snapshot.write() {
            *slot = snap;
        }
        #[cfg(feature = "host-perf-harness")]
        if let Some(metrics) = self.perf_metrics.as_mut() {
            metrics.push(
                snapshot_build,
                snapshot_read_lock,
                write_lock_started.elapsed(),
                publish_started.elapsed(),
                self.timer_wakeups,
                self.command_wakeups,
            );
        }
    }

    fn expire_toast(&self) {
        let stale = self
            .shared
            .toast
            .read()
            .ok()
            .and_then(|t| t.clone())
            .is_some_and(|t| t.at.elapsed() > Duration::from_secs(4));
        if stale
            && let Ok(mut slot) = self.shared.toast.write() {
                *slot = None;
            }
    }

    fn notify(&self, kind: ToastKind, text: impl Into<String>) {
        self.shared.notify(kind, text);
    }

    async fn mutate<F: FnOnce(&mut CliConfig)>(&mut self, f: F) -> bool {
        let Some(engine) = self.connected_engine() else {
            f(&mut self.staged_config);
            self.notify_preview_only();
            return true;
        };
        let mut cfg = engine.config().await;
        f(&mut cfg);
        engine.set_config(cfg).await;
        true
    }

    async fn mutate_channel<F: FnOnce(&mut ChannelConfig)>(&mut self, ch: Channel, f: F) -> bool {
        self.mutate(|cfg| {
            f(match ch {
                Channel::A => &mut cfg.channel_a,
                Channel::B => &mut cfg.channel_b,
            })
        })
        .await
    }

    async fn apply(&mut self, cmd: Command) {
        let persists = cmd.persists();
        match cmd {
            Command::SwitchDevice(i) => {
                if i < self.state.slot_infos().len() {
                    self.active_device = i;
                }
            }

            Command::AddZone(ch, id) => {
                let entry = ZoneEntry::with_default_mode(id.clone());
                let added = self
                    .mutate_channel(ch, |c| {
                        if let Some(existing) = c.zones.iter_mut().find(|e| e.id == entry.id) {
                            existing.mode = entry.mode;
                        } else {
                            c.zones.push(entry);
                        }
                    })
                    .await;
                if added {
                    log::info!("[ch-{}] Zone added: {id} [depth]", ch.label());
                }
            }
            Command::AddAllAvatarZones(ch) => {
                let zones = self.state.scanner.zones().await;
                let mut added = 0usize;
                let ok = self
                    .mutate_channel(ch, |c| {
                        for z in &zones {
                            let id = ZoneId::new(z.zone_type, &z.id);
                            if !c.zones.iter().any(|e| e.id.matches(&id)) {
                                c.zones.push(ZoneEntry::with_default_mode(id));
                                added += 1;
                            }
                        }
                    })
                    .await;
                if ok {
                    log::info!("[ch-{}] Added {added} zone(s) from avatar", ch.label());
                    self.notify(
                        ToastKind::Success,
                        format!("Added {added} zone(s) to channel {}", ch.label()),
                    );
                }
            }
            Command::RemoveZone(ch, i) => {
                let mut removed = None;
                let ok = self
                    .mutate_channel(ch, |c| {
                        if i < c.zones.len() {
                            removed = Some(c.zones.remove(i).id);
                        }
                    })
                    .await;
                if let (true, Some(id)) = (ok, removed) {
                    log::info!("[ch-{}] Zone removed: {id}", ch.label());
                }
            }
            Command::SetZoneMode(ch, i, mode) => {
                let mut name = None;
                let ok = self
                    .mutate_channel(ch, |c| {
                        if let Some(e) = c.zones.get_mut(i) {
                            e.mode = mode;
                            name = Some(e.id.clone());
                        }
                    })
                    .await;
                if let (true, Some(id)) = (ok, name) {
                    log::info!("[ch-{}] Mode for {id} set to {mode}", ch.label());
                }
            }
            Command::SetZoneScale(ch, i, v) => {
                self.mutate_channel(ch, |c| {
                    if let Some(e) = c.zones.get_mut(i) {
                        e.scale = v.min(100);
                    }
                })
                .await;
            }
            Command::SetZoneThresholdMin(ch, i, v) => {
                self.mutate_channel(ch, |c| {
                    if let Some(e) = c.zones.get_mut(i) {
                        e.min_threshold = v
                            .clamp(ZoneEntry::THRESHOLD_MIN, ZoneEntry::THRESHOLD_MAX)
                            .min(e.max_threshold);
                    }
                })
                .await;
            }
            Command::SetZoneThresholdMax(ch, i, v) => {
                self.mutate_channel(ch, |c| {
                    if let Some(e) = c.zones.get_mut(i) {
                        e.max_threshold = v
                            .clamp(ZoneEntry::THRESHOLD_MIN, ZoneEntry::THRESHOLD_MAX)
                            .max(e.min_threshold);
                    }
                })
                .await;
            }

            Command::SetFrequency(ch, seg, v) => {
                self.mutate_channel(ch, |c| c.frequency[seg.min(3)] = v.clamp(10, 255))
                    .await;
            }
            Command::SetIntensity(ch, seg, v) => {
                self.mutate_channel(ch, |c| c.intensity[seg.min(3)] = v.min(100))
                    .await;
            }
            Command::SetLimitMin(ch, v) => {
                let cap = self.power_limit_cap(ch);
                self.mutate_channel(ch, |c| set_limit_min(&mut c.limits, v, cap)).await;
                if let Some(e) = self.connected_engine() {
                    e.sync_hardware_limits().await;
                }
            }
            Command::SetLimitMax(ch, v) => {
                let cap = self.power_limit_cap(ch);
                self.mutate_channel(ch, |c| set_limit_max(&mut c.limits, v, cap)).await;
                if let Some(e) = self.connected_engine() {
                    e.sync_hardware_limits().await;
                }
            }
            Command::SetPowerLimitPolicy(policy) => self.power_limit_policy = policy,
            Command::SetAggregation(ch, mode) => {
                if self.mutate_channel(ch, |c| c.aggregation = mode).await {
                    log::info!("[ch-{}] Aggregation set to {mode:?}", ch.label());
                }
            }
            Command::SaveConfig => {
                let Some(engine) = self.connected_engine() else {
                    self.notify_preview_only();
                    return;
                };
                let Some(mac) = self.active_mac() else {
                    self.notify(ToastKind::Error, "No device to save for");
                    return;
                };
                let cfg = engine.config().await;
                let mut ok = true;
                match device_config::save_device_config(&mac, &cfg) {
                    Ok(_) => log::info!("[config] Saved device config for {mac}"),
                    Err(e) => {
                        ok = false;
                        log::error!("[config] Save failed: {e}");
                    }
                }
                match prefs::save_config(prefs::CONFIG_FILE, &cfg) {
                    Ok(_) => log::info!("[config] Saved default to {}", prefs::CONFIG_FILE),
                    Err(e) => {
                        ok = false;
                        log::error!("[config] Default save failed: {e}");
                    }
                }
                if ok {
                    self.notify(ToastKind::Success, "Settings saved");
                } else {
                    self.notify(ToastKind::Error, "Save failed — see the log");
                }
            }
            Command::LoadConfig => {
                let Some(engine) = self.connected_engine() else {
                    self.notify_preview_only();
                    return;
                };
                let Some(mac) = self.active_mac() else { return };
                let mut cfg = device_config::load_device_config(&mac, &self.state.default_config);
                self.clamp_config_to_power_policy(&mut cfg);
                engine.set_config(cfg).await;
                engine.sync_hardware_limits().await;
                log::info!("[config] Loaded device config for {mac}");
                self.notify(ToastKind::Success, "Settings reloaded from disk");
            }

            Command::SetUkf(p) => {
                if let Some(e) = self.connected_engine() {
                    e.set_ukf_params(p).await;
                } else {
                    self.staged_config.ukf = p;
                    self.notify_preview_only();
                }
            }
            Command::ResetUkf => {
                if let Some(e) = self.connected_engine() {
                    e.set_ukf_params(UkfConfig::default()).await;
                    log::info!("[ukf] Reset to defaults");
                    self.notify(ToastKind::Info, "UKF reset to defaults");
                } else {
                    self.staged_config.ukf = UkfConfig::default();
                    self.notify_preview_only();
                }
            }
            Command::SetNorms(n) => {
                if let Some(e) = self.connected_engine() {
                    e.set_norms(n).await;
                } else {
                    self.staged_config.norms = n;
                    self.notify_preview_only();
                }
            }
            Command::ResetNorms => {
                if let Some(e) = self.connected_engine() {
                    e.set_norms(MotionNorms::default()).await;
                    log::info!("[norms] Reset to defaults");
                    self.notify(ToastKind::Info, "Normalisation reset to defaults");
                } else {
                    self.staged_config.norms = MotionNorms::default();
                    self.notify_preview_only();
                }
            }

            Command::ApplyModulation(ch, kind, seg, cfg) => {
                let mut cfg = *cfg;
                cfg.sanitise(kind == ModKind::Intensity);
                let summary = cfg.to_string();
                let ok = self
                    .mutate_channel(ch, |c| {
                        let slot = match kind {
                            ModKind::Freq => &mut c.freq_modulation,
                            ModKind::Intensity => &mut c.intensity_modulation,
                        };
                        slot[seg.min(3)] = Some(cfg);
                    })
                    .await;
                if ok {
                    log::info!(
                        "[ch-{}] {}[{seg}] modulation set: {summary}",
                        ch.label(),
                        kind.short()
                    );
                    if self.connected_engine().is_some() {
                        self.notify(
                            ToastKind::Success,
                            format!("Modulation applied to {} {}[{seg}]", ch.label(), kind.short()),
                        );
                    } else {
                        self.notify_preview_only();
                    }
                }
            }
            Command::ClearModulation(ch, kind, seg) => {
                let ok = self
                    .mutate_channel(ch, |c| {
                        let slot = match kind {
                            ModKind::Freq => &mut c.freq_modulation,
                            ModKind::Intensity => &mut c.intensity_modulation,
                        };
                        slot[seg.min(3)] = None;
                    })
                    .await;
                if ok {
                    log::info!(
                        "[ch-{}] Modulation seg[{seg}] disabled ({})",
                        ch.label(),
                        kind.short()
                    );
                }
            }
            Command::ClearChannelModulation(ch) => {
                let ok = self
                    .mutate_channel(ch, |c| {
                        for i in 0..4 {
                            c.freq_modulation[i] = None;
                            c.intensity_modulation[i] = None;
                        }
                    })
                    .await;
                if ok {
                    log::info!("[ch-{}] All modulation disabled (both)", ch.label());
                    self.notify(
                        ToastKind::Info,
                        format!("Cleared all modulation on channel {}", ch.label()),
                    );
                }
            }

            Command::RefreshPresets => self.start_preset_refresh(),
            Command::ApplyPreset(ch, i) => {
                let entry = self
                    .shared
                    .presets
                    .read()
                    .ok()
                    .and_then(|p| p.entries.get(i).cloned());
                let Some(entry) = entry else {
                    self.notify(ToastKind::Error, "No preset selected");
                    return;
                };
                let preset = entry.preset.clone();
                if self.mutate_channel(ch, |c| preset.apply_to(c)).await {
                    log::info!(
                        "[ch-{}] Preset applied: {} (limits unchanged)",
                        ch.label(),
                        entry.name
                    );
                    if self.connected_engine().is_some() {
                        self.notify(
                            ToastKind::Success,
                            format!("“{}” applied to channel {}", entry.name, ch.label()),
                        );
                    } else {
                        self.notify_preview_only();
                    }
                }
            }
            Command::SaveUserPreset {
                channel,
                name,
                author,
            } => {
                let cfg = self.current_config().await;
                let ch_cfg = match channel {
                    Channel::A => &cfg.channel_a,
                    Channel::B => &cfg.channel_b,
                };
                match preset_store::save_user_preset(ch_cfg, &name, &author) {
                    Ok(entry) => {
                        log::info!(
                            "[presets] Saved \"{}\" by \"{}\" from ch {} → presets/user/",
                            entry.name,
                            author,
                            channel.label()
                        );
                        self.notify(ToastKind::Success, format!("Saved preset “{}”", entry.name));
                        self.insert_preset(entry);
                    }
                    Err(e) => {
                        log::error!("[presets] Save failed: {e}");
                        self.notify(ToastKind::Error, format!("Save failed: {e}"));
                    }
                }
            }
            Command::DeleteUserPreset(i) => {
                let entry = self
                    .shared
                    .presets
                    .read()
                    .ok()
                    .and_then(|p| p.entries.get(i).cloned());
                let Some(entry) = entry.filter(|e| e.user) else {
                    return;
                };
                match preset_store::delete_user_preset(&entry.id) {
                    Ok(()) => {
                        log::info!("[presets] Deleted \"{}\" from presets/user/", entry.name);
                        self.notify(ToastKind::Info, format!("Deleted “{}”", entry.name));
                        if let Ok(mut p) = self.shared.presets.write() {
                            p.entries.retain(|e| e.id != entry.id);
                        }
                    }
                    Err(e) => {
                        log::error!("[presets] Delete failed: {e}");
                        self.notify(ToastKind::Error, format!("Delete failed: {e}"));
                    }
                }
            }

            Command::ReloadZonePresets => self.reload_zone_presets(),
            Command::ApplyZonePreset(i) => {
                let entry = self
                    .shared
                    .zone_presets
                    .read()
                    .ok()
                    .and_then(|z| z.entries.get(i).cloned());
                let Some(entry) = entry else { return };
                let preset = entry.preset.clone();
                if self.mutate(|cfg| preset.apply_to(cfg)).await {
                    let (a, b) = entry.preset.counts();
                    log::info!(
                        "[zone-sets] Loaded \"{}\" — A: {a} zone(s), B: {b} zone(s)",
                        entry.name()
                    );
                    if self.connected_engine().is_some() {
                        self.notify(
                            ToastKind::Success,
                            format!("Loaded “{}” — A:{a}, B:{b}", entry.name()),
                        );
                    } else {
                        self.notify_preview_only();
                    }
                }
            }
            Command::SaveZonePreset(name) => {
                let cfg = self.current_config().await;
                match zone_presets::save_zone_preset(&name, &cfg) {
                    Ok(entry) => {
                        let (a, b) = entry.preset.counts();
                        log::info!(
                            "[zone-sets] Saved \"{}\" (A:{a}, B:{b}) → presets/zones/",
                            entry.name()
                        );
                        self.notify(
                            ToastKind::Success,
                            format!("Saved zone set “{}”", entry.name()),
                        );
                        self.reload_zone_presets();
                    }
                    Err(e) => {
                        log::error!("[zone-sets] Save failed: {e}");
                        self.notify(ToastKind::Error, format!("Save failed: {e}"));
                    }
                }
            }
            Command::DeleteZonePreset(i) => {
                let entry = self
                    .shared
                    .zone_presets
                    .read()
                    .ok()
                    .and_then(|z| z.entries.get(i).cloned());
                let Some(entry) = entry else { return };
                match zone_presets::delete_zone_preset(&entry.id) {
                    Ok(()) => {
                        log::info!("[zone-sets] Deleted \"{}\" from presets/zones/", entry.name());
                        self.notify(ToastKind::Info, format!("Deleted “{}”", entry.name()));
                        self.reload_zone_presets();
                    }
                    Err(e) => {
                        log::error!("[zone-sets] Delete failed: {e}");
                        self.notify(ToastKind::Error, format!("Delete failed: {e}"));
                    }
                }
            }

            Command::SetAlarmConfig(cfg) => self.write_alarm(*cfg),
            Command::SetAlarmChannels(ch) => {
                let mut cfg = self.state.alarm.config();
                cfg.channels = ch;
                self.write_alarm(cfg);
                log::info!("[alarm] Channel: {}", ch.label());
            }
            Command::AlarmTest => self.state.alarm.test(),
            Command::AlarmStop => self.state.alarm.stop(),
            Command::AlarmSnooze => self.state.alarm.snooze(),

            Command::SetAutoSave(on) => {
                self.auto_save = on;
                match prefs::save_ui_prefs(on) {
                    Ok(()) => log::info!(
                        "[prefs] Auto-save {}",
                        if on { "enabled" } else { "disabled" }
                    ),
                    Err(e) => log::error!("[prefs] Failed to save: {e}"),
                }
                if on {
                    self.persist_config().await;
                }
            }
            Command::SetAlarmTabVisible(on) => {
                if let Err(e) = prefs::save_alarm_tab_pref(on) {
                    log::error!("[prefs] Failed to save: {e}");
                }
                if on {
                    log::info!("[prefs] Alarm section shown");
                } else {
                    self.state.alarm.stop();
                    let mut cfg = self.state.alarm.config();
                    if cfg.enabled {
                        cfg.enabled = false;
                        self.write_alarm(cfg);
                    }
                    log::info!("[prefs] Alarm section hidden — alarm disarmed");
                }
            }

            Command::CheckUpdate => self.start_update_check(),
            Command::StartUpdateDownload => self.start_update_download(),
            Command::SkipUpdateVersion => {
                let tag = self
                    .shared
                    .update
                    .read()
                    .ok()
                    .and_then(|u| u.available.as_ref().map(|r| r.tag.clone()));
                if let Some(tag) = tag {
                    if let Err(e) = prefs::update_ui_prefs(|p| {
                        p.skipped_update_version = Some(tag.clone())
                    }) {
                        log::warn!("[update] failed to save skip preference: {e}");
                    }
                    log::info!("[update] Skipped version {tag}");
                }
                self.dismiss_update();
            }
            Command::DismissUpdate => self.dismiss_update(),
            #[cfg(feature = "host-perf-harness")]
            Command::StopPerformanceHarness => {}
        }

        if persists {
            self.persist_config().await;
        }
    }

    fn write_alarm(&self, mut cfg: AlarmConfig) {
        cfg.sanitise();
        self.state.alarm.set_config(cfg.clone());
        if let Err(e) = shocking_vrc_app::alarm_store::save_alarm(&cfg) {
            log::error!(
                "[alarm] Failed to save {}: {e}",
                shocking_vrc_app::alarm_store::ALARM_FILE
            );
        }
    }

    async fn persist_config(&mut self) {
        if !self.auto_save {
            return;
        }
        let Some(mac) = self.active_mac() else { return };
        let Some(engine) = self.connected_engine() else { return };
        let cfg = engine.config().await;
        match device_config::save_device_config(&mac, &cfg) {
            Ok(()) => log::debug!("[config] Auto-saved device config for {mac}"),
            Err(e) => log::error!("[config] Auto-save failed: {e}"),
        }
    }

    fn insert_preset(&self, entry: PresetEntry) {
        if let Ok(mut p) = self.shared.presets.write() {
            match p.entries.iter().position(|e| e.id == entry.id) {
                Some(i) => p.entries[i] = entry,
                None => p.entries.push(entry),
            }
            preset_store::sort_preset_entries(&mut p.entries);
        }
    }

    fn reload_zone_presets(&self) {
        let entries = zone_presets::load_zone_presets();
        if let Ok(mut z) = self.shared.zone_presets.write() {
            z.entries = entries;
        }
    }

    fn start_preset_refresh(&self) {
        {
            let Ok(mut p) = self.shared.presets.write() else {
                return;
            };
            if p.loading {
                return;
            }
            p.loading = true;
            p.error = None;
        }
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move {
            let result = preset_store::load_catalog().await;
            if let Ok(mut p) = shared.presets.write() {
                p.loading = false;
                match result {
                    Ok(r) => {
                        log::info!("[presets] Loaded {} preset(s)", r.entries.len());
                        p.entries = r.entries;
                        p.source = r.source;
                        p.error = None;
                    }
                    Err(e) => {
                        log::warn!("[presets] {e}");
                        p.error = Some(e);
                    }
                }
            }
        });
    }

    fn start_update_check(&self) {
        {
            let Ok(mut u) = self.shared.update.write() else {
                return;
            };
            if u.checking || u.available.is_some() {
                return;
            }
            u.checking = true;
        }
        let shared = Arc::clone(&self.shared);
        let skipped = prefs::load_ui_prefs().skipped_update_version;
        tokio::spawn(async move {
            let result = shocking_vrc_core::update::check_for_update().await;
            if let Ok(mut u) = shared.update.write() {
                u.checking = false;
                match result {
                    Ok(Some(release)) => {
                        if skipped.as_deref() == Some(release.tag.as_str()) {
                            log::debug!("[update] {} was skipped by the user", release.tag);
                        } else {
                            log::info!("[update] New version available");
                            u.available = Some(release);
                            u.dismissed = false;
                        }
                    }
                    Ok(None) => log::debug!("[update] up to date"),
                    Err(e) => log::debug!("[update] check failed: {e}"),
                }
            }
        });
    }

    fn start_update_download(&self) {
        let release = {
            let Ok(mut u) = self.shared.update.write() else {
                return;
            };
            if u.downloading {
                return;
            }
            let Some(release) = u.available.clone() else {
                return;
            };
            u.downloading = true;
            u.error = None;
            u.last_progress_at = Some(Instant::now());
            u.progress = Some(DownloadProgress {
                downloaded: 0,
                total: (release.asset_size > 0).then_some(release.asset_size),
            });
            release
        };
        log::info!("[update] Downloading {}…", release.tag);

        let shared = Arc::clone(&self.shared);
        let progress_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let result = shocking_vrc_core::update::download_release(&release, move |p| {
                if let Ok(mut u) = progress_shared.update.write() {
                    u.progress = Some(p);
                    u.last_progress_at = Some(Instant::now());
                }
            })
            .await;

            if let Ok(mut u) = shared.update.write() {
                u.downloading = false;
                match result {
                    Ok(path) => {
                        log::info!("[update] Download complete — shutting down to install");
                        let args: Vec<String> = std::env::args().skip(1).collect();
                        stage_update(path.clone(), args.clone());
                        u.pending = Some((path, args));
                    }
                    Err(e) => {
                        log::warn!("[update] {e}");
                        u.error = Some(e);
                        u.progress = None;
                    }
                }
            }
        });
    }

    fn check_download_stall(&self) {
        let Ok(mut u) = self.shared.update.write() else {
            return;
        };
        if !u.downloading || u.error.is_some() {
            return;
        }
        let Some(last) = u.last_progress_at else {
            return;
        };
        if last.elapsed() < Duration::from_secs(20) {
            return;
        }
        let at_start = u.progress.as_ref().is_none_or(|p| p.downloaded == 0);
        u.error = Some(
            if at_start {
                "Download did not start — check your internet connection"
            } else {
                "Download stalled — connection may have been lost"
            }
            .to_string(),
        );
        u.downloading = false;
        u.progress = None;
        log::warn!("[update] {}", u.error.as_deref().unwrap_or_default());
    }

    fn dismiss_update(&self) {
        if let Ok(mut u) = self.shared.update.write() {
            u.dismissed = true;
            u.error = None;
            u.progress = None;
        }
    }
}

pub fn preview_preset(name: &str, author: &str, ch: &ChannelConfig) -> ChannelPreset {
    ChannelPreset::from_channel(name, author, ch)
}

static PENDING_UPDATE: RwLock<Option<(std::path::PathBuf, Vec<String>)>> = RwLock::new(None);

fn stage_update(path: std::path::PathBuf, args: Vec<String>) {
    if let Ok(mut slot) = PENDING_UPDATE.write() {
        *slot = Some((path, args));
    }
}

pub fn pending_update() -> Option<(std::path::PathBuf, Vec<String>)> {
    PENDING_UPDATE.write().ok().and_then(|mut s| s.take())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shocking_vrc_app::app_state::DeviceSlot;
    use shocking_vrc_core::modulation::config::ModulationFunction;
    use shocking_vrc_core::{AlarmController, AvatarScanner, CliEngine, OldZoneType};
    use std::sync::atomic::AtomicBool;

    async fn idle_worker() -> Worker {
        let scanner = AvatarScanner::new(None);
        let alarm = AlarmController::new(AlarmConfig::default());
        let state = Arc::new(AppState {
            scanner,
            monitor_enabled: Arc::new(AtomicBool::new(true)),
            default_config: CliConfig::default(),
            device_slots: Arc::new(RwLock::new(Vec::new())),
            alarm,
        });

        Worker {
            state,
            shared: Arc::new(Shared::new()),
            active_device: 0,
            staged_config: CliConfig::default(),
            power_limit_policy: RemoteStrengthPolicy::default(),
            auto_save: false,
            repaint: Box::new(|| {}),
        }
    }

    #[test]
    fn power_ceiling_reconciles_limits_down_without_raising_them() {
        let mut limits = PowerLimits::new(30, 100);
        assert!(clamp_limits_to_ceiling(&mut limits, 60));
        assert_eq!((limits.min, limits.max), (30, 60));

        let mut over_minimum = PowerLimits::new(80, 100);
        assert!(clamp_limits_to_ceiling(&mut over_minimum, 60));
        assert_eq!((over_minimum.min, over_minimum.max), (60, 60));

        assert!(!clamp_limits_to_ceiling(&mut over_minimum, 90));
        assert_eq!((over_minimum.min, over_minimum.max), (60, 60));
    }

    fn policy(enabled: bool, a: Option<u8>, b: Option<u8>) -> RemoteStrengthPolicy {
        RemoteStrengthPolicy {
            enabled,
            max_by_channel: shocking_vrc_remote::ChannelCaps { a, b },
        }
    }

    #[tokio::test]
    async fn enabled_ceiling_reconciles_disconnected_staged_limits_before_snapshot() {
        let mut w = idle_worker().await;
        w.staged_config.channel_a.limits = PowerLimits::new(35, 110);
        w.staged_config.channel_b.limits = PowerLimits::new(80, 120);

        w.apply(Command::SetPowerLimitPolicy(policy(true, Some(65), Some(70))))
            .await;
        w.publish().await;

        let snapshot = w.shared.snapshot();
        assert!(snapshot.staged_preview_only);
        assert_eq!((snapshot.config.channel_a.limits.min, snapshot.config.channel_a.limits.max), (35, 65));
        assert_eq!((snapshot.config.channel_b.limits.min, snapshot.config.channel_b.limits.max), (70, 70));
    }

    #[tokio::test]
    async fn cap_reduction_clamps_active_engine_and_publishes_authoritative_snapshot() {
        let mut w = worker().await;
        let engine = w.engine().unwrap();
        let mut cfg = engine.config().await;
        cfg.channel_a.limits = PowerLimits::new(30, 100);
        engine.set_config(cfg).await;

        w.apply(Command::SetPowerLimitPolicy(policy(true, Some(60), Some(200))))
            .await;
        w.publish().await;

        let actual = engine.config().await.channel_a.limits;
        let snapshot = w.shared.snapshot();
        assert_eq!((actual.min, actual.max), (30, 60));
        assert_eq!((snapshot.config.channel_a.limits.min, snapshot.config.channel_a.limits.max), (30, 60));
        assert!(snapshot.revision > 0);
    }

    #[tokio::test]
    async fn rapid_cap_reductions_are_monotonic_and_raise_or_disable_never_restores_limits() {
        let mut w = worker().await;
        let engine = w.engine().unwrap();
        let mut cfg = engine.config().await;
        cfg.channel_a.limits = PowerLimits::new(10, 120);
        engine.set_config(cfg).await;

        for cap in [100, 75, 40] {
            w.apply(Command::SetPowerLimitPolicy(policy(true, Some(cap), Some(200))))
                .await;
            w.publish().await;
            let limits = engine.config().await.channel_a.limits;
            assert_eq!(limits.max, cap);
            assert_eq!(limits.min, 10);
        }
        w.apply(Command::SetPowerLimitPolicy(policy(true, Some(150), Some(200))))
            .await;
        w.publish().await;
        w.apply(Command::SetPowerLimitPolicy(policy(false, Some(150), Some(200))))
            .await;
        w.publish().await;

        let limits = engine.config().await.channel_a.limits;
        assert_eq!((limits.min, limits.max), (10, 40));
    }

    #[tokio::test]
    async fn local_and_remote_limit_commands_share_the_worker_ceiling_and_minimum_invariant() {
        let mut w = worker().await;
        let engine = w.engine().unwrap();
        let mut cfg = engine.config().await;
        cfg.channel_a.limits = PowerLimits::new(30, 70);
        engine.set_config(cfg).await;
        w.apply(Command::SetPowerLimitPolicy(policy(true, Some(60), Some(200))))
            .await;

        // The remote adapter maps channels.set_limit_max to this same Worker command.
        w.apply(Command::SetLimitMax(Channel::A, 180)).await;
        assert_eq!((config_of(&w).await.channel_a.limits.min, config_of(&w).await.channel_a.limits.max), (30, 60));

        w.apply(Command::SetLimitMin(Channel::A, 100)).await;
        let limits = config_of(&w).await.channel_a.limits;
        assert_eq!((limits.min, limits.max), (60, 60));
    }

    #[tokio::test]
    async fn disabled_ceiling_preserves_existing_power_limits_normalization() {
        let mut w = worker().await;
        w.apply(Command::SetLimitMin(Channel::A, 80)).await;
        w.apply(Command::SetLimitMax(Channel::A, 50)).await;

        let limits = config_of(&w).await.channel_a.limits;
        assert_eq!((limits.min, limits.max), (80, 80));
    }

    #[tokio::test]
    async fn active_remote_session_limit_write_is_clamped_by_the_authoritative_worker() {
        let mut w = worker().await;
        let engine = w.engine().unwrap();
        let mut config = engine.config().await;
        config.channel_a.limits = PowerLimits::new(25, 100);
        engine.set_config(config).await;
        w.power_limit_policy = policy(true, Some(55), Some(200));

        let (tx, mut rx) = unbounded_channel();
        let shared = Arc::new(Shared::new());
        let bridge = Bridge { tx, shared: Arc::clone(&shared) };
        w.shared = shared;
        let logs = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let backend = crate::remote_adapter::BridgeRemoteBackend::new(bridge, logs, None);
        shocking_vrc_remote::RemoteBackend::enqueue(
            backend.as_ref(),
            shocking_vrc_remote::RemoteCommand {
                request_id: uuid::Uuid::new_v4(),
                op: "channels.set_limit_max".into(),
                args: serde_json::json!({"channel":"A","value":180}),
            },
        )
        .unwrap();

        let command = rx.recv().await.expect("remote command queued to existing Worker");
        w.apply(command).await;
        w.publish().await;

        let limits = w.shared.snapshot().config.channel_a.limits;
        assert_eq!((limits.min, limits.max), (25, 55));
        assert_eq!(engine.config().await.channel_a.limits.max, 55);
    }

    #[tokio::test]
    async fn stale_shared_snapshot_cannot_override_worker_cap_reconciliation() {
        let mut w = worker().await;
        let engine = w.engine().unwrap();
        let mut actual = engine.config().await;
        actual.channel_a.limits = PowerLimits::new(25, 105);
        engine.set_config(actual).await;

        let mut stale = Snapshot::empty();
        stale.config.channel_a.limits.min = 180;
        stale.config.channel_a.limits.max = 15;
        *w.shared.snapshot.write().unwrap() = stale;
        w.apply(Command::SetPowerLimitPolicy(policy(true, Some(55), Some(200))))
            .await;
        w.publish().await;

        let snapshot = w.shared.snapshot();
        assert_eq!((snapshot.config.channel_a.limits.min, snapshot.config.channel_a.limits.max), (25, 55));
        assert_eq!(engine.config().await.channel_a.limits.max, 55);
    }

    #[tokio::test]
    async fn enabled_policy_with_missing_channel_cap_fails_closed() {
        let mut w = idle_worker().await;
        w.staged_config.channel_b.limits = PowerLimits::new(30, 90);

        w.apply(Command::SetPowerLimitPolicy(policy(true, Some(60), None)))
            .await;
        w.publish().await;

        let limits = w.staged_config.channel_b.limits;
        assert_eq!((limits.min, limits.max), (0, 0));
    }

    #[tokio::test]
    async fn no_device_modulation_and_tuning_edits_are_preview_only() {
        let mut w = idle_worker().await;
        let modulation = ModulationConfig {
            function: ModulationFunction::Triangle,
            ..ModulationConfig::default()
        };
        w.apply(Command::ApplyModulation(
            Channel::A,
            ModKind::Freq,
            1,
            Box::new(modulation),
        )).await;
        w.apply(Command::SetUkf(UkfConfig { q: 0.42, ..UkfConfig::default() })).await;
        w.publish().await;

        let snapshot = w.shared.snapshot();
        assert!(snapshot.status.device_connected == false);
        assert!(snapshot.devices.is_empty());
        assert_eq!(snapshot.config.ukf.q, 0.42);
        assert_eq!(snapshot.config.channel_a.freq_modulation[1].as_ref().unwrap().function, ModulationFunction::Triangle);
        assert_eq!(w.state.default_config.ukf.q, CliConfig::default().ukf.q);
        assert!(w.state.default_config.channel_a.freq_modulation[1].is_none());
    }

    #[tokio::test]
    async fn staged_configuration_is_not_applied_when_a_device_appears() {
        let mut w = idle_worker().await;
        let modulation = ModulationConfig {
            function: ModulationFunction::Triangle,
            ..ModulationConfig::default()
        };
        w.apply(Command::ApplyModulation(Channel::A, ModKind::Freq, 0, Box::new(modulation))).await;
        assert!(w.shared.snapshot().config.channel_a.freq_modulation[0].is_none());

        let engine = CliEngine::new(CliConfig::default(), w.state.alarm.clone());
        let stop_handle = engine.start(&w.state.scanner).await;
        *w.state.device_slots.write().unwrap() = vec![DeviceSlot {
            mac: "AA:BB:CC:DD:EE:FF".into(),
            name: "Synthetic Coyote".into(),
            engine: engine.clone(),
            stop_handle,
            battery: None,
            connected: true,
            reconnecting: false,
        }];

        w.publish().await;

        assert!(w.shared.snapshot().config.channel_a.freq_modulation[0].is_none());
        assert!(engine.config().await.channel_a.freq_modulation[0].is_none());
    }

    #[test]
    fn worker_publish_interval_preserves_engine_and_no_engine_rates() {
        assert_eq!(Worker::publish_interval(false), Duration::from_millis(250));
        assert_eq!(Worker::publish_interval(true), Duration::from_millis(50));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_worker_waits_before_republishing_an_unchanged_snapshot() {
        let worker = idle_worker().await;
        let shared = Arc::clone(&worker.shared);
        let (tx, rx) = unbounded_channel();
        let task = tokio::spawn(worker.run(rx, false));

        for _ in 0..8 {
            if shared.snapshot.read().unwrap().revision > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        let initial_revision = shared.snapshot.read().unwrap().revision;
        assert!(initial_revision > 0, "worker should publish its initial snapshot");

        tokio::time::advance(Duration::from_millis(200)).await;
        tokio::task::yield_now().await;

        let revision_after_idle_window = shared.snapshot.read().unwrap().revision;
        assert_eq!(
            revision_after_idle_window, initial_revision,
            "an idle Host without a device should not republish unchanged data every 50 ms"
        );

        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn no_device_command_publishes_preview_immediately() {
        let worker = idle_worker().await;
        let shared = Arc::clone(&worker.shared);
        let (tx, rx) = unbounded_channel();
        let task = tokio::spawn(worker.run(rx, false));

        for _ in 0..8 {
            if shared.snapshot.read().unwrap().revision > 0 { break; }
            tokio::task::yield_now().await;
        }
        let initial_revision = shared.snapshot.read().unwrap().revision;
        tx.send(Command::SetFrequency(Channel::A, 0, 144)).unwrap();
        for _ in 0..8 {
            if shared.snapshot.read().unwrap().revision > initial_revision { break; }
            tokio::task::yield_now().await;
        }

        let updated = shared.snapshot();
        assert_eq!(updated.revision, initial_revision + 1);
        assert_eq!(updated.config.channel_a.frequency[0], 144);
        tokio::time::advance(Duration::from_millis(200)).await;
        tokio::task::yield_now().await;
        assert_eq!(shared.snapshot().revision, updated.revision);

        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn clearing_staged_modulation_updates_preview_without_creating_device_state() {
        let mut worker = idle_worker().await;
        let modulation = ModulationConfig {
            function: ModulationFunction::Triangle,
            ..ModulationConfig::default()
        };

        worker.apply(Command::ApplyModulation(
            Channel::A,
            ModKind::Freq,
            1,
            Box::new(modulation),
        )).await;
        worker.publish().await;
        assert!(worker.shared.snapshot().config.channel_a.freq_modulation[1].is_some());

        worker.apply(Command::ClearModulation(Channel::A, ModKind::Freq, 1)).await;
        worker.publish().await;
        let snapshot = worker.shared.snapshot();

        assert!(snapshot.staged_preview_only);
        assert!(snapshot.devices.is_empty());
        assert!(!snapshot.status.device_connected);
        assert!(snapshot.config.channel_a.freq_modulation[1].is_none());
    }

    async fn worker() -> Worker {
        let scanner = AvatarScanner::new(Some(0));
        let alarm = AlarmController::new(AlarmConfig::default());
        let engine = CliEngine::new(CliConfig::default(), alarm.clone());
        let stop_handle = engine.start(&scanner).await;

        let state = Arc::new(AppState {
            scanner: scanner.clone(),
            monitor_enabled: Arc::new(AtomicBool::new(true)),
            default_config: CliConfig::default(),
            device_slots: Arc::new(RwLock::new(vec![DeviceSlot {
                mac: "AA:BB:CC:DD:EE:FF".into(),
                name: "Test Coyote".into(),
                engine,
                stop_handle,
                battery: Some(80),
                connected: true,
                reconnecting: false,
            }])),
            alarm,
        });

        Worker {
            state,
            shared: Arc::new(Shared::new()),
            active_device: 0,
            staged_config: CliConfig::default(),
            power_limit_policy: RemoteStrengthPolicy::default(),
            auto_save: false,
            repaint: Box::new(|| {}),
        }
    }

    async fn config_of(w: &Worker) -> CliConfig {
        w.engine().expect("engine").config().await
    }

    fn pen(name: &str) -> ZoneId {
        ZoneId::new(OldZoneType::Pen, name)
    }

    #[tokio::test]
    async fn zone_commands_map_and_calibrate() {
        let mut w = worker().await;

        w.apply(Command::AddZone(Channel::A, pen("Cock"))).await;
        assert_eq!(config_of(&w).await.channel_a.zones.len(), 1);

        w.apply(Command::AddZone(Channel::A, pen("Cock"))).await;
        assert_eq!(config_of(&w).await.channel_a.zones.len(), 1);

        w.apply(Command::SetZoneMode(Channel::A, 0, ContactMode::Recoil))
            .await;
        w.apply(Command::SetZoneScale(Channel::A, 0, 60)).await;
        w.apply(Command::SetZoneThresholdMin(Channel::A, 0, 30)).await;
        w.apply(Command::SetZoneThresholdMax(Channel::A, 0, 80)).await;

        let e = config_of(&w).await.channel_a.zones[0].clone();
        assert_eq!(e.mode, ContactMode::Recoil);
        assert_eq!(e.scale, 60);
        assert_eq!((e.min_threshold, e.max_threshold), (30, 80));

        w.apply(Command::RemoveZone(Channel::A, 0)).await;
        assert!(config_of(&w).await.channel_a.zones.is_empty());
    }

    #[tokio::test]
    async fn thresholds_cannot_cross_each_other() {
        let mut w = worker().await;
        w.apply(Command::AddZone(Channel::B, pen("Z"))).await;

        w.apply(Command::SetZoneThresholdMax(Channel::B, 0, 40)).await;
        w.apply(Command::SetZoneThresholdMin(Channel::B, 0, 90)).await;
        let e = config_of(&w).await.channel_b.zones[0].clone();
        assert!(
            e.min_threshold <= e.max_threshold,
            "min {} rose past max {}",
            e.min_threshold,
            e.max_threshold
        );
    }

    #[tokio::test]
    async fn waveform_and_limits_clamp_to_hardware_range() {
        let mut w = worker().await;

        w.apply(Command::SetFrequency(Channel::A, 0, 5)).await;
        w.apply(Command::SetFrequency(Channel::A, 1, 250)).await;
        w.apply(Command::SetIntensity(Channel::A, 0, 200)).await;
        let a = config_of(&w).await.channel_a;
        assert_eq!(a.frequency[0], 10, "frequency floors at the hardware minimum");
        assert_eq!(a.frequency[1], 250);
        assert_eq!(a.intensity[0], 100, "intensity is a percentage");

        w.apply(Command::SetLimitMax(Channel::A, 45)).await;
        w.apply(Command::SetLimitMin(Channel::A, 10)).await;
        let lim = config_of(&w).await.channel_a.limits;
        assert_eq!((lim.min, lim.max), (10, 45));
    }

    #[tokio::test]
    async fn aggregation_round_trips() {
        let mut w = worker().await;
        for mode in [
            AggregationMode::Sum,
            AggregationMode::Average,
            AggregationMode::Max,
        ] {
            w.apply(Command::SetAggregation(Channel::B, mode)).await;
            assert_eq!(config_of(&w).await.channel_b.aggregation, mode);
        }
    }

    #[tokio::test]
    async fn modulation_slots_apply_and_clear_independently() {
        let mut w = worker().await;
        let mut cfg = ModulationConfig {
            function: ModulationFunction::Triangle,
            ..ModulationConfig::default()
        };
        cfg.sanitise(false);

        w.apply(Command::ApplyModulation(
            Channel::A,
            ModKind::Freq,
            2,
            Box::new(cfg.clone()),
        ))
        .await;
        let a = config_of(&w).await.channel_a;
        assert!(a.freq_modulation[2].is_some(), "the chosen slot is filled");
        assert!(a.freq_modulation[0].is_none(), "its neighbours are not");
        assert!(a.intensity_modulation[2].is_none(), "nor is the other kind");

        w.apply(Command::ApplyModulation(
            Channel::A,
            ModKind::Intensity,
            2,
            Box::new(cfg),
        ))
        .await;
        assert!(config_of(&w).await.channel_a.intensity_modulation[2].is_some());

        w.apply(Command::ClearModulation(Channel::A, ModKind::Freq, 2))
            .await;
        let a = config_of(&w).await.channel_a;
        assert!(a.freq_modulation[2].is_none(), "cleared");
        assert!(
            a.intensity_modulation[2].is_some(),
            "clearing one kind leaves the other alone"
        );

        w.apply(Command::ClearChannelModulation(Channel::A)).await;
        let a = config_of(&w).await.channel_a;
        assert!(a.freq_modulation.iter().all(|s| s.is_none()));
        assert!(a.intensity_modulation.iter().all(|s| s.is_none()));
    }

    #[tokio::test]
    async fn intensity_modulation_is_sanitised_into_percentage_bounds() {
        let mut w = worker().await;
        let cfg = ModulationConfig {
            clamp_min: -50.0,
            clamp_max: 900.0,
            ..ModulationConfig::default()
        };
        w.apply(Command::ApplyModulation(
            Channel::A,
            ModKind::Intensity,
            0,
            Box::new(cfg),
        ))
        .await;
        let slot = config_of(&w).await.channel_a.intensity_modulation[0]
            .clone()
            .expect("slot");
        assert_eq!((slot.clamp_min, slot.clamp_max), (0.0, 100.0));
    }

    #[tokio::test]
    async fn tuning_commands_reach_the_engine_and_reset() {
        let mut w = worker().await;

        let ukf = UkfConfig {
            q: 0.05,
            r: 0.2,
            alpha: 0.5,
            beta: 1.0,
            kappa: -1.0,
        };
        w.apply(Command::SetUkf(ukf)).await;
        assert_eq!(config_of(&w).await.ukf, ukf);
        w.apply(Command::ResetUkf).await;
        assert_eq!(config_of(&w).await.ukf, UkfConfig::default());

        let norms = MotionNorms {
            speed: 9.0,
            acc: 44.0,
            recoil: 150.0,
        };
        w.apply(Command::SetNorms(norms)).await;
        assert_eq!(config_of(&w).await.norms, norms);
        w.apply(Command::ResetNorms).await;
        assert_eq!(config_of(&w).await.norms, MotionNorms::default());
    }

    #[tokio::test]
    async fn a_preset_rewrites_the_waveform_but_never_the_limits_or_zones() {
        let mut w = worker().await;
        w.apply(Command::AddZone(Channel::A, pen("Keep"))).await;
        w.apply(Command::SetLimitMax(Channel::A, 25)).await;
        w.apply(Command::SetAggregation(Channel::A, AggregationMode::Sum))
            .await;

        if let Ok(mut p) = w.shared.presets.write() {
            p.entries = vec![PresetEntry {
                id: "t".into(),
                name: "Test".into(),
                description: String::new(),
                preset: ChannelPreset {
                    name: "Test".into(),
                    description: String::new(),
                    author: String::new(),
                    frequency: [180, 50, 160, 70],
                    intensity: [75, 85, 80, 70],
                    freq_modulation: Default::default(),
                    intensity_modulation: Default::default(),
                },
                user: false,
            }];
        }

        w.apply(Command::ApplyPreset(Channel::A, 0)).await;
        let a = config_of(&w).await.channel_a;
        assert_eq!(a.frequency, [180, 50, 160, 70], "the waveform is replaced");
        assert_eq!(a.intensity, [75, 85, 80, 70]);
        assert_eq!(a.limits.max, 25, "the power ceiling is untouched");
        assert_eq!(a.aggregation, AggregationMode::Sum, "so is the zone mix");
        assert_eq!(a.zones.len(), 1, "and so are the mapped zones");
    }

    #[tokio::test]
    async fn a_zone_set_replaces_zones_but_keeps_the_waveform() {
        let mut w = worker().await;
        w.apply(Command::SetFrequency(Channel::A, 0, 123)).await;
        w.apply(Command::SetLimitMax(Channel::A, 33)).await;
        w.apply(Command::AddZone(Channel::A, pen("Old"))).await;

        let mut source = CliConfig::default();
        source.channel_a.zones = vec![ZoneEntry::with_default_mode(pen("New"))];
        source.channel_b.zones = vec![ZoneEntry::with_default_mode(pen("BSide"))];
        if let Ok(mut z) = w.shared.zone_presets.write() {
            z.entries = vec![ZonePresetEntry {
                id: "s".into(),
                preset: shocking_vrc_core::presets::ZonePreset::from_config("S", &source),
            }];
        }

        w.apply(Command::ApplyZonePreset(0)).await;
        let cfg = config_of(&w).await;
        assert_eq!(cfg.channel_a.zones[0].id.name, "New");
        assert_eq!(cfg.channel_b.zones[0].id.name, "BSide");
        assert_eq!(cfg.channel_a.frequency[0], 123, "the waveform survives");
        assert_eq!(cfg.channel_a.limits.max, 33, "so does the ceiling");
    }

    #[tokio::test]
    async fn add_all_avatar_zones_is_a_no_op_with_no_avatar() {
        let mut w = worker().await;
        w.apply(Command::AddAllAvatarZones(Channel::A)).await;
        assert!(config_of(&w).await.channel_a.zones.is_empty());
    }

    #[tokio::test]
    async fn alarm_config_is_sanitised_on_the_way_in() {
        let mut w = worker().await;
        let cfg = AlarmConfig {
            enabled: true,
            hour: 99,
            minute: 99,
            start_strength: 90,
            peak_strength: 10,
            repeats: 0,
            ..AlarmConfig::default()
        };
        w.apply(Command::SetAlarmConfig(Box::new(cfg))).await;

        let stored = w.state.alarm.config();
        assert_eq!(stored.hour, 23, "the hour is clamped to a real time");
        assert_eq!(stored.minute, 59);
        assert!(
            stored.peak_strength >= stored.start_strength,
            "peak must not sit below the start"
        );
        assert!(stored.repeats >= 1, "an alarm rings at least once");
    }

    #[tokio::test]
    async fn switching_to_a_device_that_does_not_exist_is_ignored() {
        let mut w = worker().await;
        w.apply(Command::SwitchDevice(7)).await;
        assert_eq!(w.active_device, 0, "the selection stays on a real device");
    }

    #[tokio::test]
    async fn commands_that_change_the_config_are_the_ones_that_persist() {
        assert!(Command::SetFrequency(Channel::A, 0, 100).persists());
        assert!(Command::ApplyPreset(Channel::A, 0).persists());
        assert!(Command::ApplyZonePreset(0).persists());
        assert!(Command::ResetUkf.persists());

        assert!(!Command::SwitchDevice(1).persists());
        assert!(!Command::RefreshPresets.persists());
        assert!(!Command::AlarmTest.persists());
        assert!(
            !Command::SaveConfig.persists(),
            "an explicit save is not an auto-save"
        );
    }

    #[tokio::test]
    async fn a_detached_bridge_accepts_commands_without_a_worker() {
        let (bridge, mut rx) = Bridge::detached();
        bridge.send(Command::AlarmTest);
        assert!(matches!(rx.try_recv(), Ok(Command::AlarmTest)));
    }
}

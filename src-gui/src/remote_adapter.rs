use std::sync::{Arc, Mutex};

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use shocking_vrc_app::{logger::LogBuffer, prefs};
use shocking_vrc_core::{
    OldZoneType,
    cli::{
        AggregationMode, AlarmChannels, AlarmConfig, AlarmPhase, ChannelConfig, ContactMode, MotionNorms, UkfConfig,
        ZoneId,
    },
    modulation::{
        ModulationConfig,
        config::{ModulationFunction, ModulationSource},
    },
};
use shocking_vrc_remote::{
    BackendError, RemoteBackend, RemoteCommand,
    protocol::{
        ActiveZoneDto, AlarmConfigDto, AlarmDto, AlarmStatusDto, AvatarZoneDto, ChannelConfigDto, ChannelPresetDto,
        ChannelStatusDto, ConfigDto, DeviceDto, KinematicsDto, LimitsDto, LogLineDto, LogPageDto, ModulationDto,
        ModulationFunctionDto, MotionNormsDto, PrefsDto, PresetCatalogDto, PresetEntryDto, RemotePolicyDto,
        RemoteStrengthDto, SnapshotDto, StatusDto, UkfDto, VrchatDto, ZoneDto, ZoneIdDto, ZonePresetCatalogDto,
        ZonePresetDto, ZonePresetEntryDto,
    },
};

use crate::{
    bridge::{Bridge, Command},
    model::{Channel, ModKind},
};

const LOG_RING_CAPACITY: usize = 2000;
const LOG_LINE_LIMIT: usize = 4096;

#[derive(Clone)]
pub struct BridgeRemoteBackend {
    bridge: Bridge,
    logs: LogBuffer,
    log_state: Arc<Mutex<LogSequenceState>>,
}

#[derive(Default)]
struct LogSequenceState {
    previous: Vec<String>,
    first_sequence: u64,
    last_sequence: u64,
}

impl BridgeRemoteBackend {
    pub fn new(bridge: Bridge, logs: LogBuffer) -> Arc<Self> {
        Arc::new(Self {
            bridge,
            logs,
            log_state: Arc::new(Mutex::new(LogSequenceState::default())),
        })
    }

    fn snapshot_dto(&self) -> Result<SnapshotDto, BackendError> {
        let snapshot = self.bridge.snapshot();
        let prefs = prefs::load_ui_prefs();
        let mut max_by_channel = std::collections::BTreeMap::new();
        max_by_channel.insert("A".to_owned(), None);
        max_by_channel.insert("B".to_owned(), None);

        let dto = SnapshotDto {
            bridge_revision: snapshot.revision,
            active_device: snapshot.active_device,
            active_device_mac: snapshot.active().map(|device| device.mac.clone()),
            devices: snapshot
                .devices
                .iter()
                .map(|device| DeviceDto {
                    mac: device.mac.clone(),
                    name: device.name.clone(),
                    battery: device.battery,
                    connected: device.connected,
                    reconnecting: device.reconnecting,
                })
                .collect(),
            vrchat: VrchatDto {
                connected: snapshot.vrchat_found,
                osc_port: snapshot.osc_port,
                oscquery_port: snapshot.oscquery_port,
            },
            avatar_zones: snapshot
                .avatar_zones
                .iter()
                .map(|zone| AvatarZoneDto {
                    zone_type: zone_type_wire(zone.zone_type).to_owned(),
                    id: zone.id.clone(),
                    is_tps: zone.is_tps,
                    level: zone.level,
                    velocity: zone.velocity,
                    acceleration: zone.acceleration,
                    recoil: zone.recoil,
                })
                .collect(),
            config: ConfigDto {
                channel_a: channel_config_dto(&snapshot.config.channel_a)?,
                channel_b: channel_config_dto(&snapshot.config.channel_b)?,
                ukf: UkfDto {
                    q: snapshot.config.ukf.q,
                    r: snapshot.config.ukf.r,
                    alpha: snapshot.config.ukf.alpha,
                    beta: snapshot.config.ukf.beta,
                    kappa: snapshot.config.ukf.kappa,
                },
                norms: MotionNormsDto {
                    speed: snapshot.config.norms.speed,
                    acc: snapshot.config.norms.acc,
                    recoil: snapshot.config.norms.recoil,
                },
            },
            status: StatusDto {
                device_connected: snapshot.status.device_connected,
                channel_a: channel_status_dto(&snapshot.status.channel_a),
                channel_b: channel_status_dto(&snapshot.status.channel_b),
            },
            alarm: AlarmDto {
                config: alarm_config_dto(&snapshot.alarm),
                status: AlarmStatusDto {
                    phase: alarm_phase_wire(snapshot.alarm_status.phase).to_owned(),
                    strength: snapshot.alarm_status.strength,
                    elapsed_secs: snapshot.alarm_status.elapsed_secs,
                    auto_stop_in_secs: snapshot.alarm_status.auto_stop_in_secs,
                    snooze_left_secs: snapshot.alarm_status.snooze_left_secs,
                    next_fire_in_mins: snapshot.alarm_status.next_fire_in_mins,
                    attempt: snapshot.alarm_status.attempt,
                    retrying: snapshot.alarm_status.retrying,
                    test: snapshot.alarm_status.test,
                },
            },
            prefs: Some(PrefsDto {
                auto_save: prefs.auto_save,
                alarm_tab_visible: prefs.alarm_tab,
            }),
            remote_policy: RemotePolicyDto {
                remote_strength: RemoteStrengthDto {
                    enabled: false,
                    max_by_channel,
                    scope: "channel_power_limits_min_max",
                },
                max_remote_sessions: 1,
            },
        };
        ensure_snapshot_finite(&dto)?;
        Ok(dto)
    }

    fn preset_catalog(&self) -> Result<PresetCatalogDto, BackendError> {
        let state = self
            .bridge
            .shared
            .presets
            .read()
            .map_err(|_| unavailable("Preset catalog is unavailable"))?;
        let mut entries = Vec::with_capacity(state.entries.len());
        for (index, entry) in state.entries.iter().enumerate() {
            entries.push(PresetEntryDto {
                index,
                id: entry.id.clone(),
                name: entry.name.clone(),
                description: entry.description.clone(),
                user: entry.user,
                preset: ChannelPresetDto {
                    name: entry.preset.name.clone(),
                    description: entry.preset.description.clone(),
                    author: entry.preset.author.clone(),
                    frequency: entry.preset.frequency,
                    intensity: entry.preset.intensity,
                    freq_modulation: map_modulation_slots(&entry.preset.freq_modulation)?,
                    intensity_modulation: map_modulation_slots(&entry.preset.intensity_modulation)?,
                },
            });
        }
        Ok(PresetCatalogDto { entries })
    }

    fn zone_preset_catalog(&self) -> Result<ZonePresetCatalogDto, BackendError> {
        let state = self
            .bridge
            .shared
            .zone_presets
            .read()
            .map_err(|_| unavailable("Zone preset catalog is unavailable"))?;
        let entries = state
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| ZonePresetEntryDto {
                index,
                id: entry.id.clone(),
                preset: ZonePresetDto {
                    version: entry.preset.version,
                    name: entry.preset.name.clone(),
                    description: entry.preset.description.clone(),
                    channel_a: entry.preset.channel_a.iter().map(zone_dto).collect(),
                    channel_b: entry.preset.channel_b.iter().map(zone_dto).collect(),
                },
            })
            .collect();
        Ok(ZonePresetCatalogDto { entries })
    }

    fn log_page(&self, cursor: Option<u64>, limit: u16) -> Result<LogPageDto, BackendError> {
        let lines = self
            .logs
            .lock()
            .map_err(|_| unavailable("Log buffer is unavailable"))?
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let (first_sequence, last_sequence) = self.update_log_sequences(&lines)?;
        let max_start = last_sequence
            .saturating_sub(limit as u64)
            .saturating_add(1)
            .max(first_sequence);
        let start_sequence = cursor
            .map(|value| value.saturating_add(1))
            .unwrap_or(max_start)
            .max(first_sequence);
        let offset = start_sequence.saturating_sub(first_sequence) as usize;
        let selected = lines
            .iter()
            .skip(offset)
            .take(limit as usize)
            .enumerate()
            .map(|(index, line)| LogLineDto {
                seq: start_sequence + index as u64,
                text: sanitize_log_line(line),
            })
            .collect::<Vec<_>>();
        let last_returned = selected
            .last()
            .map(|line| line.seq)
            .unwrap_or(start_sequence.saturating_sub(1));
        let next_cursor = (last_returned < last_sequence).then(|| last_returned.to_string());
        Ok(LogPageDto {
            cursor: start_sequence.saturating_sub(1).to_string(),
            next_cursor,
            lines: selected,
        })
    }

    fn update_log_sequences(&self, current: &[String]) -> Result<(u64, u64), BackendError> {
        let mut state = self
            .log_state
            .lock()
            .map_err(|_| unavailable("Log cursor is unavailable"))?;
        let overlap = (0..=state.previous.len().min(current.len()))
            .rev()
            .find(|count| state.previous[state.previous.len() - *count..] == current[..*count])
            .unwrap_or(0);
        if state.previous.is_empty() {
            state.first_sequence = 1;
            state.last_sequence = current.len() as u64;
        } else if overlap == 0 {
            state.first_sequence = state.last_sequence.saturating_add(1);
            state.last_sequence = state
                .first_sequence
                .saturating_add(current.len() as u64)
                .saturating_sub(1);
        } else {
            let appended = current.len().saturating_sub(overlap) as u64;
            state.last_sequence = state.last_sequence.saturating_add(appended);
            state.first_sequence = state
                .last_sequence
                .saturating_sub(current.len() as u64)
                .saturating_add(1);
        }
        state.previous.clear();
        state.previous.extend(current.iter().cloned());
        if state.previous.len() > LOG_RING_CAPACITY {
            let excess = state.previous.len() - LOG_RING_CAPACITY;
            state.previous.drain(..excess);
        }
        Ok((state.first_sequence, state.last_sequence))
    }
}

impl RemoteBackend for BridgeRemoteBackend {
    fn snapshot(&self) -> Result<SnapshotDto, BackendError> {
        self.snapshot_dto()
    }

    fn presets(&self) -> Result<PresetCatalogDto, BackendError> {
        self.preset_catalog()
    }

    fn zone_presets(&self) -> Result<ZonePresetCatalogDto, BackendError> {
        self.zone_preset_catalog()
    }

    fn logs(&self, cursor: Option<u64>, limit: u16) -> Result<LogPageDto, BackendError> {
        if !(1..=200).contains(&limit) {
            return Err(invalid("Log limit must be between 1 and 200"));
        }
        self.log_page(cursor, limit)
    }

    fn enqueue(&self, command: RemoteCommand) -> Result<(), BackendError> {
        let bridge_command = map_command(&command.op, &command.args)?;
        self.bridge
            .try_send(bridge_command)
            .map_err(|_| unavailable("The existing Host command queue is unavailable"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelArg {
    channel: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexArg {
    index: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelIndexArg {
    channel: String,
    index: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelValueArg {
    channel: String,
    value: u16,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelSegmentValueArg {
    channel: String,
    segment: usize,
    value: u16,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelModeArg {
    channel: String,
    mode: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelIndexModeArg {
    channel: String,
    index: usize,
    mode: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelIndexValueArg {
    channel: String,
    index: usize,
    value: u16,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ZoneAddArg {
    channel: String,
    zone_id: ZoneIdDto,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelConfigArg<T> {
    config: T,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModulationArg {
    channel: String,
    kind: String,
    segment: usize,
    config: ModulationInput,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModulationClearArg {
    channel: String,
    kind: String,
    segment: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetApplyArg {
    channel: String,
    index: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavePresetArg {
    channel: String,
    name: String,
    author: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameArg {
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlarmChannelsArg {
    channels: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnabledArg {
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UkfInput {
    q: f32,
    r: f32,
    alpha: f32,
    beta: f32,
    kappa: f32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NormsInput {
    speed: f32,
    acc: f32,
    recoil: f32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModulationInput {
    source: String,
    function: ModulationFunctionDto,
    base_speed: f32,
    sensitivity: f32,
    max_deviation: f32,
    phase: f32,
    frequency_multiplier: f32,
    offset: f32,
    power: f32,
    clamp_min: f32,
    clamp_max: f32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlarmInput {
    enabled: bool,
    hour: u8,
    minute: u8,
    channels: String,
    start_strength: u8,
    peak_strength: u8,
    ramp_secs: u16,
    max_duration_secs: u16,
    repeats: u8,
    pulse_on_ms: u16,
    pulse_off_ms: u16,
    snooze_mins: u8,
}

fn map_command(operation: &str, args: &Value) -> Result<Command, BackendError> {
    let command = match operation {
        "devices.switch" => Command::SwitchDevice(decode::<IndexArg>(args)?.index),
        "zones.add" => {
            let input = decode::<ZoneAddArg>(args)?;
            let zone_type = zone_type(&input.zone_id.zone_type)?;
            if input.zone_id.name.is_empty() || input.zone_id.name.chars().count() > 256 {
                return Err(invalid("Invalid zone name"));
            }
            Command::AddZone(channel(&input.channel)?, ZoneId::new(zone_type, input.zone_id.name))
        }
        "zones.add_all_from_avatar" => Command::AddAllAvatarZones(channel(&decode::<ChannelArg>(args)?.channel)?),
        "zones.remove" => {
            let input = decode::<ChannelIndexArg>(args)?;
            Command::RemoveZone(channel(&input.channel)?, input.index)
        }
        "zones.set_mode" => {
            let input = decode::<ChannelIndexModeArg>(args)?;
            Command::SetZoneMode(channel(&input.channel)?, input.index, contact_mode(&input.mode)?)
        }
        "zones.set_scale" => {
            let input = decode::<ChannelIndexValueArg>(args)?;
            Command::SetZoneScale(
                channel(&input.channel)?,
                input.index,
                bounded_u8(input.value, 100, "Zone scale")?,
            )
        }
        "zones.set_threshold_min" => {
            let input = decode::<ChannelIndexValueArg>(args)?;
            Command::SetZoneThresholdMin(
                channel(&input.channel)?,
                input.index,
                bounded_range(input.value, 1, 100, "Minimum threshold")?,
            )
        }
        "zones.set_threshold_max" => {
            let input = decode::<ChannelIndexValueArg>(args)?;
            Command::SetZoneThresholdMax(
                channel(&input.channel)?,
                input.index,
                bounded_range(input.value, 1, 100, "Maximum threshold")?,
            )
        }
        "channels.set_frequency" => {
            let input = decode::<ChannelSegmentValueArg>(args)?;
            validate_segment(input.segment)?;
            Command::SetFrequency(
                channel(&input.channel)?,
                input.segment,
                bounded_range(input.value, 10, 255, "Frequency")?,
            )
        }
        "channels.set_intensity" => {
            let input = decode::<ChannelSegmentValueArg>(args)?;
            validate_segment(input.segment)?;
            Command::SetIntensity(
                channel(&input.channel)?,
                input.segment,
                bounded_u8(input.value, 100, "Intensity")?,
            )
        }
        "channels.set_limit_min" => {
            let input = decode::<ChannelValueArg>(args)?;
            Command::SetLimitMin(channel(&input.channel)?, bounded_u8(input.value, 200, "Power limit")?)
        }
        "channels.set_limit_max" => {
            let input = decode::<ChannelValueArg>(args)?;
            Command::SetLimitMax(channel(&input.channel)?, bounded_u8(input.value, 200, "Power limit")?)
        }
        "channels.set_aggregation" => {
            let input = decode::<ChannelModeArg>(args)?;
            Command::SetAggregation(channel(&input.channel)?, aggregation(&input.mode)?)
        }
        "config.save" => Command::SaveConfig,
        "config.load" => Command::LoadConfig,
        "tuning.set_ukf" => {
            let input = decode::<ChannelConfigArg<UkfInput>>(args)?.config;
            ensure_finite(&[input.q, input.r, input.alpha, input.beta, input.kappa])?;
            Command::SetUkf(UkfConfig {
                q: input.q,
                r: input.r,
                alpha: input.alpha,
                beta: input.beta,
                kappa: input.kappa,
            })
        }
        "tuning.reset_ukf" => Command::ResetUkf,
        "tuning.set_norms" => {
            let input = decode::<ChannelConfigArg<NormsInput>>(args)?.config;
            ensure_finite(&[input.speed, input.acc, input.recoil])?;
            Command::SetNorms(MotionNorms {
                speed: input.speed,
                acc: input.acc,
                recoil: input.recoil,
            })
        }
        "tuning.reset_norms" => Command::ResetNorms,
        "modulation.apply" => {
            let input = decode::<ModulationArg>(args)?;
            validate_segment(input.segment)?;
            if shocking_vrc_remote::protocol::modulation_function_depth(&input.config.function)
                > shocking_vrc_remote::protocol::MAX_MODULATION_DEPTH
            {
                return Err(invalid("Modulation function nesting is too deep"));
            }
            Command::ApplyModulation(
                channel(&input.channel)?,
                mod_kind(&input.kind)?,
                input.segment,
                Box::new(modulation(input.config)?),
            )
        }
        "modulation.clear_segment" => {
            let input = decode::<ModulationClearArg>(args)?;
            validate_segment(input.segment)?;
            Command::ClearModulation(channel(&input.channel)?, mod_kind(&input.kind)?, input.segment)
        }
        "modulation.clear_channel" => Command::ClearChannelModulation(channel(&decode::<ChannelArg>(args)?.channel)?),
        "presets.refresh" => Command::RefreshPresets,
        "presets.apply" => {
            let input = decode::<PresetApplyArg>(args)?;
            Command::ApplyPreset(channel(&input.channel)?, input.index)
        }
        "presets.save_user" => {
            let input = decode::<SavePresetArg>(args)?;
            validate_text(&input.name, 256, "Preset name")?;
            validate_text(&input.author, 256, "Preset author")?;
            Command::SaveUserPreset {
                channel: channel(&input.channel)?,
                name: input.name,
                author: input.author,
            }
        }
        "presets.delete_user" => Command::DeleteUserPreset(decode::<IndexArg>(args)?.index),
        "zone_presets.reload" => Command::ReloadZonePresets,
        "zone_presets.apply" => Command::ApplyZonePreset(decode::<IndexArg>(args)?.index),
        "zone_presets.save" => {
            let input = decode::<NameArg>(args)?;
            validate_text(&input.name, 256, "Zone preset name")?;
            Command::SaveZonePreset(input.name)
        }
        "zone_presets.delete" => Command::DeleteZonePreset(decode::<IndexArg>(args)?.index),
        "alarm.set_config" => {
            Command::SetAlarmConfig(Box::new(alarm_config(decode::<ChannelConfigArg<AlarmInput>>(args)?.config)?))
        }
        "alarm.set_channels" => Command::SetAlarmChannels(alarm_channels(&decode::<AlarmChannelsArg>(args)?.channels)?),
        "alarm.test" => Command::AlarmTest,
        "alarm.stop" => Command::AlarmStop,
        "alarm.snooze" => Command::AlarmSnooze,
        "prefs.set_auto_save" => Command::SetAutoSave(decode::<EnabledArg>(args)?.enabled),
        "prefs.set_alarm_tab_visible" => Command::SetAlarmTabVisible(decode::<EnabledArg>(args)?.enabled),
        _ => return Err(invalid("Unsupported remote operation")),
    };
    Ok(command)
}

fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, BackendError> {
    serde_json::from_value(value.clone()).map_err(|_| invalid("Invalid command arguments"))
}

fn channel(value: &str) -> Result<Channel, BackendError> {
    match value {
        "A" => Ok(Channel::A),
        "B" => Ok(Channel::B),
        _ => Err(invalid("Invalid channel")),
    }
}
fn zone_type(value: &str) -> Result<OldZoneType, BackendError> {
    match value {
        "Any" => Ok(OldZoneType::Any),
        "Pen" => Ok(OldZoneType::Pen),
        "Orf" => Ok(OldZoneType::Orf),
        "Touch" => Ok(OldZoneType::Touch),
        "DGB" => Ok(OldZoneType::DGB),
        _ => Err(invalid("Invalid zone type")),
    }
}
fn contact_mode(value: &str) -> Result<ContactMode, BackendError> {
    match value {
        "Depth" => Ok(ContactMode::Depth),
        "Speed" => Ok(ContactMode::Speed),
        "Acc" => Ok(ContactMode::Acc),
        "Recoil" => Ok(ContactMode::Recoil),
        _ => Err(invalid("Invalid zone mode")),
    }
}
fn aggregation(value: &str) -> Result<AggregationMode, BackendError> {
    match value {
        "Max" => Ok(AggregationMode::Max),
        "Sum" => Ok(AggregationMode::Sum),
        "Average" => Ok(AggregationMode::Average),
        _ => Err(invalid("Invalid aggregation mode")),
    }
}
fn mod_kind(value: &str) -> Result<ModKind, BackendError> {
    match value {
        "freq" => Ok(ModKind::Freq),
        "intensity" => Ok(ModKind::Intensity),
        _ => Err(invalid("Invalid modulation kind")),
    }
}
fn alarm_channels(value: &str) -> Result<AlarmChannels, BackendError> {
    match value {
        "A" => Ok(AlarmChannels::A),
        "B" => Ok(AlarmChannels::B),
        "Both" => Ok(AlarmChannels::Both),
        _ => Err(invalid("Invalid alarm channels")),
    }
}
fn bounded_u8(value: u16, max: u16, name: &str) -> Result<u8, BackendError> {
    if value <= max {
        Ok(value as u8)
    } else {
        Err(invalid(format!("{name} is out of range")))
    }
}
fn bounded_range(value: u16, min: u16, max: u16, name: &str) -> Result<u8, BackendError> {
    if (min..=max).contains(&value) {
        Ok(value as u8)
    } else {
        Err(invalid(format!("{name} is out of range")))
    }
}
fn validate_segment(segment: usize) -> Result<(), BackendError> {
    if segment < 4 {
        Ok(())
    } else {
        Err(invalid("Segment must be between 0 and 3"))
    }
}
fn validate_text(value: &str, max: usize, name: &str) -> Result<(), BackendError> {
    if !value.is_empty() && value.chars().count() <= max {
        Ok(())
    } else {
        Err(invalid(format!("{name} is invalid")))
    }
}
fn ensure_finite(values: &[f32]) -> Result<(), BackendError> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(invalid("Numeric values must be finite"))
    }
}

fn alarm_config(input: AlarmInput) -> Result<AlarmConfig, BackendError> {
    if input.hour > 23
        || input.minute > 59
        || input.start_strength > 200
        || input.peak_strength > 200
        || input.ramp_secs > 3600
        || !(30..=3600).contains(&input.max_duration_secs)
        || !(1..=20).contains(&input.repeats)
        || !(1..=60).contains(&input.snooze_mins)
    {
        return Err(invalid("Alarm configuration is out of range"));
    }
    let mut config = AlarmConfig {
        enabled: input.enabled,
        hour: input.hour,
        minute: input.minute,
        channels: alarm_channels(&input.channels)?,
        start_strength: input.start_strength,
        peak_strength: input.peak_strength,
        ramp_secs: input.ramp_secs,
        max_duration_secs: input.max_duration_secs,
        repeats: input.repeats,
        pulse_on_ms: input.pulse_on_ms,
        pulse_off_ms: input.pulse_off_ms,
        snooze_mins: input.snooze_mins,
    };
    config.sanitise();
    Ok(config)
}

fn modulation(input: ModulationInput) -> Result<ModulationConfig, BackendError> {
    let source = match input.source.as_str() {
        "Depth" => ModulationSource::Depth,
        "Speed" => ModulationSource::Speed,
        "Acc" => ModulationSource::Acc,
        "Recoil" => ModulationSource::Recoil,
        _ => return Err(invalid("Invalid modulation source")),
    };
    let values = [
        input.base_speed,
        input.sensitivity,
        input.max_deviation,
        input.phase,
        input.frequency_multiplier,
        input.offset,
        input.power,
        input.clamp_min,
        input.clamp_max,
    ];
    ensure_finite(&values)?;
    let function = modulation_function(input.function)?;
    let config = ModulationConfig {
        source,
        function,
        base_speed: input.base_speed,
        sensitivity: input.sensitivity,
        max_deviation: input.max_deviation,
        phase: input.phase,
        frequency_multiplier: input.frequency_multiplier,
        offset: input.offset,
        power: input.power,
        clamp_min: input.clamp_min,
        clamp_max: input.clamp_max,
    };
    Ok(config)
}

fn modulation_function(value: ModulationFunctionDto) -> Result<ModulationFunction, BackendError> {
    use ModulationFunctionDto as W;
    Ok(match value {
        W::None => ModulationFunction::None,
        W::Sin => ModulationFunction::Sin,
        W::Cos => ModulationFunction::Cos,
        W::Tan => ModulationFunction::Tan,
        W::Asin => ModulationFunction::Asin,
        W::Acos => ModulationFunction::Acos,
        W::Atan => ModulationFunction::Atan,
        W::SinCos => ModulationFunction::SinCos,
        W::Sin2 => ModulationFunction::Sin2,
        W::Cos2 => ModulationFunction::Cos2,
        W::SinPlusCos => ModulationFunction::SinPlusCos,
        W::SinPow { exponent } => {
            ensure_finite(&[exponent])?;
            ModulationFunction::SinPow(exponent)
        }
        W::CosPow { exponent } => {
            ensure_finite(&[exponent])?;
            ModulationFunction::CosPow(exponent)
        }
        W::Sinh => ModulationFunction::Sinh,
        W::Cosh => ModulationFunction::Cosh,
        W::Tanh => ModulationFunction::Tanh,
        W::Square => ModulationFunction::Square,
        W::Cube => ModulationFunction::Cube,
        W::Pow4 => ModulationFunction::Pow4,
        W::Sqrt => ModulationFunction::Sqrt,
        W::Cbrt => ModulationFunction::Cbrt,
        W::Abs => ModulationFunction::Abs,
        W::Sign => ModulationFunction::Sign,
        W::Exp => ModulationFunction::Exp,
        W::ExpNeg => ModulationFunction::ExpNeg,
        W::Pow2x => ModulationFunction::Pow2x,
        W::Pow10x => ModulationFunction::Pow10x,
        W::Ln => ModulationFunction::Ln,
        W::Log2 => ModulationFunction::Log2,
        W::Log10 => ModulationFunction::Log10,
        W::Triangle => ModulationFunction::Triangle,
        W::Saw => ModulationFunction::Saw,
        W::ReverseSaw => ModulationFunction::ReverseSaw,
        W::SquareWave => ModulationFunction::SquareWave,
        W::Pulse => ModulationFunction::Pulse,
        W::Bounce => ModulationFunction::Bounce,
        W::Sigmoid => ModulationFunction::Sigmoid,
        W::SmoothStep => ModulationFunction::SmoothStep,
        W::SmootherStep => ModulationFunction::SmootherStep,
        W::Logistic => ModulationFunction::Logistic,
        W::SoftSign => ModulationFunction::SoftSign,
        W::Perlin => ModulationFunction::Perlin,
        W::Simplex => ModulationFunction::Simplex,
        W::Fractal => ModulationFunction::Fractal,
        W::ValueNoise => ModulationFunction::ValueNoise,
        W::SinPlusNoise => ModulationFunction::SinPlusNoise,
        W::SinTimesNoise => ModulationFunction::SinTimesNoise,
        W::TrianglePlusSin => ModulationFunction::TrianglePlusSin,
        W::SquareTimesSigmoid => ModulationFunction::SquareTimesSigmoid,
        W::Compose { outer, inner } => ModulationFunction::Compose {
            outer: Box::new(modulation_function(*outer)?),
            inner: Box::new(modulation_function(*inner)?),
        },
    })
}

fn channel_config_dto(config: &ChannelConfig) -> Result<ChannelConfigDto, BackendError> {
    let freq_modulation = map_modulation_slots(&config.freq_modulation)?;
    let intensity_modulation = map_modulation_slots(&config.intensity_modulation)?;
    Ok(ChannelConfigDto {
        zones: config.zones.iter().map(zone_dto).collect(),
        frequency: config.frequency,
        intensity: config.intensity,
        limits: LimitsDto {
            min: config.limits.min,
            max: config.limits.max,
        },
        aggregation: aggregation_wire(config.aggregation).to_owned(),
        freq_modulation,
        intensity_modulation,
    })
}

fn zone_dto(zone: &shocking_vrc_core::cli::ZoneEntry) -> ZoneDto {
    ZoneDto {
        zone_type: zone_type_wire(zone.id.zone_type).to_owned(),
        name: zone.id.name.clone(),
        mode: contact_mode_wire(zone.mode).to_owned(),
        scale: zone.scale,
        min_threshold: zone.min_threshold,
        max_threshold: zone.max_threshold,
    }
}

fn channel_status_dto(status: &shocking_vrc_core::cli::ChannelStatus) -> ChannelStatusDto {
    ChannelStatusDto {
        raw_level: status.raw_level,
        strength: status.strength,
        frequency: status.frequency,
        active_zones: status
            .active_zones
            .iter()
            .map(|(zone_id, level)| ActiveZoneDto {
                zone_id: ZoneIdDto {
                    zone_type: zone_type_wire(zone_id.zone_type).to_owned(),
                    name: zone_id.name.clone(),
                },
                level: *level,
            })
            .collect(),
        kinematics: KinematicsDto {
            depth: status.kinematics.depth,
            speed: status.kinematics.speed,
            acc: status.kinematics.acc,
            recoil: status.kinematics.recoil,
        },
    }
}

fn alarm_config_dto(config: &AlarmConfig) -> AlarmConfigDto {
    AlarmConfigDto {
        enabled: config.enabled,
        hour: config.hour,
        minute: config.minute,
        channels: match config.channels {
            AlarmChannels::A => "A",
            AlarmChannels::B => "B",
            AlarmChannels::Both => "Both",
        }
        .to_owned(),
        start_strength: config.start_strength,
        peak_strength: config.peak_strength,
        ramp_secs: config.ramp_secs,
        max_duration_secs: config.max_duration_secs,
        repeats: config.repeats,
        pulse_on_ms: config.pulse_on_ms,
        pulse_off_ms: config.pulse_off_ms,
        snooze_mins: config.snooze_mins,
    }
}

fn zone_type_wire(value: OldZoneType) -> &'static str {
    match value {
        OldZoneType::Any => "Any",
        OldZoneType::Pen => "Pen",
        OldZoneType::Orf => "Orf",
        OldZoneType::Touch => "Touch",
        OldZoneType::DGB => "DGB",
    }
}

fn contact_mode_wire(value: ContactMode) -> &'static str {
    match value {
        ContactMode::Depth => "Depth",
        ContactMode::Speed => "Speed",
        ContactMode::Acc => "Acc",
        ContactMode::Recoil => "Recoil",
    }
}

fn aggregation_wire(value: AggregationMode) -> &'static str {
    match value {
        AggregationMode::Max => "Max",
        AggregationMode::Sum => "Sum",
        AggregationMode::Average => "Average",
    }
}

fn alarm_phase_wire(value: AlarmPhase) -> &'static str {
    match value {
        AlarmPhase::Idle => "Idle",
        AlarmPhase::Ringing => "Ringing",
        AlarmPhase::Snoozed => "Snoozed",
    }
}

fn modulation_source_wire(value: ModulationSource) -> &'static str {
    match value {
        ModulationSource::Depth => "Depth",
        ModulationSource::Speed => "Speed",
        ModulationSource::Acc => "Acc",
        ModulationSource::Recoil => "Recoil",
    }
}

fn map_modulation_slots(values: &[Option<ModulationConfig>; 4]) -> Result<[Option<ModulationDto>; 4], BackendError> {
    let mut result: [Option<ModulationDto>; 4] = std::array::from_fn(|_| None);
    for (index, config) in values.iter().enumerate() {
        if let Some(config) = config {
            ensure_finite(&[
                config.base_speed,
                config.sensitivity,
                config.max_deviation,
                config.phase,
                config.frequency_multiplier,
                config.offset,
                config.power,
                config.clamp_min,
                config.clamp_max,
            ])?;
            result[index] = Some(ModulationDto {
                source: modulation_source_wire(config.source).to_owned(),
                function: modulation_function_dto(&config.function)?,
                base_speed: config.base_speed,
                sensitivity: config.sensitivity,
                max_deviation: config.max_deviation,
                phase: config.phase,
                frequency_multiplier: config.frequency_multiplier,
                offset: config.offset,
                power: config.power,
                clamp_min: config.clamp_min,
                clamp_max: config.clamp_max,
            });
        }
    }
    Ok(result)
}

fn modulation_function_dto(value: &ModulationFunction) -> Result<ModulationFunctionDto, BackendError> {
    use ModulationFunction as C;
    Ok(match value {
        C::None => ModulationFunctionDto::None,
        C::Sin => ModulationFunctionDto::Sin,
        C::Cos => ModulationFunctionDto::Cos,
        C::Tan => ModulationFunctionDto::Tan,
        C::Asin => ModulationFunctionDto::Asin,
        C::Acos => ModulationFunctionDto::Acos,
        C::Atan => ModulationFunctionDto::Atan,
        C::SinCos => ModulationFunctionDto::SinCos,
        C::Sin2 => ModulationFunctionDto::Sin2,
        C::Cos2 => ModulationFunctionDto::Cos2,
        C::SinPlusCos => ModulationFunctionDto::SinPlusCos,
        C::SinPow(exponent) => {
            ensure_finite(&[*exponent])?;
            ModulationFunctionDto::SinPow { exponent: *exponent }
        }
        C::CosPow(exponent) => {
            ensure_finite(&[*exponent])?;
            ModulationFunctionDto::CosPow { exponent: *exponent }
        }
        C::Sinh => ModulationFunctionDto::Sinh,
        C::Cosh => ModulationFunctionDto::Cosh,
        C::Tanh => ModulationFunctionDto::Tanh,
        C::Square => ModulationFunctionDto::Square,
        C::Cube => ModulationFunctionDto::Cube,
        C::Pow4 => ModulationFunctionDto::Pow4,
        C::Sqrt => ModulationFunctionDto::Sqrt,
        C::Cbrt => ModulationFunctionDto::Cbrt,
        C::Abs => ModulationFunctionDto::Abs,
        C::Sign => ModulationFunctionDto::Sign,
        C::Exp => ModulationFunctionDto::Exp,
        C::ExpNeg => ModulationFunctionDto::ExpNeg,
        C::Pow2x => ModulationFunctionDto::Pow2x,
        C::Pow10x => ModulationFunctionDto::Pow10x,
        C::Ln => ModulationFunctionDto::Ln,
        C::Log2 => ModulationFunctionDto::Log2,
        C::Log10 => ModulationFunctionDto::Log10,
        C::Triangle => ModulationFunctionDto::Triangle,
        C::Saw => ModulationFunctionDto::Saw,
        C::ReverseSaw => ModulationFunctionDto::ReverseSaw,
        C::SquareWave => ModulationFunctionDto::SquareWave,
        C::Pulse => ModulationFunctionDto::Pulse,
        C::Bounce => ModulationFunctionDto::Bounce,
        C::Sigmoid => ModulationFunctionDto::Sigmoid,
        C::SmoothStep => ModulationFunctionDto::SmoothStep,
        C::SmootherStep => ModulationFunctionDto::SmootherStep,
        C::Logistic => ModulationFunctionDto::Logistic,
        C::SoftSign => ModulationFunctionDto::SoftSign,
        C::Perlin => ModulationFunctionDto::Perlin,
        C::Simplex => ModulationFunctionDto::Simplex,
        C::Fractal => ModulationFunctionDto::Fractal,
        C::ValueNoise => ModulationFunctionDto::ValueNoise,
        C::SinPlusNoise => ModulationFunctionDto::SinPlusNoise,
        C::SinTimesNoise => ModulationFunctionDto::SinTimesNoise,
        C::TrianglePlusSin => ModulationFunctionDto::TrianglePlusSin,
        C::SquareTimesSigmoid => ModulationFunctionDto::SquareTimesSigmoid,
        C::Compose { outer, inner } => ModulationFunctionDto::Compose {
            outer: Box::new(modulation_function_dto(outer)?),
            inner: Box::new(modulation_function_dto(inner)?),
        },
    })
}

fn ensure_snapshot_finite(snapshot: &SnapshotDto) -> Result<(), BackendError> {
    let mut values = vec![
        snapshot.config.ukf.q,
        snapshot.config.ukf.r,
        snapshot.config.ukf.alpha,
        snapshot.config.ukf.beta,
        snapshot.config.ukf.kappa,
        snapshot.config.norms.speed,
        snapshot.config.norms.acc,
        snapshot.config.norms.recoil,
    ];
    for zone in &snapshot.avatar_zones {
        values.extend([zone.level, zone.velocity, zone.acceleration, zone.recoil]);
    }
    for status in [&snapshot.status.channel_a, &snapshot.status.channel_b] {
        values.push(status.raw_level);
        values.extend([
            status.kinematics.depth,
            status.kinematics.speed,
            status.kinematics.acc,
            status.kinematics.recoil,
        ]);
        values.extend(status.active_zones.iter().map(|zone| zone.level));
    }
    ensure_finite(&values)
}

fn sanitize_log_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if [
        "bearer ",
        "authorization",
        "password",
        "access_token",
        "resume_ticket",
        "private_key",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "[redacted]".to_owned();
    }
    line.chars().take(LOG_LINE_LIMIT).collect()
}

fn invalid(message: impl Into<String>) -> BackendError {
    BackendError::new("INVALID_ARGUMENT", message)
}
fn unavailable(message: impl Into<String>) -> BackendError {
    BackendError::new("WORKER_UNAVAILABLE", message)
}

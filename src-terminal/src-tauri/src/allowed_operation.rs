use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::protocol::PROTOCOL_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "args", deny_unknown_fields)]
pub enum AllowedOperation {
    #[serde(rename = "devices.switch")]
    DevicesSwitch { index: usize },
    #[serde(rename = "zones.add")]
    ZonesAdd { channel: ChannelId, zone_id: ZoneIdInput },
    #[serde(rename = "zones.add_all_from_avatar")]
    ZonesAddAllFromAvatar { channel: ChannelId },
    #[serde(rename = "zones.remove")]
    ZonesRemove { channel: ChannelId, index: usize },
    #[serde(rename = "zones.set_mode")]
    ZonesSetMode { channel: ChannelId, index: usize, mode: ContactMode },
    #[serde(rename = "zones.set_scale")]
    ZonesSetScale { channel: ChannelId, index: usize, value: u16 },
    #[serde(rename = "zones.set_threshold_min")]
    ZonesSetThresholdMin { channel: ChannelId, index: usize, value: u16 },
    #[serde(rename = "zones.set_threshold_max")]
    ZonesSetThresholdMax { channel: ChannelId, index: usize, value: u16 },
    #[serde(rename = "channels.set_frequency")]
    ChannelsSetFrequency { channel: ChannelId, segment: usize, value: u16 },
    #[serde(rename = "channels.set_intensity")]
    ChannelsSetIntensity { channel: ChannelId, segment: usize, value: u16 },
    #[serde(rename = "channels.set_limit_min")]
    ChannelsSetLimitMin { channel: ChannelId, value: u16 },
    #[serde(rename = "channels.set_limit_max")]
    ChannelsSetLimitMax { channel: ChannelId, value: u16 },
    #[serde(rename = "channels.set_aggregation")]
    ChannelsSetAggregation { channel: ChannelId, mode: AggregationMode },
    #[serde(rename = "config.save")]
    ConfigSave {},
    #[serde(rename = "config.load")]
    ConfigLoad {},
    #[serde(rename = "tuning.set_ukf")]
    TuningSetUkf { config: UkfConfigInput },
    #[serde(rename = "tuning.reset_ukf")]
    TuningResetUkf {},
    #[serde(rename = "tuning.set_norms")]
    TuningSetNorms { config: NormsInput },
    #[serde(rename = "tuning.reset_norms")]
    TuningResetNorms {},
    #[serde(rename = "modulation.apply")]
    ModulationApply { channel: ChannelId, kind: ModulationKind, segment: usize, config: ModulationConfigInput },
    #[serde(rename = "modulation.clear_segment")]
    ModulationClearSegment { channel: ChannelId, kind: ModulationKind, segment: usize },
    #[serde(rename = "modulation.clear_channel")]
    ModulationClearChannel { channel: ChannelId },
    #[serde(rename = "presets.refresh")]
    PresetsRefresh {},
    #[serde(rename = "presets.apply")]
    PresetsApply { channel: ChannelId, index: usize },
    #[serde(rename = "presets.save_user")]
    PresetsSaveUser { channel: ChannelId, name: String, author: String },
    #[serde(rename = "presets.delete_user")]
    PresetsDeleteUser { index: usize },
    #[serde(rename = "zone_presets.reload")]
    ZonePresetsReload {},
    #[serde(rename = "zone_presets.apply")]
    ZonePresetsApply { index: usize },
    #[serde(rename = "zone_presets.save")]
    ZonePresetsSave { name: String },
    #[serde(rename = "zone_presets.delete")]
    ZonePresetsDelete { index: usize },
    #[serde(rename = "alarm.set_config")]
    AlarmSetConfig { config: AlarmConfigInput },
    #[serde(rename = "alarm.set_channels")]
    AlarmSetChannels { channels: AlarmChannels },
    #[serde(rename = "alarm.test")]
    AlarmTest {},
    #[serde(rename = "alarm.stop")]
    AlarmStop {},
    #[serde(rename = "alarm.snooze")]
    AlarmSnooze {},
    #[serde(rename = "prefs.set_auto_save")]
    PrefsSetAutoSave { enabled: bool },
    #[serde(rename = "prefs.set_alarm_tab_visible")]
    PrefsSetAlarmTabVisible { enabled: bool },
}

impl AllowedOperation {
    pub fn into_wire_frame(self, request_id: Uuid) -> Result<Value, String> {
        self.validate()?;
        let encoded = serde_json::to_value(self).map_err(|_| "Could not encode the typed Host operation".to_owned())?;
        let op = encoded
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| "Typed Host operation omitted its operation name".to_owned())?;
        let args = encoded.get("args").cloned().unwrap_or_else(|| json!({}));
        Ok(json!({
            "type": "command",
            "protocol": PROTOCOL_VERSION,
            "request_id": request_id,
            "op": op,
            "args": args
        }))
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::ZonesAdd { zone_id, .. } => validate_text(&zone_id.name, 256, "Zone name")?,
            Self::PresetsSaveUser { name, author, .. } => {
                validate_text(name, 256, "Preset name")?;
                validate_text(author, 256, "Preset author")?;
            }
            Self::ZonePresetsSave { name } => validate_text(name, 256, "Zone preset name")?,
            Self::ModulationApply { segment, config, .. } => {
                validate_segment(*segment)?;
                if function_depth(&config.function) > 8 {
                    return Err("Modulation function nesting exceeds the Remote API v1 limit".to_owned());
                }
            }
            Self::ModulationClearSegment { segment, .. } => validate_segment(*segment)?,
            Self::ChannelsSetFrequency { segment, value, .. } => {
                validate_segment(*segment)?;
                if !(10..=255).contains(value) {
                    return Err("Frequency must be between 10 and 255".to_owned());
                }
            }
            Self::ChannelsSetIntensity { segment, value, .. } => {
                validate_segment(*segment)?;
                if *value > 100 {
                    return Err("Intensity must be between 0 and 100".to_owned());
                }
            }
            Self::ZonesSetScale { value, .. } if *value > 100 => {
                return Err("Zone scale must be between 0 and 100".to_owned());
            }
            Self::ZonesSetThresholdMin { value, .. } | Self::ZonesSetThresholdMax { value, .. }
                if !(1..=100).contains(value) =>
            {
                return Err("Zone thresholds must be between 1 and 100".to_owned());
            }
            Self::ChannelsSetLimitMin { value, .. } | Self::ChannelsSetLimitMax { value, .. }
                if *value > 200 =>
            {
                return Err("Power Limits must be between 0 and 200".to_owned());
            }
            Self::AlarmSetConfig { config } => config.validate()?,
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelId {
    #[serde(rename = "A")]
    A,
    #[serde(rename = "B")]
    B,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneIdInput {
    pub zone_type: ZoneType,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ZoneType {
    Any,
    Pen,
    Orf,
    Touch,
    DGB,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContactMode {
    Depth,
    Speed,
    Acc,
    Recoil,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AggregationMode {
    Max,
    Sum,
    Average,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModulationKind {
    #[serde(rename = "freq")]
    Frequency,
    #[serde(rename = "intensity")]
    Intensity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UkfConfigInput {
    pub q: f32,
    pub r: f32,
    pub alpha: f32,
    pub beta: f32,
    pub kappa: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormsInput {
    pub speed: f32,
    pub acc: f32,
    pub recoil: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModulationConfigInput {
    pub source: ModulationSource,
    pub function: ModulationFunction,
    pub base_speed: f32,
    pub sensitivity: f32,
    pub max_deviation: f32,
    pub phase: f32,
    pub frequency_multiplier: f32,
    pub offset: f32,
    pub power: f32,
    pub clamp_min: f32,
    pub clamp_max: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModulationSource {
    Depth,
    Speed,
    Acc,
    Recoil,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModulationFunction {
    None,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    SinCos,
    Sin2,
    Cos2,
    SinPlusCos,
    SinPow { exponent: f32 },
    CosPow { exponent: f32 },
    Sinh,
    Cosh,
    Tanh,
    Square,
    Cube,
    Pow4,
    Sqrt,
    Cbrt,
    Abs,
    Sign,
    Exp,
    ExpNeg,
    Pow2x,
    Pow10x,
    Ln,
    Log2,
    Log10,
    Triangle,
    Saw,
    ReverseSaw,
    SquareWave,
    Pulse,
    Bounce,
    Sigmoid,
    SmoothStep,
    SmootherStep,
    Logistic,
    SoftSign,
    Perlin,
    Simplex,
    Fractal,
    ValueNoise,
    SinPlusNoise,
    SinTimesNoise,
    TrianglePlusSin,
    SquareTimesSigmoid,
    Compose { outer: Box<ModulationFunction>, inner: Box<ModulationFunction> },
}

fn function_depth(function: &ModulationFunction) -> usize {
    match function {
        ModulationFunction::Compose { outer, inner } => {
            1 + function_depth(outer).max(function_depth(inner))
        }
        _ => 1,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlarmChannels {
    A,
    B,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlarmConfigInput {
    pub enabled: bool,
    pub hour: u8,
    pub minute: u8,
    pub channels: AlarmChannels,
    pub start_strength: u8,
    pub peak_strength: u8,
    pub ramp_secs: u16,
    pub max_duration_secs: u16,
    pub repeats: u8,
    pub pulse_on_ms: u16,
    pub pulse_off_ms: u16,
    pub snooze_mins: u8,
}

impl AlarmConfigInput {
    fn validate(&self) -> Result<(), String> {
        if self.hour > 23
            || self.minute > 59
            || self.start_strength > 200
            || self.peak_strength > 200
            || self.ramp_secs > 3600
            || !(30..=3600).contains(&self.max_duration_secs)
            || !(1..=20).contains(&self.repeats)
            || !(1..=60).contains(&self.snooze_mins)
        {
            return Err("Alarm configuration contains a value outside the Remote API v1 range".to_owned());
        }
        Ok(())
    }
}

fn validate_segment(segment: usize) -> Result<(), String> {
    if segment < 4 {
        Ok(())
    } else {
        Err("Segment must be between 0 and 3".to_owned())
    }
}

fn validate_text(value: &str, max: usize, label: &str) -> Result<(), String> {
    if !value.is_empty() && value.chars().count() <= max {
        Ok(())
    } else {
        Err(format!("{label} must contain between 1 and {max} characters"))
    }
}

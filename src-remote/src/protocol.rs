use std::{collections::BTreeMap, fmt};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use thiserror::Error;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_STRING_CHARS: usize = 4096;
pub const MAX_JSON_DEPTH: usize = 32;
pub const MAX_MODULATION_DEPTH: usize = 8;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("message exceeds the 256 KiB frame limit")]
    FrameTooLarge,
    #[error("invalid JSON: {0}")]
    Json(String),
    #[error("message nesting exceeds the protocol limit")]
    TooDeep,
    #[error("unsupported protocol version")]
    Version,
    #[error("invalid request UUID")]
    RequestId,
    #[error("unknown operation")]
    UnknownOperation,
    #[error("invalid command arguments: {0}")]
    InvalidArguments(String),
}

/// Parses JSON while rejecting duplicate keys and excessive nesting before any
/// protocol action is taken.
pub fn parse_unique_json(bytes: &[u8]) -> Result<Value, ProtocolError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = UniqueValue { depth: 0 }
        .deserialize(&mut deserializer)
        .map_err(|error| ProtocolError::Json(error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| ProtocolError::Json(error.to_string()))?;
    Ok(value)
}

struct UniqueValue {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for UniqueValue {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        if self.depth > MAX_JSON_DEPTH {
            return Err(serde::de::Error::custom("message nesting is too deep"));
        }
        deserializer.deserialize_any(UniqueValueVisitor { depth: self.depth })
    }
}

struct UniqueValueVisitor {
    depth: usize,
}

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite numbers are not valid JSON"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.chars().count() > MAX_STRING_CHARS {
            return Err(serde::de::Error::custom("string exceeds the protocol limit"));
        }
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(UniqueValue { depth: self.depth + 1 })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if key.chars().count() > MAX_STRING_CHARS {
                return Err(serde::de::Error::custom("object key exceeds the protocol limit"));
            }
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom(format!("duplicate object key: {key}")));
            }
            let value = object.next_value_seed(UniqueValue { depth: self.depth + 1 })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    #[serde(rename = "type")]
    pub message_type: String,
    pub protocol: u8,
    pub request_id: String,
    pub op: String,
    pub args: Value,
}

impl CommandRequest {
    pub fn validate(self) -> Result<RemoteCommand, ProtocolError> {
        if self.message_type != "command" {
            return Err(ProtocolError::InvalidArguments("type must be command".into()));
        }
        if self.protocol != PROTOCOL_VERSION {
            return Err(ProtocolError::Version);
        }
        let request_id = Uuid::parse_str(&self.request_id).map_err(|_| ProtocolError::RequestId)?;
        validate_remote_command(request_id, self.op, self.args)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestCommandRequest {
    request_id: String,
    op: String,
    args: Value,
}

pub fn parse_rest_command(bytes: &[u8]) -> Result<RemoteCommand, ProtocolError> {
    let value = parse_unique_json(bytes)?;
    let request: RestCommandRequest =
        serde_json::from_value(value).map_err(|error| ProtocolError::InvalidArguments(error.to_string()))?;
    let request_id = Uuid::parse_str(&request.request_id).map_err(|_| ProtocolError::RequestId)?;
    validate_remote_command(request_id, request.op, request.args)
}

fn validate_remote_command(request_id: Uuid, op: String, args: Value) -> Result<RemoteCommand, ProtocolError> {
    if !super::operations::is_remote_operation(&op) {
        return Err(ProtocolError::UnknownOperation);
    }
    validate_arg_shape(&op, &args)?;
    Ok(RemoteCommand { request_id, op, args })
}

#[derive(Debug, Clone)]
pub struct RemoteCommand {
    pub request_id: Uuid,
    pub op: String,
    pub args: Value,
}

fn validate_arg_shape(operation: &str, args: &Value) -> Result<(), ProtocolError> {
    let object = args
        .as_object()
        .ok_or_else(|| ProtocolError::InvalidArguments("args must be an object".into()))?;
    let required: &[&str] = match operation {
        "devices.switch" | "presets.delete_user" | "zone_presets.apply" | "zone_presets.delete" => &["index"],
        "zones.add" => &["channel", "zone_id"],
        "zones.add_all_from_avatar" | "modulation.clear_channel" => &["channel"],
        "zones.remove" => &["channel", "index"],
        "zones.set_mode" => &["channel", "index", "mode"],
        "zones.set_scale" | "zones.set_threshold_min" | "zones.set_threshold_max" => &["channel", "index", "value"],
        "channels.set_frequency" | "channels.set_intensity" => &["channel", "segment", "value"],
        "channels.set_limit_min" | "channels.set_limit_max" => &["channel", "value"],
        "channels.set_aggregation" => &["channel", "mode"],
        "tuning.set_ukf" | "tuning.set_norms" => &["config"],
        "modulation.apply" => &["channel", "kind", "segment", "config"],
        "modulation.clear_segment" => &["channel", "kind", "segment"],
        "presets.apply" => &["channel", "index"],
        "presets.save_user" => &["channel", "name", "author"],
        "zone_presets.save" => &["name"],
        "alarm.set_config" => &["config"],
        "alarm.set_channels" => &["channels"],
        "prefs.set_auto_save" | "prefs.set_alarm_tab_visible" => &["enabled"],
        "config.save"
        | "config.load"
        | "tuning.reset_ukf"
        | "tuning.reset_norms"
        | "presets.refresh"
        | "zone_presets.reload"
        | "alarm.test"
        | "alarm.stop"
        | "alarm.snooze" => &[],
        _ => return Err(ProtocolError::UnknownOperation),
    };
    let allowed = required.iter().copied().collect::<std::collections::HashSet<_>>();
    if let Some(extra) = object.keys().find(|key| !allowed.contains(key.as_str())) {
        return Err(ProtocolError::InvalidArguments(format!("unexpected field {extra}")));
    }
    if let Some(missing) = required.iter().find(|key| !object.contains_key(**key)) {
        return Err(ProtocolError::InvalidArguments(format!("missing field {missing}")));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotDto {
    pub bridge_revision: u64,
    pub active_device: usize,
    pub active_device_mac: Option<String>,
    pub devices: Vec<DeviceDto>,
    pub vrchat: VrchatDto,
    pub avatar_zones: Vec<AvatarZoneDto>,
    pub config: ConfigDto,
    pub status: StatusDto,
    pub alarm: AlarmDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefs: Option<PrefsDto>,
    pub remote_policy: RemotePolicyDto,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceDto {
    pub mac: String,
    pub name: String,
    pub battery: Option<u8>,
    pub connected: bool,
    pub reconnecting: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VrchatDto {
    pub connected: bool,
    pub osc_port: u16,
    pub oscquery_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AvatarZoneDto {
    pub zone_type: String,
    pub id: String,
    pub is_tps: bool,
    pub level: f32,
    pub velocity: f32,
    pub acceleration: f32,
    pub recoil: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigDto {
    pub channel_a: ChannelConfigDto,
    pub channel_b: ChannelConfigDto,
    pub ukf: UkfDto,
    pub norms: MotionNormsDto,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelConfigDto {
    pub zones: Vec<ZoneDto>,
    pub frequency: [u8; 4],
    pub intensity: [u8; 4],
    pub limits: LimitsDto,
    pub aggregation: String,
    pub freq_modulation: [Option<ModulationDto>; 4],
    pub intensity_modulation: [Option<ModulationDto>; 4],
}

#[derive(Debug, Clone, Serialize)]
pub struct ZoneDto {
    pub zone_type: String,
    pub name: String,
    pub mode: String,
    pub scale: u8,
    pub min_threshold: u8,
    pub max_threshold: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct LimitsDto {
    pub min: u8,
    pub max: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct UkfDto {
    pub q: f32,
    pub r: f32,
    pub alpha: f32,
    pub beta: f32,
    pub kappa: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct MotionNormsDto {
    pub speed: f32,
    pub acc: f32,
    pub recoil: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModulationDto {
    pub source: String,
    pub function: ModulationFunctionDto,
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModulationFunctionDto {
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
    SinPow {
        exponent: f32,
    },
    CosPow {
        exponent: f32,
    },
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
    Compose {
        outer: Box<ModulationFunctionDto>,
        inner: Box<ModulationFunctionDto>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusDto {
    pub device_connected: bool,
    pub channel_a: ChannelStatusDto,
    pub channel_b: ChannelStatusDto,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelStatusDto {
    pub raw_level: f32,
    pub strength: u8,
    pub frequency: [u8; 4],
    pub active_zones: Vec<ActiveZoneDto>,
    pub kinematics: KinematicsDto,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveZoneDto {
    pub zone_id: ZoneIdDto,
    pub level: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneIdDto {
    pub zone_type: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KinematicsDto {
    pub depth: f32,
    pub speed: f32,
    pub acc: f32,
    pub recoil: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlarmDto {
    pub config: AlarmConfigDto,
    pub status: AlarmStatusDto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlarmConfigDto {
    pub enabled: bool,
    pub hour: u8,
    pub minute: u8,
    pub channels: String,
    pub start_strength: u8,
    pub peak_strength: u8,
    pub ramp_secs: u16,
    pub max_duration_secs: u16,
    pub repeats: u8,
    pub pulse_on_ms: u16,
    pub pulse_off_ms: u16,
    pub snooze_mins: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlarmStatusDto {
    pub phase: String,
    pub strength: u8,
    pub elapsed_secs: u32,
    pub auto_stop_in_secs: u32,
    pub snooze_left_secs: u32,
    pub next_fire_in_mins: u32,
    pub attempt: u8,
    pub retrying: bool,
    pub test: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrefsDto {
    pub auto_save: bool,
    pub alarm_tab_visible: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemotePolicyDto {
    pub remote_strength: RemoteStrengthDto,
    pub max_remote_sessions: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteStrengthDto {
    pub enabled: bool,
    pub max_by_channel: BTreeMap<String, Option<u8>>,
    pub scope: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetCatalogDto {
    pub entries: Vec<PresetEntryDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetEntryDto {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub description: String,
    pub user: bool,
    pub preset: ChannelPresetDto,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelPresetDto {
    pub name: String,
    pub description: String,
    pub author: String,
    pub frequency: [u8; 4],
    pub intensity: [u8; 4],
    pub freq_modulation: [Option<ModulationDto>; 4],
    pub intensity_modulation: [Option<ModulationDto>; 4],
}

#[derive(Debug, Clone, Serialize)]
pub struct ZonePresetCatalogDto {
    pub entries: Vec<ZonePresetEntryDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ZonePresetEntryDto {
    pub index: usize,
    pub id: String,
    pub preset: ZonePresetDto,
}

#[derive(Debug, Clone, Serialize)]
pub struct ZonePresetDto {
    pub version: u32,
    pub name: String,
    pub description: String,
    pub channel_a: Vec<ZoneDto>,
    pub channel_b: Vec<ZoneDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogPageDto {
    pub cursor: String,
    pub next_cursor: Option<String>,
    pub lines: Vec<LogLineDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLineDto {
    pub seq: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilitiesDto {
    pub operations: Vec<&'static str>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthDto {
    pub protocol_min: u8,
    pub protocol_max: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandAcceptedDto {
    pub request_id: Uuid,
    pub queued: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandReceivedDto {
    pub request_id: Uuid,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorDto {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<Uuid>,
}

pub fn modulation_function_depth(function: &ModulationFunctionDto) -> usize {
    match function {
        ModulationFunctionDto::Compose { outer, inner } => {
            1 + modulation_function_depth(outer).max(modulation_function_depth(inner))
        }
        _ => 1,
    }
}

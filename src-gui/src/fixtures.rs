use std::sync::Arc;

use shocking_vrc_app::app_state::DeviceSlotInfo;
use shocking_vrc_core::cli::{
    AggregationMode, AlarmChannels, AlarmConfig, AlarmPhase, AlarmStatus, ChannelConfig,
    ChannelStatus, CliConfig, CliStatus, ContactMode, PowerLimits, ZoneEntry, ZoneId,
};
use shocking_vrc_core::modulation::config::{
    ModulationConfig, ModulationFunction, ModulationSource,
};
use shocking_vrc_core::modulation::evaluator::KinematicsInput;
use shocking_vrc_core::presets::{ChannelPreset, PresetEntry, ZonePreset, ZonePresetEntry};
use shocking_vrc_core::{OldZoneType, ZoneEvent};

use crate::bridge::{Bridge, PresetState, Shared, Snapshot, ZonePresetState};

pub fn detached_bridge() -> (Bridge, tokio::sync::mpsc::UnboundedReceiver<crate::bridge::Command>)
{
    Bridge::detached()
}

fn zone(t: OldZoneType, name: &str, mode: ContactMode) -> ZoneEntry {
    ZoneEntry::new(ZoneId::new(t, name), mode)
}

fn modulation(function: ModulationFunction, source: ModulationSource) -> ModulationConfig {
    let mut c = ModulationConfig {
        function,
        source,
        base_speed: 1.4,
        max_deviation: 35.0,
        ..ModulationConfig::default()
    };
    c.sanitise(false);
    c
}

pub fn demo_config() -> CliConfig {
    let mut a = ChannelConfig {
        zones: vec![
            zone(OldZoneType::Orf, "Pussy", ContactMode::Depth),
            {
                let mut z = zone(OldZoneType::DGB, "TouchAreaA", ContactMode::Speed);
                z.scale = 70;
                z.min_threshold = 12;
                z.max_threshold = 88;
                z
            },
        ],
        frequency: [30, 200, 30, 200],
        intensity: [100, 90, 100, 85],
        limits: PowerLimits::new(0, 30),
        aggregation: AggregationMode::Max,
        ..ChannelConfig::default()
    };
    a.freq_modulation[0] = Some(modulation(
        ModulationFunction::Sin,
        ModulationSource::Depth,
    ));
    a.intensity_modulation[2] = Some({
        let mut c = modulation(ModulationFunction::Triangle, ModulationSource::Speed);
        c.max_deviation = 12.0;
        c.clamp_min = 0.0;
        c.clamp_max = 100.0;
        c.sanitise(true);
        c
    });

    let b = ChannelConfig {
        zones: vec![zone(OldZoneType::Pen, "Cock", ContactMode::Depth)],
        frequency: [30, 220, 60, 140],
        intensity: [100, 100, 100, 100],
        limits: PowerLimits::new(0, 42),
        aggregation: AggregationMode::Sum,
        ..ChannelConfig::default()
    };

    let mut cfg = CliConfig {
        channel_a: a,
        channel_b: b,
        ..CliConfig::default()
    };
    cfg.sanitise();
    cfg
}

fn avatar_zone(t: OldZoneType, id: &str, level: f32) -> ZoneEvent {
    ZoneEvent {
        zone_type: t,
        id: id.to_string(),
        is_tps: false,
        level,
        velocity: level * 2.0,
        acceleration: level * 6.0,
        recoil: level * 20.0,
    }
}

pub fn demo_snapshot() -> Snapshot {
    let config = demo_config();
    let status = CliStatus {
        channel_a: ChannelStatus {
            raw_level: 0.62,
            strength: 19,
            frequency: [34, 200, 30, 200],
            active_zones: vec![
                (ZoneId::new(OldZoneType::Orf, "Pussy"), 0.62),
                (ZoneId::new(OldZoneType::DGB, "TouchAreaA"), 0.28),
            ],
            kinematics: KinematicsInput {
                depth: 0.62,
                speed: 0.31,
                acc: 0.18,
                recoil: 0.05,
            },
        },
        channel_b: ChannelStatus {
            raw_level: 0.0,
            strength: 0,
            frequency: [30, 220, 60, 140],
            active_zones: Vec::new(),
            kinematics: KinematicsInput::default(),
        },
        device_connected: true,
    };

    Snapshot {
        config,
        status,
        alarm: AlarmConfig {
            enabled: true,
            hour: 7,
            minute: 30,
            channels: AlarmChannels::Both,
            ..AlarmConfig::default()
        },
        alarm_status: AlarmStatus {
            phase: AlarmPhase::Idle,
            next_fire_in_mins: 265,
            ..AlarmStatus::default()
        },
        avatar_zones: {
            let mut zones = vec![
                avatar_zone(OldZoneType::Orf, "Pussy", 0.62),
                avatar_zone(OldZoneType::Pen, "Cock", 0.0),
                avatar_zone(OldZoneType::DGB, "TouchAreaA", 0.28),
                avatar_zone(OldZoneType::DGB, "Chest", 0.0),
                avatar_zone(OldZoneType::Touch, "HeadPat", 0.0),
            ];
            for i in 0..18 {
                zones.push(avatar_zone(
                    OldZoneType::DGB,
                    &format!("Contact_{i:02}"),
                    0.0,
                ));
            }
            zones
        },
        vrchat_found: true,
        osc_port: 33777,
        oscquery_port: Some(33777),
        devices: vec![
            DeviceSlotInfo {
                mac: "AA:BB:CC:DD:EE:FF".into(),
                name: "DG-Lab Coyote V3".into(),
                battery: Some(75),
                connected: true,
                reconnecting: false,
            },
            DeviceSlotInfo {
                mac: "11:22:33:44:55:66".into(),
                name: "Coyote V3 (2)".into(),
                battery: Some(38),
                connected: false,
                reconnecting: true,
            },
        ],
        active_device: 0,
        revision: 1,
    }
}

pub fn ringing_snapshot() -> Snapshot {
    let mut s = demo_snapshot();
    s.alarm_status = AlarmStatus {
        phase: AlarmPhase::Ringing,
        strength: 22,
        elapsed_secs: 74,
        auto_stop_in_secs: 226,
        attempt: 1,
        ..AlarmStatus::default()
    };
    s
}

fn preset(id: &str, name: &str, desc: &str, author: &str, user: bool) -> PresetEntry {
    PresetEntry {
        id: id.into(),
        name: name.into(),
        description: desc.into(),
        preset: ChannelPreset {
            name: name.into(),
            description: desc.into(),
            author: author.into(),
            frequency: [180, 50, 160, 70],
            intensity: [75, 85, 80, 70],
            freq_modulation: Default::default(),
            intensity_modulation: Default::default(),
        },
        user,
    }
}

pub fn fill_shared(shared: &Arc<Shared>) {
    if let Ok(mut p) = shared.presets.write() {
        *p = PresetState {
            entries: vec![
                preset("my-warm-up", "Warm Up", "Gentle opener", "", true),
                preset("steady-hum", "Steady Hum", "A calm constant buzz", "nullshii", false),
                preset("thunder-roll", "Thunder Roll", "Rolling low pulses", "nullshii", false),
                preset("velvet-purr", "Velvet Purr", "Soft and slow", "Amigo_FPS", false),
            ],
            loading: false,
            error: None,
            source: "GitHub + mine".into(),
        };
    }
    if let Ok(mut z) = shared.zone_presets.write() {
        *z = ZonePresetState {
            entries: vec![
                ZonePresetEntry {
                    id: "my-avatar".into(),
                    preset: ZonePreset::from_config("My Avatar", &demo_config()),
                },
                ZonePresetEntry {
                    id: "minimal".into(),
                    preset: ZonePreset::from_config("Minimal", &CliConfig::default()),
                },
            ],
        };
    }
}

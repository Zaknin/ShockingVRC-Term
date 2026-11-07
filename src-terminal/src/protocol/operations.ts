export type ChannelId = "A" | "B";
export type ZoneType = "Any" | "Pen" | "Orf" | "Touch" | "DGB";
export type ContactMode = "Depth" | "Speed" | "Acc" | "Recoil";
export type AggregationMode = "Max" | "Sum" | "Average";
export type ModulationKind = "freq" | "intensity";
export type AlarmChannels = "A" | "B" | "Both";

export type ModulationFunction =
  | { kind: "none" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sin_cos" | "sin2" | "cos2" | "sin_plus_cos" }
  | { kind: "sin_pow" | "cos_pow"; exponent: number }
  | { kind: "sinh" | "cosh" | "tanh" | "square" | "cube" | "pow4" | "sqrt" | "cbrt" | "abs" | "sign" | "exp" | "exp_neg" | "pow2x" | "pow10x" | "ln" | "log2" | "log10" | "triangle" | "saw" | "reverse_saw" | "square_wave" | "pulse" | "bounce" | "sigmoid" | "smooth_step" | "smoother_step" | "logistic" | "soft_sign" | "perlin" | "simplex" | "fractal" | "value_noise" | "sin_plus_noise" | "sin_times_noise" | "triangle_plus_sin" | "square_times_sigmoid" }
  | { kind: "compose"; outer: ModulationFunction; inner: ModulationFunction };

export interface ModulationConfigInput {
  source: "Depth" | "Speed" | "Acc" | "Recoil";
  function: ModulationFunction;
  base_speed: number;
  sensitivity: number;
  max_deviation: number;
  phase: number;
  frequency_multiplier: number;
  offset: number;
  power: number;
  clamp_min: number;
  clamp_max: number;
}

export interface AlarmConfigInput {
  enabled: boolean;
  hour: number;
  minute: number;
  channels: AlarmChannels;
  start_strength: number;
  peak_strength: number;
  ramp_secs: number;
  max_duration_secs: number;
  repeats: number;
  pulse_on_ms: number;
  pulse_off_ms: number;
  snooze_mins: number;
}

export type AllowedOperation =
  | { op: "devices.switch"; args: { index: number } }
  | { op: "zones.add"; args: { channel: ChannelId; zone_id: { zone_type: ZoneType; name: string } } }
  | { op: "zones.add_all_from_avatar"; args: { channel: ChannelId } }
  | { op: "zones.remove"; args: { channel: ChannelId; index: number } }
  | { op: "zones.set_mode"; args: { channel: ChannelId; index: number; mode: ContactMode } }
  | { op: "zones.set_scale" | "zones.set_threshold_min" | "zones.set_threshold_max"; args: { channel: ChannelId; index: number; value: number } }
  | { op: "channels.set_frequency" | "channels.set_intensity"; args: { channel: ChannelId; segment: number; value: number } }
  | { op: "channels.set_limit_min" | "channels.set_limit_max"; args: { channel: ChannelId; value: number } }
  | { op: "channels.set_aggregation"; args: { channel: ChannelId; mode: AggregationMode } }
  | { op: "config.save" | "config.load" | "tuning.reset_ukf" | "tuning.reset_norms" | "presets.refresh" | "zone_presets.reload" | "alarm.test" | "alarm.stop" | "alarm.snooze"; args: Record<string, never> }
  | { op: "tuning.set_ukf"; args: { config: { q: number; r: number; alpha: number; beta: number; kappa: number } } }
  | { op: "tuning.set_norms"; args: { config: { speed: number; acc: number; recoil: number } } }
  | { op: "modulation.apply"; args: { channel: ChannelId; kind: ModulationKind; segment: number; config: ModulationConfigInput } }
  | { op: "modulation.clear_segment"; args: { channel: ChannelId; kind: ModulationKind; segment: number } }
  | { op: "modulation.clear_channel"; args: { channel: ChannelId } }
  | { op: "presets.apply"; args: { channel: ChannelId; index: number } }
  | { op: "presets.save_user"; args: { channel: ChannelId; name: string; author: string } }
  | { op: "presets.delete_user" | "zone_presets.apply" | "zone_presets.delete"; args: { index: number } }
  | { op: "zone_presets.save"; args: { name: string } }
  | { op: "alarm.set_config"; args: { config: AlarmConfigInput } }
  | { op: "alarm.set_channels"; args: { channels: AlarmChannels } }
  | { op: "prefs.set_auto_save" | "prefs.set_alarm_tab_visible"; args: { enabled: boolean } };

export const remoteOperations = [
  "devices.switch",
  "zones.add",
  "zones.add_all_from_avatar",
  "zones.remove",
  "zones.set_mode",
  "zones.set_scale",
  "zones.set_threshold_min",
  "zones.set_threshold_max",
  "channels.set_frequency",
  "channels.set_intensity",
  "channels.set_limit_min",
  "channels.set_limit_max",
  "channels.set_aggregation",
  "config.save",
  "config.load",
  "tuning.set_ukf",
  "tuning.reset_ukf",
  "tuning.set_norms",
  "tuning.reset_norms",
  "modulation.apply",
  "modulation.clear_segment",
  "modulation.clear_channel",
  "presets.refresh",
  "presets.apply",
  "presets.save_user",
  "presets.delete_user",
  "zone_presets.reload",
  "zone_presets.apply",
  "zone_presets.save",
  "zone_presets.delete",
  "alarm.set_config",
  "alarm.set_channels",
  "alarm.test",
  "alarm.stop",
  "alarm.snooze",
  "prefs.set_auto_save",
  "prefs.set_alarm_tab_visible",
] as const;

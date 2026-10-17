<script setup lang="ts">
import TerminalButton from "./TerminalButton.vue";
import TerminalPanel from "./TerminalPanel.vue";
import TerminalProgressBar from "./TerminalProgressBar.vue";
import TerminalSlider from "./TerminalSlider.vue";
import type { JsonValue, HostSnapshot } from "../protocol/types";
import type { AllowedOperation } from "../protocol/operations";

const props = defineProps<{
  snapshot: HostSnapshot;
  status: JsonValue;
  writeEnabled: boolean;
  supports: (operation: AllowedOperation["op"]) => boolean;
}>();
const emit = defineEmits<{ send: [operation: AllowedOperation] }>();
const ukfRanges = {
  q: [0.0001, 50, 0.001, 4], r: [0.0001, 0.5, 0.001, 4], alpha: [0.001, 2, 0.05, 3],
  beta: [0, 5, 0.1, 3], kappa: [-3, 3, 0.1, 3],
} as const;
const normRanges = { speed: [0.001, 40, 0.1, 2], acc: [0.001, 200, 0.5, 2], recoil: [0.001, 600, 1, 2] } as const;
const ukfLabels = { q: "Q", r: "R", alpha: "Alpha", beta: "Beta", kappa: "Kappa" } as const;
const normLabels = { speed: "Speed divisor", acc: "Accel divisor", recoil: "Recoil divisor" } as const;
function record(value: JsonValue | null | undefined): Record<string, JsonValue> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, JsonValue> : {};
}
function number(value: JsonValue | undefined): number | null { return typeof value === "number" && Number.isFinite(value) ? value : null; }
function kinematics(channel: "channel_a" | "channel_b") {
  const channelStatus = record(record(props.status)[channel]);
  const kin = record(channelStatus.kinematics);
  return { depth: number(kin.depth), speed: number(kin.speed), acc: number(kin.acc), recoil: number(kin.recoil) };
}
function setUkf(key: keyof typeof ukfRanges, value: number) {
  const current = record(props.snapshot.config.ukf);
  const config = Object.fromEntries(Object.keys(ukfRanges).map((field) => [field, number(current[field]) ?? 0.0001])) as Record<keyof typeof ukfRanges, number>;
  config[key] = value;
  emit("send", { op: "tuning.set_ukf", args: { config } });
}
function setNorm(key: keyof typeof normRanges, value: number) {
  const current = record(props.snapshot.config.norms);
  const config = Object.fromEntries(Object.keys(normRanges).map((field) => [field, number(current[field]) ?? 1])) as Record<keyof typeof normRanges, number>;
  config[key] = value;
  emit("send", { op: "tuning.set_norms", args: { config } });
}
</script>

<template>
  <div class="tuning-grid">
    <TerminalPanel kicker="FILTER CONFIGURATION" title="Unscented Kalman Filter">
      <p class="microcopy">Host-side smoothing parameters. Values are read from the current Snapshot and writes are confirmed only by a later Snapshot.</p>
      <TerminalSlider v-for="key in Object.keys(ukfRanges) as (keyof typeof ukfRanges)[]" :key="key" :label="ukfLabels[key]" :value="number(record(snapshot.config.ukf)[key]) ?? 0"
        :min="ukfRanges[key][0]" :max="ukfRanges[key][1]" :step="ukfRanges[key][2]" :precision="ukfRanges[key][3]" :revision="snapshot.bridge_revision"
        :disabled="!writeEnabled || !supports('tuning.set_ukf')" @commit="setUkf(key, $event)" />
      <div class="button-row section-gap-small">
        <TerminalButton variant="subtle" :disabled="!writeEnabled || !supports('tuning.reset_ukf')" @click="emit('send', { op: 'tuning.reset_ukf', args: {} })">RESET UKF</TerminalButton>
      </div>
    </TerminalPanel>

    <TerminalPanel kicker="MOTION NORMALISATION" title="Input divisors">
      <p class="microcopy">Divisors are owned by Host. A larger divisor makes a motion source less sensitive.</p>
      <TerminalSlider v-for="key in Object.keys(normRanges) as (keyof typeof normRanges)[]" :key="key" :label="normLabels[key]" :value="number(record(snapshot.config.norms)[key]) ?? 1"
        :min="normRanges[key][0]" :max="normRanges[key][1]" :step="normRanges[key][2]" :precision="normRanges[key][3]" :revision="snapshot.bridge_revision"
        :disabled="!writeEnabled || !supports('tuning.set_norms')" @commit="setNorm(key, $event)" />
      <div class="button-row section-gap-small">
        <TerminalButton variant="subtle" :disabled="!writeEnabled || !supports('tuning.reset_norms')" @click="emit('send', { op: 'tuning.reset_norms', args: {} })">RESET NORMALISATION</TerminalButton>
      </div>
    </TerminalPanel>

    <TerminalPanel class="tuning-live" kicker="SNAPSHOT TELEMETRY" title="Live kinematics · Channel A / B">
      <div class="status-telemetry-grid">
        <div v-for="channel in [{ id: 'A', key: 'channel_a' }, { id: 'B', key: 'channel_b' }] as const" :key="channel.id">
          <div class="zone-pair-heading"><strong>CHANNEL {{ channel.id }}</strong><span>HOST NORMALISED INPUT</span></div>
          <TerminalProgressBar v-for="mode in ['depth','speed','acc','recoil'] as const" :key="mode" :label="mode.toUpperCase()" :value="kinematics(channel.key)[mode]" :suffix="'%'" />
          <p v-if="Object.values(kinematics(channel.key)).every(value => value == null)" class="empty-note">No kinematics were included in the latest Host Snapshot.</p>
        </div>
      </div>
    </TerminalPanel>
  </div>
</template>

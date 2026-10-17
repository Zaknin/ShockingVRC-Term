<script setup lang="ts">
import TerminalButton from "./TerminalButton.vue";
import TerminalPanel from "./TerminalPanel.vue";
import TerminalSegmentedControl from "./TerminalSegmentedControl.vue";
import TerminalSlider from "./TerminalSlider.vue";
import TerminalWaveform from "./TerminalWaveform.vue";
import { formatNumber, hzToRaw, rawToHz } from "../domain/preview-math.mjs";
import type { ChannelConfig, HostSnapshot } from "../protocol/types";
import type { AllowedOperation, ChannelId } from "../protocol/operations";

const props = defineProps<{
  channel: ChannelId;
  config: ChannelConfig;
  snapshot: HostSnapshot;
  writeEnabled: boolean;
  supports: (operation: AllowedOperation["op"]) => boolean;
  strength: number | null;
}>();
const emit = defineEmits<{ send: [operation: AllowedOperation] }>();
const policy = () => props.snapshot.remote_policy.remote_strength;
const remoteLimit = () => policy().enabled ? policy().max_by_channel[props.channel] : null;
const limitMax = () => remoteLimit() == null ? 200 : Math.max(0, Math.min(200, remoteLimit()!));
const frequencyLabel = (value: number) => `${formatNumber(value, 1)} Hz`;
function operationEnabled(op: AllowedOperation["op"]) { return props.writeEnabled && props.supports(op); }
function send(op: AllowedOperation) { emit("send", op); }
</script>

<template>
  <TerminalPanel :kicker="`OUTPUT CHANNEL ${channel}`" :title="`Channel ${channel} · four-segment waveform`">
    <div class="channel-editor">
      <div class="channel-editor__summary">
        <strong>ACTUAL HOST OUTPUT · {{ strength == null ? 'NO TELEMETRY' : `${formatNumber(strength, 1)}%` }}</strong>
        <span class="term-tag">{{ config.aggregation.toUpperCase() }} ZONE MIX</span>
      </div>
      <TerminalWaveform :frequency="config.frequency" :intensity="config.intensity" :label="`Channel ${channel} Host waveform`" />

      <div class="channel-editor__range">
        <div class="eyebrow">POWER LIMITS · HOST SNAPSHOT</div>
        <div class="kv-row compact"><span>Minimum actual</span><b>{{ config.limits.min }}</b></div>
        <div class="kv-row compact"><span>Maximum actual</span><b>{{ config.limits.max }}</b></div>
        <p v-if="config.limits.max > 80" class="channel-editor__warning">High Host ceiling. Changes remain subject to the Host's remote policy.</p>
        <p v-if="policy().enabled && remoteLimit() != null" class="microcopy">Remote writes are capped at {{ remoteLimit() }} for channel {{ channel }}. Actual Host values above that cap remain visible.</p>
        <TerminalSlider label="Minimum limit" :value="config.limits.min" :min="0" :max="limitMax()" :step="1" :revision="snapshot.bridge_revision" units="power"
          :disabled="!operationEnabled('channels.set_limit_min')" @commit="send({ op: 'channels.set_limit_min', args: { channel, value: Math.round($event) } })" />
        <TerminalSlider label="Maximum limit" :value="config.limits.max" :min="0" :max="limitMax()" :step="1" :revision="snapshot.bridge_revision" units="power"
          :disabled="!operationEnabled('channels.set_limit_max')" @commit="send({ op: 'channels.set_limit_max', args: { channel, value: Math.round($event) } })" />
      </div>

      <div class="eyebrow">SEGMENT FREQUENCY / INTENSITY</div>
      <div class="channel-editor__segments">
        <article v-for="(frequency, index) in config.frequency" :key="index" class="channel-segment">
          <strong>SEG {{ index + 1 }}</strong>
          <TerminalSlider label="Frequency" :value="rawToHz(frequency)" :min="1" :max="100" :step="0.1" :precision="1" :revision="snapshot.bridge_revision" units="Hz" :formatter="frequencyLabel"
            :disabled="!operationEnabled('channels.set_frequency')" @commit="send({ op: 'channels.set_frequency', args: { channel, segment: index, value: hzToRaw($event) } })" />
          <TerminalSlider label="Intensity" :value="config.intensity[index]" :min="0" :max="100" :step="1" :revision="snapshot.bridge_revision" units="%"
            :disabled="!operationEnabled('channels.set_intensity')" @commit="send({ op: 'channels.set_intensity', args: { channel, segment: index, value: Math.round($event) } })" />
        </article>
      </div>

      <div class="channel-editor__mix">
        <span class="eyebrow">ZONE MIX</span>
        <TerminalSegmentedControl :value="config.aggregation" :options="['Max', 'Sum', 'Average']" :disabled="!operationEnabled('channels.set_aggregation')"
          label="Channel zone aggregation" @change="send({ op: 'channels.set_aggregation', args: { channel, mode: $event as 'Max' | 'Sum' | 'Average' } })" />
      </div>
      <TerminalButton variant="subtle" :disabled="!operationEnabled('config.save')" @click="send({ op: 'config.save', args: {} })">SAVE HOST CONFIG</TerminalButton>
    </div>
  </TerminalPanel>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import FunctionNodeEditor from "./FunctionNodeEditor.vue";
import TerminalButton from "./TerminalButton.vue";
import TerminalChart from "./TerminalChart.vue";
import TerminalPanel from "./TerminalPanel.vue";
import TerminalSegmentedControl from "./TerminalSegmentedControl.vue";
import TerminalSlider from "./TerminalSlider.vue";
import { composeDepth, defaultModulationConfig, formatNumber, sampleModulation } from "../domain/preview-math.mjs";
import { parseModulationFunctionJson } from "../domain/modulation-json.mjs";
import type { ChannelConfig, HostSnapshot, JsonValue } from "../protocol/types";
import type { AllowedOperation, ChannelId, ModulationConfigInput, ModulationFunction, ModulationKind } from "../protocol/operations";

const props = defineProps<{
  snapshot: HostSnapshot;
  writeEnabled: boolean;
  supports: (operation: AllowedOperation["op"]) => boolean;
}>();
const emit = defineEmits<{ send: [operation: AllowedOperation] }>();
const channel = ref<ChannelId>("A");
const kind = ref<ModulationKind>("freq");
const segment = ref(0);
const draft = ref<ModulationConfigInput>(defaultModulationConfig("freq"));
const dirty = ref(false);
const pending = ref(false);
const rejected = ref(false);
const applyRevision = ref(0);
const appliedValue = ref("");
const clearPending = ref(false);
const advancedJson = ref("");
const advancedError = ref("");
const configByChannel = computed(() => channel.value === "A" ? props.snapshot.config.channel_a : props.snapshot.config.channel_b);
const slotConfig = computed(() => (kind.value === "freq" ? configByChannel.value.freq_modulation : configByChannel.value.intensity_modulation)[segment.value]);
const activeValue = computed(() => kind.value === "freq" ? configByChannel.value.frequency[segment.value] : configByChannel.value.intensity[segment.value]);
const outputMin = computed(() => kind.value === "freq" ? 10 : 0);
const outputMax = computed(() => kind.value === "freq" ? 255 : 100);

function record(value: JsonValue | null | undefined): Record<string, JsonValue> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, JsonValue> : {};
}
function slotValue(targetChannel: ChannelId, targetKind: ModulationKind, index: number): JsonValue | null {
  const config: ChannelConfig = targetChannel === "A" ? props.snapshot.config.channel_a : props.snapshot.config.channel_b;
  return (targetKind === "freq" ? config.freq_modulation : config.intensity_modulation)[index];
}
function slotFunctionLabel(targetChannel: ChannelId, targetKind: ModulationKind, index: number): string {
  const functionValue = record(slotValue(targetChannel, targetKind, index)).function;
  const functionKind = record(functionValue).kind;
  return typeof functionKind === "string" ? functionKind.replaceAll("_", " ").toUpperCase() : "COMPOSE";
}
function snapshotConfig(value: JsonValue | null, targetKind: ModulationKind): ModulationConfigInput {
  const defaults = defaultModulationConfig(targetKind);
  const stored = record(value);
  const source = stored.source;
  if (source === "Depth" || source === "Speed" || source === "Acc" || source === "Recoil") defaults.source = source;
  if (stored.function && typeof stored.function === "object") {
    const candidate = stored.function as ModulationFunction;
    if (composeDepth(candidate) <= 8) defaults.function = candidate;
  }
  for (const key of ["base_speed", "sensitivity", "max_deviation", "phase", "frequency_multiplier", "offset", "power", "clamp_min", "clamp_max"] as const) {
    const numeric = stored[key];
    if (typeof numeric === "number" && Number.isFinite(numeric)) defaults[key] = numeric;
  }
  return defaults;
}
function loadActive() {
  draft.value = snapshotConfig(slotConfig.value, kind.value);
  dirty.value = false;
  pending.value = false;
  rejected.value = false;
  advancedError.value = "";
  advancedJson.value = JSON.stringify(draft.value.function, null, 2);
}
watch(() => [props.snapshot.bridge_revision, JSON.stringify(slotConfig.value)] as const, () => {
  if (pending.value && props.snapshot.bridge_revision > applyRevision.value) {
    if (clearPending.value) {
      clearPending.value = false;
      pending.value = false;
      if (slotConfig.value == null) {
        rejected.value = false;
        dirty.value = false;
        draft.value = defaultModulationConfig(kind.value);
      } else {
        rejected.value = true;
      }
      return;
    }
    const actual = snapshotConfig(slotConfig.value, kind.value);
    if (JSON.stringify(actual) === appliedValue.value) {
      pending.value = false;
      rejected.value = false;
      if (JSON.stringify(draft.value) === appliedValue.value) dirty.value = false;
    } else {
      pending.value = false;
      rejected.value = true;
    }
  } else if (!dirty.value) {
    loadActive();
  }
}, { immediate: true });

function selectSlot(nextChannel: ChannelId, nextKind: ModulationKind, nextSegment: number) {
  channel.value = nextChannel;
  kind.value = nextKind;
  segment.value = nextSegment;
  dirty.value = false;
  pending.value = false;
  clearPending.value = false;
  loadActive();
}
function setValue<K extends keyof ModulationConfigInput>(key: K, value: ModulationConfigInput[K]) {
  draft.value = { ...draft.value, [key]: value };
  dirty.value = true;
  rejected.value = false;
}
function updateFunction(value: ModulationFunction) { setValue("function", value); advancedJson.value = JSON.stringify(value, null, 2); }
function updateSource(value: string) {
  if (value === "Depth" || value === "Speed" || value === "Acc" || value === "Recoil") setValue("source", value);
}
function applyAdvancedJson() {
  try {
    const parsed = parseModulationFunctionJson(advancedJson.value);
    updateFunction(parsed);
    advancedError.value = "";
  } catch (error) {
    advancedError.value = `Function JSON is invalid: ${String(error)}`;
  }
}
function apply() {
  if (!dirty.value || !props.writeEnabled || !props.supports("modulation.apply")) return;
  const payload = structuredClone(draft.value);
  appliedValue.value = JSON.stringify(payload);
  applyRevision.value = props.snapshot.bridge_revision;
  pending.value = true;
  clearPending.value = false;
  rejected.value = false;
  emit("send", { op: "modulation.apply", args: { channel: channel.value, kind: kind.value, segment: segment.value, config: payload } });
}
function clearSlot() {
  if (!props.writeEnabled || !props.supports("modulation.clear_segment") || !slotConfig.value) return;
  emit("send", { op: "modulation.clear_segment", args: { channel: channel.value, kind: kind.value, segment: segment.value } });
  pending.value = true;
  appliedValue.value = "null";
  clearPending.value = true;
  applyRevision.value = props.snapshot.bridge_revision;
}
function clearChannel(target: ChannelId) {
  if (props.writeEnabled && props.supports("modulation.clear_channel")) emit("send", { op: "modulation.clear_channel", args: { channel: target } });
}
function sourceValue(targetChannel: ChannelId, source: ModulationConfigInput["source"]): number | null {
  const status = record(props.snapshot.status);
  const channelStatus = record(status[targetChannel === "A" ? "channel_a" : "channel_b"]);
  const kin = record(channelStatus.kinematics);
  const key = source.toLowerCase();
  const value = kin[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}
const selectedSourceValue = computed(() => sourceValue(channel.value, draft.value.source));
const previewPoints = computed(() => {
  if (selectedSourceValue.value == null) return [];
  try { return sampleModulation(draft.value, activeValue.value, selectedSourceValue.value, 180, 2.5); }
  catch { return []; }
});
const previewError = computed(() => {
  if (selectedSourceValue.value == null) return "No live Host kinematics for this motion source; local curve is withheld.";
  if (!previewPoints.value.length) return "Preview unavailable for this configuration.";
  return "Local JavaScript-number preview only; small rounding differences from Host f32 are possible. It sends no operation and is not measured physical output.";
});
const actualSlotLabel = computed(() => slotConfig.value ? "HOST SLOT ACTIVE" : "HOST SLOT EMPTY");
const modulationFields = [
  { key: "base_speed", label: "Base speed", min: 0, max: 10, step: 0.05, unit: "Hz", precision: 2 },
  { key: "sensitivity", label: "Sensitivity", min: 0, max: 10, step: 0.05, unit: "", precision: 3 },
  { key: "max_deviation", label: "Max deviation", min: 0, max: 245, step: 1, unit: "", precision: 1 },
  { key: "phase", label: "Phase", min: 0, max: 1, step: 0.01, unit: "cycles", precision: 2 },
  { key: "frequency_multiplier", label: "Frequency multiplier", min: 0.001, max: 8, step: 0.01, unit: "×", precision: 3 },
  { key: "offset", label: "Offset", min: -255, max: 255, step: 1, unit: "", precision: 1 },
  { key: "power", label: "Power", min: 0.001, max: 6, step: 0.05, unit: "", precision: 3 },
  { key: "clamp_min", label: "Clamp minimum", min: outputMin.value, max: outputMax.value, step: 1, unit: "", precision: 1 },
  { key: "clamp_max", label: "Clamp maximum", min: outputMin.value, max: outputMax.value, step: 1, unit: "", precision: 1 },
] as const;
const visibleFields = computed(() => modulationFields.map((field) => ({ ...field,
  max: field.key === "max_deviation" ? (kind.value === "intensity" ? 100 : 245) : field.key === "offset" ? outputMax.value : field.max,
  min: field.key === "offset" ? -outputMax.value : field.key === "clamp_min" || field.key === "clamp_max" ? outputMin.value : field.min,
  ...(field.key === "clamp_min" || field.key === "clamp_max" ? { max: outputMax.value } : {}),
})))
</script>

<template>
  <div class="modulation-layout">
    <TerminalPanel kicker="16 CONFIGURATION SLOTS" title="Channel / segment" compact>
      <div class="modulation-slot-groups">
        <div v-for="targetChannel in ['A','B'] as const" :key="targetChannel">
          <div class="zone-pair-heading"><strong>CHANNEL {{ targetChannel }}</strong><TerminalButton size="small" variant="subtle" :disabled="!writeEnabled || !supports('modulation.clear_channel')" @click="clearChannel(targetChannel)">CLEAR ALL</TerminalButton></div>
          <div v-for="targetKind in ['freq','intensity'] as const" :key="targetKind" class="modulation-slot-group">
            <strong>{{ targetKind === 'freq' ? 'FREQUENCY' : 'INTENSITY' }}</strong>
            <button v-for="slot in [0,1,2,3]" :key="slot" type="button" class="modulation-slot"
              :class="{ active: channel === targetChannel && kind === targetKind && segment === slot }"
              @click="selectSlot(targetChannel, targetKind, slot)">
              <span>SEG {{ slot + 1 }}</span>
              <small>{{ slotValue(targetChannel, targetKind, slot) ? slotFunctionLabel(targetChannel, targetKind, slot) : 'EMPTY' }}</small>
            </button>
          </div>
        </div>
      </div>
    </TerminalPanel>

    <div class="modulation-editor-column">
      <TerminalPanel :kicker="`CHANNEL ${channel} · SEGMENT ${segment + 1}`" :title="`${kind === 'freq' ? 'Frequency' : 'Intensity'} modulation`" :suffix="actualSlotLabel">
        <div class="modulation-base-row">
          <div><span class="eyebrow">HOST BASE VALUE</span><strong>{{ formatNumber(activeValue, 1) }}{{ kind === 'freq' ? ' raw' : '%' }}</strong></div>
          <div><span class="eyebrow">PREVIEW SOURCE</span><strong>{{ selectedSourceValue == null ? 'NO DATA' : `${formatNumber(selectedSourceValue, 3)} · ${draft.source}` }}</strong></div>
        </div>
        <div v-if="previewPoints.length"><TerminalChart :points="previewPoints" :label="`${kind} modulation local output preview`" /></div>
        <div v-else class="empty-note">{{ previewError }}</div>
        <p class="modulation-preview-note">{{ previewError }}</p>

        <div class="modulation-source-row">
          <label>Motion source<select :value="draft.source" :disabled="!writeEnabled" @change="updateSource(($event.target as HTMLSelectElement).value)"><option>Depth</option><option>Speed</option><option>Acc</option><option>Recoil</option></select></label>
          <span class="term-tag">{{ dirty ? 'LOCAL DRAFT' : pending ? 'WAITING FOR SNAPSHOT' : rejected ? 'HOST DID NOT CONFIRM' : actualSlotLabel }}</span>
        </div>
        <div class="function-editor-wrap">
          <div class="eyebrow">FUNCTION TREE · ALL REMOTE API VARIANTS</div>
          <FunctionNodeEditor :model-value="draft.function" :depth="0" :disabled="!writeEnabled" @update:model-value="updateFunction" />
        </div>
        <div class="modulation-editor-grid">
          <TerminalSlider v-for="field in visibleFields" :key="field.key" :label="field.label" :value="draft[field.key]" :min="field.min" :max="field.max" :step="field.step" :precision="field.precision"
            :revision="snapshot.bridge_revision" :units="field.unit" :disabled="!writeEnabled" @preview="setValue(field.key, $event)" />
        </div>

        <details class="modulation-advanced">
          <summary>ADVANCED FUNCTION JSON</summary>
          <textarea v-model="advancedJson" class="function-json mono" rows="5" :disabled="!writeEnabled" spellcheck="false"></textarea>
          <div class="button-row section-gap-small"><TerminalButton variant="subtle" :disabled="!writeEnabled" @click="applyAdvancedJson">LOAD FUNCTION JSON INTO EDITOR</TerminalButton><small v-if="advancedError" class="inline-error">{{ advancedError }}</small></div>
        </details>

        <div class="button-row section-gap-small">
          <TerminalButton variant="primary" :disabled="!dirty || !writeEnabled || !supports('modulation.apply')" @click="apply">APPLY TO HOST</TerminalButton>
          <TerminalButton variant="subtle" :disabled="!dirty" @click="loadActive">REVERT</TerminalButton>
          <TerminalButton variant="subtle" :disabled="!writeEnabled" @click="draft = defaultModulationConfig(kind); dirty = true; rejected = false">RESET DRAFT</TerminalButton>
          <TerminalButton variant="danger" :disabled="!slotConfig || !writeEnabled || !supports('modulation.clear_segment')" @click="clearSlot">CLEAR SLOT</TerminalButton>
          <small v-if="pending" class="term-control-state pending">WAITING FOR A NEW HOST SNAPSHOT</small>
          <small v-else-if="rejected" class="term-control-state rejected">HOST SNAPSHOT DID NOT MATCH THIS DRAFT</small>
        </div>
      </TerminalPanel>
    </div>
  </div>
</template>

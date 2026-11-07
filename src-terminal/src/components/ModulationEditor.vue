<script setup lang="ts">
import { reactive, ref, watch } from "vue";
import type { JsonValue } from "../protocol/types";
import type { ModulationConfigInput, ModulationFunction, ModulationKind } from "../protocol/operations";

const props = defineProps<{
  channel: "A" | "B";
  kind: ModulationKind;
  segment: number;
  current: JsonValue | null;
  disabled: boolean;
  canClear: boolean;
}>();
const emit = defineEmits<{ apply: [config: ModulationConfigInput]; clear: [] }>();

const sources = ["Depth", "Speed", "Acc", "Recoil"] as const;
const fieldNames = ["base_speed", "sensitivity", "max_deviation", "phase", "frequency_multiplier", "offset", "power", "clamp_min", "clamp_max"] as const;
type NumericField = typeof fieldNames[number];
const source = ref<ModulationConfigInput["source"]>("Depth");
const functionJson = ref('{"kind":"none"}');
const parseError = ref("");
const values = reactive<Record<NumericField, number>>({
  base_speed: 1,
  sensitivity: 1,
  max_deviation: 100,
  phase: 0,
  frequency_multiplier: 1,
  offset: 0,
  power: 1,
  clamp_min: 0,
  clamp_max: 100,
});

watch(() => JSON.stringify(props.current), () => {
  const current = props.current;
  const data = asRecord(current);
  if (!data) return;
  if (sources.includes(data.source as typeof sources[number])) source.value = data.source as typeof sources[number];
  if (data.function && typeof data.function === "object") functionJson.value = JSON.stringify(data.function, null, 2);
  for (const key of fieldNames) {
    if (typeof data[key] === "number") values[key] = data[key] as number;
  }
}, { immediate: true });

function asRecord(value: JsonValue | null): Record<string, JsonValue> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, JsonValue> : null;
}

function apply() {
  try {
    const fn = JSON.parse(functionJson.value) as ModulationFunction;
    parseError.value = "";
    emit("apply", { source: source.value, function: fn, ...values });
  } catch {
    parseError.value = "Function must be a valid Remote API v1 function object.";
  }
}
</script>

<template>
  <details class="modulation-editor">
    <summary>EDIT CHANNEL {{ channel }} · {{ kind === 'freq' ? 'FREQUENCY' : 'INTENSITY' }} · SEGMENT {{ segment + 1 }}</summary>
    <div class="form-grid section-gap-small">
      <label>Source<select v-model="source" :disabled="disabled"><option v-for="item in sources" :key="item" :value="item">{{ item }}</option></select></label>
      <div class="modulation-number-grid">
        <label v-for="field in fieldNames" :key="field">{{ field.replaceAll('_', ' ') }}<input v-model.number="values[field]" type="number" step="any" :disabled="disabled" /></label>
      </div>
      <label>Function JSON
        <textarea v-model="functionJson" class="mono function-json" spellcheck="false" rows="5" :disabled="disabled"></textarea>
      </label>
      <small class="field-help">Supports every Remote API v1 function, including nested Compose (maximum depth 8).</small>
      <small v-if="parseError" class="inline-error">{{ parseError }}</small>
      <div class="button-row"><button class="button primary" :disabled="disabled" @click="apply">APPLY MODULATION</button><button class="button subtle" :disabled="!canClear" @click="emit('clear')">CLEAR SEGMENT MODULATION</button></div>
    </div>
  </details>
</template>

<script setup lang="ts">
import { computed, reactive, watch } from "vue";
import { beginEdit, commitDraft, initialDraft, syncDraftOnSnapshot, updateDraft } from "../domain/editable-draft.mjs";
import { formatNumber } from "../domain/preview-math.mjs";
import TerminalNumberInput from "./TerminalNumberInput.vue";

const props = withDefaults(defineProps<{
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  revision?: number;
  disabled?: boolean;
  inverted?: boolean;
  units?: string;
  precision?: number;
  fixedPrecision?: boolean;
  formatter?: (value: number) => string;
}>(), { step: 1, revision: 0, disabled: false, inverted: false, units: "", precision: 3, fixedPrecision: false });
const emit = defineEmits<{ preview: [value: number]; commit: [value: number] }>();
const draft = reactive(initialDraft(props.value, props.revision));
const sliderValue = computed(() => Math.max(props.min, Math.min(props.max, draft.draft)));
const formatted = (value: number) => props.formatter?.(value) ?? formatNumber(value, props.precision);

watch(() => [props.value, props.revision] as const, ([hostValue, revision]) => {
  Object.assign(draft, syncDraftOnSnapshot(draft, hostValue, revision));
});

function startEdit() { Object.assign(draft, beginEdit(draft)); }
function update(value: number) {
  if (!Number.isFinite(value)) return;
  Object.assign(draft, updateDraft(draft, value));
  emit("preview", value);
}
function finishEdit() {
  const committed = commitDraft(draft, props.value, props.revision);
  Object.assign(draft, committed);
  if (Number.isFinite(committed.target) && !Object.is(committed.target, props.value)) emit("commit", committed.target);
}
</script>

<template>
  <div class="term-slider" :class="{ 'is-inverted': inverted, 'is-pending': draft.pending, 'is-rejected': draft.rejected }">
    <div class="term-slider__head">
      <label>{{ label }}</label>
      <TerminalNumberInput :value="draft.draft" :min="min" :max="max" :step="step" :precision="precision" :fixed-precision="fixedPrecision" :disabled="disabled"
        @focus="startEdit" @update:value="update" @commit="update($event); finishEdit()" />
    </div>
    <div class="term-slider__track">
      <input type="range" :min="min" :max="max" :step="step" :value="sliderValue" :disabled="disabled"
        :aria-label="label" @pointerdown="startEdit" @keydown="startEdit" @input="update(Number(($event.target as HTMLInputElement).value))"
        @change="finishEdit" />
      <span v-if="units" class="term-slider__units">{{ units }}</span>
    </div>
    <small v-if="draft.pending" class="term-control-state pending">WAITING FOR HOST SNAPSHOT</small>
    <small v-else-if="draft.rejected" class="term-control-state rejected">HOST VALUE {{ formatted(value) }} · REQUEST NOT CONFIRMED</small>
    <small v-else-if="value < min || value > max" class="term-control-state warning">HOST REPORTED {{ formatted(value) }} OUTSIDE {{ formatted(min) }}–{{ formatted(max) }}</small>
  </div>
</template>

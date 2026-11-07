<script setup lang="ts">
import { computed, reactive, watch } from "vue";
import { beginEdit, commitDraft, initialDraft, syncDraftOnSnapshot, updateDraft } from "../domain/editable-draft.mjs";
import { formatNumber } from "../domain/preview-math.mjs";
import type { ConfigurationMutation } from "../domain/configuration-mutations.mjs";
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
  prefix?: string;
  suffix?: string;
  precision?: number;
  fixedPrecision?: boolean;
  layout?: "stacked" | "row" | "parameter";
  fullWidth?: boolean;
  labelWidth?: number;
  valueWidth?: number;
  formatter?: (value: number) => string;
  mutationStatus?: ConfigurationMutation;
  mutationTarget?: number;
}>(), { step: 1, revision: 0, disabled: false, inverted: false, units: "", precision: 3, fixedPrecision: false, layout: "stacked", fullWidth: false, labelWidth: 96, valueWidth: 84 });
const emit = defineEmits<{ preview: [value: number]; commit: [value: number] }>();
const draft = reactive(initialDraft(props.value, props.revision));
const sliderValue = computed(() => Math.max(props.min, Math.min(props.max, draft.draft)));
const rangeStyle = computed(() => {
  const span = Math.max(Number.EPSILON, props.max - props.min);
  const progress = Math.max(0, Math.min(100, ((sliderValue.value - props.min) / span) * 100));
  return {
    "--slider-progress": `${progress}%`,
    "--slider-thumb-position": `${props.inverted ? 100 - progress : progress}%`,
  };
});
const formatted = (value: number) => props.formatter?.(value) ?? formatNumber(value, props.precision);

watch(() => [props.value, props.revision, props.mutationStatus?.status, props.mutationTarget, props.mutationStatus?.target] as const, ([hostValue, revision, status, displayTarget, target]) => {
  const requestedTarget = typeof displayTarget === "number" ? displayTarget : target;
  Object.assign(draft, syncDraftOnSnapshot(draft, hostValue, revision, status ?? null, typeof requestedTarget === "number" ? requestedTarget : undefined));
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
  const supersedesPendingRequest = props.mutationStatus?.status === "pending";
  if (Number.isFinite(committed.target) && (!Object.is(committed.target, props.value) || supersedesPendingRequest)) emit("commit", committed.target);
}
const mutationPhase = computed(() => props.mutationStatus?.status ?? null);
const isPending = computed(() => mutationPhase.value === "pending" || draft.pending);
const isRejected = computed(() => mutationPhase.value === "rejected" || draft.rejected);
const isUnknown = computed(() => mutationPhase.value === "unknown" || draft.unknown);
function onRangeKeydown(event: KeyboardEvent) {
  if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"].includes(event.key)) {
    startEdit();
  }
}
</script>

<template>
  <div class="term-slider" :class="[{ 'is-inverted': inverted, 'is-pending': isPending, 'is-rejected': isRejected, 'is-disabled': disabled, 'term-slider--wide': fullWidth }, `term-slider--${layout}`]"
    :style="{ '--term-slider-label-width': `${labelWidth}px`, '--term-slider-value-width': `${valueWidth}px` }">
    <div class="term-slider__head">
      <label>{{ label }}</label>
      <div class="term-slider__value">
        <TerminalNumberInput :value="draft.draft" :min="min" :max="max" :step="step" :precision="precision" :fixed-precision="fixedPrecision" :disabled="disabled" :layout="layout === 'parameter' ? 'parameter' : 'default'"
          :aria-label="`${label}${prefix ? ' plus-minus' : ''}${units || suffix ? ` ${units || suffix}` : ''}`" :prefix="prefix" :suffix="suffix"
          @focus="startEdit" @update:value="update" @commit="update($event); finishEdit()" />
      </div>
    </div>
    <div class="term-slider__track">
      <div class="term-slider__range" :style="rangeStyle">
        <span class="term-slider__track-visual"></span>
        <span class="term-slider__fill-visual"></span>
        <span class="term-slider__thumb-visual"></span>
        <input type="range" :min="min" :max="max" :step="step" :value="sliderValue" :disabled="disabled"
          :aria-label="label" @pointerdown="startEdit" @keydown="onRangeKeydown" @input="update(Number(($event.target as HTMLInputElement).value))"
          @change="finishEdit" />
      </div>
      <span class="term-slider__units" :class="{ 'is-empty': !units }" :aria-hidden="units ? undefined : 'true'">{{ units || ' ' }}</span>
    </div>
    <small v-if="isPending" class="term-control-state pending">QUEUED · WAITING FOR MATCHING HOST SNAPSHOT</small>
    <small v-else-if="isRejected" class="term-control-state rejected">HOST REJECTED · CURRENT VALUE {{ formatted(value) }}</small>
    <small v-else-if="isUnknown" class="term-control-state warning">OUTCOME UNKNOWN · RECONCILED TO HOST VALUE {{ formatted(value) }}</small>
    <small v-else-if="value < min || value > max" class="term-control-state warning">HOST REPORTED {{ formatted(value) }} OUTSIDE {{ formatted(min) }}–{{ formatted(max) }}</small>
  </div>
</template>

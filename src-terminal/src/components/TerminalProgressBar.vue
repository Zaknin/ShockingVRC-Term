<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { telemetryBarPresentation } from "../domain/telemetry-smoothing.mjs";

const props = withDefaults(defineProps<{ value: number | null; label: string; suffix?: string; precision?: number; smooth?: boolean; resetKey?: string | number | null }>(), {
  suffix: "%", precision: 0, smooth: false,
});
const previousValue = ref<number | null>(null);
watch(
  () => [props.value, props.resetKey] as const,
  ([, resetKey], [previous, previousResetKey]) => {
    previousValue.value = Object.is(resetKey, previousResetKey) ? previous : null;
  },
);
const presentation = computed(() => telemetryBarPresentation(previousValue.value, props.value, props.smooth));
</script>

<template>
  <div class="term-progress">
    <span class="term-progress__label">{{ label }}</span>
    <div class="term-progress__track"><i v-if="presentation.value != null" :style="{ width: `${presentation.value * 100}%`, transition: presentation.transitionMs ? `width ${presentation.transitionMs}ms linear` : 'none' }"></i></div>
    <b class="term-progress__value">{{ value == null ? '—' : `${(Math.max(0, Math.min(1, value)) * 100).toFixed(precision)}${suffix}` }}</b>
  </div>
</template>

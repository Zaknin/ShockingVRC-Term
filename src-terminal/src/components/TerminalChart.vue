<script setup lang="ts">
import { computed } from "vue";
import { formatNumber, modulationChartDomain } from "../domain/preview-math.mjs";

const props = withDefaults(defineProps<{
  points: readonly number[];
  baseValue: number;
  unit: string;
  sampleCount?: number;
  stepSeconds?: number;
  label?: string;
}>(), { sampleCount: 240, stepSeconds: 0.035, label: "Host modulation preview" });
const width = 720;
const height = 104;
const domain = computed(() => modulationChartDomain(props.points, props.baseValue));
const outputRange = computed(() => ({ min: Math.min(...props.points), max: Math.max(...props.points) }));
const baseLabel = computed(() => `BASE ${formatNumber(props.baseValue, 0)} ${props.unit}`);
const rangeLabel = computed(() => `${formatNumber(outputRange.value.min, 0)} – ${formatNumber(outputRange.value.max, 0)} ${props.unit}`);
const baseY = computed(() => height - Math.max(0, Math.min(1,
  (props.baseValue - domain.value.min) / Math.max(1e-9, domain.value.max - domain.value.min),
)) * height);
const endSeconds = computed(() => props.sampleCount * props.stepSeconds);
const coordinates = computed(() => {
  const range = Math.max(1e-9, domain.value.max - domain.value.min);
  return props.points.map((value, index) => {
    const x = props.points.length <= 1 ? 0 : (index / (props.points.length - 1)) * width;
    const y = height - Math.max(0, Math.min(1, (value - domain.value.min) / range)) * height;
    return `${index === 0 ? "M" : "L"}${x.toFixed(2)},${y.toFixed(2)}`;
  }).join(" ");
});
</script>

<template>
  <div class="term-chart">
    <div class="term-chart__header"><span>{{ baseLabel }}</span><span>{{ rangeLabel }}</span></div>
    <svg :viewBox="`0 0 ${width} ${height}`" preserveAspectRatio="none" role="img" :aria-label="label">
      <line x1="0" :x2="width" :y1="baseY" :y2="baseY" class="term-chart__base" />
      <path v-if="coordinates" :d="coordinates" class="term-chart__line" />
    </svg>
    <div class="term-chart__axis"><span>{{ formatNumber(stepSeconds, 3) }} s</span><span>HOST PREVIEW · {{ sampleCount }} SAMPLES</span><span>{{ formatNumber(endSeconds, 3) }} s</span></div>
  </div>
</template>

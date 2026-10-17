<script setup lang="ts">
import { computed } from "vue";
import { formatNumber, modulationChartDomain, modulationChartLabelPrecision } from "../domain/preview-math.mjs";

const props = withDefaults(defineProps<{ points: readonly number[]; height?: number; label?: string }>(), { height: 180, label: "Local modulation preview" });
const width = 720;
const domain = computed(() => modulationChartDomain(props.points));
const labelPrecision = computed(() => modulationChartLabelPrecision(domain.value));
const coordinates = computed(() => {
  const range = Math.max(1e-9, domain.value.max - domain.value.min);
  return props.points.map((value, index) => {
    const x = props.points.length <= 1 ? 0 : (index / (props.points.length - 1)) * width;
    const y = props.height - Math.max(0, Math.min(1, (value - domain.value.min) / range)) * props.height;
    return `${index === 0 ? "M" : "L"}${x.toFixed(2)},${y.toFixed(2)}`;
  }).join(" ");
});
</script>

<template>
  <div class="term-chart">
    <div class="term-chart__scale"><span>{{ formatNumber(domain.max, labelPrecision) }}</span><span>{{ formatNumber(domain.min, labelPrecision) }}</span></div>
    <svg :viewBox="`0 0 ${width} ${height}`" preserveAspectRatio="none" role="img" :aria-label="label">
      <line v-for="n in 4" :key="n" x1="0" :x2="width" :y1="height * n / 5" :y2="height * n / 5" class="term-chart__grid" />
      <path v-if="coordinates" :d="coordinates" class="term-chart__line" />
    </svg>
    <div class="term-chart__axis"><span>PREVIEW START</span><span>LOCAL PREVIEW · HOST OUTPUT UNCHANGED</span><span>PREVIEW END</span></div>
  </div>
</template>

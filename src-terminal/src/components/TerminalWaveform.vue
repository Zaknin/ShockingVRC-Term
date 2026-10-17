<script setup lang="ts">
import { computed } from "vue";
import { buildWaveformSegments } from "../domain/waveform-preview.mjs";

const props = defineProps<{ frequency: readonly number[]; intensity: readonly number[]; label?: string }>();
const segments = computed(() => buildWaveformSegments(props.frequency, props.intensity));
</script>

<template>
  <div class="term-waveform" role="img" :aria-label="label ?? 'Four-segment Host waveform preview'">
    <div v-for="segment in segments" :key="segment.index" class="term-waveform__segment" :style="{ flexBasis: segment.basis }">
      <div class="term-waveform__fill" :style="{ height: `${segment.strength}%` }"></div>
      <span>s{{ segment.index + 1 }} · {{ segment.hz }}Hz / {{ segment.intensityLabel }}</span>
    </div>
  </div>
</template>

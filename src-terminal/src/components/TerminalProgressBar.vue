<script setup lang="ts">
withDefaults(defineProps<{ value: number | null; label: string; suffix?: string; precision?: number }>(), { suffix: "%", precision: 0 });
</script>

<template>
  <div class="term-progress">
    <div class="term-progress__head"><span>{{ label }}</span><b>{{ value == null ? '—' : `${(Math.max(0, Math.min(1, value)) * 100).toFixed(precision)}${suffix}` }}</b></div>
    <div class="term-progress__track"><i v-if="value != null" :style="{ width: `${Math.max(0, Math.min(1, value)) * 100}%` }"></i></div>
  </div>
</template>

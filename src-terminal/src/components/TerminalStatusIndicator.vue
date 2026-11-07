<script setup lang="ts">
withDefaults(defineProps<{ state: "ok" | "warn" | "error" | "idle"; label: string; detail?: string }>(), { state: "idle" });
</script>

<template>
  <div class="term-status" :class="`state-${state}`">
    <span>{{ label }}</span>
    <small v-if="detail">{{ detail }}</small>
  </div>
</template>

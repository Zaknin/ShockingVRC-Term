<script setup lang="ts">
defineProps<{ value: string; options: readonly string[]; disabled?: boolean; label?: string }>();
const emit = defineEmits<{ change: [value: string] }>();
</script>

<template>
  <div class="term-segmented" :aria-label="label" role="group">
    <button v-for="option in options" :key="option" type="button" :disabled="disabled" :aria-pressed="value === option"
      :class="{ active: value === option }" @click="emit('change', option)">{{ option }}</button>
  </div>
</template>

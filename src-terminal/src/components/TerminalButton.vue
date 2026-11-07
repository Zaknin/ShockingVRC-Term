<script setup lang="ts">
withDefaults(defineProps<{
  variant?: "default" | "primary" | "subtle" | "danger";
  size?: "normal" | "small";
  type?: "button" | "submit" | "reset";
  disabled?: boolean;
  title?: string;
}>(), { variant: "default", size: "normal", type: "button", disabled: false });
</script>

<template>
  <button :type="type" class="term-button" :class="[`is-${variant}`, `is-${size}`]" :disabled="disabled" :title="title">
    <slot />
  </button>
</template>

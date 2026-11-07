<script setup lang="ts">
defineProps<{ kicker?: string; title: string; suffix?: string; compact?: boolean }>();
</script>

<template>
  <section class="terminal-panel" :class="{ compact }">
    <header class="terminal-panel__heading">
      <div class="terminal-panel__title">
        <span v-if="kicker" class="eyebrow">{{ kicker }}</span>
        <h2>{{ title }}</h2>
      </div>
      <slot name="suffix"><span v-if="suffix" class="term-tag">{{ suffix }}</span></slot>
    </header>
    <div class="terminal-panel__body"><slot /></div>
  </section>
</template>

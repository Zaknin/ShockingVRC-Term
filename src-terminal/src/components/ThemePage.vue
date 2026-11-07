<script setup lang="ts">
import { HOST_THEMES, hostPalette } from "../domain/theme-palette.mjs";
import TerminalPanel from "./TerminalPanel.vue";

defineProps<{ selectedTheme: string }>();
const emit = defineEmits<{ select: [themeId: string] }>();
</script>

<template>
  <TerminalPanel class="theme-panel" kicker="APPEARANCE" title="Theme" suffix="SAVED ON THIS TERMINAL">
    <p class="theme-intro">Choose one of the nine Host palettes. The choice stays on this Terminal.</p>
    <div class="theme-grid">
      <button v-for="theme in HOST_THEMES" :key="theme.id" class="theme-choice"
        :class="{ selected: selectedTheme === theme.id }" :aria-pressed="selectedTheme === theme.id"
        @click="emit('select', theme.id)">
        <span class="theme-swatch" :style="{ background: hostPalette(theme.id).base, color: hostPalette(theme.id).text, borderColor: hostPalette(theme.id).tokens['--line'] }">
          <i :style="{ background: hostPalette(theme.id).accent }"></i><b>Aa</b><small>{{ theme.name.toUpperCase() }}</small>
        </span>
        <strong>{{ theme.name }}</strong><small>{{ theme.id === selectedTheme ? 'SELECTED' : 'SELECT' }}</small>
      </button>
    </div>
  </TerminalPanel>
</template>

<style scoped>
.theme-intro { margin: 0 0 14px; color: var(--muted); font-size: var(--type-micro); line-height: var(--line-micro); }
.theme-grid { display: grid; grid-template-columns: repeat(3,minmax(0,1fr)); gap: 10px; }
.theme-choice { min-width: 0; display: grid; gap: 7px; padding: 7px; border: 1px solid var(--line); background: var(--surface); color: var(--text); text-align: left; cursor: pointer; }
.theme-choice.selected { border-color: var(--accent); box-shadow: inset 2px 0 var(--accent); }
.theme-swatch { min-height: 56px; display: grid; grid-template-columns: 1fr auto; grid-template-rows: 1fr auto; align-items: center; gap: 3px 8px; padding: 8px; border: 1px solid; }
.theme-swatch i { grid-row: 1 / -1; width: 100%; height: 12px; }
.theme-swatch b { font-size: var(--type-heading); line-height: var(--line-heading); }.theme-swatch small { grid-column: 1 / -1; font: var(--type-micro)/var(--line-micro) var(--mono); letter-spacing: .08em; }
.theme-choice > strong { font-size: var(--type-micro); line-height: var(--line-micro); }.theme-choice > small { color: var(--muted); font: var(--type-micro)/var(--line-micro) var(--mono); }
@media (max-width: 900px) { .theme-grid { grid-template-columns: repeat(2,minmax(0,1fr)); } }
</style>

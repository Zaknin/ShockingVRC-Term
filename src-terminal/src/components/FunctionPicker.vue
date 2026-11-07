<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import TerminalButton from "./TerminalButton.vue";
import { displayFunction, functionListFor, functionSparkline, groupFunctions } from "../domain/modulation-picker.mjs";
import type { ModulationFunction } from "../protocol/operations";

const props = defineProps<{ modelValue: ModulationFunction; disabled?: boolean }>();
const emit = defineEmits<{ "update:modelValue": [value: ModulationFunction] }>();
const open = ref(false);
const search = ref("");
const searchInput = ref<HTMLInputElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const dialog = ref<HTMLElement | null>(null);
const groups = computed(() => groupFunctions(functionListFor(props.modelValue), search.value));
const selectedName = computed(() => displayFunction(props.modelValue));

function pointsAttribute(func: ModulationFunction) {
  return functionSparkline(func).map((value, index) => `${((180 * index) / 63).toFixed(2)},${(22 - value * 18).toFixed(2)}`).join(" ");
}
async function show() {
  if (props.disabled) return;
  open.value = true;
  await nextTick();
  searchInput.value?.focus();
}
function close() {
  open.value = false;
  search.value = "";
  void nextTick(() => trigger.value?.focus());
}
function select(func: ModulationFunction) {
  emit("update:modelValue", func);
  close();
}
function handleDialogKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    close();
    return;
  }
  if (event.key !== "Tab" || !dialog.value) return;
  const focusable = [...dialog.value.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)")];
  if (!focusable.length) { event.preventDefault(); return; }
  const first = focusable[0];
  const last = focusable.at(-1)!;
  if (event.shiftKey && (document.activeElement === first || !dialog.value.contains(document.activeElement))) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && (document.activeElement === last || !dialog.value.contains(document.activeElement))) {
    event.preventDefault();
    first.focus();
  }
}
watch(open, (isOpen) => {
  if (isOpen) document.body.classList.add("function-picker-open");
  else document.body.classList.remove("function-picker-open");
});
onBeforeUnmount(() => document.body.classList.remove("function-picker-open"));
</script>

<template>
  <button ref="trigger" type="button" class="function-picker-trigger" :disabled="disabled" :aria-label="`Choose function, currently ${selectedName}`" @click="show">
    <span class="function-picker-trigger__label">{{ selectedName }}</span><span aria-hidden="true">▾</span>
  </button>
  <Teleport to="body">
    <div v-if="open" class="function-picker-backdrop" @click.self="close" @keydown.capture="handleDialogKeydown">
      <section ref="dialog" class="function-picker-dialog" role="dialog" aria-modal="true" aria-labelledby="function-picker-title" aria-describedby="function-picker-hint">
        <header class="function-picker-heading">
          <h2 id="function-picker-title">FUNCTION</h2>
          <TerminalButton variant="subtle" size="small" aria-label="Close function picker" @click="close">CLOSE</TerminalButton>
        </header>
        <p id="function-picker-hint" class="function-picker-hint">The shape the value traces over time. Hover a row to preview it.</p>
        <input ref="searchInput" v-model="search" class="function-picker-search" type="search" aria-label="Search functions" placeholder="search…" autocomplete="off" />
        <div class="function-picker-list" aria-label="Available functions">
          <template v-for="group in groups" :key="group.label">
            <h3 class="function-picker-group">{{ group.label }}</h3>
            <button v-for="func in group.functions" :key="displayFunction(func)" type="button" class="function-picker-row"
              :class="{ 'is-selected': displayFunction(func) === selectedName }" :aria-pressed="displayFunction(func) === selectedName" @click="select(func)">
              <span>{{ displayFunction(func) }}</span>
              <svg class="function-picker-sparkline" viewBox="0 0 180 24" role="img" :aria-label="`${displayFunction(func)} waveform preview`">
                <polyline :points="pointsAttribute(func)" />
              </svg>
            </button>
          </template>
          <p v-if="!groups.length" class="function-picker-empty">No function matches that search.</p>
        </div>
        <footer class="function-picker-footer"><TerminalButton variant="subtle" @click="close">CLOSE</TerminalButton><span>Currently: {{ selectedName }}</span></footer>
      </section>
    </div>
  </Teleport>
</template>

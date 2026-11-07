<script setup lang="ts">
import { computed, ref } from "vue";

const props = withDefaults(defineProps<{
  value: number;
  min?: number;
  max?: number;
  step?: number | "any";
  disabled?: boolean;
  label?: string;
  ariaLabel?: string;
  precision?: number;
  fixedPrecision?: boolean;
  prefix?: string;
  suffix?: string;
  layout?: "default" | "parameter";
}>(), { step: "any", disabled: false, precision: 4, fixedPrecision: false, prefix: "", suffix: "", layout: "default" });

const emit = defineEmits<{ "update:value": [value: number]; commit: [value: number]; focus: [] }>();
const editing = ref(false);
const displayValue = computed(() => Number.isFinite(props.value)
  ? (props.fixedPrecision ? props.value.toFixed(props.precision) : Number(props.value.toFixed(props.precision)))
  : "");
const longEditValue = computed(() => editing.value && String(displayValue.value).length > 5);

function startEdit() { editing.value = true; emit("focus"); }
function read(event: Event, action: "update" | "commit") {
  const raw = (event.target as HTMLInputElement).value;
  const parsed = raw.trim() === "" ? Number.NaN : Number(raw);
  if (!Number.isFinite(parsed)) return;
  const constrained = Math.max(props.min ?? Number.NEGATIVE_INFINITY, Math.min(props.max ?? Number.POSITIVE_INFINITY, parsed));
  if (action === "update") emit("update:value", parsed);
  else emit("commit", constrained);
}
function commitOnBlur(event: FocusEvent) {
  editing.value = false;
  const raw = (event.target as HTMLInputElement).value.trim();
  if (raw === "" || !Number.isFinite(Number(raw))) emit("commit", props.value);
  else read(event, "commit");
}
</script>

<template>
  <label class="term-number-field" :class="{ 'has-prefix': prefix || layout === 'parameter', 'has-suffix': suffix, 'is-parameter': layout === 'parameter' }">
    <span v-if="label">{{ label }}</span>
    <span v-if="prefix || layout === 'parameter'" class="term-number-field__prefix" aria-hidden="true">{{ prefix || '\u00a0' }}</span>
    <input type="number" :class="{ 'is-long-edit-value': longEditValue }" :aria-label="ariaLabel" :value="displayValue" :min="min" :max="max" :step="step" :disabled="disabled"
      @focus="startEdit" @input="read($event, 'update')" @change="read($event, 'commit')" @blur="commitOnBlur" />
    <span v-if="suffix" class="term-number-field__suffix">{{ suffix }}</span>
  </label>
</template>

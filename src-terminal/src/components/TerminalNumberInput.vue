<script setup lang="ts">
const props = withDefaults(defineProps<{
  value: number;
  min?: number;
  max?: number;
  step?: number | "any";
  disabled?: boolean;
  label?: string;
  precision?: number;
  fixedPrecision?: boolean;
}>(), { step: "any", disabled: false, precision: 4, fixedPrecision: false });

const emit = defineEmits<{ "update:value": [value: number]; commit: [value: number]; focus: [] }>();
function read(event: Event, action: "update" | "commit") {
  const raw = (event.target as HTMLInputElement).value;
  const parsed = raw.trim() === "" ? Number.NaN : Number(raw);
  if (!Number.isFinite(parsed)) return;
  const constrained = Math.max(props.min ?? Number.NEGATIVE_INFINITY, Math.min(props.max ?? Number.POSITIVE_INFINITY, parsed));
  if (action === "update") emit("update:value", parsed);
  else emit("commit", constrained);
}
function commitOnBlur(event: FocusEvent) {
  const raw = (event.target as HTMLInputElement).value.trim();
  if (raw === "" || !Number.isFinite(Number(raw))) emit("commit", props.value);
  else read(event, "commit");
}
</script>

<template>
  <label class="term-number-field">
    <span v-if="label">{{ label }}</span>
    <input type="number" :value="Number.isFinite(value) ? (fixedPrecision ? value.toFixed(precision) : Number(value.toFixed(precision))) : ''" :min="min" :max="max" :step="step" :disabled="disabled"
      @focus="emit('focus')" @input="read($event, 'update')" @change="read($event, 'commit')" @blur="commitOnBlur" />
  </label>
</template>

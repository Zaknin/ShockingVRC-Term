<script setup lang="ts">
import { computed } from "vue";
import TerminalNumberInput from "./TerminalNumberInput.vue";
import type { ModulationFunction } from "../protocol/operations";

const props = defineProps<{ modelValue: ModulationFunction; depth: number; disabled: boolean }>();
const emit = defineEmits<{ "update:modelValue": [value: ModulationFunction] }>();
const options = [
  "none", "sin", "cos", "tan", "asin", "acos", "atan", "sin_cos", "sin2", "cos2", "sin_plus_cos", "sin_pow", "cos_pow",
  "sinh", "cosh", "tanh", "square", "cube", "pow4", "sqrt", "cbrt", "abs", "sign", "exp", "exp_neg", "pow2x", "pow10x",
  "ln", "log2", "log10", "triangle", "saw", "reverse_saw", "square_wave", "pulse", "bounce", "sigmoid", "smooth_step",
  "smoother_step", "logistic", "soft_sign", "perlin", "simplex", "fractal", "value_noise", "sin_plus_noise", "sin_times_noise",
  "triangle_plus_sin", "square_times_sigmoid", "compose",
] as const;
const kind = computed(() => props.modelValue.kind);
const exponent = computed(() => props.modelValue.kind === "sin_pow" || props.modelValue.kind === "cos_pow" ? props.modelValue.exponent : 1);
const innerFunction = computed(() => props.modelValue.kind === "compose" ? props.modelValue.inner : { kind: "none" } as ModulationFunction);
const outerFunction = computed(() => props.modelValue.kind === "compose" ? props.modelValue.outer : { kind: "none" } as ModulationFunction);
const label = (value: string) => value.replaceAll("_", " ").toUpperCase();

function selectKind(value: string) {
  if (value === "compose") {
    if (props.depth >= 8) return;
    emit("update:modelValue", { kind: "compose", outer: { kind: "sin" }, inner: { kind: "sin" } });
  } else if (value === "sin_pow" || value === "cos_pow") {
    emit("update:modelValue", { kind: value, exponent: 1 } as ModulationFunction);
  } else if (options.includes(value as typeof options[number])) {
    emit("update:modelValue", { kind: value } as ModulationFunction);
  }
}

function setExponent(value: number) {
  if (props.modelValue.kind === "sin_pow" || props.modelValue.kind === "cos_pow") {
    emit("update:modelValue", { ...props.modelValue, exponent: value });
  }
}

function setChild(which: "outer" | "inner", value: ModulationFunction) {
  if (props.modelValue.kind === "compose") emit("update:modelValue", { ...props.modelValue, [which]: value });
}
</script>

<template>
  <div class="function-node" :class="{ nested: depth > 0 }">
    <label class="function-node__kind">FUNCTION · DEPTH {{ depth }}
      <select :value="kind" :disabled="disabled" @change="selectKind(($event.target as HTMLSelectElement).value)">
        <option v-for="option in options" :key="option" :value="option" :disabled="option === 'compose' && depth >= 8">{{ label(option) }}</option>
      </select>
    </label>
    <TerminalNumberInput v-if="kind === 'sin_pow' || kind === 'cos_pow'" label="Exponent" :value="exponent" :precision="6" :disabled="disabled" @update:value="setExponent" />
    <div v-else-if="kind === 'compose'" class="function-node__children">
      <FunctionNodeEditor :model-value="innerFunction" :depth="depth + 1" :disabled="disabled" @update:model-value="setChild('inner', $event)" />
      <FunctionNodeEditor :model-value="outerFunction" :depth="depth + 1" :disabled="disabled" @update:model-value="setChild('outer', $event)" />
    </div>
  </div>
</template>

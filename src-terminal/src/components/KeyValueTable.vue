<script setup lang="ts">
import type { JsonValue } from "../protocol/types";

defineProps<{ value: JsonValue | undefined | null; emptyLabel?: string }>();

function display(value: JsonValue | undefined | null): string {
  if (value === null || value === undefined) return "—";
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}
</script>

<template>
  <div v-if="value && typeof value === 'object' && !Array.isArray(value)" class="kv-list">
    <div v-for="([key, item]) in Object.entries(value)" :key="key" class="kv-row">
      <span>{{ key.replaceAll('_', ' ') }}</span><strong>{{ display(item as JsonValue) }}</strong>
    </div>
  </div>
  <div v-else class="empty-note">{{ emptyLabel || 'Host has not supplied this state.' }}</div>
</template>

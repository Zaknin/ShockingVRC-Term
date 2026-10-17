<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import type { HostLogLine } from "../protocol/types";
import { filterLogEntries } from "../domain/log-format.mjs";

const props = defineProps<{ lines: readonly (HostLogLine | string)[]; emptyLabel?: string; title?: string }>();
const emit = defineEmits<{ copy: [lines: string[]] }>();
const filter = ref<"ALL" | "ERROR" | "WARN" | "INFO" | "DEBUG">("ALL");
const query = ref("");
const follow = ref(true);
const viewCleared = ref(false);
const viewport = ref<HTMLElement | null>(null);
const visible = computed(() => viewCleared.value ? [] : filterLogEntries(props.lines, filter.value, query.value));
watch(() => props.lines, () => { viewCleared.value = false; });
watch(() => visible.value.length, async () => {
  if (!follow.value) return;
  await nextTick();
  if (viewport.value) viewport.value.scrollTop = viewport.value.scrollHeight;
});
</script>

<template>
  <div class="term-log-viewer">
    <div class="term-log-toolbar">
      <strong>{{ title ?? 'LOG' }}</strong>
      <select v-model="filter" aria-label="Filter log level"><option>ALL</option><option>ERROR</option><option>WARN</option><option>INFO</option><option>DEBUG</option></select>
      <input v-model="query" aria-label="Filter log text" placeholder="Filter text…" />
      <button type="button" :aria-pressed="follow" @click="follow = !follow">{{ follow ? 'FOLLOW' : 'HOLD' }}</button>
      <button type="button" :disabled="!visible.length" @click="emit('copy', visible.map(line => line.text))">COPY</button>
      <button type="button" :disabled="!visible.length" @click="viewCleared = true">CLEAR VIEW</button>
    </div>
    <div ref="viewport" class="term-log-lines" aria-live="polite">
      <article v-for="line in visible" :key="line.seq" class="term-log-line" :class="`level-${line.level.toLowerCase()}`">
        <span class="term-log-line__seq">{{ line.seq }}</span><time>{{ line.timestamp ?? '—' }}</time><code>{{ line.text }}</code>
      </article>
      <p v-if="!visible.length" class="term-log-empty">{{ emptyLabel ?? 'No matching log lines.' }}</p>
    </div>
  </div>
</template>

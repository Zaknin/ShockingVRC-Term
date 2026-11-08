<script setup lang="ts">
import { computed } from "vue";
import TerminalButton from "./TerminalButton.vue";
import type { EngineModeChange } from "../domain/graph-session.mjs";
import type { EngineMode, GraphDocument, GraphPresetCatalog } from "../protocol/types";

const props = defineProps<{
  engineMode: string | null | undefined;
  modeChange: EngineModeChange;
  connected: boolean;
  capabilities: string[];
  document: GraphDocument | null;
  presets: GraphPresetCatalog | null;
  loadStatus: "idle" | "loading" | "loaded" | "error" | "unsupported" | "reconnecting";
  loadError: string;
}>();

const emit = defineEmits<{
  refresh: [];
  requestMode: [mode: EngineMode];
}>();

const documentReadSupported = computed(() => props.capabilities.includes("graph.document.read.v1"));
const presetsSupported = computed(() => props.capabilities.includes("graph.presets.catalog.v1"));
const modeChangeSupported = computed(() => props.capabilities.includes("engine.mode.set.v1"));
const modeBusy = computed(() => ["pending", "queued", "reconnecting"].includes(props.modeChange.status));
const selectedMode = computed(() => modeBusy.value && props.modeChange.requestedMode
  ? props.modeChange.requestedMode
  : props.engineMode === "classic" || props.engineMode === "graph" ? props.engineMode : "");
const canRefresh = computed(() => props.connected && documentReadSupported.value && props.loadStatus !== "loading");
const modeStatusClass = computed(() => `term-control-state ${props.modeChange.status}`);

function requestMode(event: Event) {
  const mode = (event.target as HTMLSelectElement).value;
  if (mode === "classic" || mode === "graph") emit("requestMode", mode);
}

function nodeKindLabel(node: GraphDocument["graph"]["nodes"][number]) {
  if (node.title?.trim()) return node.title.trim();
  if (typeof node.kind === "string") return node.kind;
  if (node.kind && typeof node.kind === "object") {
    try { return JSON.stringify(node.kind); } catch { return "Graph node"; }
  }
  return "Graph node";
}

function edgeLabel(edge: GraphDocument["graph"]["edges"][number]) {
  return `Node ${edge.from.node} · ${edge.from.pin} → Node ${edge.to.node} · ${edge.to.pin}`;
}
</script>

<template>
  <div class="graph-workspace">
    <section class="panel graph-mode-panel">
      <div class="panel-heading">
        <div><span class="eyebrow">HOST ENGINE</span><h3>Engine mode</h3></div>
        <span class="tag">{{ engineMode?.toUpperCase() ?? 'UNKNOWN' }}</span>
      </div>
      <div v-if="modeChangeSupported" class="graph-mode-control">
        <label>Change mode
          <select aria-label="Engine mode" class="select-compact" :value="selectedMode" :disabled="!connected || modeBusy" @change="requestMode">
            <option value="" disabled>Awaiting Host state</option>
            <option value="classic">Classic</option>
            <option value="graph">Graph</option>
          </select>
        </label>
        <small :class="modeStatusClass" role="status">{{ modeChange.message || 'Mode changes require a newer Host snapshot for confirmation.' }}</small>
      </div>
      <p v-else class="empty-note">Engine mode switching is not supported by this Host.</p>
      <small v-if="modeChange.status === 'unsupported'" class="term-control-state unsupported">{{ modeChange.message }}</small>
    </section>

    <section class="panel graph-document-panel">
      <div class="panel-heading">
        <div><span class="eyebrow">HOST GRAPH</span><h3>Read-only workspace</h3></div>
        <div class="button-row">
          <span class="tag">READ ONLY</span>
          <TerminalButton size="small" :disabled="!canRefresh" @click="emit('refresh')">REFRESH</TerminalButton>
        </div>
      </div>

      <p v-if="!documentReadSupported" class="empty-note">This Host does not support Graph document reads.</p>
      <p v-else-if="loadStatus === 'reconnecting'" class="empty-note" role="status">Reconnecting to the Host. Graph state will load after a fresh snapshot.</p>
      <p v-else-if="loadStatus === 'loading'" class="empty-note" role="status">Loading the Host Graph document…</p>
      <div v-else-if="loadStatus === 'error'" class="error-banner graph-inline-error" role="alert"><strong>Graph read failed</strong><small>{{ loadError }}</small></div>
      <p v-else-if="!document" class="empty-note">Connect to a Host and refresh to read its current Graph document.</p>

      <template v-if="document && documentReadSupported">
        <div class="graph-document-summary">
          <div><span class="eyebrow">GRAPH REVISION</span><strong>{{ document.graph_revision }}</strong></div>
          <div><span class="eyebrow">VALIDATION</span><strong :class="document.valid ? 'graph-valid' : 'graph-invalid'">{{ document.valid ? 'VALID' : 'ISSUES FOUND' }}</strong></div>
          <div><span class="eyebrow">OUTPUT SAFETY</span><strong>{{ document.editor.panic ? 'PANIC ACTIVE' : 'NORMAL' }}</strong></div>
          <div><span class="eyebrow">UI PANIC</span><strong>{{ document.editor.ui_panic ? 'ACTIVE' : 'CLEAR' }}</strong></div>
        </div>
        <p v-if="document.editor.panic || document.editor.ui_panic" class="graph-panic-note" role="status">
          Host reports a panic state. Terminal displays this state and provides no panic release control.
        </p>

        <div class="graph-read-columns">
          <section class="graph-read-list" aria-labelledby="graph-nodes-heading">
            <div class="panel-heading"><div><span class="eyebrow">{{ document.graph.nodes.length }} ITEMS</span><h4 id="graph-nodes-heading">Nodes</h4></div></div>
            <div v-if="document.graph.nodes.length" class="graph-entry-list">
              <div v-for="node in document.graph.nodes" :key="node.id" class="graph-entry">
                <span class="graph-entry-id">{{ node.id }}</span><strong>{{ nodeKindLabel(node) }}</strong>
                <small v-if="node.comment">{{ node.comment }}</small>
                <small v-else class="graph-position">{{ node.pos[0] }}, {{ node.pos[1] }}</small>
              </div>
            </div>
            <p v-else class="empty-note">The Host Graph has no nodes.</p>
          </section>

          <section class="graph-read-list" aria-labelledby="graph-connections-heading">
            <div class="panel-heading"><div><span class="eyebrow">{{ document.graph.edges.length }} LINKS</span><h4 id="graph-connections-heading">Connections</h4></div></div>
            <div v-if="document.graph.edges.length" class="graph-entry-list">
              <div v-for="(edge, index) in document.graph.edges" :key="`${edge.from.node}-${edge.from.pin}-${edge.to.node}-${edge.to.pin}-${index}`" class="graph-entry graph-edge">
                <span class="graph-edge-source">{{ edge.from.node }} · {{ edge.from.pin }}</span><span aria-hidden="true">→</span><span class="graph-edge-target">{{ edge.to.node }} · {{ edge.to.pin }}</span>
              </div>
            </div>
            <p v-else class="empty-note">The Host Graph has no connections.</p>
          </section>
        </div>

        <section class="graph-validation" aria-labelledby="graph-validation-heading">
          <div class="panel-heading"><div><span class="eyebrow">{{ document.issues.length }} REPORTS</span><h4 id="graph-validation-heading">Validation</h4></div></div>
          <ul v-if="document.issues.length" class="graph-issues">
            <li v-for="(issue, index) in document.issues" :key="`${issue.node_id ?? 'graph'}-${index}`">
              <span class="tag">{{ issue.severity.toUpperCase() }}</span><span>{{ issue.message }}</span><small v-if="issue.node_id !== null">NODE {{ issue.node_id }}</small>
            </li>
          </ul>
          <p v-else class="empty-note">Host reports no Graph validation issues.</p>
        </section>
      </template>
    </section>

    <section v-if="presetsSupported" class="panel graph-presets-panel">
      <div class="panel-heading"><div><span class="eyebrow">HOST CATALOG</span><h3>Graph presets</h3></div><span class="tag">READ ONLY</span></div>
      <div v-if="presets?.entries.length" class="graph-presets-list">
        <article v-for="preset in presets.entries" :key="preset.id" class="graph-preset-entry">
          <strong>{{ preset.name }}</strong><span class="tag">{{ preset.kind.toUpperCase() }}</span>
          <small>{{ preset.description || 'No description supplied.' }}</small><small>{{ preset.node_count }} nodes{{ preset.user ? ' · USER' : '' }}</small>
        </article>
      </div>
      <p v-else class="empty-note">{{ presets ? 'The Host has no Graph presets.' : 'Graph presets have not been loaded.' }}</p>
    </section>
  </div>
</template>

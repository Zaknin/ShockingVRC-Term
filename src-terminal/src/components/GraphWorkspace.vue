<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import TerminalButton from "./TerminalButton.vue";
import {
  addGraphNode,
  cloneGraph,
  connectGraphPins,
  disconnectGraphEdge,
  editGraphNodeParameter,
  graphNodePins,
  graphNodeTemplate,
  moveGraphNodes,
  removeGraphNodes,
  setGraphInputDefault,
} from "../domain/graph-editor.mjs";
import { clampContextMenuPosition, clientToGraphPosition, clientToViewBoxPosition } from "../domain/graph-canvas.mjs";
import type { EngineModeChange } from "../domain/graph-session.mjs";
import type {
  EngineMode,
  GraphApplyResult,
  GraphDocument,
  GraphDraftValidation,
  GraphModel,
  GraphNode,
  GraphNodeCatalog,
  GraphNodeTemplate,
  GraphPresetCatalog,
} from "../protocol/types";

interface GraphDraft {
  graph: GraphModel;
  baseRevision: number | null;
  stale: boolean;
  dirty: boolean;
  saved: boolean;
  valid: boolean;
  issues: GraphDocument["issues"];
  missingTypes: string[];
  validating: boolean;
}

interface DragState {
  mode: "node" | "pan";
  pointerId: number;
  startClient: [number, number];
  initialGraph?: GraphModel;
  initialPositions?: Map<number, [number, number]>;
  initialPan?: [number, number];
}

const VIEW_WIDTH = 960;
const VIEW_HEIGHT = 620;
const NODE_WIDTH = 194;
const PIN_TOP = 68;
const PIN_GAP = 22;
const EMPTY_GRAPH: GraphModel = { version: 1, nodes: [], edges: [] };

const props = defineProps<{
  engineMode: string | null | undefined;
  modeChange: EngineModeChange;
  connected: boolean;
  capabilities: string[];
  document: GraphDocument | null;
  draft: GraphDraft | null;
  nodeCatalog: GraphNodeCatalog | null;
  presets: GraphPresetCatalog | null;
  loadStatus: "idle" | "loading" | "loaded" | "error" | "unsupported" | "reconnecting";
  loadError: string;
  draftError: string;
  applyResult: GraphApplyResult | null;
  draftLoading: boolean;
  canUndo: boolean;
  canRedo: boolean;
}>();

const emit = defineEmits<{
  refresh: [];
  requestMode: [mode: EngineMode];
  updateDraft: [payload: { graph: GraphModel; undoFrom?: GraphModel; validation?: GraphDraftValidation }];
  saveDraft: [];
  reloadDraft: [];
  discardDraft: [];
  importDraft: [json: string];
  transformDraft: [operation: Record<string, unknown>];
  applyPreset: [presetId: string, mode: "replace" | "insert", position: [number, number]];
  applyDraft: [];
  validateDraft: [];
  undo: [];
  redo: [];
}>();

const documentReadSupported = computed(() => props.capabilities.includes("graph.document.read.v1"));
const presetsSupported = computed(() => props.capabilities.includes("graph.presets.catalog.v1"));
const presetDetailSupported = computed(() => props.capabilities.includes("graph.presets.detail.chunked.v1"));
const modeChangeSupported = computed(() => props.capabilities.includes("engine.mode.set.v1"));
const applySupported = computed(() => props.capabilities.includes("graph.apply.upload.status.v1"));
const modeBusy = computed(() => ["pending", "queued", "reconnecting"].includes(props.modeChange.status));
const selectedMode = computed(() => modeBusy.value && props.modeChange.requestedMode
  ? props.modeChange.requestedMode
  : props.engineMode === "classic" || props.engineMode === "graph" ? props.engineMode : "");
const canRefresh = computed(() => props.connected && documentReadSupported.value && props.loadStatus !== "loading");
const modeStatusClass = computed(() => `term-control-state ${props.modeChange.status}`);
const workingGraph = ref<GraphModel>(cloneGraph(EMPTY_GRAPH));
const svg = ref<SVGSVGElement | null>(null);
const selectedNodeIds = ref<number[]>([]);
const linkSource = ref<{ node: number; pin: string } | null>(null);
const dragState = ref<DragState | null>(null);
const canvasZoom = ref(1);
const canvasPan = ref<[number, number]>([24, 24]);
const paletteSearch = ref("");
const selectedTemplateId = ref("");
const selectedPresetId = ref("");
const presetMode = ref<"replace" | "insert">("replace");
const groupName = ref("Group");
const localError = ref("");
const fileInput = ref<HTMLInputElement | null>(null);
const addNodeMenuOpen = ref(false);
const addNodeMenu = ref<HTMLElement | null>(null);
const addNodeSearchInput = ref<HTMLInputElement | null>(null);
const addNodeMenuSearch = ref("");
const addNodeMenuIndex = ref(0);
const addNodeMenuAnchor = ref<[number, number]>([0, 0]);
const addNodeMenuGraphPosition = ref<[number, number]>([0, 0]);
const addNodeMenuStyle = ref({ left: "0px", top: "0px" });

watch(() => props.draft?.graph, (graph) => {
  if (graph) workingGraph.value = cloneGraph(graph);
}, { immediate: true });

watch(() => props.nodeCatalog, (catalog) => {
  if (!selectedTemplateId.value && catalog?.entries.length) selectedTemplateId.value = catalog.entries[0].id;
}, { immediate: true });

const graph = computed(() => props.draft?.graph ?? props.document?.graph ?? workingGraph.value);
const selectedNodes = computed(() => graph.value.nodes.filter((node) => selectedNodeIds.value.includes(node.id)));
const selectedNode = computed(() => selectedNodes.value.length === 1 ? selectedNodes.value[0] : null);
const selectedTemplate = computed(() => props.nodeCatalog?.entries.find((entry) => entry.id === selectedTemplateId.value) ?? null);
const filteredTemplates = computed(() => {
  const needle = paletteSearch.value.trim().toLowerCase();
  return (props.nodeCatalog?.entries ?? []).filter((entry) => !needle
    || `${entry.label} ${entry.category} ${entry.keywords} ${entry.id}`.toLowerCase().includes(needle));
});
const addNodeMenuTemplates = computed(() => {
  const needle = addNodeMenuSearch.value.trim().toLowerCase();
  return (props.nodeCatalog?.entries ?? []).filter((entry) => !needle
    || `${entry.label} ${entry.category} ${entry.keywords} ${entry.id}`.toLowerCase().includes(needle));
});
const addNodeMenuGroups = computed(() => {
  const groups = new Map<string, GraphNodeTemplate[]>();
  for (const template of addNodeMenuTemplates.value) {
    const entries = groups.get(template.category) ?? [];
    entries.push(template);
    groups.set(template.category, entries);
  }
  return [...groups.entries()];
});
const selectedPreset = computed(() => props.presets?.entries.find((entry) => entry.id === selectedPresetId.value) ?? null);
watch(selectedPreset, (preset) => {
  if (preset) presetMode.value = preset.kind === "fragment" ? "insert" : "replace";
});
const inspectorInputs = computed(() => selectedNode.value ? graphNodePins(selectedNode.value, props.nodeCatalog, "inputs") : []);
const inspectorOutputs = computed(() => selectedNode.value ? graphNodePins(selectedNode.value, props.nodeCatalog, "outputs") : []);
const parameterFields = computed(() => {
  const kind = selectedNode.value?.kind;
  if (!kind || typeof kind !== "object" || Array.isArray(kind)) return [];
  return Object.entries(kind as Record<string, unknown>)
    .filter(([key, value]) => key !== "type" && ["number", "string", "boolean"].includes(typeof value))
    .map(([key, value]) => ({ key, value: value as string | number | boolean, valueType: typeof value }));
});
const canApply = computed(() => Boolean(props.draft && props.connected && applySupported.value
  && !props.draft.stale && props.draft.baseRevision !== null && props.draft.valid && !props.draft.validating));

watch(addNodeMenuTemplates, (templates) => {
  addNodeMenuIndex.value = templates.length ? Math.min(addNodeMenuIndex.value, templates.length - 1) : 0;
});

function requestMode(event: Event) {
  const mode = (event.target as HTMLSelectElement).value;
  if (mode === "classic" || mode === "graph") emit("requestMode", mode);
}

function nodeType(node: GraphNode | null | undefined) {
  const kind = node?.kind;
  return kind && typeof kind === "object" && !Array.isArray(kind) && typeof (kind as Record<string, unknown>).type === "string"
    ? String((kind as Record<string, unknown>).type)
    : null;
}

function nodeLabel(node: GraphNode) {
  if (node.title?.trim()) return node.title.trim();
  const type = nodeType(node);
  return type ? graphNodeTemplate(props.nodeCatalog, type)?.label ?? type : "Unsupported node";
}

function nodeSize(node: GraphNode) {
  const count = Math.max(
    graphNodePins(node, props.nodeCatalog, "inputs").length,
    graphNodePins(node, props.nodeCatalog, "outputs").length,
  );
  return Math.max(82, PIN_TOP + count * PIN_GAP + 12);
}

function pinY(node: GraphNode, direction: "inputs" | "outputs", key: string) {
  const pins = graphNodePins(node, props.nodeCatalog, direction);
  const index = pins.findIndex((pin) => pin.key === key);
  return node.pos[1] + PIN_TOP + Math.max(0, index) * PIN_GAP;
}

function edgePath(edge: GraphDocument["graph"]["edges"][number]) {
  const from = graph.value.nodes.find((node) => node.id === edge.from.node);
  const to = graph.value.nodes.find((node) => node.id === edge.to.node);
  if (!from || !to) return "";
  const x1 = from.pos[0] + NODE_WIDTH;
  const y1 = pinY(from, "outputs", edge.from.pin);
  const x2 = to.pos[0];
  const y2 = pinY(to, "inputs", edge.to.pin);
  const bend = Math.max(48, Math.abs(x2 - x1) * 0.45);
  return `M ${x1} ${y1} C ${x1 + bend} ${y1}, ${x2 - bend} ${y2}, ${x2} ${y2}`;
}

function pinIsConnected(nodeId: number, direction: "inputs" | "outputs", key: string) {
  return graph.value.edges.some((edge) => direction === "inputs"
    ? edge.to.node === nodeId && edge.to.pin === key
    : edge.from.node === nodeId && edge.from.pin === key);
}

function commitGraphEdit(next: GraphModel, undoFrom?: GraphModel, validation?: GraphDraftValidation) {
  workingGraph.value = cloneGraph(next);
  emit("updateDraft", { graph: workingGraph.value, undoFrom, validation });
}

function selectedPosition(): [number, number] {
  const rect = svg.value?.getBoundingClientRect();
  if (!rect) return [0, 0];
  return [
    (VIEW_WIDTH / 2 - canvasPan.value[0]) / canvasZoom.value,
    (VIEW_HEIGHT / 2 - canvasPan.value[1]) / canvasZoom.value,
  ];
}

function addNodeAt(template: GraphNodeTemplate | null, position: [number, number]) {
  if (!template) return false;
  try {
    const before = cloneGraph(workingGraph.value);
    const next = addGraphNode(workingGraph.value, template, position);
    const added = next.nodes.at(-1);
    commitGraphEdit(next, before);
    if (added) selectedNodeIds.value = [added.id];
    localError.value = "";
    return true;
  } catch (cause) {
    localError.value = String(cause);
    return false;
  }
}

function addSelectedNode() {
  addNodeAt(selectedTemplate.value, selectedPosition());
}

function closeAddNodeMenu() {
  addNodeMenuOpen.value = false;
}

function positionAddNodeMenu() {
  const menuRect = addNodeMenu.value?.getBoundingClientRect();
  if (!menuRect) return;
  const [left, top] = clampContextMenuPosition(
    addNodeMenuAnchor.value[0],
    addNodeMenuAnchor.value[1],
    menuRect.width,
    menuRect.height,
    window.innerWidth,
    window.innerHeight,
  );
  addNodeMenuStyle.value = { left: `${left}px`, top: `${top}px` };
}

function onCanvasContextMenu(event: MouseEvent) {
  const target = event.target;
  if (!(target instanceof Element) || !target.closest(".graph-canvas-background")) return;
  event.preventDefault();
  const rect = svg.value?.getBoundingClientRect();
  if (!rect) return;
  addNodeMenuGraphPosition.value = clientToGraphPosition(
    event.clientX,
    event.clientY,
    rect,
    canvasPan.value,
    canvasZoom.value,
    VIEW_WIDTH,
    VIEW_HEIGHT,
  );
  addNodeMenuAnchor.value = [event.clientX, event.clientY];
  addNodeMenuSearch.value = "";
  addNodeMenuIndex.value = 0;
  addNodeMenuOpen.value = true;
  void nextTick(() => {
    positionAddNodeMenu();
    addNodeSearchInput.value?.focus();
  });
}

function selectAddNodeMenuTemplate(template: GraphNodeTemplate) {
  if (!addNodeAt(template, addNodeMenuGraphPosition.value)) return;
  closeAddNodeMenu();
}

function onAddNodeMenuKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    closeAddNodeMenu();
    return;
  }
  if (!addNodeMenuTemplates.value.length) return;
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    const delta = event.key === "ArrowDown" ? 1 : -1;
    const count = addNodeMenuTemplates.value.length;
    addNodeMenuIndex.value = (addNodeMenuIndex.value + delta + count) % count;
    nextTick(() => addNodeMenu.value?.querySelector('[aria-selected="true"]')?.scrollIntoView?.({ block: "nearest" }));
  } else if (event.key === "Enter") {
    event.preventDefault();
    const template = addNodeMenuTemplates.value[addNodeMenuIndex.value];
    if (template) selectAddNodeMenuTemplate(template);
  }
}

function onAddNodeMenuOutsidePointerDown(event: PointerEvent) {
  if (!addNodeMenuOpen.value || addNodeMenu.value?.contains(event.target as Node)) return;
  closeAddNodeMenu();
}

function deleteSelection() {
  if (!selectedNodeIds.value.length) return;
  const before = cloneGraph(workingGraph.value);
  commitGraphEdit(removeGraphNodes(workingGraph.value, selectedNodeIds.value), before);
  selectedNodeIds.value = [];
  linkSource.value = null;
}

function duplicateSelection() {
  if (!selectedNodeIds.value.length) return;
  emit("transformDraft", { action: "duplicate", node_ids: [...selectedNodeIds.value], offset: [40, 40] });
}

function groupSelection() {
  if (selectedNodeIds.value.length < 2) return;
  emit("transformDraft", {
    action: "group",
    node_ids: [...selectedNodeIds.value],
    name: groupName.value.trim() || "Group",
  });
  selectedNodeIds.value = [];
}

function ungroupSelected() {
  if (selectedNode.value && nodeType(selectedNode.value) === "Group") {
    emit("transformDraft", { action: "ungroup", node_id: selectedNode.value.id });
    selectedNodeIds.value = [];
  }
}

function selectNode(event: PointerEvent, node: GraphNode) {
  if (event.shiftKey || event.ctrlKey || event.metaKey) {
    selectedNodeIds.value = selectedNodeIds.value.includes(node.id)
      ? selectedNodeIds.value.filter((id) => id !== node.id)
      : [...selectedNodeIds.value, node.id];
  } else if (!selectedNodeIds.value.includes(node.id)) {
    selectedNodeIds.value = [node.id];
  }
}

function startNodeDrag(event: PointerEvent, node: GraphNode) {
  if (event.button !== 0) return;
  selectNode(event, node);
  if (event.shiftKey || event.ctrlKey || event.metaKey) return;
  dragState.value = {
    mode: "node",
    pointerId: event.pointerId,
    startClient: [event.clientX, event.clientY],
    initialGraph: cloneGraph(workingGraph.value),
    initialPositions: new Map(selectedNodeIds.value
      .map((id) => workingGraph.value.nodes.find((entry) => entry.id === id))
      .filter((entry): entry is GraphNode => Boolean(entry))
      .map((entry) => [entry.id, [...entry.pos]])),
  };
  svg.value?.setPointerCapture?.(event.pointerId);
}

function startCanvasPan(event: PointerEvent) {
  if (event.button !== 0 && event.button !== 1) return;
  selectedNodeIds.value = [];
  linkSource.value = null;
  dragState.value = {
    mode: "pan",
    pointerId: event.pointerId,
    startClient: [event.clientX, event.clientY],
    initialPan: [...canvasPan.value],
  };
  svg.value?.setPointerCapture?.(event.pointerId);
}

function onPointerMove(event: PointerEvent) {
  const drag = dragState.value;
  if (!drag || drag.pointerId !== event.pointerId) return;
  const rect = svg.value?.getBoundingClientRect();
  if (!rect) return;
  const [startX, startY] = clientToViewBoxPosition(drag.startClient[0], drag.startClient[1], rect, VIEW_WIDTH, VIEW_HEIGHT);
  const [currentX, currentY] = clientToViewBoxPosition(event.clientX, event.clientY, rect, VIEW_WIDTH, VIEW_HEIGHT);
  const dx = currentX - startX;
  const dy = currentY - startY;
  if (drag.mode === "pan") {
    canvasPan.value = [drag.initialPan![0] + dx, drag.initialPan![1] + dy];
    return;
  }
  const delta: [number, number] = [dx / canvasZoom.value, dy / canvasZoom.value];
  const positions = new Map([...drag.initialPositions!.entries()].map(([id, position]) => [
    id,
    [position[0] + delta[0], position[1] + delta[1]] as [number, number],
  ]));
  const next = moveGraphNodes(workingGraph.value, positions);
  workingGraph.value = next;
  emit("updateDraft", { graph: next });
}

function endPointer(event: PointerEvent) {
  const drag = dragState.value;
  if (!drag || drag.pointerId !== event.pointerId) return;
  if (drag.mode === "node" && drag.initialGraph) {
    emit("updateDraft", { graph: workingGraph.value, undoFrom: drag.initialGraph });
  }
  dragState.value = null;
  if (svg.value?.hasPointerCapture?.(event.pointerId)) svg.value.releasePointerCapture(event.pointerId);
}

function onWheel(event: WheelEvent) {
  const rect = svg.value?.getBoundingClientRect();
  if (!rect) return;
  const [x, y] = clientToViewBoxPosition(event.clientX, event.clientY, rect, VIEW_WIDTH, VIEW_HEIGHT);
  const before = canvasZoom.value;
  const after = Math.min(2.5, Math.max(0.25, before * (event.deltaY < 0 ? 1.12 : 1 / 1.12)));
  const worldX = (x - canvasPan.value[0]) / before;
  const worldY = (y - canvasPan.value[1]) / before;
  canvasZoom.value = after;
  canvasPan.value = [x - worldX * after, y - worldY * after];
}

function zoomBy(factor: number) {
  const before = canvasZoom.value;
  const after = Math.min(2.5, Math.max(0.25, before * factor));
  const worldX = (VIEW_WIDTH / 2 - canvasPan.value[0]) / before;
  const worldY = (VIEW_HEIGHT / 2 - canvasPan.value[1]) / before;
  canvasZoom.value = after;
  canvasPan.value = [VIEW_WIDTH / 2 - worldX * after, VIEW_HEIGHT / 2 - worldY * after];
}

function frameGraph() {
  if (!graph.value.nodes.length) {
    canvasZoom.value = 1;
    canvasPan.value = [VIEW_WIDTH / 2, VIEW_HEIGHT / 2];
    return;
  }
  const minX = Math.min(...graph.value.nodes.map((node) => node.pos[0]));
  const minY = Math.min(...graph.value.nodes.map((node) => node.pos[1]));
  const maxX = Math.max(...graph.value.nodes.map((node) => node.pos[0] + NODE_WIDTH));
  const maxY = Math.max(...graph.value.nodes.map((node) => node.pos[1] + nodeSize(node)));
  const width = Math.max(260, maxX - minX + 120);
  const height = Math.max(180, maxY - minY + 120);
  canvasZoom.value = Math.min(1.4, Math.max(0.25, Math.min((VIEW_WIDTH - 40) / width, (VIEW_HEIGHT - 40) / height)));
  canvasPan.value = [
    VIEW_WIDTH / 2 - (minX + maxX) / 2 * canvasZoom.value,
    VIEW_HEIGHT / 2 - (minY + maxY) / 2 * canvasZoom.value,
  ];
}

function onPinClick(node: GraphNode, key: string, direction: "inputs" | "outputs") {
  if (direction === "outputs") {
    linkSource.value = { node: node.id, pin: key };
    localError.value = "Select a compatible input pin to finish the connection.";
    return;
  }
  if (!linkSource.value) {
    localError.value = "Select an output pin first.";
    return;
  }
  try {
    const before = cloneGraph(workingGraph.value);
    const next = connectGraphPins(workingGraph.value, props.nodeCatalog, linkSource.value, { node: node.id, pin: key });
    commitGraphEdit(next, before);
    localError.value = "";
  } catch (cause) {
    localError.value = String(cause);
  } finally {
    linkSource.value = null;
  }
}

function changeDefault(pin: { key: string; value_type: string }, event: Event) {
  if (!selectedNode.value) return;
  const input = event.target as HTMLInputElement;
  const value = pin.value_type === "Bool" ? (input.checked ? 1 : 0) : Number(input.value);
  try {
    const before = cloneGraph(workingGraph.value);
    commitGraphEdit(setGraphInputDefault(workingGraph.value, props.nodeCatalog, selectedNode.value.id, pin.key, value), before);
    localError.value = "";
  } catch (cause) {
    localError.value = String(cause);
  }
}

function changeParameter(key: string, valueType: string, event: Event) {
  if (!selectedNode.value) return;
  const target = event.target as HTMLInputElement | HTMLTextAreaElement;
  const value = valueType === "number" ? Number(target.value) : valueType === "boolean" ? (target as HTMLInputElement).checked : target.value;
  try {
    const before = cloneGraph(workingGraph.value);
    commitGraphEdit(editGraphNodeParameter(workingGraph.value, selectedNode.value.id, key, value), before);
    localError.value = "";
  } catch (cause) {
    localError.value = String(cause);
  }
}

function changeMetadata(key: "title" | "comment", event: Event) {
  if (!selectedNode.value) return;
  const before = cloneGraph(workingGraph.value);
  const next = cloneGraph(workingGraph.value);
  const node = next.nodes.find((entry) => entry.id === selectedNode.value!.id);
  if (!node) return;
  const value = (event.target as HTMLInputElement | HTMLTextAreaElement).value;
  if (key === "title") node.title = value.slice(0, 128);
  else node.comment = value.slice(0, 2048);
  commitGraphEdit(next, before);
}

function requestPreset() {
  if (!selectedPresetId.value || !presetDetailSupported.value) return;
  emit("applyPreset", selectedPresetId.value, presetMode.value, selectedPosition());
}

function requestApply() {
  if (!canApply.value || !props.draft) return;
  const active = props.engineMode === "graph";
  const runtime = active ? "Graph Mode is active, so the new graph may change running output." : "The graph will be replaced on the Host and used when Graph Mode is enabled.";
  const message = `Apply this draft based on Host revision ${props.draft.baseRevision}?\n\n${runtime}\n\nThe Host will reject the draft if its graph revision changed.`;
  if (window.confirm(message)) emit("applyDraft");
}

async function importFile(event: Event) {
  const file = (event.target as HTMLInputElement).files?.[0];
  if (!file) return;
  try {
    if (file.size > 4 * 1024 * 1024) throw new Error("Import must not exceed 4 MiB");
    emit("importDraft", await file.text());
    localError.value = "";
  } catch (cause) {
    localError.value = String(cause);
  } finally {
    (event.target as HTMLInputElement).value = "";
  }
}

function exportDraft() {
  if (!props.draft) return;
  const blob = new Blob([JSON.stringify(props.draft.graph, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const anchor = window.document.createElement("a");
  anchor.href = url;
  anchor.download = "shockingvrc-graph-draft.json";
  anchor.click();
  URL.revokeObjectURL(url);
}

function reloadLocalDraft() {
  if (props.draft?.dirty && !window.confirm("Replace the current unsaved draft with the locally saved copy?")) return;
  emit("reloadDraft");
}

function discardAndReloadHost() {
  if (!window.confirm("Discard this local draft and replace it with the current Host graph?")) return;
  emit("discardDraft");
}

function edgeLabel(edge: GraphDocument["graph"]["edges"][number]) {
  return `Node ${edge.from.node} · ${edge.from.pin} → Node ${edge.to.node} · ${edge.to.pin}`;
}

function onKeydown(event: KeyboardEvent) {
  if (addNodeMenuOpen.value) {
    if (event.key === "Escape") closeAddNodeMenu();
    return;
  }
  const target = event.target;
  if (target instanceof HTMLElement && ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)) return;
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "z") {
    event.preventDefault();
    if (event.shiftKey) emit("redo");
    else emit("undo");
  } else if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "y") {
    event.preventDefault();
    emit("redo");
  } else if ((event.key === "Delete" || event.key === "Backspace") && selectedNodeIds.value.length) {
    event.preventDefault();
    deleteSelection();
  } else if (event.key === "Escape") {
    linkSource.value = null;
    selectedNodeIds.value = [];
  }
}

onMounted(() => {
  window.addEventListener("keydown", onKeydown);
  window.addEventListener("resize", positionAddNodeMenu);
  document.addEventListener("pointerdown", onAddNodeMenuOutsidePointerDown);
});
onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKeydown);
  window.removeEventListener("resize", positionAddNodeMenu);
  document.removeEventListener("pointerdown", onAddNodeMenuOutsidePointerDown);
});
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
        <small v-if="modeChange.message" :class="modeStatusClass" role="status">{{ modeChange.message }}</small>
      </div>
      <p v-else class="empty-note">Engine mode switching is not supported by this Host.</p>
      <small v-if="modeChange.status === 'unsupported'" class="term-control-state unsupported">{{ modeChange.message }}</small>
    </section>

    <section class="panel graph-document-panel">
      <div class="panel-heading">
        <div><span class="eyebrow">HOST GRAPH</span><h3>Graph workspace</h3></div>
        <div class="button-row">
          <span class="tag">REV {{ document?.graph_revision ?? '—' }}</span>
          <span v-if="document?.editor.panic || document?.editor.ui_panic" class="tag graph-panic-tag">PANIC ACTIVE</span>
          <TerminalButton size="small" :disabled="!canRefresh" @click="emit('refresh')">REFRESH HOST</TerminalButton>
        </div>
      </div>
      <p v-if="!documentReadSupported && !draft" class="empty-note">This Host does not support Graph document reads.</p>
      <p v-else-if="loadStatus === 'reconnecting'" class="empty-note" role="status">Reconnecting. Your local draft remains available and will be marked stale if Host Graph changed.</p>
      <p v-else-if="loadStatus === 'loading' && !draft" class="empty-note" role="status">Loading the Host Graph document…</p>
      <div v-else-if="loadStatus === 'error' && !draft" class="error-banner graph-inline-error" role="alert"><strong>Graph read failed</strong><small>{{ loadError }}</small></div>
      <p v-else-if="!draft && !document" class="empty-note">Connect to a Graph capable Host to create a local draft.</p>

      <template v-if="draft">
        <div class="graph-draft-summary">
          <div><span class="eyebrow">LOCAL DRAFT</span><strong>{{ draft.dirty ? (draft.saved ? 'SAVED LOCAL CHANGES' : 'UNSAVED LOCAL CHANGES') : 'MATCHES HOST' }}</strong></div>
          <div><span class="eyebrow">BASED ON</span><strong>{{ draft.baseRevision ?? 'UNKNOWN REVISION' }}</strong></div>
          <div><span class="eyebrow">HOST REVISION</span><strong>{{ document?.graph_revision ?? 'NOT LOADED' }}</strong></div>
          <div><span class="eyebrow">DRAFT VALIDATION</span><strong :class="draft.valid && !draft.validating ? 'graph-valid' : 'graph-invalid'">{{ draft.validating ? 'CHECKING' : draft.valid ? 'VALID' : 'ISSUES FOUND' }}</strong></div>
        </div>
        <p v-if="draft.stale" class="graph-stale-note" role="alert">
          This draft is preserved from Host revision {{ draft.baseRevision ?? 'unknown' }}. The Host is now at revision {{ document?.graph_revision ?? 'unknown' }}. Apply is blocked until you explicitly reload the Host graph or recover the draft.
        </p>
        <p v-if="document?.editor.panic || document?.editor.ui_panic" class="graph-panic-note" role="status">
          Host reports a panic state. Terminal displays this state and provides no panic release control.
        </p>

        <div class="graph-editor-actions">
          <div class="button-row">
            <TerminalButton size="small" :disabled="!canUndo" @click="emit('undo')">UNDO</TerminalButton>
            <TerminalButton size="small" :disabled="!canRedo" @click="emit('redo')">REDO</TerminalButton>
            <TerminalButton size="small" :disabled="draftLoading" @click="emit('saveDraft')">SAVE LOCAL</TerminalButton>
            <TerminalButton size="small" :disabled="draftLoading || !connected" @click="reloadLocalDraft">RELOAD LOCAL</TerminalButton>
            <TerminalButton size="small" @click="exportDraft">EXPORT JSON</TerminalButton>
            <TerminalButton size="small" @click="fileInput?.click()">IMPORT JSON</TerminalButton>
            <input ref="fileInput" class="graph-file-input" type="file" accept="application/json,.json" aria-label="Import Graph JSON" @change="importFile">
            <TerminalButton size="small" :disabled="!document" @click="discardAndReloadHost">DISCARD / RELOAD HOST</TerminalButton>
            <TerminalButton variant="primary" size="small" :disabled="!canApply" @click="requestApply">APPLY TO HOST</TerminalButton>
          </div>
          <small class="graph-local-only-note">Edits, presets, import, and local saves stay in this Terminal draft until you confirm Apply.</small>
        </div>
        <div v-if="applyResult" class="graph-apply-result" :class="`state-${applyResult.state}`" role="status">
          <strong>Apply {{ applyResult.state.toUpperCase() }}</strong>
          <span>{{ applyResult.message || (applyResult.state === 'conflict' ? `Host is at revision ${applyResult.current_revision}.` : '') }}</span>
        </div>
        <div v-if="draftError || localError" class="error-banner graph-inline-error" role="alert"><strong>Graph workspace</strong><small>{{ localError || draftError }}</small></div>

        <div class="graph-editor-layout">
          <aside class="graph-toolbox" aria-label="Graph tools">
            <section class="graph-tool-section">
              <div class="panel-heading"><div><span class="eyebrow">NODE CATALOG</span><h4>Add node</h4></div></div>
              <input v-model="paletteSearch" type="search" aria-label="Search nodes" placeholder="Search nodes">
              <select v-model="selectedTemplateId" size="7" aria-label="Graph node catalog">
                <option v-for="template in filteredTemplates" :key="template.id" :value="template.id">{{ template.category }} · {{ template.label }}</option>
              </select>
              <TerminalButton size="small" :disabled="!selectedTemplate" @click="addSelectedNode">ADD NODE</TerminalButton>
              <p v-if="!nodeCatalog" class="empty-note">Host Graph node catalog is unavailable.</p>
            </section>

            <section v-if="presetsSupported" class="graph-tool-section">
              <div class="panel-heading"><div><span class="eyebrow">HOST PRESETS</span><h4>Load preset</h4></div></div>
              <select v-model="selectedPresetId" aria-label="Graph preset">
                <option value="">Choose a Host preset</option>
                <option v-for="preset in presets?.entries ?? []" :key="preset.id" :value="preset.id">{{ preset.name }} · {{ preset.kind }}</option>
              </select>
              <select v-model="presetMode" aria-label="Preset application mode">
                <option value="replace">Replace draft</option>
                <option value="insert">Insert into draft</option>
              </select>
              <TerminalButton size="small" :disabled="!connected || !selectedPreset || !presetDetailSupported" @click="requestPreset">LOAD TO DRAFT</TerminalButton>
              <small v-if="!presetDetailSupported" class="empty-note">This Host does not support preset detail downloads.</small>
            </section>

            <section class="graph-tool-section">
              <div class="panel-heading"><div><span class="eyebrow">SELECTION</span><h4>{{ selectedNodeIds.length }} nodes</h4></div></div>
              <div class="button-row">
                <TerminalButton size="small" :disabled="!selectedNodeIds.length" @click="duplicateSelection">DUPLICATE</TerminalButton>
                <TerminalButton size="small" :disabled="!selectedNodeIds.length" @click="deleteSelection">DELETE</TerminalButton>
              </div>
              <input v-model="groupName" aria-label="Group name" maxlength="128" placeholder="Group name">
              <TerminalButton size="small" :disabled="selectedNodeIds.length < 2" @click="groupSelection">GROUP SELECTION</TerminalButton>
            </section>
          </aside>

          <section class="graph-canvas-panel" aria-label="Graph canvas">
            <div class="graph-canvas-toolbar">
              <span>{{ Math.round(canvasZoom * 100) }}%</span>
              <div class="button-row">
                <TerminalButton size="small" aria-label="Zoom out" @click="zoomBy(1 / 1.2)">−</TerminalButton>
                <TerminalButton size="small" aria-label="Zoom in" @click="zoomBy(1.2)">+</TerminalButton>
                <TerminalButton size="small" @click="frameGraph">FRAME GRAPH</TerminalButton>
              </div>
            </div>
            <svg
              ref="svg"
              class="graph-canvas"
              viewBox="0 0 960 620"
              role="application"
              aria-label="Editable Graph canvas. Drag nodes to move them. Select output then input pins to connect."
              @pointermove="onPointerMove"
              @pointerup="endPointer"
              @pointercancel="endPointer"
              @wheel.prevent="onWheel"
              @contextmenu.prevent="onCanvasContextMenu"
            >
              <defs>
                <pattern id="graph-grid" width="24" height="24" patternUnits="userSpaceOnUse">
                  <path d="M 24 0 L 0 0 0 24" class="graph-grid-line" fill="none" />
                </pattern>
              </defs>
              <rect x="0" y="0" width="960" height="620" fill="url(#graph-grid)" class="graph-canvas-background" @pointerdown="startCanvasPan" />
              <g :transform="`translate(${canvasPan[0]} ${canvasPan[1]}) scale(${canvasZoom})`">
                <g class="graph-edge-layer">
                  <g v-for="(edge, index) in graph.edges" :key="`${edge.from.node}-${edge.from.pin}-${edge.to.node}-${edge.to.pin}-${index}`">
                    <path :d="edgePath(edge)" class="graph-edge-path" />
                    <path :d="edgePath(edge)" class="graph-edge-hit" :aria-label="edgeLabel(edge)" @dblclick.stop="commitGraphEdit(disconnectGraphEdge(workingGraph, index), workingGraph)" />
                  </g>
                </g>
                <g v-for="node in graph.nodes" :key="node.id" class="graph-node" :class="{ selected: selectedNodeIds.includes(node.id), unsupported: !graphNodeTemplate(nodeCatalog, nodeType(node) ?? '') }" :transform="`translate(${node.pos[0]} ${node.pos[1]})`" @pointerdown.stop="startNodeDrag($event, node)">
                  <rect class="graph-node-body" :width="NODE_WIDTH" :height="nodeSize(node)" rx="5" />
                  <rect class="graph-node-header" :width="NODE_WIDTH" height="32" rx="5" />
                  <text class="graph-node-title" x="12" y="21">{{ nodeLabel(node) }}</text>
                  <text class="graph-node-id" :x="NODE_WIDTH - 10" y="20" text-anchor="end">#{{ node.id }}</text>
                  <g v-for="(pin, index) in graphNodePins(node, nodeCatalog, 'inputs')" :key="`in-${pin.key}`" class="graph-pin graph-pin-input" @pointerdown.stop @click.stop="onPinClick(node, pin.key, 'inputs')">
                    <circle cx="0" :cy="PIN_TOP + index * PIN_GAP" r="6" :class="{ connected: pinIsConnected(node.id, 'inputs', pin.key) }" />
                    <text x="12" :y="PIN_TOP + index * PIN_GAP + 4">{{ pin.label }}</text>
                  </g>
                  <g v-for="(pin, index) in graphNodePins(node, nodeCatalog, 'outputs')" :key="`out-${pin.key}`" class="graph-pin graph-pin-output" @pointerdown.stop @click.stop="onPinClick(node, pin.key, 'outputs')">
                    <circle :cx="NODE_WIDTH" :cy="PIN_TOP + index * PIN_GAP" r="6" :class="{ connected: pinIsConnected(node.id, 'outputs', pin.key), linking: linkSource?.node === node.id && linkSource?.pin === pin.key }" />
                    <text :x="NODE_WIDTH - 12" :y="PIN_TOP + index * PIN_GAP + 4" text-anchor="end">{{ pin.label }}</text>
                  </g>
                </g>
              </g>
            </svg>
            <Teleport to="body">
              <div
                v-if="addNodeMenuOpen"
                ref="addNodeMenu"
                class="graph-context-menu"
                role="group"
                aria-label="Add Graph node"
                :style="addNodeMenuStyle"
                @pointerdown.stop
                @click.stop
                @contextmenu.prevent
              >
                <div class="graph-context-menu-heading">ADD NODE</div>
                <input
                  ref="addNodeSearchInput"
                  v-model="addNodeMenuSearch"
                  type="search"
                  aria-label="Search node types"
                  placeholder="Search node types"
                  @keydown="onAddNodeMenuKeydown"
                >
                <div v-if="addNodeMenuGroups.length" class="graph-context-menu-list" role="listbox" aria-label="Supported node types">
                  <section v-for="([category, templates]) in addNodeMenuGroups" :key="category" class="graph-context-menu-group" role="group" :aria-label="category">
                    <div class="graph-context-menu-category">{{ category }}</div>
                    <button
                      v-for="template in templates"
                      :key="template.id"
                      type="button"
                      role="option"
                      :aria-selected="addNodeMenuTemplates[addNodeMenuIndex]?.id === template.id"
                      @mouseenter="addNodeMenuIndex = addNodeMenuTemplates.findIndex((entry) => entry.id === template.id)"
                      @click.prevent="selectAddNodeMenuTemplate(template)"
                    >{{ template.label }}</button>
                  </section>
                </div>
                <p v-else class="graph-context-menu-empty">{{ nodeCatalog ? 'No supported node types match.' : 'Host node catalog is unavailable.' }}</p>
                <small class="graph-context-menu-hint">↑ ↓ navigate · Enter add · Esc close</small>
              </div>
            </Teleport>
            <small class="graph-canvas-hint">Drag blank space to pan · scroll to zoom · Shift-click to multi-select · double-click a wire to disconnect</small>
          </section>

          <aside class="graph-inspector" aria-label="Node inspector">
            <div class="panel-heading"><div><span class="eyebrow">INSPECTOR</span><h4>{{ selectedNode ? nodeLabel(selectedNode) : 'Select a node' }}</h4></div></div>
            <template v-if="selectedNode">
              <label>Title<input :value="selectedNode.title ?? ''" maxlength="128" @change="changeMetadata('title', $event)"></label>
              <label>Comment<textarea :value="selectedNode.comment ?? ''" maxlength="2048" rows="3" @change="changeMetadata('comment', $event)"></textarea></label>
              <div class="graph-inspector-section">
                <strong>Parameters</strong>
                <label v-for="field in parameterFields" :key="field.key" class="graph-parameter-field">
                  {{ field.key }}
                  <input v-if="field.valueType === 'number'" type="number" step="any" :value="field.value" @change="changeParameter(field.key, field.valueType, $event)">
                  <input v-else-if="field.valueType === 'boolean'" type="checkbox" :checked="Boolean(field.value)" @change="changeParameter(field.key, field.valueType, $event)">
                  <input v-else type="text" :value="field.value" maxlength="4096" @change="changeParameter(field.key, field.valueType, $event)">
                </label>
              </div>
              <div class="graph-inspector-section">
                <strong>Input defaults</strong>
                <label v-for="pin in inspectorInputs" :key="pin.key" class="graph-parameter-field">
                  {{ pin.label }} <small>{{ pin.value_type }}</small>
                  <input v-if="pin.value_type === 'Bool'" type="checkbox" :checked="(selectedNode.defaults?.[pin.key] ?? pin.default) >= 0.5" @change="changeDefault(pin, $event)">
                  <input v-else type="number" step="any" :min="pin.min" :max="pin.max" :value="selectedNode.defaults?.[pin.key] ?? pin.default" @change="changeDefault(pin, $event)">
                </label>
                <p v-if="!inspectorInputs.length" class="empty-note">This node has no editable input defaults.</p>
              </div>
              <div class="graph-inspector-section">
                <strong>Connections</strong>
                <div v-for="(edge, index) in graph.edges.filter((item) => item.from.node === selectedNode?.id || item.to.node === selectedNode?.id)" :key="`${edge.from.node}-${edge.from.pin}-${edge.to.node}-${edge.to.pin}-${index}`" class="graph-inspector-edge">
                  <span>{{ edgeLabel(edge) }}</span>
                  <TerminalButton size="small" aria-label="Disconnect edge" @click="commitGraphEdit(disconnectGraphEdge(workingGraph, graph.edges.indexOf(edge)), workingGraph)">DISCONNECT</TerminalButton>
                </div>
                <p v-if="!graph.edges.some((edge) => edge.from.node === selectedNode?.id || edge.to.node === selectedNode?.id)" class="empty-note">No connections.</p>
              </div>
              <TerminalButton v-if="nodeType(selectedNode) === 'Group'" size="small" @click="ungroupSelected">UNGROUP</TerminalButton>
            </template>
            <p v-else class="empty-note">Select a graph node to edit its supported parameters, input defaults, and connections.</p>
          </aside>
        </div>

        <section class="graph-validation" aria-labelledby="graph-validation-heading">
          <div class="panel-heading"><div><span class="eyebrow">{{ draft.issues.length }} REPORTS</span><h4 id="graph-validation-heading">Draft validation</h4></div>
            <TerminalButton size="small" :disabled="draft.validating" @click="emit('validateDraft')">VALIDATE</TerminalButton>
          </div>
          <ul v-if="draft.issues.length" class="graph-issues">
            <li v-for="(issue, index) in draft.issues" :key="`${issue.node_id ?? 'graph'}-${index}`">
              <span class="tag">{{ issue.severity.toUpperCase() }}</span><span>{{ issue.message }}</span><small v-if="issue.node_id !== null">NODE {{ issue.node_id }}</small>
            </li>
          </ul>
          <p v-else-if="draft.valid && !draft.validating" class="empty-note">Rust Graph validation reports no errors.</p>
          <p v-else class="empty-note" role="status">{{ draft.validating ? 'Checking the local draft…' : 'Validate this draft before applying it.' }}</p>
        </section>
      </template>
    </section>
  </div>
</template>

import test, { after } from "node:test";
import assert from "node:assert/strict";
import { readFile, unlink, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { Window } from "happy-dom";
import { parse, compileScript } from "@vue/compiler-sfc";
import ts from "typescript";

const terminalRoot = fileURLToPath(new URL("..", import.meta.url));
const componentPath = join(terminalRoot, "src", "components", "GraphWorkspace.vue");
const window = new Window({ url: "http://localhost/" });
Object.assign(globalThis, {
  window,
  document: window.document,
  Node: window.Node,
  Element: window.Element,
  HTMLElement: window.HTMLElement,
  SVGElement: window.SVGElement,
  Event: window.Event,
  MouseEvent: window.MouseEvent,
});

const source = await readFile(componentPath, "utf8");
const { descriptor } = parse(source, { filename: componentPath });
let compiled = compileScript(descriptor, { id: "graph-workspace-ui-test", inlineTemplate: true }).content;
compiled = compiled.replace(
  'import TerminalButton from "./TerminalButton.vue";',
  `import { h } from "vue";
const TerminalButton = {
  inheritAttrs: true,
  props: ["variant", "size", "disabled"],
  setup(props, { attrs, slots }) {
    return () => h("button", { ...attrs, disabled: props.disabled }, slots.default?.());
  },
};`,
);
const modulePath = join(dirname(componentPath), `.GraphWorkspace.ui-test-${process.pid}.mjs`);
await writeFile(modulePath, ts.transpileModule(compiled, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
}).outputText, "utf8");
const { default: GraphWorkspace } = await import(`${pathToFileURL(modulePath).href}?test=${Date.now()}`);
await unlink(modulePath);
const { createApp, defineComponent, h, nextTick, reactive } = await import("vue");
const mountedApps = new Set();

after(async () => {
  for (const app of mountedApps) app.unmount();
  window.happyDOM.abort();
});

const catalog = {
  entries: [
    { id: "constant", label: "Constant", category: "Inputs", keywords: "value", kind: { type: "Constant", value: 0.5 }, inputs: [], outputs: [{ key: "out", label: "Value", value_type: "Float", default: 0.5, min: 0, max: 1 }] },
    { id: "math", label: "Math", category: "Math", keywords: "add", kind: { type: "Math", op: "Add" }, inputs: [{ key: "a", label: "A", value_type: "Float", default: 0, min: -1, max: 1 }], outputs: [{ key: "out", label: "Result", value_type: "Float", default: 0, min: -1, max: 1 }] },
  ],
};

function baseProps(overrides = {}) {
  const graph = {
    version: 1,
    nodes: [
      { id: 1, kind: { type: "Constant", value: 0.5 }, pos: [20, 20] },
      { id: 2, kind: { type: "Math", op: "Add" }, pos: [300, 20] },
    ],
    edges: [],
  };
  return {
    engineMode: "classic",
    modeChange: { status: "idle", message: "", requestedMode: null },
    connected: true,
    capabilities: ["graph.document.read.v1", "graph.presets.catalog.v1", "graph.presets.detail.chunked.v1", "graph.apply.upload.status.v1", "engine.mode.set.v1"],
    document: {
      graph_revision: 4,
      graph,
      valid: true,
      issues: [],
      editor: { mode: "classic", panic: false, ui_panic: false, caps: [50, 50], tick: 5 },
    },
    draft: { graph, baseRevision: 4, stale: false, dirty: true, saved: false, valid: true, issues: [], missingTypes: [], validating: false },
    nodeCatalog: catalog,
    presets: { entries: [{ id: "builtin:fragment", name: "Fragment", description: "A fragment", kind: "fragment", user: false, node_count: 1 }] },
    loadStatus: "loaded",
    loadError: "",
    draftError: "",
    applyResult: null,
    draftLoading: false,
    canUndo: false,
    canRedo: false,
    ...overrides,
  };
}

function mountWorkspace(overrides = {}) {
  const root = window.document.createElement("div");
  window.document.body.append(root);
  const props = reactive(baseProps(overrides));
  const events = { updateDraft: [], applyDraft: 0, applyPreset: [], transformDraft: [] };
  const app = createApp(defineComponent({
    setup: () => () => h(GraphWorkspace, {
      ...props,
      onUpdateDraft(payload) {
        events.updateDraft.push(payload);
        props.draft = { ...props.draft, graph: payload.graph };
      },
      onApplyDraft() { events.applyDraft += 1; },
      onApplyPreset(...args) { events.applyPreset.push(args); },
      onTransformDraft(operation) { events.transformDraft.push(operation); },
    }),
  }));
  app.mount(root);
  mountedApps.add(app);
  return { app, props, events, root };
}

function click(element) {
  assert.ok(element);
  element.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
}

test("canvas adds nodes and connects only the selected output and input pins", async () => {
  const view = mountWorkspace();
  click([...view.root.querySelectorAll("button")].find((button) => button.textContent.includes("ADD NODE")));
  await nextTick();
  assert.equal(view.props.draft.graph.nodes.length, 3);

  const nodes = [...view.root.querySelectorAll(".graph-node")];
  click(nodes[0].querySelector(".graph-pin-output"));
  click(nodes[1].querySelector(".graph-pin-input"));
  await nextTick();
  assert.equal(view.props.draft.graph.edges.length, 1);
  assert.equal(view.props.draft.graph.edges[0].from.node, 1);
  assert.equal(view.props.draft.graph.edges[0].to.node, 2);
});

test("dragging a selected node emits one undoable local movement", async () => {
  const view = mountWorkspace();
  const canvas = view.root.querySelector("svg.graph-canvas");
  canvas.getBoundingClientRect = () => ({ left: 0, top: 0, width: 960, height: 620 });
  const firstNode = view.root.querySelector(".graph-node");
  firstNode.dispatchEvent(new window.MouseEvent("pointerdown", {
    bubbles: true,
    button: 0,
    clientX: 20,
    clientY: 20,
  }));
  canvas.dispatchEvent(new window.MouseEvent("pointermove", {
    bubbles: true,
    clientX: 60,
    clientY: 50,
  }));
  canvas.dispatchEvent(new window.MouseEvent("pointerup", {
    bubbles: true,
    clientX: 60,
    clientY: 50,
  }));
  await nextTick();
  assert.deepEqual(view.props.draft.graph.nodes[0].pos, [60, 50]);
  assert.equal(view.events.updateDraft.at(-1).undoFrom.nodes[0].pos[0], 20);
});

test("preset selection applies into the local draft and Apply requires confirmation", async () => {
  const view = mountWorkspace();
  const presetSelect = view.root.querySelector('select[aria-label="Graph preset"]');
  presetSelect.value = "builtin:fragment";
  presetSelect.dispatchEvent(new window.Event("change", { bubbles: true }));
  await nextTick();
  click([...view.root.querySelectorAll("button")].find((button) => button.textContent.includes("LOAD TO DRAFT")));
  assert.equal(view.events.applyPreset.length, 1);
  assert.equal(view.events.applyPreset[0][0], "builtin:fragment");
  assert.equal(view.events.applyPreset[0][1], "insert");

  window.confirm = () => true;
  click([...view.root.querySelectorAll("button")].find((button) => button.textContent.includes("APPLY TO HOST")));
  assert.equal(view.events.applyDraft, 1);
});

test("a stale draft remains visible and Apply stays disabled", async () => {
  const view = mountWorkspace({ draft: { ...baseProps().draft, stale: true, baseRevision: 3 } });
  const apply = [...view.root.querySelectorAll("button")].find((button) => button.textContent.includes("APPLY TO HOST"));
  assert.equal(apply.disabled, true);
  assert.match(view.root.textContent, /preserved from Host revision 3/);
  click(apply);
  assert.equal(view.events.applyDraft, 0);
});

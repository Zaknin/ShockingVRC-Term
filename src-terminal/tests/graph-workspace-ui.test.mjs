import test, { after } from "node:test";
import assert from "node:assert/strict";
import { readFile, unlink, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { Window } from "happy-dom";
import { parse, compileScript } from "@vue/compiler-sfc";
import ts from "typescript";
import { createGraphHistory, recordGraphHistory, redoGraphHistory, undoGraphHistory } from "../src/domain/graph-editor.mjs";

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

function rightClick(element, clientX = 100, clientY = 100) {
  const event = new window.MouseEvent("contextmenu", {
    bubbles: true,
    cancelable: true,
    button: 2,
    clientX,
    clientY,
  });
  element.dispatchEvent(event);
  return event;
}

function latestGraphMenu() {
  return [...window.document.querySelectorAll('[role="group"][aria-label="Add Graph node"]')].at(-1) ?? null;
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

test("right-click on empty canvas opens an app menu and suppresses the browser menu", async () => {
  const view = mountWorkspace();
  const canvas = view.root.querySelector("svg.graph-canvas");
  canvas.getBoundingClientRect = () => ({ left: 0, top: 0, width: 960, height: 620 });
  const background = canvas.querySelector(".graph-canvas-background");

  const event = rightClick(background, 240, 180);
  await nextTick();

  assert.equal(event.defaultPrevented, true);
  assert.ok(latestGraphMenu());
  assert.equal(window.document.activeElement?.getAttribute("aria-label"), "Search node types");
});

test("context menu offers only Host catalog entries and filters by their categories and keywords", async () => {
  const view = mountWorkspace({ nodeCatalog: { entries: [
    { ...catalog.entries[0], id: "host:source", label: "Source from Host", category: "Host Inputs", keywords: "remote value" },
    { ...catalog.entries[1], id: "host:operator", label: "Operator from Host", category: "Host Math", keywords: "combine" },
  ] } });
  const canvas = view.root.querySelector("svg.graph-canvas");
  canvas.getBoundingClientRect = () => ({ left: 0, top: 0, width: 960, height: 620 });
  rightClick(canvas.querySelector(".graph-canvas-background"));
  await nextTick();

  const menu = latestGraphMenu();
  assert.deepEqual([...menu.querySelectorAll('[role="option"]')].map((option) => option.textContent.trim()), [
    "Source from Host", "Operator from Host",
  ]);
  assert.match(menu.textContent, /Host Inputs/);
  assert.match(menu.textContent, /Host Math/);
  const search = menu.querySelector('[aria-label="Search node types"]');
  search.value = "combine";
  search.dispatchEvent(new window.Event("input", { bubbles: true }));
  await nextTick();
  assert.deepEqual([...menu.querySelectorAll('[role="option"]')].map((option) => option.textContent.trim()), ["Operator from Host"]);
});

test("context menu selection places a node at scaled pointer coordinates in the local undoable draft", async () => {
  const view = mountWorkspace();
  const canvas = view.root.querySelector("svg.graph-canvas");
  canvas.getBoundingClientRect = () => ({ left: 100, top: 50, width: 1440, height: 930 });
  rightClick(canvas.querySelector(".graph-canvas-background"), 700, 500);
  await nextTick();

  const menu = latestGraphMenu();
  click(menu.querySelector('[role="option"]'));
  await nextTick();

  const payload = view.events.updateDraft.at(-1);
  const added = payload.graph.nodes.at(-1);
  assert.deepEqual(added.pos, [376, 276]);
  assert.deepEqual(payload.undoFrom, baseProps().draft.graph);
  assert.equal(view.props.draft.graph.nodes.length, 3);
  assert.equal(view.events.applyDraft, 0);
  assert.equal(menu.isConnected, false);

  const history = recordGraphHistory({ ...createGraphHistory(payload.undoFrom), graph: payload.undoFrom }, payload.graph);
  assert.equal(undoGraphHistory(history).graph.nodes.length, 2);
  assert.equal(redoGraphHistory(undoGraphHistory(history)).graph.nodes.length, 3);
});

test("context menu placement follows canvas pan and zoom coordinates", async () => {
  const view = mountWorkspace();
  const canvas = view.root.querySelector("svg.graph-canvas");
  canvas.getBoundingClientRect = () => ({ left: 0, top: 0, width: 960, height: 620 });
  const background = canvas.querySelector(".graph-canvas-background");
  background.dispatchEvent(new window.MouseEvent("pointerdown", { bubbles: true, button: 0, clientX: 10, clientY: 10 }));
  canvas.dispatchEvent(new window.MouseEvent("pointermove", { bubbles: true, clientX: 50, clientY: 40 }));
  canvas.dispatchEvent(new window.MouseEvent("pointerup", { bubbles: true, clientX: 50, clientY: 40 }));
  click([...view.root.querySelectorAll("button")].find((button) => button.getAttribute("aria-label") === "Zoom in"));
  await nextTick();

  rightClick(background, 500, 320);
  await nextTick();
  click(latestGraphMenu().querySelector('[role="option"]'));
  await nextTick();

  // Initial pan [24,24], drag delta [40,30], then 1.2x zoom about the viewBox center.
  // The pointer [500,320] maps to graph coordinate [432.666…, 264.333…].
  assert.deepEqual(view.events.updateDraft.at(-1).graph.nodes.at(-1).pos, [432.66666666666674, 264.3333333333333]);
});

test("keyboard navigation chooses a node and reopening captures the new canvas position", async () => {
  const view = mountWorkspace();
  const canvas = view.root.querySelector("svg.graph-canvas");
  canvas.getBoundingClientRect = () => ({ left: 0, top: 0, width: 960, height: 620 });
  const background = canvas.querySelector(".graph-canvas-background");

  rightClick(background, 300, 200);
  await nextTick();
  const search = latestGraphMenu().querySelector('[aria-label="Search node types"]');
  search.dispatchEvent(new window.KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
  search.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  await nextTick();
  assert.equal(view.events.updateDraft.at(-1).graph.nodes.at(-1).kind.type, "Math");
  assert.deepEqual(view.events.updateDraft.at(-1).graph.nodes.at(-1).pos, [276, 176]);

  rightClick(background, 500, 350);
  await nextTick();
  click(latestGraphMenu().querySelector('[role="option"]'));
  await nextTick();
  assert.deepEqual(view.events.updateDraft.at(-1).graph.nodes.at(-1).pos, [476, 326]);
});

test("context menu clamps its measured size inside the app viewport", async () => {
  const view = mountWorkspace();
  const canvas = view.root.querySelector("svg.graph-canvas");
  canvas.getBoundingClientRect = () => ({ left: 0, top: 0, width: 960, height: 620 });
  const original = window.HTMLElement.prototype.getBoundingClientRect;
  window.HTMLElement.prototype.getBoundingClientRect = function getRect() {
    if (this.classList?.contains("graph-context-menu")) {
      return { left: 0, top: 0, width: 240, height: 180, right: 240, bottom: 180, x: 0, y: 0, toJSON() {} };
    }
    return original.call(this);
  };
  try {
    rightClick(canvas.querySelector(".graph-canvas-background"), window.innerWidth - 1, window.innerHeight - 1);
    await nextTick();
    await nextTick();
    const menu = latestGraphMenu();
    assert.equal(menu.style.left, `${Math.max(8, window.innerWidth - 248)}px`);
    assert.equal(menu.style.top, `${Math.max(8, window.innerHeight - 188)}px`);
  } finally {
    window.HTMLElement.prototype.getBoundingClientRect = original;
  }
});

test("Escape and outside pointer dismiss the menu without creating nodes", async () => {
  const view = mountWorkspace();
  const canvas = view.root.querySelector("svg.graph-canvas");
  canvas.getBoundingClientRect = () => ({ left: 0, top: 0, width: 960, height: 620 });
  const background = canvas.querySelector(".graph-canvas-background");

  rightClick(background);
  await nextTick();
  const escapeMenu = latestGraphMenu();
  escapeMenu.querySelector('[aria-label="Search node types"]').dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await nextTick();
  assert.equal(escapeMenu.isConnected, false);

  rightClick(background);
  await nextTick();
  const outsideMenu = latestGraphMenu();
  window.document.body.dispatchEvent(new window.MouseEvent("pointerdown", { bubbles: true }));
  await nextTick();
  assert.equal(outsideMenu.isConnected, false);
  assert.equal(view.props.draft.graph.nodes.length, 2);
});

test("canvas elements suppress native context menus without opening Add Node, while toolbar controls do neither", async () => {
  const view = mountWorkspace();
  const canvas = view.root.querySelector("svg.graph-canvas");
  const node = view.root.querySelector(".graph-node");
  const menusBefore = window.document.querySelectorAll('[role="group"][aria-label="Add Graph node"]').length;
  const nodeContext = rightClick(node, 100, 100);
  await nextTick();
  assert.equal(nodeContext.defaultPrevented, true);
  assert.equal(window.document.querySelectorAll('[role="group"][aria-label="Add Graph node"]').length, menusBefore);

  const toolbar = view.root.querySelector('[aria-label="Zoom in"]');
  const toolbarContext = rightClick(toolbar, 100, 100);
  await nextTick();
  assert.equal(toolbarContext.defaultPrevented, false);
  assert.equal(window.document.querySelectorAll('[role="group"][aria-label="Add Graph node"]').length, menusBefore);

  const textSearch = view.root.querySelector('[aria-label="Search nodes"]');
  const textContext = rightClick(textSearch, 100, 100);
  assert.equal(textContext.defaultPrevented, false);
  assert.equal(window.document.querySelectorAll('[role="group"][aria-label="Add Graph node"]').length, menusBefore);
  assert.equal(view.props.draft.graph.nodes.length, 2);
});

test("mode labels are concise and the idle confirmation fallback is absent", async () => {
  const confirmed = mountWorkspace({ modeChange: { status: "confirmed", message: "Host confirmed", requestedMode: "graph" } });
  assert.match(confirmed.root.textContent, /Host confirmed/);
  assert.doesNotMatch(confirmed.root.textContent, /Mode changes require a newer Host snapshot/);
  assert.doesNotMatch(confirmed.root.textContent, /snapshot \d+/);

  const queued = mountWorkspace({ modeChange: { status: "queued", message: "Switching mode…", requestedMode: "graph" } });
  assert.match(queued.root.textContent, /Switching mode…/);
  const lost = mountWorkspace({ modeChange: { status: "reconnecting", message: "Connection lost", requestedMode: "graph" } });
  assert.match(lost.root.textContent, /Connection lost/);
  const unconfirmed = mountWorkspace({ modeChange: { status: "unknown", message: "Change not confirmed", requestedMode: "graph" } });
  assert.match(unconfirmed.root.textContent, /Change not confirmed/);
});

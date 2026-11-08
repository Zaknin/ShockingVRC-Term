import test from "node:test";
import assert from "node:assert/strict";
import {
  addGraphNode,
  connectGraphPins,
  createGraphHistory,
  disconnectGraphEdge,
  editGraphNodeParameter,
  recordGraphHistory,
  moveGraphNode,
  moveGraphNodes,
  removeGraphNodes,
  setGraphInputDefault,
  undoGraphHistory,
  redoGraphHistory,
} from "../src/domain/graph-editor.mjs";

const catalog = {
  entries: [
    {
      label: "Constant",
      kind: { type: "Constant", value: 0.5 },
      inputs: [],
      outputs: [{ key: "out", label: "Value", value_type: "Float", min: 0, max: 1, default: 0.5 }],
    },
    {
      label: "Math",
      kind: { type: "Math", op: "Add" },
      inputs: [
        { key: "a", label: "A", value_type: "Float", min: -1, max: 1, default: 0 },
        { key: "b", label: "B", value_type: "Float", min: -1, max: 1, default: 0 },
      ],
      outputs: [{ key: "out", label: "Result", value_type: "Float", min: -1, max: 1, default: 0 }],
    },
    {
      label: "And",
      kind: { type: "Logic", op: "And" },
      inputs: [{ key: "a", label: "A", value_type: "Bool", min: 0, max: 1, default: 0 }],
      outputs: [{ key: "out", label: "Result", value_type: "Bool", min: 0, max: 1, default: 0 }],
    },
  ],
};

function graph() {
  return {
    version: 1,
    nodes: [
      { id: 1, kind: { type: "Constant", value: 0.5 }, pos: [10, 20] },
      { id: 2, kind: { type: "Math", op: "Add" }, pos: [200, 20] },
      { id: 3, kind: { type: "Logic", op: "And" }, pos: [420, 20] },
    ],
    edges: [],
  };
}

test("nodes can be added, moved, and removed without leaving dangling edges", () => {
  const start = graph();
  const added = addGraphNode(start, catalog.entries[0], [80, 140]);
  assert.equal(added.nodes.at(-1).id, 4);
  assert.deepEqual(added.nodes.at(-1).pos, [80, 140]);
  const moved = moveGraphNode(added, 4, [120, 160]);
  assert.deepEqual(moved.nodes.at(-1).pos, [120, 160]);
  const movedSelection = moveGraphNodes(moved, new Map([[1, [20, 30]], [2, [210, 30]]]));
  assert.deepEqual(movedSelection.nodes[0].pos, [20, 30]);
  assert.deepEqual(movedSelection.nodes[1].pos, [210, 30]);
  const connected = connectGraphPins(moved, catalog, { node: 1, pin: "out" }, { node: 2, pin: "a" });
  assert.equal(connected.edges.length, 1);
  const removed = removeGraphNodes(connected, [1]);
  assert.equal(removed.nodes.some((node) => node.id === 1), false);
  assert.deepEqual(removed.edges, []);
});

test("connections enforce Host pin types and replace the existing input edge", () => {
  const start = addGraphNode(graph(), catalog.entries[0], [600, 20]);
  const once = connectGraphPins(start, catalog, { node: 1, pin: "out" }, { node: 2, pin: "a" });
  const replaced = connectGraphPins(once, catalog, { node: 4, pin: "out" }, { node: 2, pin: "a" });
  assert.equal(replaced.edges.length, 1);
  assert.equal(replaced.edges[0].from.node, 4);
  assert.throws(() => connectGraphPins(start, catalog, { node: 1, pin: "out" }, { node: 3, pin: "a" }), /type/i);
});

test("typed defaults and supported scalar parameters remain bounded", () => {
  const start = graph();
  assert.equal(setGraphInputDefault(start, catalog, 2, "a", 3).nodes[1].defaults.a, 1);
  assert.throws(() => setGraphInputDefault(start, catalog, 1, "out", 0.25), /input/i);
  assert.equal(editGraphNodeParameter(start, 1, "value", 0.75).nodes[0].kind.value, 0.75);
  assert.throws(() => editGraphNodeParameter(start, 1, "type", "Device"), /supported/i);
  assert.throws(() => editGraphNodeParameter(start, 1, "value", Number.NaN), /finite/i);
});

test("edge removal removes only the chosen local draft connection", () => {
  const connected = connectGraphPins(graph(), catalog, { node: 1, pin: "out" }, { node: 2, pin: "a" });
  assert.deepEqual(disconnectGraphEdge(connected, 0).edges, []);
});

test("draft history supports undo and redo while bounding its stack", () => {
  const initial = graph();
  const withNode = addGraphNode(initial, catalog.entries[0], [100, 100]);
  const history = createGraphHistory(initial);
  const edited = { ...history, graph: withNode, undo: [...history.undo, initial], redo: [] };
  const undone = undoGraphHistory(edited);
  assert.equal(undone.graph.nodes.length, 3);
  const redone = redoGraphHistory(undone);
  assert.equal(redone.graph.nodes.length, 4);
  let bounded = history;
  for (let index = 0; index < 100; index += 1) {
    bounded = recordGraphHistory(bounded, { ...bounded.graph, view: { pan: [index, 0], zoom: 1 } });
  }
  assert.equal(bounded.undo.length, 80);
});

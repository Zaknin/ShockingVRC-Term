import test from "node:test";
import assert from "node:assert/strict";
import {
  confirmGraphDraftApply,
  createGraphDraftState,
  reconcileGraphDraftWithHost,
  updateGraphDraftState,
} from "../src/domain/graph-workspace-state.mjs";

const hostGraph = { version: 1, nodes: [], edges: [] };
const editedGraph = { version: 1, nodes: [{ id: 1, kind: { type: "Time" }, pos: [20, 20] }], edges: [] };

test("Host refresh preserves a local draft and marks a changed base revision stale", () => {
  const draft = createGraphDraftState(editedGraph, 4, hostGraph, false);
  const refreshed = reconcileGraphDraftWithHost(draft, 5, hostGraph);
  assert.equal(refreshed.baseRevision, 4);
  assert.equal(refreshed.stale, true);
  assert.deepEqual(refreshed.graph, editedGraph);
});

test("saved drafts without revision provenance are stale until explicitly reloaded", () => {
  const draft = createGraphDraftState(editedGraph, null, hostGraph, true);
  assert.equal(draft.stale, true);
  assert.equal(draft.dirty, true);
});

test("editing keeps the original revision and marks a local draft dirty", () => {
  const draft = createGraphDraftState(hostGraph, 9, hostGraph, false);
  const edited = updateGraphDraftState(draft, editedGraph, hostGraph);
  assert.equal(edited.baseRevision, 9);
  assert.equal(edited.dirty, true);
  assert.deepEqual(edited.graph, editedGraph);
});

test("offline edits stay dirty when the current Host document is unavailable", () => {
  const draft = createGraphDraftState(hostGraph, 9, hostGraph, false);
  assert.equal(updateGraphDraftState(draft, editedGraph, null).dirty, true);
});

test("only confirmed apply advances the draft base and conflict keeps the draft stale", () => {
  const draft = createGraphDraftState(editedGraph, 9, hostGraph, true);
  const applied = confirmGraphDraftApply(draft, { state: "applied", graph_revision: 10 });
  assert.equal(applied.baseRevision, 10);
  assert.equal(applied.stale, false);
  assert.equal(applied.dirty, false);

  const conflict = confirmGraphDraftApply(draft, { state: "conflict", current_revision: 12 });
  assert.equal(conflict.baseRevision, 9);
  assert.equal(conflict.stale, true);
  assert.equal(conflict.dirty, true);
});

test("a changed Host revision after confirmed apply keeps the local draft visible and stale", () => {
  const draft = createGraphDraftState(editedGraph, 9, hostGraph, true);
  const applied = confirmGraphDraftApply(draft, {
    state: "host_changed",
    graph_revision: 10,
    current_revision: 11,
  });
  const refreshed = reconcileGraphDraftWithHost(applied, 11, {
    version: 1,
    nodes: [{ id: 2, kind: { type: "Device", mac: "AA:BB" }, pos: [40, 60] }],
    edges: [],
  });

  assert.equal(refreshed.baseRevision, 10);
  assert.equal(refreshed.stale, true);
  assert.equal(refreshed.dirty, true);
  assert.deepEqual(refreshed.graph, editedGraph);
});

test("unknown apply identity stays visible and blocks replay across Host refresh", () => {
  const draft = createGraphDraftState(editedGraph, 9, hostGraph, true);
  const unknown = confirmGraphDraftApply(draft, {
    state: "unknown",
    operation_id: "2d7f96f5-e0a0-4236-89f0-9a9f901b345c",
  });
  const refreshed = reconcileGraphDraftWithHost(unknown, 9, hostGraph);

  assert.equal(refreshed.stale, true);
  assert.equal(refreshed.pendingApplyId, "2d7f96f5-e0a0-4236-89f0-9a9f901b345c");
  assert.deepEqual(refreshed.graph, editedGraph);
});

test("confirmed apply without a readable Host document remains stale until reconciliation", () => {
  const draft = createGraphDraftState(editedGraph, 9, hostGraph, true);
  const unreconciled = confirmGraphDraftApply(draft, {
    state: "applied_unreconciled",
    graph_revision: 10,
    current_revision: null,
  });
  assert.equal(unreconciled.baseRevision, 10);
  assert.equal(unreconciled.stale, true);

  const refreshed = reconcileGraphDraftWithHost(unreconciled, 10, editedGraph);
  assert.equal(refreshed.stale, false);
  assert.equal(refreshed.dirty, false);
});

test("confirmed apply adopts Host normalization only at the receipt revision", () => {
  const submitted = createGraphDraftState(editedGraph, 9, hostGraph, true);
  const applied = confirmGraphDraftApply(submitted, {
    state: "applied",
    graph_revision: 10,
    current_revision: 10,
  });
  const normalizedHostGraph = {
    version: 1,
    nodes: [{ id: 2, kind: { type: "Device", mac: "AA:BB" }, pos: [40, 60] }],
    edges: [],
  };

  const reconciled = reconcileGraphDraftWithHost(applied, 10, normalizedHostGraph);
  assert.deepEqual(reconciled.graph, normalizedHostGraph);
  assert.equal(reconciled.baseRevision, 10);
  assert.equal(reconciled.stale, false);
  assert.equal(reconciled.dirty, false);
});

test("confirmed apply keeps edits made while the request was pending", () => {
  const submitted = createGraphDraftState(editedGraph, 9, hostGraph, true);
  const draftChangedDuringApply = updateGraphDraftState(submitted, {
    version: 1,
    nodes: [{ id: 3, kind: { type: "Device", mac: "CC:DD" }, pos: [80, 90] }],
    edges: [],
  }, hostGraph);
  const applied = confirmGraphDraftApply(draftChangedDuringApply, {
    state: "applied",
    graph_revision: 10,
    current_revision: 10,
  }, submitted.graph);
  const hostGraphAtReceipt = {
    version: 1,
    nodes: [{ id: 2, kind: { type: "Device", mac: "AA:BB" }, pos: [40, 60] }],
    edges: [],
  };

  const reconciled = reconcileGraphDraftWithHost(applied, 10, hostGraphAtReceipt);
  assert.deepEqual(reconciled.graph, draftChangedDuringApply.graph);
  assert.equal(reconciled.baseRevision, 9);
  assert.equal(reconciled.stale, true);
  assert.equal(reconciled.dirty, true);
});

test("confirmed apply followed by a later Host revision keeps its draft stale", () => {
  const submitted = createGraphDraftState(editedGraph, 9, hostGraph, true);
  const applied = confirmGraphDraftApply(submitted, {
    state: "applied",
    graph_revision: 10,
    current_revision: 10,
  });
  const laterHostGraph = {
    version: 1,
    nodes: [{ id: 2, kind: { type: "Device", mac: "AA:BB" }, pos: [40, 60] }],
    edges: [],
  };

  const reconciled = reconcileGraphDraftWithHost(applied, 11, laterHostGraph);
  assert.deepEqual(reconciled.graph, editedGraph);
  assert.equal(reconciled.baseRevision, 10);
  assert.equal(reconciled.stale, true);
  assert.equal(reconciled.dirty, true);
});

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

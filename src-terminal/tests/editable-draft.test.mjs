import assert from "node:assert/strict";
import test from "node:test";
import { beginEdit, commitDraft, initialDraft, syncDraftOnSnapshot, updateDraft } from "../src/domain/editable-draft.mjs";

test("incoming snapshots never overwrite a draft during a pointer or number edit", () => {
  let state = beginEdit(initialDraft(20, 4));
  state = updateDraft(state, 35);
  const next = syncDraftOnSnapshot(state, 22, 5);
  assert.equal(next.draft, 35);
  assert.equal(next.editing, true);
});

test("committed values remain pending until a newer snapshot confirms them", () => {
  let state = updateDraft(initialDraft(20, 4), 35);
  state = commitDraft(state, 20, 4);
  assert.equal(syncDraftOnSnapshot(state, 20, 4).pending, true);
  const confirmed = syncDraftOnSnapshot(state, 35, 5);
  assert.equal(confirmed.draft, 35);
  assert.equal(confirmed.pending, false);
  assert.equal(confirmed.rejected, false);
});

test("a newer telemetry snapshot that disagrees with the target keeps the target pending", () => {
  let state = updateDraft(initialDraft(20, 4), 35);
  state = commitDraft(state, 20, 4);
  const rejected = syncDraftOnSnapshot(state, 24, 5);
  assert.equal(rejected.draft, 35);
  assert.equal(rejected.pending, true);
  assert.equal(rejected.rejected, false);
});

test("a higher telemetry revision with the unchanged Host value does not reject the queued target", () => {
  let state = updateDraft(initialDraft(30, 10), 60);
  state = commitDraft(state, 30, 10);

  const afterTelemetry = syncDraftOnSnapshot(state, 30, 11);

  assert.equal(afterTelemetry.draft, 60);
  assert.equal(afterTelemetry.pending, true);
  assert.equal(afterTelemetry.rejected, false);
});

test("leaving an unchanged numeric edit ends editing so newer snapshots are accepted", () => {
  const focused = beginEdit(initialDraft(20, 4));
  const committed = commitDraft(focused, 20, 4);
  assert.equal(committed.editing, false);
  assert.equal(committed.pending, false);
  assert.equal(syncDraftOnSnapshot(committed, 22, 5).draft, 22);
});

test("only the matching tracked outcome settles a pending draft", () => {
  let state = updateDraft(initialDraft(20, 4), 35);
  state = commitDraft(state, 20, 4);

  const queuedMatchingSnapshot = syncDraftOnSnapshot(state, 35, 5, "pending");
  assert.equal(queuedMatchingSnapshot.draft, 35);
  assert.equal(queuedMatchingSnapshot.pending, true);

  const confirmed = syncDraftOnSnapshot(queuedMatchingSnapshot, 35, 6, "confirmed");
  assert.equal(confirmed.draft, 35);
  assert.equal(confirmed.pending, false);
  assert.equal(confirmed.rejected, false);

  const rejected = syncDraftOnSnapshot(state, 24, 5, "rejected");
  assert.equal(rejected.draft, 24);
  assert.equal(rejected.pending, false);
  assert.equal(rejected.rejected, true);

  const unknown = syncDraftOnSnapshot(state, 27, 5, "unknown");
  assert.equal(unknown.draft, 27);
  assert.equal(unknown.pending, false);
  assert.equal(unknown.unknown, true);
});

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

test("a newer snapshot that disagrees with the request restores actual Host state", () => {
  let state = updateDraft(initialDraft(20, 4), 35);
  state = commitDraft(state, 20, 4);
  const rejected = syncDraftOnSnapshot(state, 24, 5);
  assert.equal(rejected.draft, 24);
  assert.equal(rejected.pending, false);
  assert.equal(rejected.rejected, true);
});

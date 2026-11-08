import test from "node:test";
import assert from "node:assert/strict";
import {
  acknowledgeEngineModeChange,
  acceptEngineModeSnapshot,
  beginEngineModeChange,
  canRequestEngineMode,
  createGraphSessionState,
  expireEngineModeChange,
  hasGraphFeature,
  rejectEngineModeChange,
  updateEngineModeConnection,
} from "./graph-session.mjs";

const context = (overrides = {}) => ({
  session_generation: 4,
  server_epoch: "host-epoch",
  transport_generation: 8,
  seq: 20,
  ...overrides,
});

const queuedAck = (overrides = {}) => ({
  request_id: "mode-request",
  queued: true,
  session_generation: 4,
  server_epoch: "host-epoch",
  transport_generation: 8,
  snapshot_seq: 22,
  ...overrides,
});

function requestedMode(state = createGraphSessionState(), now = 100) {
  return beginEngineModeChange(state, {
    mode: "graph",
    context: context(),
    supported: true,
    now,
  });
}

test("mode acknowledgement stays pending until a newer Host snapshot confirms it", () => {
  let state = requestedMode();
  assert.equal(state.modeChange.status, "pending");
  assert.equal(state.modeChange.message, "Switching mode…");
  state = acknowledgeEngineModeChange(state, queuedAck(), 120);
  assert.equal(state.modeChange.status, "queued");
  assert.equal(state.modeChange.message, "Switching mode…");

  state = acceptEngineModeSnapshot(state, {
    context: context({ seq: 22 }), engineMode: "graph", phase: "online", now: 130,
  });
  assert.equal(state.modeChange.status, "queued", "the acknowledgement snapshot is not newer");

  state = acceptEngineModeSnapshot(state, {
    context: context({ seq: 23 }), engineMode: "graph", phase: "online", now: 140,
  });
  assert.equal(state.modeChange.status, "confirmed");
  assert.equal(state.modeChange.requestedMode, "graph");
  assert.equal(state.modeChange.message, "Host confirmed");
  assert.equal(state.modeChange.acknowledgementContext.seq, 22, "confirmation metadata remains available internally");
});

test("a fresh snapshot that races ahead of the acknowledgement is reconciled after the queue ack", () => {
  let state = requestedMode();
  state = acceptEngineModeSnapshot(state, {
    context: context({ seq: 24 }), engineMode: "graph", phase: "online", now: 110,
  });
  assert.equal(state.modeChange.status, "pending");
  state = acknowledgeEngineModeChange(state, queuedAck(), 120);
  assert.equal(state.modeChange.status, "confirmed");
});

test("competing newer Host state becomes stale and an older frame cannot change it", () => {
  let state = requestedMode();
  state = acknowledgeEngineModeChange(state, queuedAck(), 120);
  state = acceptEngineModeSnapshot(state, {
    context: context({ seq: 23 }), engineMode: "classic", phase: "online", now: 130,
  });
  assert.equal(state.modeChange.status, "stale");
  state = acceptEngineModeSnapshot(state, {
    context: context({ seq: 21 }), engineMode: "graph", phase: "online", now: 140,
  });
  assert.equal(state.modeChange.status, "stale");
});

test("same-session reconnect is visible and can confirm from a fresh resumed snapshot", () => {
  let state = requestedMode();
  state = acknowledgeEngineModeChange(state, queuedAck(), 120);
  state = updateEngineModeConnection(state, { phase: "reconnecting", context: context() });
  assert.equal(state.modeChange.status, "reconnecting");
  assert.equal(state.modeChange.message, "Connection lost");
  state = updateEngineModeConnection(state, {
    phase: "online", context: context({ transport_generation: 9 }),
  });
  assert.equal(state.modeChange.status, "queued");
  state = acceptEngineModeSnapshot(state, {
    context: context({ transport_generation: 9, seq: 23 }), engineMode: "graph", phase: "online", now: 150,
  });
  assert.equal(state.modeChange.status, "confirmed");
});

test("a different Host session makes the pending request stale", () => {
  let state = requestedMode();
  state = acknowledgeEngineModeChange(state, queuedAck(), 120);
  state = updateEngineModeConnection(state, {
    phase: "online", context: context({ session_generation: 5, server_epoch: "new-host" }),
  });
  assert.equal(state.modeChange.status, "stale");
  assert.equal(canRequestEngineMode(state, true), true, "a user may start a fresh request explicitly");
});

test("timeouts and ambiguous command errors remain unknown and are never retried", () => {
  let state = requestedMode(createGraphSessionState(), 1_000);
  state = expireEngineModeChange(state, 11_000);
  assert.equal(state.modeChange.status, "unknown");
  assert.equal(state.modeChange.message, "Change not confirmed");
  assert.equal(canRequestEngineMode(state, true), true);

  state = requestedMode(state, 12_000);
  state = rejectEngineModeChange(state, "Timed out waiting for Host command acknowledgement; outcome is unknown.", 22_001);
  assert.equal(state.modeChange.status, "unknown");
  assert.equal(state.modeChange.message, "Change not confirmed");
  assert.equal(canRequestEngineMode(state, true), true);
});

test("stale and unknown changes stay distinct from queued, rejected, and confirmed outcomes", () => {
  let stale = requestedMode();
  stale = acknowledgeEngineModeChange(stale, queuedAck(), 120);
  stale = acceptEngineModeSnapshot(stale, {
    context: context({ seq: 23 }), engineMode: "classic", phase: "online", now: 130,
  });
  assert.equal(stale.modeChange.status, "stale");
  assert.equal(stale.modeChange.message, "Change not confirmed");

  let rejected = requestedMode();
  rejected = rejectEngineModeChange(rejected, "Host rejected the engine mode change.", 130);
  assert.equal(rejected.modeChange.status, "rejected");
  assert.equal(rejected.modeChange.message, "Host rejected the engine mode change.");
});

test("rejections and missing mode support are visible without a queued action", () => {
  let state = requestedMode();
  state = rejectEngineModeChange(state, "Host rejected engine.set_mode", 130);
  assert.equal(state.modeChange.status, "rejected");
  assert.equal(canRequestEngineMode(state, true), true);

  assert.equal(hasGraphFeature({ features: ["engine.mode.set.v1"] }, "engine.mode.set.v1"), true);
  assert.equal(hasGraphFeature({ operations: [] }, "engine.mode.set.v1"), false);
  assert.equal(hasGraphFeature(null, "engine.mode.set.v1"), false);
  const unsupported = beginEngineModeChange(createGraphSessionState(), {
    mode: "graph", context: context(), supported: false, now: 500,
  });
  assert.equal(unsupported.modeChange.status, "unsupported");
  assert.equal(canRequestEngineMode(unsupported, false), false);
});

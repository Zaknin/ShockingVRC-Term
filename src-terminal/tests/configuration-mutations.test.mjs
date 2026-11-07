import assert from "node:assert/strict";
import test from "node:test";
import {
  acceptConfigurationSnapshot,
  beginConfigurationMutation,
  configurationMutationDescriptors,
  createConfigurationSyncState,
  expireConfigurationMutations,
  markConfigurationCommandRejected,
  markConfigurationCommandQueued,
  reconcileConfigurationSession,
  readConfigurationValue,
  configurationValuesEqual,
} from "../src/domain/configuration-mutations.mjs";

const active = { session_generation: 4, server_epoch: "epoch-a", transport_generation: 2 };
const context = (seq, overrides = {}) => ({ ...active, seq, ...overrides });
const snapshot = (seq, value, overrides = {}) => ({
  ...context(seq, overrides),
  bridge_revision: seq * 100,
  config: { channel_a: { limits: { max: value, min: 10 } } },
});
const begin = (state, target, operationId, seq = 1, now = 0, key = "channels.A.limits.max") =>
  beginConfigurationMutation(state, {
    key,
    operation: "channels.set_limit_max",
    target,
    operationId,
    context: context(seq),
    now,
  });
const queue = (state, operationId, seq = 1) =>
  markConfigurationCommandQueued(state, {
    operationId,
    requestId: `request-${operationId}`,
    context: context(seq),
  });
const accept = (state, seq, value, now = 0, overrides = {}, activeOverrides = overrides) =>
  acceptConfigurationSnapshot(state, snapshot(seq, value, overrides), { ...active, ...activeOverrides }, now);

test("30 → 60 → 90 → 40 → 70 survives old Host values at higher telemetry revisions", () => {
  let state = createConfigurationSyncState();
  state = accept(state, 1, 30).state;
  for (const [index, target] of [60, 90, 40, 70].entries()) {
    const operationId = index + 1;
    state = begin(state, target, operationId, index + 1, index * 10);
    state = queue(state, operationId, index + 1);
    const unchanged = accept(state, index + 2, 30, index * 10 + 1);
    assert.equal(unchanged.accepted, true);
    state = unchanged.state;
    assert.equal(state.mutations["channels.A.limits.max"].status, "pending");
    assert.equal(state.mutations["channels.A.limits.max"].target, target);
  }
  state = accept(state, 6, 70, 50).state;
  assert.equal(state.mutations["channels.A.limits.max"].status, "confirmed");
  assert.equal(state.mutations["channels.A.limits.max"].observedHostValue, 70);
});

test("Worker delay allows several newer telemetry snapshots without clearing a queued target", () => {
  let state = accept(createConfigurationSyncState(), 1, 30).state;
  state = queue(begin(state, 60, 7, 1), 7, 1);
  for (const seq of [2, 3, 4, 5]) {
    state = accept(state, seq, 30).state;
    assert.equal(state.mutations["channels.A.limits.max"].status, "pending");
  }
  state = accept(state, 6, 60).state;
  assert.equal(state.mutations["channels.A.limits.max"].status, "confirmed");
});

test("a delayed older snapshot cannot roll back a newer accepted Host snapshot or target", () => {
  let state = accept(createConfigurationSyncState(), 10, 30).state;
  state = queue(begin(state, 90, 1, 10), 1, 10);
  state = accept(state, 12, 40).state;
  const stale = accept(state, 11, 60);
  assert.equal(stale.accepted, false);
  assert.equal(stale.state.latestSnapshot.config.channel_a.limits.max, 40);
  assert.equal(stale.state.mutations["channels.A.limits.max"].target, 90);
  assert.equal(stale.state.mutations["channels.A.limits.max"].status, "pending");
});

test("multiple pending fields are tracked independently and duplicate snapshots are ignored", () => {
  let state = accept(createConfigurationSyncState(), 1, 30).state;
  state = begin(state, 70, 11, 1, 0, "channels.A.limits.max");
  state = begin(state, 20, 12, 1, 0, "channels.A.limits.min");
  state = queue(state, 11, 1);
  state = queue(state, 12, 1);
  const first = accept(state, 2, 30);
  assert.equal(first.accepted, true);
  const duplicate = accept(first.state, 2, 30);
  assert.equal(duplicate.accepted, false);
  assert.equal(duplicate.state.mutations["channels.A.limits.max"].status, "pending");
  assert.equal(duplicate.state.mutations["channels.A.limits.min"].status, "pending");
});

test("an explicit Host rejection settles only the matching latest operation", () => {
  let state = accept(createConfigurationSyncState(), 1, 30).state;
  state = begin(state, 60, 1, 1);
  state = begin(state, 90, 2, 1);
  state = markConfigurationCommandRejected(state, { operationId: 1, now: 5 });
  assert.equal(state.mutations["channels.A.limits.max"].status, "pending");
  state = markConfigurationCommandRejected(state, { operationId: 2, now: 6 });
  assert.equal(state.mutations["channels.A.limits.max"].status, "rejected");
  assert.equal(state.mutations["channels.A.limits.max"].target, 90);
});

test("Host normalization remains unconfirmed and timeout reconciles to the actual Host value", () => {
  let state = accept(createConfigurationSyncState(), 1, 30).state;
  state = queue(begin(state, 90, 1, 1, 0), 1, 1);
  state = accept(state, 2, 70, 100).state;
  assert.equal(state.mutations["channels.A.limits.max"].status, "pending");
  state = expireConfigurationMutations(state, 10_000);
  assert.equal(state.mutations["channels.A.limits.max"].status, "unknown");
  assert.equal(state.mutations["channels.A.limits.max"].observedHostValue, 70);
});

test("a snapshot that matches before the queued acknowledgement does not confirm the request", () => {
  let state = accept(createConfigurationSyncState(), 1, 30).state;
  state = begin(state, 60, 9, 1);
  state = accept(state, 2, 60).state;
  assert.equal(state.mutations["channels.A.limits.max"].status, "pending");
  state = queue(state, 9, 2);
  state = accept(state, 3, 60).state;
  assert.equal(state.mutations["channels.A.limits.max"].status, "confirmed");
});

test("same-value edits do not create a pending mutation", () => {
  const state = accept(createConfigurationSyncState(), 1, 30).state;
  const next = begin(state, 30, 3, 1);
  assert.equal(next, state);
  assert.equal(next.mutations["channels.A.limits.max"], undefined);
});

test("a concurrent Host-local change is not misreported as command rejection", () => {
  let state = accept(createConfigurationSyncState(), 1, 30).state;
  state = queue(begin(state, 60, 1, 1), 1, 1);
  state = accept(state, 2, 45, 100).state;
  assert.equal(state.mutations["channels.A.limits.max"].status, "pending");
  state = expireConfigurationMutations(state, 10_000);
  assert.equal(state.mutations["channels.A.limits.max"].status, "unknown");
  assert.equal(state.mutations["channels.A.limits.max"].observedHostValue, 45);
});

test("disconnect/reconnect invalidates pending context and reconciles from the new Host snapshot", () => {
  let state = accept(createConfigurationSyncState(), 1, 30).state;
  state = queue(begin(state, 60, 1, 1), 1, 1);
  state = reconcileConfigurationSession(state, { ...active, transport_generation: 3 }, 5);
  assert.equal(state.mutations["channels.A.limits.max"].status, "unknown");
  const recovered = accept(state, 1, 45, 6, { transport_generation: 3 });
  assert.equal(recovered.accepted, true);
  assert.equal(recovered.state.mutations["channels.A.limits.max"].observedHostValue, 45);
  assert.equal(recovered.state.mutations["channels.A.limits.max"].status, "unknown");
});

test("snapshots from older transport or local session generations are ignored", () => {
  let state = accept(createConfigurationSyncState(), 20, 30).state;
  const olderTransport = accept(state, 99, 60, 0, { transport_generation: 1 }, active);
  assert.equal(olderTransport.accepted, false);
  const olderSession = accept(state, 100, 60, 0, { session_generation: 3 }, active);
  assert.equal(olderSession.accepted, false);
  assert.equal(state.latestSnapshot.config.channel_a.limits.max, 30);
});

test("sequence numbers reset across server epochs instead of being compared", () => {
  const epochB = { ...active, server_epoch: "epoch-b", transport_generation: 1 };
  let state = accept(createConfigurationSyncState(), 500, 30).state;
  const newerEpoch = acceptConfigurationSnapshot(state, {
    ...epochB, seq: 1, bridge_revision: 1,
    config: { channel_a: { limits: { max: 80, min: 10 } } },
  }, epochB, 10);
  assert.equal(newerEpoch.accepted, true);
  assert.equal(newerEpoch.state.cursor.seq, 1);
  assert.equal(newerEpoch.state.cursor.server_epoch, "epoch-b");
});

test("configuration lookup reads the HostSnapshot payload inside a sequenced event envelope", () => {
  const value = snapshot(5, 45);
  assert.equal(readConfigurationValue({ state: value, ...context(5) }, "channels.A.limits.max"), 45);
});

test("a queued acknowledgement from another session context cannot authorize snapshot confirmation", () => {
  let state = accept(createConfigurationSyncState(), 1, 30).state;
  state = begin(state, 60, 17, 1);
  state = markConfigurationCommandQueued(state, {
    operationId: 17,
    requestId: "stale-ack",
    context: context(1, { session_generation: 3 }),
  });
  assert.equal(state.mutations["channels.A.limits.max"].queued, false);
  state = accept(state, 2, 60).state;
  assert.equal(state.mutations["channels.A.limits.max"].status, "pending");
});

test("JSON configuration values compare structurally across Rust serialization key order", () => {
  assert.equal(configurationValuesEqual({ function: { kind: "sine", rate: 2 }, source: "Depth" },
    { source: "Depth", function: { rate: 2, kind: "sine" } }), true);
  assert.equal(configurationValuesEqual({ value: 2 }, { value: 3 }), false);
});

test("modulation apply and clear mutations wait for their exact Host slot snapshot", () => {
  const slot = { source: "Depth", function: { kind: "sine", rate: 2 } };
  const initialSnapshot = {
    bridge_revision: 1,
    config: { channel_a: { freq_modulation: [null, null, null, null] } },
  };
  const applyOperation = { op: "modulation.apply", args: { channel: "A", kind: "freq", segment: 0, config: slot } };
  const descriptors = configurationMutationDescriptors(applyOperation, initialSnapshot);
  assert.equal(descriptors.length, 1);
  assert.equal(descriptors[0].key, "channels.A.freq_modulation.0");
  let state = createConfigurationSyncState();
  state = { ...state, latestSnapshot: { ...context(1), state: initialSnapshot } };
  state = beginConfigurationMutation(state, { ...descriptors[0], operationId: 23, context: active, now: 0 });
  state = queue(state, 23, 1);
  const applied = { ...initialSnapshot, config: { channel_a: { freq_modulation: [{ function: { rate: 2, kind: "sine" }, source: "Depth" }, null, null, null] } } };
  const accepted = acceptConfigurationSnapshot(state, { ...context(2), state: applied }, active, 1);
  assert.equal(accepted.state.mutations[descriptors[0].key].status, "confirmed");

  const clear = configurationMutationDescriptors({ op: "modulation.clear_segment", args: { channel: "A", kind: "freq", segment: 0 } }, applied);
  assert.deepEqual(clear.map(({ key, target }) => [key, target]), [["channels.A.freq_modulation.0", null]]);
});

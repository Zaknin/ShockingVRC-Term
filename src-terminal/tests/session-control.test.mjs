import test from "node:test";
import assert from "node:assert/strict";
import * as sessionControl from "../src/domain/session-control.mjs";
import {
  canBeginConnection,
  canManuallyReconnect,
  clearsSnapshotForPhase,
  DEFAULT_TERMINAL_SECTION,
  sectionAfterConnectionUpdate,
  shouldAcceptConnectionEvent,
} from "../src/domain/session-control.mjs";

test("offline terminal end states clear snapshots while reconnecting may retain stale state", () => {
  assert.equal(clearsSnapshotForPhase("reconnecting"), false);
  assert.equal(clearsSnapshotForPhase("online"), false);
  for (const phase of ["user_disconnected", "host_kicked", "session_revoked", "host_unavailable", "transport_lost"]) {
    assert.equal(clearsSnapshotForPhase(phase), true, `${phase} must not present a cached snapshot as current`);
  }
});

test("manual reconnect is offered for recoverable endings but not revoked pairings", () => {
  for (const phase of ["user_disconnected", "host_kicked", "host_unavailable", "transport_lost"]) {
    assert.equal(canManuallyReconnect(phase), true, `${phase} should offer a user initiated reconnect`);
  }
  assert.equal(canManuallyReconnect("session_revoked"), false);
  assert.equal(canManuallyReconnect("online"), false);
  assert.equal(canManuallyReconnect("connecting"), false);
  assert.equal(canManuallyReconnect("reconnecting"), false);
});

test("new pairing can start after revocation but cannot overlap an active or connecting session", () => {
  assert.equal(canBeginConnection("session_revoked"), true);
  assert.equal(canBeginConnection("user_disconnected"), true);
  assert.equal(canBeginConnection("connecting"), false);
  assert.equal(canBeginConnection("online"), false);
  assert.equal(canBeginConnection("reconnecting"), false);
});

test("a pending invitation can start pairing after revocation while saved reconnect stays blocked", () => {
  const canStartProfileConnection = sessionControl.canStartProfileConnection;
  assert.equal(typeof canStartProfileConnection, "function", "the UI needs a distinct gate for a new invitation");

  assert.equal(canStartProfileConnection("session_revoked", true), true);
  assert.equal(canStartProfileConnection("session_revoked", false), false);
  assert.equal(canStartProfileConnection("connecting", true), false);
  assert.equal(canStartProfileConnection("online", true), false);
});

test("deleting the selected final profile clears stale selection but preserves another live selection", () => {
  const selectedProfileIdAfterListChange = sessionControl.selectedProfileIdAfterListChange;
  assert.equal(typeof selectedProfileIdAfterListChange, "function", "deleted profiles must not remain selected");

  assert.equal(selectedProfileIdAfterListChange("host-a", [], null), "");
  assert.equal(selectedProfileIdAfterListChange("host-a", ["host-b"], null), "");
  assert.equal(selectedProfileIdAfterListChange("host-a", ["host-a"], null), "host-a");
  assert.equal(selectedProfileIdAfterListChange("host-a", [], "host-a"), "host-a");
});

test("a stale pairing callback cannot replace a newer invitation result", () => {
  const canApplyConnectionResult = sessionControl.canApplyConnectionResult;
  assert.equal(typeof canApplyConnectionResult, "function", "pairing callbacks must be fenced by request generation");

  assert.equal(canApplyConnectionResult(8, 9, "host-a", "host-a"), false);
  assert.equal(canApplyConnectionResult(9, 9, "host-a", "host-a"), true);
  assert.equal(canApplyConnectionResult(9, 9, "host-a", "host-b"), false);
  assert.equal(canApplyConnectionResult(9, 9, "host-a", null), true);
});

test("cold start opens Connect and the reconnect journey remains on Status", () => {
  assert.equal(DEFAULT_TERMINAL_SECTION, "Connect");
  let section = sectionAfterConnectionUpdate(DEFAULT_TERMINAL_SECTION, "online");
  assert.equal(section, "Status");
  section = sectionAfterConnectionUpdate(section, "user_disconnected");
  assert.equal(section, "Status");
  section = sectionAfterConnectionUpdate(section, "connecting");
  assert.equal(section, "Status");
  section = sectionAfterConnectionUpdate(section, "online");
  assert.equal(section, "Status");
});

test("connection events cannot resurrect an ended session generation", () => {
  const ended = { phase: "disconnected", session_generation: 4, server_epoch: null, transport_generation: null };
  const staleOnline = { phase: "online", session_generation: 4, server_epoch: "epoch-a", transport_generation: 2 };
  const nextAttempt = { phase: "connecting", session_generation: 5, server_epoch: null, transport_generation: null };
  assert.equal(shouldAcceptConnectionEvent(ended, staleOnline, 4), false);
  assert.equal(shouldAcceptConnectionEvent(ended, nextAttempt, 4), true);
  assert.equal(shouldAcceptConnectionEvent({ phase: "online", session_generation: 5, server_epoch: "epoch-a", transport_generation: 3 },
    { phase: "online", session_generation: 5, server_epoch: "epoch-a", transport_generation: 2 }, 5), false);
  assert.equal(shouldAcceptConnectionEvent({ phase: "online", session_generation: 5 },
    { phase: "online", session_generation: null }, 5), false);
});

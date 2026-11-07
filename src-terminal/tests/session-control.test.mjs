import test from "node:test";
import assert from "node:assert/strict";
import {
  canBeginConnection,
  canManuallyReconnect,
  clearsSnapshotForPhase,
  DEFAULT_TERMINAL_SECTION,
  sectionAfterConnectionUpdate,
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

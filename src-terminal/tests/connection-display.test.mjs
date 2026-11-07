import test from "node:test";
import assert from "node:assert/strict";
import * as connectionDisplay from "../src/domain/connection-display.mjs";
import { connectionIndicator, connectionStatusLabel, transportLabel } from "../src/domain/connection-display.mjs";

test("transport labels distinguish direct WSS, ICE peer paths, and UDP TURN relay", () => {
  assert.equal(transportLabel("direct_wss", null, "online"), "Direct");
  assert.equal(transportLabel("webrtc_datachannel", "LOCAL HOST/UDP ↔ REMOTE HOST/UDP", "online"), "ICE peer path · UDP");
  assert.equal(transportLabel("webrtc_datachannel", "LOCAL RELAY/UDP ↔ REMOTE RELAY/UDP", "online"), "TURN relay · UDP");
});

test("unknown and establishing transport paths use short status labels", () => {
  assert.equal(transportLabel(null, null, "connecting"), "Connecting");
  assert.equal(transportLabel(null, null, "online"), "Path unknown");
  assert.equal(transportLabel(null, null, "reconnecting"), "Reconnecting");
});

test("session end reasons have distinct status labels and do not look connected", () => {
  assert.equal(connectionStatusLabel("user_disconnected"), "DISCONNECTED BY YOU");
  assert.equal(connectionStatusLabel("host_kicked"), "HOST ENDED SESSION");
  assert.equal(connectionStatusLabel("session_revoked"), "ACCESS REVOKED");
  assert.equal(connectionStatusLabel("host_unavailable"), "HOST UNAVAILABLE");
  assert.equal(connectionStatusLabel("transport_lost"), "CONNECTION LOST");
  for (const phase of ["user_disconnected", "host_kicked", "session_revoked", "host_unavailable", "transport_lost"]) {
    assert.equal(connectionIndicator(phase), "error", `${phase} must present an ended connection state`);
  }
});

test("empty Host state explains kick, manual disconnect, and revocation separately", () => {
  const emptyConnectionGuidance = connectionDisplay.emptyConnectionGuidance;
  assert.equal(typeof emptyConnectionGuidance, "function", "the empty state needs reason-aware guidance");

  assert.match(emptyConnectionGuidance("host_kicked"), /Host ended this session/i);
  assert.match(emptyConnectionGuidance("user_disconnected"), /You disconnected/i);
  assert.match(emptyConnectionGuidance("session_revoked"), /new Host invitation/i);
});

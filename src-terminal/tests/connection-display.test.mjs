import test from "node:test";
import assert from "node:assert/strict";
import { transportLabel } from "../src/domain/connection-display.mjs";

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

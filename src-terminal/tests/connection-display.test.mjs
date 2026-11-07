import test from "node:test";
import assert from "node:assert/strict";
import { transportLabel } from "../src/domain/connection-display.mjs";

test("transport labels stay human-friendly while distinguishing direct WSS from WebRTC paths", () => {
  assert.equal(transportLabel("direct_wss", null, "online"), "Direct");
  assert.equal(transportLabel("webrtc_datachannel", "LOCAL HOST ↔ REMOTE HOST", "online"), "P2P");
  assert.equal(transportLabel("webrtc_datachannel", "LOCAL RELAY ↔ REMOTE HOST", "online"), "Relay");
});

test("unknown and establishing transport paths use short status labels", () => {
  assert.equal(transportLabel(null, null, "connecting"), "Connecting");
  assert.equal(transportLabel(null, null, "online"), "Path unknown");
  assert.equal(transportLabel(null, null, "reconnecting"), "Reconnecting");
});

import test from "node:test";
import assert from "node:assert/strict";
import { connectionIndicator, connectionStatusLabel, transportLabel } from "../src/domain/connection-display.mjs";

test("connection display follows the live transport and candidate pair", () => {
  assert.equal(transportLabel("direct_wss", null, "online"), "DIRECT WSS");
  assert.equal(transportLabel("webrtc_datachannel", "local host -> remote relay", "online"), "TURN RELAY");
  assert.equal(transportLabel("webrtc_datachannel", "local host -> remote host", "online"), "ICE P2P");
  assert.equal(transportLabel("webrtc_datachannel", null, "online"), "ICE PATH UNKNOWN");
});

test("connection display does not claim online for stale or disconnected sessions", () => {
  assert.equal(connectionStatusLabel("reconnecting"), "RECONNECTING");
  assert.equal(connectionStatusLabel("disconnected"), "DISCONNECTED");
  assert.equal(connectionIndicator("online"), "ok");
  assert.equal(connectionIndicator("connecting"), "warn");
  assert.equal(connectionIndicator("revoked"), "error");
  assert.equal(connectionIndicator("disconnected"), "idle");
});

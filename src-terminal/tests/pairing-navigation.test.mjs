import assert from "node:assert/strict";
import test from "node:test";
import { sectionAfterPairingInvitation } from "../src/pairing-navigation.mjs";

test("an incoming pairing invitation routes every active section to Connect", () => {
  for (const section of ["Status", "Zones", "Channels", "Tuning", "Modulation", "Presets", "Alarm", "Setup", "Logs"]) {
    assert.equal(sectionAfterPairingInvitation(section, 1, "host-profile"), "Connect");
  }
});

test("pairing navigation leaves the current section alone without a pending invitation", () => {
  assert.equal(sectionAfterPairingInvitation("Status", 0, "host-profile"), "Status");
  assert.equal(sectionAfterPairingInvitation("Status", 1, null), "Status");
});

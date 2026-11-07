import assert from "node:assert/strict";
import test from "node:test";
import { telemetryBarPresentation, telemetryResetIdentity } from "../src/domain/telemetry-smoothing.mjs";

test("only continuing positive contact telemetry receives a short display transition", () => {
  assert.deepEqual(telemetryBarPresentation(0.4, 0.5, true), { value: 0.5, transitionMs: 80 });
  assert.deepEqual(telemetryBarPresentation(0, 0.5, true), { value: 0.5, transitionMs: 0 });
  assert.deepEqual(telemetryBarPresentation(0.5, 0, true), { value: 0, transitionMs: 0 });
  assert.deepEqual(telemetryBarPresentation(0.5, null, true), { value: null, transitionMs: 0 });
  assert.deepEqual(telemetryBarPresentation(0.4, 0.5, false), { value: 0.5, transitionMs: 0 });
});

test("display clamping never alters the exact value kept in application state", () => {
  const raw = 1.25;
  assert.deepEqual(telemetryBarPresentation(0.5, raw, true), { value: 1, transitionMs: 80 });
  assert.equal(raw, 1.25);
});

test("smoothing identity resets on session, device, or avatar contact-set changes only", () => {
  const connection = { session_generation: 2, server_epoch: "epoch-a", transport_generation: 4 };
  const snapshot = {
    active_device: 0,
    devices: [{ mac: "device-a" }],
    avatar_zones: [{ zone_type: "front", id: "contact-a", level: 0.4 }],
    status: { channel_a: { kinematics: { depth: 0.3 } } },
  };
  const identity = telemetryResetIdentity(connection, snapshot);
  const liveSample = structuredClone(snapshot);
  liveSample.avatar_zones[0].level = 0.8;
  liveSample.status.channel_a.kinematics.depth = 0.7;
  assert.equal(telemetryResetIdentity(connection, liveSample), identity);

  assert.notEqual(telemetryResetIdentity({ ...connection, transport_generation: 5 }, snapshot), identity);
  assert.notEqual(telemetryResetIdentity(connection, { ...snapshot, avatar_zones: [] }), identity);
  assert.notEqual(telemetryResetIdentity(connection, { ...snapshot, active_device: 1 }), identity);
});

import test from "node:test";
import assert from "node:assert/strict";
import { resetCalibrationOperations } from "../src/domain/zone-calibration.mjs";

test("reset calibration returns the Host GUI defaults through existing Remote API operations", () => {
  assert.deepEqual(resetCalibrationOperations("B", 2), [
    { op: "zones.set_scale", args: { channel: "B", index: 2, value: 100 } },
    { op: "zones.set_threshold_min", args: { channel: "B", index: 2, value: 1 } },
    { op: "zones.set_threshold_max", args: { channel: "B", index: 2, value: 100 } },
  ]);
});

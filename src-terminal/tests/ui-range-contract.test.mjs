import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import * as zoneCalibration from "../src/domain/zone-calibration.mjs";

const terminalTuning = await readFile(new URL("../src/components/TuningEditor.vue", import.meta.url), "utf8");
const hostTuning = await readFile(new URL("../../src-gui/src/model.rs", import.meta.url), "utf8");
const ukfCore = await readFile(new URL("../../src-core/src/dsp/ukf.rs", import.meta.url), "utf8");

test("Q range includes the Host default while R retains its own range", () => {
  const q = terminalTuning.match(/q:\s*\[([^\]]+)\]/)?.[1].split(",").map(Number);
  const r = terminalTuning.match(/r:\s*\[([^\]]+)\]/)?.[1].split(",").map(Number);
  assert.deepEqual(q, [0.0001, 50, 0.001, 4]);
  assert.deepEqual(r, [0.0001, 0.5, 0.001, 4]);
  assert.match(hostTuning, /UkfField::Q\s*=>\s*\(0\.0001,\s*50\.0\)/);
  assert.match(hostTuning, /UkfField::R\s*=>\s*\(0\.0001,\s*0\.5\)/);
  assert.match(ukfCore, /q:\s*50\.0/);
});

test("zone threshold editor conversion preserves the Remote API integer units", () => {
  assert.equal(typeof zoneCalibration.zoneThresholdWireToUi, "function");
  assert.equal(typeof zoneCalibration.zoneThresholdUiToWire, "function");
  assert.equal(zoneCalibration.zoneThresholdWireToUi(1), 0.01);
  assert.equal(zoneCalibration.zoneThresholdWireToUi(100), 1);
  assert.equal(zoneCalibration.zoneThresholdUiToWire(0.43), 43);
});

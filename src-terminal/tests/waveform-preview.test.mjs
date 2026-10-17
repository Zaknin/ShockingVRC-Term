import test from "node:test";
import assert from "node:assert/strict";
import { buildWaveformSegments } from "../src/domain/waveform-preview.mjs";

test("waveform projection shows all four Host frequencies and exact intensity readouts", () => {
  const segments = buildWaveformSegments([10, 255, 132, 10], [0, 25.5, 75, 100]);
  assert.equal(segments.length, 4);
  assert.deepEqual(segments.map((item) => item.hz), ["100", "1", "51", "100"]);
  assert.deepEqual(segments.map((item) => item.intensityLabel), ["0%", "25.5%", "75%", "100%"]);
  assert.ok(Math.abs(segments.reduce((sum, item) => sum + Number.parseFloat(item.basis), 0) - 100) < 1e-8);
});

test("waveform projection stays bounded for out-of-range preview intensities", () => {
  const segments = buildWaveformSegments([10, 10, 10, 10], [-5, 20, 120, Number.NaN]);
  assert.deepEqual(segments.map((item) => item.strength), [0, 20, 100, 0]);
});

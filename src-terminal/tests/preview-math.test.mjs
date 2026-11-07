import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { composeDepth, defaultModulationConfig, evaluateFunction, formatNumber, hzToRaw, modulationChartDomain, modulationChartLabelPrecision, rawToHz, sampleModulation } from "../src/domain/preview-math.mjs";

const modulation = (func) => ({
  source: "Depth", function: func, base_speed: 1, sensitivity: 0, max_deviation: 10,
  phase: 0, frequency_multiplier: 1, offset: 0, power: 1, clamp_min: 0, clamp_max: 100,
});

const parityFixture = JSON.parse(readFileSync(new URL("./fixtures/modulation-parity-v1.json", import.meta.url), "utf8"));

test("preview samples match the shared Host numeric reference fixture", () => {
  for (const row of parityFixture.cases) {
    const points = sampleModulation(row.config, row.baseValue, parityFixture.sourceValue, parityFixture.sampleCount, parityFixture.stepSeconds);
    const selected = parityFixture.indices.map((index) => points[index]);
    selected.forEach((value, index) => {
      assert.ok(Math.abs(value - row.hostValues[index]) <= 0.01, `${row.name} sample ${parityFixture.indices[index]}: ${value} vs Host ${row.hostValues[index]}`);
    });
    if (row.kind === "freq") {
      const display = selected.map((value) => rawToHz(Math.trunc(Math.max(10, Math.min(255, value)))));
      display.forEach((value, index) => {
        assert.ok(Math.abs(value - row.displayHz[index]) <= 0.0001, `${row.name} Hz sample ${parityFixture.indices[index]}: ${value} vs Host ${row.displayHz[index]}`);
      });
    }
  }
});

test("channel raw frequency follows the Host pulse-period raw-to-Hz mapping", () => {
  assert.equal(rawToHz(10), 100);
  assert.equal(rawToHz(100), 10);
  assert.equal(rawToHz(150), 1000 / 350);
  assert.equal(rawToHz(240), 1);
  assert.equal(rawToHz(255), 1);
  assert.equal(rawToHz(132.5), 1000 / 262.5);
});

test("user-facing Hz values round-trip to every supported Host pulse frequency", () => {
  assert.equal(hzToRaw(100), 10);
  assert.equal(hzToRaw(1), 240);
  assert.equal(hzToRaw(1000), 10);
  assert.equal(hzToRaw(0), 240);
  for (let raw = 10; raw <= 240; raw += 1) {
    assert.equal(hzToRaw(rawToHz(raw)), raw, `raw frequency ${raw} should survive an Hz round-trip`);
  }
});

test("modulation chart domain includes the base reference and matches Host padding", () => {
  const domain = modulationChartDomain([49.9, 50, 50.1], 50);
  assert.ok(domain.min < 49.9);
  assert.ok(domain.max > 50.1);
  assert.ok(Math.abs(domain.min - 49.87) < 1e-9);
  assert.ok(Math.abs(domain.max - 50.13) < 1e-9);
});

test("constant and low-amplitude modulation previews use Host's minimum padded domain", () => {
  const constant = modulationChartDomain([50, 50, 50]);
  const small = modulationChartDomain([0.01, 0.02, 0.01]);
  assert.ok(Math.abs(constant.min - 49.99985) < 1e-8);
  assert.ok(Math.abs(constant.max - 50.00015) < 1e-8);
  assert.ok(small.min < 0.01);
  assert.ok(small.max > 0.02);
  assert.ok(small.max - small.min < 0.02);
});

test("modulation chart labels retain enough precision to distinguish small ranges", () => {
  const small = modulationChartDomain([0.0011, 0.0014, 0.0012]);
  const precision = modulationChartLabelPrecision(small);
  assert.ok(precision >= 4);
  assert.notEqual(formatNumber(small.min, precision), formatNumber(small.max, precision));
  assert.equal(modulationChartLabelPrecision({ min: 48, max: 52 }), 2);
});

test("numeric readouts avoid binary floating-point tails", () => {
  assert.equal(formatNumber(0.30000000000000004), "0.3");
  assert.equal(formatNumber(50), "50");
});

test("new modulation slots use the Host defaults and kind-specific output clamps", () => {
  assert.deepEqual(defaultModulationConfig("freq"), {
    source: "Depth", function: { kind: "sin" }, base_speed: 1, sensitivity: 1, max_deviation: 20,
    phase: 0, frequency_multiplier: 1, offset: 0, power: 1, clamp_min: 10, clamp_max: 255,
  });
  assert.deepEqual(defaultModulationConfig("intensity").clamp_min, 0);
  assert.deepEqual(defaultModulationConfig("intensity").clamp_max, 100);
});

test("preview advances the Host accumulator before each sample at the configured interval", () => {
  const points = sampleModulation(modulation({ kind: "sin" }), 50, 0, 3, 0.25);
  assert.equal(points.length, 3);
  assert.ok(Math.abs(points[0] - 60) < 1e-5);
  assert.ok(Math.abs(points[1] - 50) < 1e-5);
  assert.ok(Math.abs(points[2] - 40) < 1e-5);
});

test("function evaluator supports nested Compose up to eight levels", () => {
  let nested = { kind: "sin" };
  for (let index = 0; index < 8; index += 1) nested = { kind: "compose", outer: { kind: "sin" }, inner: nested };
  assert.equal(composeDepth(nested), 8);
  let expected = 0.7;
  for (let index = 0; index < 9; index += 1) expected = Math.sin(expected);
  assert.ok(Math.abs(evaluateFunction(nested, 0.7) - expected) < 1e-6);
  assert.equal(sampleModulation(modulation(nested), 50, 0, 5, 1).length, 5);
  const tooDeep = { kind: "compose", outer: { kind: "sin" }, inner: nested };
  assert.throws(() => sampleModulation(modulation(tooDeep), 50, 0, 5, 1), /exceeds 8/);
});

test("preview implements every Remote API modulation function variant", () => {
  const kinds = [
    "none", "sin", "cos", "tan", "asin", "acos", "atan", "sin_cos", "sin2", "cos2", "sin_plus_cos",
    "sinh", "cosh", "tanh", "square", "cube", "pow4", "sqrt", "cbrt", "abs", "sign", "exp", "exp_neg",
    "pow2x", "pow10x", "ln", "log2", "log10", "triangle", "saw", "reverse_saw", "square_wave", "pulse",
    "bounce", "sigmoid", "smooth_step", "smoother_step", "logistic", "soft_sign", "perlin", "simplex",
    "fractal", "value_noise", "sin_plus_noise", "sin_times_noise", "triangle_plus_sin", "square_times_sigmoid",
  ];
  for (const kind of kinds) assert.ok(Number.isFinite(evaluateFunction({ kind }, 0.71)), `${kind} preview is finite`);
  assert.ok(Number.isFinite(evaluateFunction({ kind: "sin_pow", exponent: 2.5 }, 0.71)));
  assert.ok(Number.isFinite(evaluateFunction({ kind: "cos_pow", exponent: 2.5 }, 0.71)));
});

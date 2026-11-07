import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { alarmDisplayState } from "../src/domain/alarm-display.mjs";
import {
  HOST_FUNCTIONS,
  displayFunction,
  functionGroup,
  functionListFor,
  functionSparkline,
  groupFunctions,
  modulationParameterLabel,
  formatModulationParameter,
} from "../src/domain/modulation-picker.mjs";

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const sparklineFixture = JSON.parse(read("../src/domain/modulation-picker-sparklines.json"));

test("picker inventory and group labels match the Host standard list", () => {
  assert.deepEqual(HOST_FUNCTIONS.map(displayFunction), [
    "none", "sin", "cos", "tan", "sincos", "sin2", "cos2", "sin+cos",
    "sinh", "cosh", "tanh", "x2", "x3", "x4", "sqrt", "cbrt", "abs", "sign",
    "exp", "exp-", "2^x", "10^x", "ln", "log2", "log10", "triangle", "saw",
    "rsaw", "square", "pulse", "bounce", "sigmoid", "smoothstep", "smootherstep",
    "logistic", "softsign", "perlin", "simplex", "fractal", "valuenoise", "sin+noise",
    "sin*noise", "triangle+sin", "square*sigmoid",
  ]);
  assert.equal(functionGroup({ kind: "compose", outer: { kind: "sin" }, inner: { kind: "cos" } }), "Other");
  assert.equal(functionGroup({ kind: "asin" }), "Trigonometric");
  assert.deepEqual(groupFunctions(HOST_FUNCTIONS).map(({ label }) => label), [
    "Off", "Trigonometric", "Hyperbolic", "Powers & roots", "Exponential", "Logarithmic",
    "Waveshapes", "Curves & steps", "Noise", "Blends",
  ]);
});

test("current nonstandard and composed functions are retained at the start of the picker", () => {
  const custom = { kind: "compose", outer: { kind: "sin_pow", exponent: 2 }, inner: { kind: "asin" } };
  const entries = functionListFor(custom);
  assert.equal(entries.length, 45);
  assert.deepEqual(entries[1], custom);
  assert.equal(displayFunction(custom), "sin^2(asin)");
  assert.equal(functionGroup(custom), "Other");
  assert.equal(functionListFor({ kind: "sin" }).length, 44);
});

test("picker search is case-insensitive and filters within Host groups", () => {
  const entries = groupFunctions(HOST_FUNCTIONS, "SmOoTh");
  assert.deepEqual(entries.map(({ label, functions }) => [label, functions.map(displayFunction)]), [
    ["Curves & steps", ["smoothstep", "smootherstep"]],
  ]);
});

test("sparklines contain 64 evaluator samples, are amplitude-normalized, and are cached", () => {
  for (const func of HOST_FUNCTIONS) {
    const points = functionSparkline(func);
    assert.equal(points.length, 64, `${displayFunction(func)} sample count`);
    assert.ok(points.every((value) => Number.isFinite(value) && value >= 0 && value <= 1), `${displayFunction(func)} normalized values`);
    assert.strictEqual(functionSparkline(func), points, `${displayFunction(func)} cached array`);
  }
  assert.notDeepEqual(functionSparkline({ kind: "sin" }), functionSparkline({ kind: "square" }));
});

test("picker sparklines match the 64-sample Rust evaluator reference for every standard function", () => {
  assert.equal(sparklineFixture.sampleCount, 64);
  assert.equal(sparklineFixture.functions.length, HOST_FUNCTIONS.length);
  for (const row of sparklineFixture.functions) {
    const actual = functionSparkline(HOST_FUNCTIONS.find((func) => displayFunction(func) === row.name));
    actual.forEach((value, index) => {
      assert.ok(Math.abs(value - row.normalized[index]) <= 0.00002, `${row.name} sample ${index}: ${value} vs Rust ${row.normalized[index]}`);
    });
  }
});

test("modulation labels and value formatting match Host ModParam", () => {
  const cases = [
    ["base_speed", "Speed", 1.25, "1.25 Hz"],
    ["sensitivity", "Sensitivity", 0.125, "0.125"],
    ["max_deviation", "Deviation", 2.5, "±2.5"],
    ["phase", "Phase", 0.5, "0.50 cyc"],
    ["frequency_multiplier", "Freq mul", 1.25, "1.250"],
    ["offset", "Offset", -2, "-2.0"],
    ["power", "Power", 1.25, "1.250"],
    ["clamp_min", "Clamp min", 10, "10.0"],
    ["clamp_max", "Clamp max", 255, "255.0"],
  ];
  for (const [key, label, value, expected] of cases) {
    assert.equal(modulationParameterLabel(key), label);
    assert.equal(formatModulationParameter(key, value), expected);
  }
});

test("picker is compact when closed and exposes accessible modal search and close paths", () => {
  const workspace = read("../src/components/ModulationWorkspace.vue");
  const picker = read("../src/components/FunctionPicker.vue");
  assert.match(workspace, /<FunctionPicker/);
  assert.match(workspace, /Advanced Function Tree\/JSON/);
  assert.match(workspace, /@update:model-value="updateFunction"/);
  assert.match(workspace, /formatModulationParameter\(field\.key, value\)/);
  assert.match(picker, /role="dialog"/);
  assert.match(picker, /aria-modal="true"/);
  assert.match(picker, /@keydown\.capture="handleDialogKeydown"/);
  assert.match(picker, /event\.key === "Escape"/);
  assert.match(picker, /event\.key !== "Tab"/);
  assert.match(picker, /aria-label="Search functions"/);
  assert.match(picker, /@click\.self="close"/);
  assert.match(picker, /functionSparkline/);
  assert.match(workspace, /function updateFunction\(value: ModulationFunction\) \{ setValue\("function", value\)/);
  assert.match(workspace, /function apply\(\)[\s\S]*?emit\("send", \{ op: "modulation\.apply"/);
});

test("Alarm navigation stays visible at shortcut 7 and Host preference is labeled Host-only", () => {
  const app = read("../src/App.vue");
  const sectionView = read("../src/components/SectionView.vue");
  const uiPreferences = read("../src/domain/ui-preferences.mjs");
  assert.match(app, /const visibleSections = sections;/);
  assert.doesNotMatch(app, /alarm_tab_visible/);
  assert.match(uiPreferences, /name: "Alarm", code: "7"/);
  assert.match(sectionView, /Show Alarm tab in Host UI/);
  assert.match(sectionView, /class="alarm-state-label">\{\{ alarmDisplayState \}\}/);
  assert.match(sectionView, /alarmDisplayState/);
  assert.equal(alarmDisplayState({ enabled: false }, { phase: "Idle" }), "OFF / DISARMED");
  assert.equal(alarmDisplayState({ enabled: true }, { phase: "Idle" }), "ARMED");
  assert.equal(alarmDisplayState({ enabled: false }, { phase: "Ringing" }), "RINGING");
  assert.equal(alarmDisplayState({ enabled: true }, { phase: "Snoozed" }), "SNOOZED");
  assert.equal(alarmDisplayState({}, null), "NOT REPORTED");
});

test("shared panel title and numeric inputs use semantic palette tokens", () => {
  const styles = read("../src/style.css");
  assert.match(styles, /\.terminal-panel__title h2\s*\{[^}]*background:\s*var\(--accent\)[^}]*color:\s*var\(--button-text\)/s);
  assert.match(styles, /\.terminal-panel__heading\s*\{[^}]*repeating-linear-gradient\(135deg, var\(--surface-deep\) 0 4px, var\(--bg\) 4px 8px\)/s);
  assert.match(styles, /\.term-number-field input\s*\{[^}]*background:\s*var\(--surface-deep\)/s);
  assert.match(styles, /\.term-number-field input\s*\{[^}]*color:\s*var\(--text\)/s);
  const palette = read("../src/domain/theme-palette.mjs");
  assert.deepEqual([...palette.matchAll(/\{ id: "([^"]+)"/g)].map((match) => match[1]), [
    "terminal", "ember", "flare", "cobalt", "orchid", "wire", "void", "paper", "blueprint",
  ]);
});

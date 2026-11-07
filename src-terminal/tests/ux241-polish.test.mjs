import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { HOST_THEMES, hostPalette } from "../src/domain/theme-palette.mjs";
import { formatModulationParameter } from "../src/domain/modulation-picker.mjs";

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const picker = read("../src/components/FunctionPicker.vue");
const modulation = read("../src/components/ModulationWorkspace.vue");
const slider = read("../src/components/TerminalSlider.vue");
const numberInput = read("../src/components/TerminalNumberInput.vue");
const styles = read("../src/style.css");

test("Function Picker title and close button share an explicit centered header row", () => {
  assert.match(picker, /<h2 id="function-picker-title">FUNCTION<\/h2>[\s\S]*?aria-label="Close function picker"/);
  assert.match(styles, /\.function-picker-heading\s*\{[^}]*display:\s*grid;[^}]*grid-template-columns:\s*minmax\(0,1fr\)\s+auto;[^}]*align-items:\s*center;/);
  assert.match(styles, /\.function-picker-heading\s*>\s*\.term-button\s*\{[^}]*align-self:\s*center;/);
});

test("Modulation parameters use bounded four-column rows with a shared five-character value field", () => {
  const fieldCount = (modulation.match(/\{ key: "(?:base_speed|sensitivity|max_deviation|phase|frequency_multiplier|offset|power|clamp_min|clamp_max)"/g) ?? []).length;
  assert.equal(fieldCount, 9, "all nine Host parameters remain represented");
  assert.match(modulation, /layout="parameter"/);
  assert.match(modulation, /:units="field\.unit/);
  assert.match(slider, /"parameter"/);
  assert.ok(slider.includes(":aria-label=\"`${label}${prefix ? ' plus-minus' : ''}${units || suffix ? ` ${units || suffix}` : ''}`\""), "the separate unit column should remain part of the numeric field's accessible name");
  assert.match(styles, /\.term-slider--parameter\s*\{[^}]*grid-template-columns:\s*minmax\(0,var\(--term-slider-label-width,96px\)\)\s+minmax\(24px,1fr\)\s+max-content\s+42px;/);
  assert.match(styles, /\.modulation-editor-grid\s*\{[^}]*max-width:\s*720px/);
  assert.match(styles, /\.term-slider--parameter \.term-number-field input\s*\{[^}]*width:\s*calc\(5ch\s*\+\s*14px\)[^}]*text-align:\s*right/);
  assert.match(styles, /\.term-slider--parameter \.term-slider__range\s*\{[^}]*grid-column:\s*2/);
  assert.match(styles, /\.term-slider--parameter \.term-slider__units\s*\{[^}]*grid-column:\s*4/);
  assert.match(numberInput, /long-edit-value/);
  assert.match(styles, /long-edit-value\s*\{[^}]*width:\s*calc\(6ch\s*\+\s*14px\)/);
  assert.match(styles, /\.term-slider__range\s*\{[^}]*height:\s*16px/);
  assert.match(styles, /\.term-slider__track-visual\s*\{[^}]*height:\s*10px/);
  assert.match(styles, /\.term-slider__thumb-visual\s*\{[^}]*width:\s*11px;\s*height:\s*16px/);
  assert.equal(formatModulationParameter("frequency_multiplier", 1), "1.000");
  assert.equal(formatModulationParameter("clamp_max", 255), "255.0");
  assert.equal(formatModulationParameter("max_deviation", 20), "±20.0");
  assert.equal(formatModulationParameter("phase", 0), "0.00 cyc");
  assert.equal(formatModulationParameter("offset", -255), "-255.0");
});

test("shared text fields use Host semantic palette aliases in every existing theme", () => {
  assert.match(styles, /--well:\s*var\(--surface-deep\)/);
  assert.match(styles, /--sheet:\s*var\(--surface\)/);
  assert.match(styles, /--dim:\s*var\(--muted\)/);
  assert.match(styles, /--hair:\s*var\(--line\)/);
  assert.match(styles, /--contrast:\s*var\(--text\)/);
  assert.match(styles, /(?:input:not\(\[type="range"\]\)[\s\S]*?textarea[\s\S]*?select|textarea[\s\S]*?select)/);
  assert.match(styles, /::placeholder\s*\{[^}]*color:\s*var\(--dim\)/);
  assert.match(styles, /:focus(?:-visible)?\s*\{[^}]*border-color:\s*var\(--accent\)/);
  assert.match(styles, /:disabled\s*\{[^}]*color:\s*var\(--dim\)/);
  assert.match(styles, /\[readonly\][^{]*\{[^}]*color:\s*var\(--contrast\)/);
  assert.equal(HOST_THEMES.length, 9);
  for (const { id } of HOST_THEMES) {
    const { tokens } = hostPalette(id);
    for (const key of ["--surface-deep", "--surface", "--contrast", "--muted", "--line", "--accent"]) {
      assert.match(tokens[key], /^#[0-9a-f]{6}$/i, `${id} keeps a resolved ${key} palette token`);
    }
  }
});

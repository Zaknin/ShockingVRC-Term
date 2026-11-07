import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const styles = await readFile(new URL("../src/style.css", import.meta.url), "utf8");

test("normal navigation, buttons, and slider controls use Host body or small text roles", () => {
  assert.match(styles, /\.nav-item\s*\{[^}]*font:\s*var\(--type-body\)\/var\(--line-body\) var\(--mono\)/, "navigation labels should remain readable at 100 percent");
  assert.match(styles, /\.term-button\s*\{[^}]*font:\s*var\(--type-body\)\/var\(--line-body\) var\(--mono\)/, "buttons should match Host body text");
  assert.match(styles, /\.term-slider__head\s*>\s*label\s*\{[^}]*font-size:\s*var\(--type-body\)[^}]*line-height:\s*var\(--line-body\)/, "slider labels should use Host body text");
  assert.match(styles, /\.term-number-field input\s*\{[^}]*font:\s*var\(--type-body\)\/var\(--line-body\) var\(--mono\)/, "slider numeric fields should use Host body text");
  assert.match(styles, /\.term-slider__units\s*\{[^}]*font-size:\s*var\(--type-body\)[^}]*line-height:\s*var\(--line-body\)/, "slider units should use Host body text");
  assert.match(styles, /\.terminal-panel__title h2\s*\{[^}]*font:\s*500 var\(--type-heading\)\/var\(--line-heading\)/, "Terminal panel titles should use Host heading text");
  assert.match(styles, /\.term-segmented button\s*\{[^}]*font:\s*var\(--type-secondary\)\/var\(--line-secondary\) var\(--mono\)/, "segmented controls should use Host small text");
});

test("preset waveform labels fit within narrow catalog details", () => {
  assert.match(styles, /\.catalog-browser__detail \.term-waveform__segment span\s*\{[^}]*max-width:\s*100%[^}]*white-space:\s*normal[^}]*overflow-wrap:\s*anywhere/, "waveform labels should wrap within their segment instead of overlapping adjacent values");
});

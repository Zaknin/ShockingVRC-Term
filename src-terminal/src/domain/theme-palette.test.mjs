import test from "node:test";
import assert from "node:assert/strict";
import { HOST_THEMES, hostPalette, mixOklch, oklchToRgb, readSavedTheme, saveTheme } from "./theme-palette.mjs";

test("all nine Terminal palettes copy the Host OKLCH source values", () => {
  assert.deepEqual(HOST_THEMES.map(({ id }) => id), ["terminal", "ember", "flare", "cobalt", "orchid", "wire", "void", "paper", "blueprint"]);
  assert.deepEqual(HOST_THEMES[0], { id: "terminal", name: "Terminal", base: [0.17, 0.012, 265], contrast: [0.86, 0.015, 265], accent: [0.8, 0.16, 158] });
  assert.deepEqual(HOST_THEMES[7], { id: "paper", name: "Paper", base: [0.97, 0.006, 95], contrast: [0.26, 0.012, 95], accent: [0.52, 0.14, 155] });
  assert.deepEqual(HOST_THEMES[8], { id: "blueprint", name: "Blueprint", base: [0.95, 0.012, 240], contrast: [0.28, 0.03, 250], accent: [0.5, 0.16, 245] });
});

test("OKLCH conversion and derived Host semantic palette are stable", () => {
  assert.deepEqual(oklchToRgb([0, 0, 0]), [0, 0, 0]);
  const midpoint = mixOklch([0.2, 0.1, 350], [0.8, 0.2, 10], 0.5);
  assert.equal(midpoint[0], 0.5);
  assert.equal(midpoint[1], Math.fround(0.15));
  assert.equal(midpoint[2], 360);
  const terminal = hostPalette("terminal");
  assert.equal(terminal.tokens["--bg"], terminal.base);
  assert.equal(terminal.tokens["--text"], terminal.text);
  assert.equal(terminal.tokens["--accent"], terminal.accent);
  assert.equal(terminal.tokens["--surface"], "#14171d");
  assert.equal(terminal.tokens["--track"], "#1f2228");
  assert.equal(terminal.tokens["--line"], "#545860");
  assert.equal(terminal.tokens["--muted"], "#737780");
  assert.equal(terminal.tokens["--marker"], "#1a3a4d");
  assert.equal(terminal.tokens["--success"], "#5fd37f");
  assert.equal(terminal.tokens["--warning"], "#e9ab2b");
  assert.equal(terminal.tokens["--error"], "#ff7e77");
});

test("Paper and Blueprint derive light palettes with readable text contrast", () => {
  for (const id of ["paper", "blueprint"]) {
    const palette = hostPalette(id);
    assert.equal(palette.light, true);
    assert.ok(palette.tokens["--text"] !== palette.tokens["--bg"]);
    assert.equal(palette.tokens["--button-text"], palette.base);
  }
});

test("theme persistence accepts only known palette identifiers", () => {
  const values = new Map();
  const storage = { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
  assert.equal(readSavedTheme(storage), "terminal");
  assert.equal(saveTheme("paper", storage), "paper");
  assert.equal(readSavedTheme(storage), "paper");
  assert.equal(saveTheme("missing", storage), "terminal");
});

import test from "node:test";
import assert from "node:assert/strict";
import { changeScale, normalizeScale, readSavedScale, saveScale, shortcutAction } from "./ui-preferences.mjs";

test("scale supports the requested 100, 125, and 150 percent values and a 100 percent reset", () => {
  assert.equal(normalizeScale("bad"), 100);
  assert.equal(normalizeScale(20), 75);
  assert.equal(normalizeScale(190), 175);
  assert.equal(changeScale(100, -1), 90);
  assert.equal(changeScale(100, 1), 110);
  assert.equal(changeScale(120, 1), 125);
  assert.equal(normalizeScale(125), 125);
  assert.equal(changeScale(125, 1), 130);
  assert.equal(changeScale(80, -1), 75);
  assert.equal(changeScale(170, 1), 175);
});

test("preferred scale persists locally and invalid values recover to default", () => {
  const values = new Map();
  const storage = { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
  assert.equal(readSavedScale(storage), 100);
  assert.equal(saveScale(150, storage), 150);
  assert.equal(readSavedScale(storage), 150);
  values.set("terminal.scale", "not-a-number");
  assert.equal(readSavedScale(storage), 100);
});

test("Ctrl zoom shortcuts work globally, including text editing targets and NumPad", () => {
  const targets = [
    { tagName: "DIV" },
    { tagName: "INPUT" },
    { tagName: "TEXTAREA" },
    { tagName: "SELECT" },
    { tagName: "DIV", isContentEditable: true },
  ];
  for (const target of targets) {
    assert.equal(shortcutAction({ key: "+", ctrlKey: true, target }), "zoom-in");
    assert.equal(shortcutAction({ key: "=", shiftKey: true, ctrlKey: true, target }), "zoom-in");
    assert.equal(shortcutAction({ key: "Add", code: "NumpadAdd", ctrlKey: true, target }), "zoom-in");
    assert.equal(shortcutAction({ key: "-", ctrlKey: true, target }), "zoom-out");
    assert.equal(shortcutAction({ key: "Subtract", code: "NumpadSubtract", ctrlKey: true, target }), "zoom-out");
    assert.equal(shortcutAction({ key: "0", ctrlKey: true, target }), "zoom-reset");
    assert.equal(shortcutAction({ key: "Insert", code: "Numpad0", ctrlKey: true, target }), "zoom-reset");
  }
});

test("unmodified plus, minus, and zero remain ordinary text in editing targets", () => {
  const page = { tagName: "DIV" };
  for (const key of "0123456789") {
    assert.equal(shortcutAction({ key, target: page }), `section-${key}`);
  }
  assert.equal(shortcutAction({ key: "t", target: { tagName: "DIV" } }), "theme");
  for (const tagName of ["INPUT", "TEXTAREA", "SELECT"]) {
    for (const key of [..."0123456789", "t", "+", "=", "-"]) {
      assert.equal(shortcutAction({ key, target: { tagName } }), null);
    }
  }
  assert.equal(shortcutAction({ key: "T", target: { isContentEditable: true } }), null);
});

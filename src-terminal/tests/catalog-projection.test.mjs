import test from "node:test";
import assert from "node:assert/strict";
import {
  entryIndex,
  presetAuthorLabel,
  presetEntryName,
  presetModulationCount,
  presetNumberArray,
} from "../src/domain/catalog-projection.mjs";

test("catalog projection reads names, authors, arrays, modulation slots and Host indices", () => {
  const entry = {
    index: 7,
    preset: {
      name: "Pulse",
      author: "Host author",
      frequency: [10, 40, 80, 255],
      intensity: [0, 20, 50, 100],
      freq_modulation: [null, { source: "Depth" }, null, { source: "Speed" }],
      intensity_modulation: [null, null, null, { source: "Acc" }],
    },
  };

  assert.equal(presetEntryName(entry), "Pulse");
  assert.equal(presetAuthorLabel(entry), "Host author");
  assert.equal(entryIndex(entry, 0), 7);
  assert.deepEqual(presetNumberArray(entry, "frequency"), [10, 40, 80, 255]);
  assert.equal(presetModulationCount(entry), 3);
});

test("catalog projection rejects malformed four-segment arrays and preserves fallback values", () => {
  const entry = { preset: { frequency: [10, 20, 30], intensity: [0, 20, "bad", 100] } };

  assert.equal(presetEntryName(entry), "Host entry");
  assert.equal(presetAuthorLabel(entry), "Host catalog");
  assert.equal(entryIndex({}, 4), 4);
  assert.equal(presetNumberArray(entry, "frequency"), null);
  assert.equal(presetNumberArray(entry, "intensity"), null);
  assert.equal(presetModulationCount(entry), 0);
});

import test from "node:test";
import assert from "node:assert/strict";
import {
  entryIndex,
  filterCatalogEntries,
  presetAuthorLabel,
  presetEntryName,
  presetModulationCount,
  presetNumberArray,
  projectCatalogEntries,
  reconcileCatalogSelection,
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

test("catalog filtering preserves original Host operation indices", () => {
  const projected = projectCatalogEntries([
    { name: "Built in", user: false },
    { index: 12, name: "My pulse", author: "Me", user: true },
    { name: "My wave", author: "Me", user: true },
  ]);

  assert.deepEqual(projected.map(({ hostIndex }) => hostIndex), [0, 12, 2]);
  assert.deepEqual(filterCatalogEntries(projected, "my", "MINE").map(({ hostIndex }) => hostIndex), [12, 2]);
  assert.deepEqual(filterCatalogEntries(projected, "built", "MINE"), []);
});

test("catalog selection survives reorder and content refresh by stable identity", () => {
  const original = projectCatalogEntries([
    { id: "a", name: "First" },
    { id: "b", name: "Second" },
  ]);
  const selected = original[1].identity;
  const refreshed = projectCatalogEntries([
    { id: "b", name: "Second updated" },
    { id: "a", name: "First" },
  ]);

  assert.equal(reconcileCatalogSelection(refreshed, selected), selected);
  assert.equal(refreshed.find(({ identity }) => identity === selected)?.hostIndex, 0);
  assert.equal(reconcileCatalogSelection(refreshed, "missing"), null);
  assert.equal(reconcileCatalogSelection(refreshed, null), refreshed[0].identity);
  assert.equal(reconcileCatalogSelection([], selected), null);
});

test("catalog identities remain stable without explicit ids and keys are unique for duplicate rows", () => {
  const first = projectCatalogEntries([
    { name: "Repeated", author: "Host" },
    { name: "Repeated", author: "Host" },
  ]);
  const refreshed = projectCatalogEntries([{ name: "Repeated", author: "Host" }]);

  assert.notEqual(first[0].identity, first[1].identity);
  assert.equal(reconcileCatalogSelection(refreshed, first[0].identity), first[0].identity);
});

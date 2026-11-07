import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [sectionView, styles] = await Promise.all([
  readFile(new URL("../src/components/SectionView.vue", import.meta.url), "utf8"),
  readFile(new URL("../src/style.css", import.meta.url), "utf8"),
]);

test("preset catalog uses a compact selectable list and one details pane", () => {
  assert.match(sectionView, /class="catalog-browser\s/, "catalog should use the two-column browser");
  assert.match(sectionView, /class="catalog-browser__list"[\s\S]*?v-for="item in filteredCatalogEntries"/, "visible entries should remain a compact list");
  assert.match(sectionView, /class="catalog-browser__detail"[\s\S]*?selectedCatalogEntry/, "one selected entry should drive the details pane");
  assert.match(styles, /\.catalog-browser\s*\{[^}]*grid-template-columns:\s*minmax\(0,1\.15fr\) minmax\(0,\.85fr\)/, "wide catalog layout should give the list slightly more width");
  assert.match(styles, /\.catalog-browser__list\s*\{[^}]*max-height:[^;]+;\s*overflow:\s*auto/, "long catalogs should scroll inside the list");
});

test("catalog commands use source Host indices and user preset deletion is confirmed", () => {
  assert.match(sectionView, /projectCatalogEntries\(catalog\.value\?\.entries\s*\?\?\s*\[\]\)/, "Host indices should be projected before filtering");
  assert.match(sectionView, /filterCatalogEntries\(projectedCatalogEntries\.value, presetSearch\.value, catalogScope\.value\)/, "search and ownership filters should operate on projected rows");
  assert.match(sectionView, /op:\s*"presets\.apply",\s*args:\s*\{\s*channel,\s*index:\s*selected\.hostIndex\s*\}/, "apply should use the selected source Host index");
  assert.match(sectionView, /selected\.entry\.user\s*!==\s*true/, "delete should require a Host-owned user preset");
  assert.match(sectionView, /window\.confirm\(/, "delete should ask for explicit confirmation");
  assert.match(sectionView, /supports\('presets\.delete_user'\)/, "delete should remain capability-gated");
  const list = sectionView.match(/class="catalog-browser__list"[\s\S]*?class="catalog-browser__detail"/)?.[0] ?? "";
  assert.doesNotMatch(list, /presets\.apply|presets\.delete_user|TerminalWaveform/, "waveform actions and preview belong in the detail pane");
  assert.match(sectionView, /catalogKind\.value !== "zone_presets"[\s\S]*?zone_presets\.apply/, "zone set actions stay separate from waveform preset actions");
});

test("a removed selection remains explained even when the Host catalog becomes empty", () => {
  assert.match(sectionView, /catalogSelectionMissing\.value\[kind\]\s*=\s*previous\s*!==\s*null\s*&&\s*!entries\.some\(\(\{ identity \}\)\s*=>\s*identity\s*===\s*previous\)/, "an empty refreshed catalog should still explain why the previous selection was cleared");
});

test("catalog rows and selected values use the Host typography scale and compact geometry", () => {
  assert.match(sectionView, /item\.entry\.description/, "catalog rows should include their brief description");
  assert.match(styles, /\.catalog-browser__list\s*\{[^}]*max-height:\s*min\(62vh,430px\)/, "wide catalog scrolling should follow the Host height limit");
  assert.match(styles, /\.catalog-entry\s*\{[^}]*min-height:\s*40px/, "catalog rows should stay close to the Host 40px row height");
  assert.match(styles, /\.catalog-entry strong\s*\{[^}]*font:\s*var\(--type-body\)\/var\(--line-body\)/, "catalog names should use Host body text");
  assert.match(styles, /\.catalog-detail-heading h3\s*\{[^}]*font:\s*var\(--type-numeric\)\/var\(--line-numeric\)/, "selected preset names should use the Host mono-big scale");
  assert.match(styles, /\.catalog-segment-readout\s*\{[^}]*font:\s*var\(--type-body\)\/var\(--line-body\)/, "frequency segment labels and values should match Host gauge text");
  assert.match(styles, /\.catalog-detail-group \.term-progress__value\s*\{[^}]*font:\s*var\(--type-body\)\/var\(--line-body\)/, "intensity values should remain readable at 100 percent");
});

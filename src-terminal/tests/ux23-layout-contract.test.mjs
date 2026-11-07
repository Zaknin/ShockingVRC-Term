import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = (path) => readFile(new URL(path, import.meta.url), "utf8");
const [styles, slider, sectionView, channelEditor] = await Promise.all([
  source("../src/style.css"),
  source("../src/components/TerminalSlider.vue"),
  source("../src/components/SectionView.vue"),
  source("../src/components/ChannelEditor.vue"),
]);

test("ordinary sliders share capped responsive tracks and reserve value and unit columns", () => {
  assert.match(styles, /--term-slider-max-width:\s*520px/, "the shared track limit should be defined once");
  assert.match(styles, /\.term-slider__range\s*\{[^}]*width:\s*100%[^}]*max-width:\s*var\(--term-slider-max-width\)/, "tracks should fill narrow cells and cap on wide panels");
  assert.match(styles, /\.term-slider--wide \.term-slider__range\s*\{[^}]*max-width:\s*none/, "channel limit sliders may use the full panel width");
  assert.match(styles, /\.term-slider--row \.term-slider__track\s*\{[^}]*grid-template-columns: minmax\(0,1fr\) 42px/, "row sliders should reserve a fixed units column");
  assert.match(slider, /class="term-slider__units"[^>]*aria-hidden/, "empty unit columns should remain reserved without announcing blank text");
  assert.match(channelEditor, /label="Minimum limit"[^\n]*full-width/, "minimum limit may use the full available width");
  assert.match(channelEditor, /label="Maximum limit"[^\n]*full-width/, "maximum limit may use the full available width");
  assert.match(styles, /\.zone-calibration\s*\{[^}]*grid-template-columns:\s*minmax\(180px,1\.2fr\) minmax\(0,1fr\) minmax\(0,1fr\) minmax\(0,1fr\)/, "zone slider columns should have equal usable widths");
  assert.match(styles, /@media\s*\(min-width:\s*1501px\)\s*\{[^}]*\.zone-editor-grid\s*\{[^}]*grid-template-columns:\s*minmax\(0,1fr\)/, "wide screens should give each zone panel the full content width so its calibration sliders do not collapse");
});

test("log actions share aligned spacing and remain separate from the viewer", () => {
  assert.equal((sectionView.match(/class="button-row compact-row section-gap-small logs-toolbar"/g) ?? []).length, 2, "both log action groups should share the same toolbar class");
  assert.match(styles, /\.logs-toolbar\s*\{[^}]*min-height:\s*42px[^}]*display:\s*flex[^}]*align-items:\s*center[^}]*gap:\s*8px/, "log actions should share one centered, spaced row");
  assert.match(styles, /\.logs-toolbar \.term-button\s*\{[^}]*min-height:\s*32px/, "buttons should have a uniform usable height");
  assert.match(styles, /\.term-log-lines\s*\{[^}]*overflow:\s*auto/, "the log viewer should retain horizontal and vertical scrolling");
  assert.match(styles, /\.term-log-line code\s*\{[^}]*overflow-wrap:\s*anywhere[^}]*white-space:\s*pre-wrap/, "long log lines should wrap within the viewer instead of forcing row overflow");
});

test("Host log history exposes older-page, loading, empty, and retryable error states", async () => {
  const sectionView = await source("../src/components/SectionView.vue");
  const store = await source("../src/stores/terminal.ts");
  assert.match(sectionView, /LOAD OLDER/, "retained records before the latest page must be reachable");
  assert.match(sectionView, /Loading retained Host log history/);
  assert.match(sectionView, /The Host returned no retained log records/);
  assert.match(sectionView, /Could not load Host log history/);
  assert.match(sectionView, /RETRY/);
  assert.match(store, /hostLogHasOlder/, "the older-page action must stop when retained history ends");
  assert.match(store, /previousHostLogKey !== nextHostLogKey/, "Host/profile changes invalidate the outstanding request revision");
  assert.match(store, /isCurrentHostLogRequest\(revision, hostLogRequestRevision\)/, "only the current request may update the viewer");
});

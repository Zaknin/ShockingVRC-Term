import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

const source = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const channelEditor = source("../src/components/ChannelEditor.vue");
const chart = source("../src/components/TerminalChart.vue");
const numberInput = source("../src/components/TerminalNumberInput.vue");
const slider = source("../src/components/TerminalSlider.vue");
const app = source("../src/App.vue");
const connectPanel = source("../src/components/ConnectPanel.vue");
const sectionView = source("../src/components/SectionView.vue");
const statusIndicator = source("../src/components/TerminalStatusIndicator.vue");
const modulationWorkspace = source("../src/components/ModulationWorkspace.vue");
const tauriConfig = source("../src-tauri/tauri.conf.json");
const styles = source("../src/style.css");
const expectMatch = (text, pattern, description) => assert.ok(pattern.test(text), description);
const expectNoMatch = (text, pattern, description) => assert.ok(!pattern.test(text), description);

test("channel frequency editor uses the Host Hz scale and converts commits to raw API values", () => {
  expectMatch(channelEditor, /:value="rawToHz\(frequency\)"/, "the editor should supply display Hz to the slider");
  expectMatch(channelEditor, /:min="1" :max="100"/, "the display range should match the Host's 1–100 Hz");
  expectMatch(channelEditor, /hzToRaw\(\$event\)/, "commits should convert display Hz to Remote API raw values");
  expectNoMatch(channelEditor, /raw · .* Hz/, "the readout should not mix raw and display units");
});

test("modulation chart scales and labels the sampled preview domain", () => {
  expectMatch(chart, /modulationChartDomain\(props\.points\)/, "the plot should derive its Y scale from preview points");
  expectMatch(chart, /modulationChartLabelPrecision\(domain\.value\)/, "axis precision should follow small preview ranges");
  expectMatch(chart, /formatNumber\(domain\.max, labelPrecision\)/, "the upper label should report the actual plotted bound");
  expectMatch(chart, /formatNumber\(domain\.min, labelPrecision\)/, "the lower label should report the actual plotted bound");
  expectNoMatch(chart, /props\.max - props\.min/, "the plot should not use the full output clamp as its Y scale");
});

test("configured zone names keep a useful column and the mode selector reflows below them", () => {
  expectMatch(styles, /\.zone-calibration\s*\{[^}]*grid-template-columns:\s*minmax\(180px/, "the grid should reserve a useful name and mode column");
  expectMatch(styles, /\.zone-calibration\s*>\s*\.grow\s*\{[^}]*min-width:\s*180px/, "the name and mode group should not shrink below its usable width");
  expectMatch(styles, /\.zone-calibration\s*>\s*\.grow\s*>\s*\.term-segmented\s*\{[^}]*grid-row:\s*3/, "the mode selector should sit below the contact labels");
  expectMatch(styles, /\.zone-row\s*\{[^}]*display:\s*grid/, "detected contacts should use a reflowable grid");
});

test("long zone names truncate on one line and remain available as a title", () => {
  expectMatch(styles, /\.zone-row > div:first-child strong\s*\{[^}]*overflow:\s*hidden[^}]*text-overflow:\s*ellipsis[^}]*white-space:\s*nowrap/, "long discovered contact names should not collapse into vertical text");
  expectMatch(styles, /\.zone-calibration > \.grow > strong\s*\{[^}]*text-overflow:\s*ellipsis[^}]*white-space:\s*nowrap/, "long configured mapping names should stay readable");
  expectMatch(sectionView, /<strong :title="zone\.id">\{\{ zone\.id \}\}<\/strong>/, "the full discovered contact name should remain available on hover");
  expectMatch(sectionView, /<strong :title="zone\.name">\{\{ zone\.name \}\}<\/strong>/, "the full mapping name should remain available on hover");
});

test("zone numeric fields keep a readable width and wrap below labels when needed", () => {
  expectMatch(styles, /\.zone-calibration \.term-slider__head\s*\{[^}]*flex-wrap:\s*wrap/, "zone labels and number fields should wrap instead of competing for width");
  expectMatch(styles, /\.zone-calibration \.term-number-field\s*\{[^}]*min-width:\s*78px[^}]*grid-template-columns:\s*minmax\(0,1fr\)/, "the unlabeled number input should occupy a full-width 78px field");
  expectNoMatch(styles, /\.zone-calibration \.term-number-field\s*\{[^}]*grid-template-columns:\s*minmax\(0,1fr\)\s*64px/, "the empty second grid track must not shrink the visible input");
});

test("zone threshold inputs retain their two-decimal display without changing other sliders", () => {
  expectMatch(slider, /fixedPrecision\?: boolean/, "the slider should expose an opt-in fixed decimal display");
  expectMatch(slider, /:fixed-precision="fixedPrecision"/, "the slider should pass the display option to its number input");
  expectMatch(numberInput, /fixedPrecision \? value\.toFixed\(precision\)/, "fixed decimal inputs should retain trailing zeroes");
  expectMatch(sectionView, /label="Min threshold"[^\n]*:precision="2"[^\n]*:fixed-precision="true"/, "Min threshold should display hundredths");
  expectMatch(sectionView, /label="Max threshold"[^\n]*:precision="2"[^\n]*:fixed-precision="true"/, "Max threshold should display hundredths");
});

test("Terminal branding and navigation use Host text style where Host has no icon assets", () => {
  expectNoMatch(app, /brand-mark|nav-mark|mark:\s*"/, "Terminal-only logo and navigation glyphs should be absent");
  expectMatch(app, /section\.name/, "navigation should retain the section labels");
  expectMatch(app, /section\.code/, "navigation should retain its existing numbered order markers");
  expectNoMatch(connectPanel, /profile-dot|profile-arrow|identity-mark|✓|◇|›/, "host rows and identity state should use text rather than unsupported symbols");
  expectNoMatch(sectionView, /↻|✓|◈|device-state/, "session and policy states should use text labels");
  expectNoMatch(app, /↻|class="close-error"[^>]*>×|status-dot/, "connection notices should not use unsupported action glyphs");
  expectNoMatch(statusIndicator, /term-status__dot/, "connection states should use the Host text status style");
  expectNoMatch(modulationWorkspace, /term-status__dot/, "modulation slot states should use text status labels");
  expectNoMatch(styles, /\.brand-mark|\.nav-mark|\.status-dot|\.profile-dot|\.device-state|\.identity-mark|\.empty-glyph|\.term-status__dot/, "unused Terminal-only icon styles should be removed");
  assert.deepEqual(JSON.parse(tauriConfig).bundle.icon ?? [], [], "the Windows bundle should not register an invented icon in the absence of a Host source");
  assert.equal(existsSync(new URL("../src-tauri/icons/icon.ico", import.meta.url)), false, "the unreferenced Terminal-only icon should not remain a bundle candidate");
});

test("top bar separates Host label from a bounded Host name", () => {
  expectMatch(styles, /\.header-host\s*\{[^}]*display:\s*grid/, "Host label and name should occupy separate rows");
  expectMatch(styles, /\.header-host\s+strong\s*\{[^}]*text-overflow:\s*ellipsis/, "a long Host name should not push other status items away");
});

test("Connect keeps the active Host identity marked verified while online", () => {
  expectMatch(connectPanel, /function identityIsVerified\(\)[\s\S]*?store\.connection\.phase === "online" && store\.connection\.profile_id === selectedId\.value/, "the active profile should remain verified after its transient identity object is cleared");
  expectMatch(connectPanel, /identityIsVerified\(\) \? 'VERIFIED' : 'PENDING'/, "the identity badge should reflect the current online profile");
});

test("small chart and modulation labels remain at least eight pixels", () => {
  expectMatch(styles, /\.term-chart__axis\s*\{[^}]*font:\s*8px/, "chart axis labels should be at least eight pixels");
  expectMatch(styles, /\.term-control-state\s*\{[^}]*font-size:\s*8px/, "slider state labels should be at least eight pixels");
  expectMatch(styles, /\.modulation-preview-note\s*\{[^}]*font:\s*8px/, "modulation notes should be at least eight pixels");
});

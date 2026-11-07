import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

const source = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const channelEditor = source("../src/components/ChannelEditor.vue");
const chart = source("../src/components/TerminalChart.vue");
const numberInput = source("../src/components/TerminalNumberInput.vue");
const slider = source("../src/components/TerminalSlider.vue");
const progress = source("../src/components/TerminalProgressBar.vue");
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
  expectMatch(chart, /modulationChartDomain\(props\.points, props\.baseValue\)/, "the plot should derive its padded Y scale from preview points and base");
  expectMatch(chart, /baseLabel = computed\(\(\) => `BASE \$\{formatNumber\(props\.baseValue, 0\)\} \$\{props\.unit\}`\)/, "the header should label the base using Host units");
  expectMatch(chart, /outputRange = computed\(\(\) => \(\{ min: Math\.min\(\.\.\.props\.points\), max: Math\.max\(\.\.\.props\.points\) \}\)\)/, "the range readout should report sampled output values");
  expectMatch(chart, /class="term-chart__base"/, "the chart should draw a base reference line");
  expectMatch(chart, /sampleCount \* props\.stepSeconds/, "the preview axis should use the Host sample time span");
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

test("Theme is the eleventh numbered item after Logs in the shared navigation", () => {
  expectMatch(app, /\{ name: "Logs", code: "10" \},\s*\{ name: "Theme", code: "11" \}/, "Theme should follow Logs as item 11");
  expectNoMatch(app, /class="nav-item theme-nav"/, "Theme should not remain in a separate sidebar footer slot");
});

test("global zoom shortcuts prevent browser zoom and reuse persisted Tauri scaling", () => {
  expectMatch(app, /window\.addEventListener\("keydown", handleShortcut, true\)/, "capture phase makes Ctrl shortcuts global even when focus is within a text control");
  expectMatch(app, /window\.removeEventListener\("keydown", handleShortcut, true\)/, "the capture listener should be removed symmetrically");
  expectMatch(app, /if \(action\.startsWith\("zoom-"\)\) \{\s*event\.preventDefault\(\)/, "handled zoom keys must suppress WebView default zoom");
  expectMatch(app, /function applyScale\(value: number\)[\s\S]*?saveScale\(value\)[\s\S]*?setZoom\(uiScale\.value \/ 100\)/, "keyboard and buttons should share saved scale and Tauri WebView zoom");
});

test("Connect keeps the active Host identity marked verified while online", () => {
  expectMatch(connectPanel, /function identityIsVerified\(\)[\s\S]*?store\.connection\.phase === "online" && store\.connection\.profile_id === selectedId\.value/, "the active profile should remain verified after its transient identity object is cleared");
  expectMatch(connectPanel, /identityIsVerified\(\) \? 'VERIFIED' : 'PENDING'/, "the identity badge should reflect the current online profile");
});

test("Status keeps internal connection identifiers in a collapsed details disclosure", () => {
  const detailsStart = sectionView.indexOf('<details class="status-identifiers">');
  const detailsEnd = sectionView.indexOf("</details>", detailsStart);
  assert.ok(detailsStart >= 0 && detailsEnd > detailsStart, "Status should expose a collapsed identity details disclosure");
  const details = sectionView.slice(detailsStart, detailsEnd);
  assert.match(details, /SERVER ID/, "the Host identity identifier should remain available on demand");
  assert.match(details, /SESSION PROFILE/, "the profile identifier should remain available on demand");
  assert.doesNotMatch(sectionView.slice(sectionView.indexOf('<div class="status-facts">'), detailsStart), /SERVER ID|SESSION PROFILE/, "technical identifiers should not crowd the default Status overview");
});

test("Host-styled sliders keep rectangular thumbs, hatching, accent fill and state tracking", () => {
  expectMatch(slider, /--slider-progress/, "the range input should expose its draft progress to CSS");
  expectMatch(styles, /repeating-linear-gradient\(135deg/, "the unfilled track should retain Host-style hatching");
  expectMatch(styles, /\.term-slider__thumb-visual\s*\{[^}]*width:\s*11px[^}]*height:\s*16px[^}]*border-radius:\s*0/, "the thumb should remain a square-cornered Host-sized rectangle");
  expectMatch(styles, /var\(--accent-fill\)/, "the filled track should use the selected theme accent");
  expectMatch(styles, /\.term-slider\.is-disabled \.term-slider__range/, "disabled sliders should have an explicit theme-aware state");
  expectMatch(slider, /draft\.pending/, "draft pending state must remain connected to snapshot behavior");
  expectMatch(slider, /draft\.rejected/, "draft rejected state must remain connected to snapshot behavior");
});

test("Terminal slider keeps Host's 10px track and 16px interaction and thumb geometry", () => {
  expectMatch(styles, /\.term-slider__range input\[type="range"\]\s*\{[^}]*height:\s*16px[^}]*opacity:\s*0/, "the input should retain Host's 16px hit area without painting browser chrome");
  expectMatch(styles, /\.term-slider__track-visual\s*\{[^}]*height:\s*10px[^}]*border:\s*1px solid/, "the visible WebView track should be ten pixels with a thin border");
  expectMatch(styles, /\.term-slider__track-visual\s*\{[^}]*repeating-linear-gradient/, "the unfilled track should retain its hatch");
  expectMatch(styles, /\.term-slider__thumb-visual\s*\{[^}]*width:\s*11px[^}]*height:\s*16px[^}]*border:\s*1px solid var\(--bg\)/, "the thumb should match Host's 11 by 16 pixel outlined rectangle");
  expectMatch(styles, /\.term-slider__thumb-visual\s*\{[^}]*top:\s*50%[^}]*transform:\s*translate\(-50%,-50%\)/, "the visual thumb center should stay aligned to the track center");
});

test("Modulation and Tuning use the Host parameter row layout and source buttons", () => {
  const tuningEditor = source("../src/components/TuningEditor.vue");
  expectMatch(modulationWorkspace, /TerminalSegmentedControl :value="draft\.source"/, "motion source should use the four-button control");
  expectNoMatch(modulationWorkspace, /Motion source<select/, "motion source should not remain a dropdown");
  expectMatch(modulationWorkspace, /layout="row" :label-width="96" :value-width="84"/, "modulation values and sliders should align in Host-sized rows");
  expectMatch(tuningEditor, /layout="row" :label-width="128" :value-width="68"/, "tuning sliders should use the Host label and value columns");
  expectMatch(tuningEditor, /class="tuning-live"/, "live kinematics should follow the two upper tuning panels");
  expectMatch(styles, /\.tuning-grid \.tuning-live\s*\{\s*grid-column:\s*1\s*\/\s*-1/, "live kinematics should span the full lower row");
  expectMatch(styles, /\.modulation-editor-grid\s*\{\s*display:\s*grid;\s*grid-template-columns:\s*minmax\(0,1fr\)/, "modulation parameters should use sequential wide rows");
});

test("slider numeric focus commits on blur and hides native number spinners", () => {
  expectMatch(numberInput, /@blur="commitOnBlur"/, "leaving an unchanged numeric field should end its edit state");
  expectMatch(numberInput, /Math\.max\(props\.min \?\? Number\.NEGATIVE_INFINITY, Math\.min\(props\.max \?\? Number\.POSITIVE_INFINITY, parsed\)\)/, "numeric commits should stay inside their declared range");
  expectMatch(styles, /::-webkit-inner-spin-button\s*\{[^}]*-webkit-appearance:\s*none/, "Chromium number spinner arrows should be hidden");
  expectMatch(styles, /appearance:\s*textfield;\s*-moz-appearance:\s*textfield/, "Firefox number spinner arrows should be hidden");
});

test("Status progress bars match the Host gauge row geometry", () => {
  expectMatch(progress, /term-progress__label/, "the gauge label should share the bar row");
  expectMatch(styles, /\.term-progress\s*\{[^}]*min-height:\s*20px/, "the gauge row should reserve Host's 20px height");
  expectMatch(styles, /\.term-progress__track\s*\{[^}]*height:\s*10px[^}]*background:\s*var\(--track\)/, "the visible gauge should be ten pixels over the theme well");
  expectMatch(styles, /\.term-progress__track\s*\{[^}]*border:\s*1px solid var\(--line\)/, "the gauge should retain its thin Host-style frame");
});

test("Selected connection actions stay in Saved Connections before the independent Add panel", () => {
  const savedPanelStart = connectPanel.indexOf('title="Saved connections"');
  const selectedStart = connectPanel.indexOf('class="selected-host"');
  const addPanelStart = connectPanel.indexOf('kicker="ADD CONNECTION"');
  assert.ok(savedPanelStart >= 0 && selectedStart > savedPanelStart && selectedStart < addPanelStart, "Selected Connection should follow the saved list in the left panel");
  assert.doesNotMatch(connectPanel.slice(savedPanelStart, selectedStart), />RENAME<|REMOVE CONNECTION/, "profile rows should not carry their own rename/remove actions");
  assert.doesNotMatch(connectPanel.slice(addPanelStart), /class="selected-host"|>RENAME<|REMOVE CONNECTION/, "the Add panel should not include saved-profile controls");
  expectMatch(connectPanel, /store\.connection\.phase === 'online' && store\.connection\.profile_id === selectedId[\s\S]*?DISCONNECT/, "Disconnect should be available for the selected active profile");
  expectMatch(connectPanel, /saved-state[\s\S]*?OFFLINE/, "every saved profile should display ONLINE or OFFLINE state");
});

test("control grids reflow at narrow viewports without fixed-width overflow", () => {
  expectMatch(styles, /@media \(max-width:\s*1500px\)[\s\S]*?\.connect-grid,[\s\S]*?grid-template-columns:\s*1fr/, "large control groups should collapse at laptop width and below");
  expectMatch(styles, /@media \(max-width:\s*900px\)[\s\S]*?\.channel-editor-grid,[\s\S]*?\.modulation-editor-grid\s*\{\s*grid-template-columns:\s*1fr/, "channel, tuning, zone and modulation grids should fit narrow windows");
  expectMatch(styles, /\.channel-segment\s*\{[^}]*minmax\(0,1fr\)/, "channel segments should allow their controls to shrink before reflow");
  expectMatch(styles, /\.zone-calibration\s*\{[^}]*grid-template-columns:\s*1fr/, "zone editors should become a single column at narrow widths");
  expectMatch(styles, /\.modulation-layout\s*\{\s*grid-template-columns:\s*1fr/, "modulation controls should avoid horizontal overflow");
});

test("small chart and modulation labels remain at least eight pixels", () => {
  expectMatch(styles, /\.term-chart__axis\s*\{[^}]*font:\s*8px/, "chart axis labels should be at least eight pixels");
  expectMatch(styles, /\.term-control-state\s*\{[^}]*font-size:\s*8px/, "slider state labels should be at least eight pixels");
  expectMatch(styles, /\.modulation-preview-note\s*\{[^}]*font:\s*8px/, "modulation notes should be at least eight pixels");
});

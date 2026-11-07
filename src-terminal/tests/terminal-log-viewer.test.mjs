import test, { after } from "node:test";
import assert from "node:assert/strict";
import { readFile, unlink, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { Window } from "happy-dom";
import { parse, compileScript } from "@vue/compiler-sfc";
import ts from "typescript";
import { isCurrentHostLogRequest, prependOlderHostLogPage } from "../src/domain/host-log-pages.mjs";

const terminalRoot = fileURLToPath(new URL("..", import.meta.url));
const componentPath = join(terminalRoot, "src", "components", "TerminalLogViewer.vue");
const window = new Window({ url: "http://localhost/" });
Object.assign(globalThis, {
  window,
  document: window.document,
  localStorage: window.localStorage,
  Node: window.Node,
  Element: window.Element,
  HTMLElement: window.HTMLElement,
  SVGElement: window.SVGElement,
  Event: window.Event,
  MouseEvent: window.MouseEvent,
});

const componentSource = await readFile(componentPath, "utf8");
const { descriptor } = parse(componentSource, { filename: componentPath });
const compiled = compileScript(descriptor, { id: "terminal-log-viewer-test", inlineTemplate: true }).content;
const browserModulePath = join(dirname(componentPath), `.TerminalLogViewer.test-${process.pid}.mjs`);
const browserModule = ts.transpileModule(compiled, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
}).outputText;
await writeFile(browserModulePath, browserModule, "utf8");
const { default: TerminalLogViewer } = await import(`${pathToFileURL(browserModulePath).href}?test=${Date.now()}`);
await unlink(browserModulePath);
const { createApp, defineComponent, h, nextTick, reactive } = await import("vue");

after(async () => {
  for (const app of mountedApps) app.unmount();
  window.happyDOM.abort();
});

const mountedApps = new Set();

function records(count) {
  return Array.from({ length: count }, (_, index) => ({
    seq: index + 1,
    text: `[INFO ] 2026-10-25T20:00:${String(index % 60).padStart(2, "0")}Z Host event ${index + 1}`,
  }));
}

function mountViewer(lines, sourceKey = "host:profile-nightmare:host-nightmare") {
  const root = window.document.createElement("div");
  window.document.body.append(root);
  const props = reactive({ lines, sourceKey });
  const app = createApp(defineComponent({
    setup: () => () => h(TerminalLogViewer, {
      lines: props.lines,
      sourceKey: props.sourceKey,
      emptyLabel: "No Host lines match this filter.",
    }),
  }));
  app.mount(root);
  mountedApps.add(app);
  return { app, props, root };
}

function renderedRows(root) {
  return [...root.querySelectorAll(".term-log-line")];
}

test("loading the latest Host page restores rows after Clear View", async () => {
  window.localStorage.clear();
  const latest = records(8);
  const view = mountViewer(latest);
  assert.equal(renderedRows(view.root).length, latest.length);

  const clearButton = [...view.root.querySelectorAll("button")]
    .find((button) => button.textContent.trim() === "CLEAR VIEW");
  assert.ok(clearButton);
  clearButton.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  await nextTick();
  assert.equal(renderedRows(view.root).length, 0);
  assert.match(view.root.textContent, /View cleared/);

  view.app.unmount();
  mountedApps.delete(view.app);
  const reopened = mountViewer(latest.map((line) => ({ ...line })));
  assert.equal(renderedRows(reopened.root).length, 0, "the clear boundary is retained across a viewer remount");
  assert.match(reopened.root.textContent, /View cleared/);
  reopened.props.lines = latest.map((line) => ({ ...line }));
  await nextTick();
  assert.equal(renderedRows(reopened.root).length, latest.length);
  assert.equal(window.localStorage.getItem("shockingvrc.log-view-watermark.v1:host:profile-nightmare:host-nightmare"), null);
});

test("mounted viewer renders every loaded row at realistic Host history sizes", async () => {
  window.document.body.replaceChildren();
  window.localStorage.clear();
  const view = mountViewer([]);
  assert.equal(renderedRows(view.root).length, 0);
  assert.match(view.root.textContent, /No log records are available/);

  for (const size of [1, 100, 500, 591, 1_265, 1_826, 2_005]) {
    const lines = records(size);
    view.props.lines = lines;
    await nextTick();
    const rows = renderedRows(view.root);
    assert.equal(rows.length, size, `all ${size} Host rows should render with ALL and empty search`);
    assert.equal(rows[0].querySelector(".term-log-line__seq").textContent, "1");
    assert.equal(rows.at(-1).querySelector(".term-log-line__seq").textContent, String(size));
    assert.ok(rows.at(-1).querySelector("code").textContent.includes(`Host event ${size}`));
  }
  view.app.unmount();
  mountedApps.delete(view.app);
});

test("mounted filtering, long messages, and reactive array mutation preserve actual row text", async () => {
  window.document.body.replaceChildren();
  window.localStorage.clear();
  const multiline = "[WARN ] 2026-10-25T20:01:00Z first line\nsecond line\nthird line";
  const longText = ` [INFO ] 2026-10-25T20:01:01Z ${"x".repeat(32_768)}`.trimStart();
  const lines = [
    { seq: 1, text: "[INFO ] 2026-10-25T20:00:01Z alpha connected" },
    { seq: 2, text: "[WARN ] 2026-10-25T20:00:02Z beta retry" },
    { seq: 3, text: "[ERROR] 2026-10-25T20:00:03Z gamma failed" },
    { seq: 4, text: "[DEBUG] 2026-10-25T20:00:04Z delta details" },
    { seq: 5, text: multiline },
    { seq: 6, text: longText },
  ];
  const view = mountViewer(lines);
  assert.deepEqual(renderedRows(view.root).map((row) => row.querySelector(".term-log-line__seq").textContent), ["1", "2", "3", "4", "5", "6"]);

  const level = view.root.querySelector('select[aria-label="Filter log level"]');
  level.value = "ERROR";
  level.dispatchEvent(new window.Event("change", { bubbles: true }));
  await nextTick();
  assert.deepEqual(renderedRows(view.root).map((row) => row.querySelector(".term-log-line__seq").textContent), ["3"]);

  level.value = "ALL";
  level.dispatchEvent(new window.Event("change", { bubbles: true }));
  const search = view.root.querySelector('input[aria-label="Filter log text"]');
  search.value = "not present";
  search.dispatchEvent(new window.Event("input", { bubbles: true }));
  await nextTick();
  assert.equal(renderedRows(view.root).length, 0);
  assert.match(view.root.textContent, /No Host lines match this filter/);

  search.value = "second line";
  search.dispatchEvent(new window.Event("input", { bubbles: true }));
  await nextTick();
  assert.deepEqual(renderedRows(view.root).map((row) => row.querySelector(".term-log-line__seq").textContent), ["5"]);
  assert.equal(renderedRows(view.root)[0].querySelector("code").textContent, multiline);

  search.value = "";
  search.dispatchEvent(new window.Event("input", { bubbles: true }));
  view.props.lines.push({ seq: 7, text: "[INFO ] 2026-10-25T20:01:02Z appended in place" });
  await nextTick();
  assert.equal(renderedRows(view.root).length, 7);
  assert.equal(renderedRows(view.root).at(-1).querySelector(".term-log-line__seq").textContent, "7");
  assert.equal(renderedRows(view.root).find((row) => row.querySelector(".term-log-line__seq").textContent === "6").querySelector("code").textContent, longText);
  view.app.unmount();
  mountedApps.delete(view.app);
});

test("repeated older-page merges remain ordered and render without duplicate sequence rows", async () => {
  window.document.body.replaceChildren();
  window.localStorage.clear();
  const all = records(1_826);
  let page = { cursor: "1726", next_cursor: null, lines: all.slice(-100) };
  const view = mountViewer(page.lines);
  const clearButton = [...view.root.querySelectorAll("button")]
    .find((button) => button.textContent.trim() === "CLEAR VIEW");
  clearButton.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  await nextTick();
  assert.equal(renderedRows(view.root).length, 0);

  for (let end = 1_726; end >= 1; end -= 100) {
    const start = Math.max(1, end - 99);
    const older = all.slice(start - 1, end);
    page = prependOlderHostLogPage(page, {
      cursor: String(Math.max(0, start - 1)),
      next_cursor: page.cursor,
      lines: [...older, ...(page.lines.length ? [page.lines[0]] : [])],
    });
    view.props.lines = page.lines;
    await nextTick();
  }

  const rows = renderedRows(view.root);
  assert.equal(rows.length, 1_826);
  assert.deepEqual(rows.map((row) => Number(row.querySelector(".term-log-line__seq").textContent)), all.map((line) => line.seq));
  assert.equal(rows[0].querySelector("code").textContent, all[0].text);
  assert.equal(rows.at(-1).querySelector("code").textContent, all.at(-1).text);
  view.app.unmount();
  mountedApps.delete(view.app);
});

test("switching Host profiles displays the new profile records instead of reusing the old watermark", async () => {
  window.document.body.replaceChildren();
  window.localStorage.clear();
  const first = mountViewer(records(3), "host:profile-a:host-a");
  const clearButton = [...first.root.querySelectorAll("button")]
    .find((button) => button.textContent.trim() === "CLEAR VIEW");
  clearButton.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
  await nextTick();
  assert.equal(renderedRows(first.root).length, 0);
  first.props.sourceKey = "host:profile-b:host-b";
  first.props.lines = records(2).map((line) => ({ ...line, text: line.text.replace("Host event", "Profile B event") }));
  await nextTick();
  assert.equal(renderedRows(first.root).length, 2);
  assert.ok(renderedRows(first.root)[0].querySelector("code").textContent.includes("Profile B event 1"));
  first.app.unmount();
  mountedApps.delete(first.app);
});

test("delayed Host A response cannot overwrite the newer Host B log request", async () => {
  let currentRevision = 0;
  let currentLogs = null;
  let resolveHostA;
  let resolveHostB;
  const hostAResponse = new Promise((resolve) => { resolveHostA = resolve; });
  const hostBResponse = new Promise((resolve) => { resolveHostB = resolve; });
  const fetch = async (response) => {
    const revision = ++currentRevision;
    const result = await response;
    if (isCurrentHostLogRequest(revision, currentRevision)) currentLogs = result;
  };
  const hostARequest = fetch(hostAResponse);
  const hostBRequest = fetch(hostBResponse);
  resolveHostB({ host: "B", lines: records(1) });
  await hostBRequest;
  resolveHostA({ host: "A", lines: records(1) });
  await hostARequest;
  assert.equal(currentLogs.host, "B");
});

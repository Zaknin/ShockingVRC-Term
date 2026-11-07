import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const style = await readFile(new URL("../src/style.css", import.meta.url), "utf8");
const scoped = await Promise.all([
  readFile(new URL("../src/components/ConnectPanel.vue", import.meta.url), "utf8"),
  readFile(new URL("../src/components/ThemePage.vue", import.meta.url), "utf8"),
]);

test("Terminal typography tokens match the accepted Host logical font scale", () => {
  for (const declaration of [
    "--type-body: 13.5px",
    "--type-secondary: 12px",
    "--type-caption: 11px",
    "--type-micro: 11px",
    "--type-heading: 15px",
    "--type-display: 26px",
    "--type-numeric: 18px",
  ]) assert.ok(style.includes(declaration), `${declaration} should be centrally defined`);
});

test("global and scoped styles use semantic tokens instead of tiny fixed font sizes", () => {
  const source = [style, ...scoped].join("\n");
  assert.doesNotMatch(source, /font(?:-size)?:\s*(?:[0-9]+(?:\.[0-9]+)?)px\b/, "font declarations should route through typography tokens");
  assert.match(source, /--line-body:\s*1\.45/, "body line height should be explicit for measured comparison");
  assert.match(source, /--line-micro:\s*1\.3/, "diagnostic line height should be explicit for measured comparison");
});

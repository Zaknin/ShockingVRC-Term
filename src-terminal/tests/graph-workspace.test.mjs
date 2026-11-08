import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const component = readFileSync(new URL("../src/components/GraphWorkspace.vue", import.meta.url), "utf8");
const app = readFileSync(new URL("../src/App.vue", import.meta.url), "utf8");

test("Graph workspace renders an editable local graph canvas with revision and validation", () => {
  assert.match(component, /graph_revision/);
  assert.match(component, /v-for="node in graph\.nodes"/);
  assert.match(component, /v-for="\(edge, index\) in graph\.edges"/);
  assert.match(component, /issue\.message/);
  assert.match(component, /Editable Graph canvas/);
  assert.match(component, /APPLY TO HOST/);
  assert.doesNotMatch(component, /READ ONLY/);
});

test("Graph workspace exposes panic state and capability fallback without a panic release action", () => {
  assert.match(component, /editor\.panic/);
  assert.match(component, /editor\.ui_panic/);
  assert.match(component, /does not support Graph document reads/i);
  assert.match(component, /engine mode switching is not supported/i);
  assert.doesNotMatch(component, /panicRelease|releasePanic|CLEAR PANIC|RELEASE PANIC/i);
});

test("mode control is gated by capability, connection, and an in-flight request", () => {
  assert.match(component, /engine\.mode\.set\.v1/);
  assert.match(component, /modeChange\.status/);
  assert.match(component, /disabled=/);
  assert.match(component, /@change="requestMode/);
});

test("local editing controls include draft save, local import/export, presets, grouping, and explicit apply", () => {
  for (const label of ["ADD NODE", "DUPLICATE", "DELETE", "GROUP SELECTION", "SAVE LOCAL", "RELOAD LOCAL", "IMPORT JSON", "EXPORT JSON", "LOAD TO DRAFT", "APPLY TO HOST"]) {
    assert.ok(component.includes(label), `missing ${label}`);
  }
  assert.match(component, /window\.confirm\(message\)/);
  assert.match(component, /graph\.presets\.detail\.chunked\.v1/);
  assert.match(component, /graph\.apply\.upload\.status\.v1/);
});

test("stale drafts remain visible while Host apply is blocked and panic release stays absent", () => {
  assert.match(component, /draft\.stale/);
  assert.match(component, /:disabled="!canApply"/);
  assert.match(component, /preserved from Host revision/);
  assert.doesNotMatch(component, /panicRelease|releasePanic|CLEAR PANIC|RELEASE PANIC/i);
});

test("Graph mode banner no longer claims Terminal cannot edit Graphs", () => {
  assert.doesNotMatch(app, /Terminal does not edit Graphs/i);
  assert.match(app, /Classic controls are unavailable\./);
});

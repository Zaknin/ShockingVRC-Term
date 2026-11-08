import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const component = readFileSync(new URL("../src/components/GraphWorkspace.vue", import.meta.url), "utf8");

test("Graph workspace renders read-only nodes, connections, revision, and validation issues", () => {
  assert.match(component, /graph_revision/);
  assert.match(component, /v-for="node in document\.graph\.nodes"/);
  assert.match(component, /v-for="\(edge, index\) in document\.graph\.edges"/);
  assert.match(component, /issue\.message/);
  assert.match(component, /READ ONLY/);
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

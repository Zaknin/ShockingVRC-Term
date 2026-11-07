import test from "node:test";
import assert from "node:assert/strict";
import { parseModulationFunctionJson } from "../src/domain/modulation-json.mjs";

test("advanced editor accepts a valid Remote API function tree", () => {
  assert.deepEqual(parseModulationFunctionJson('{"kind":"compose","inner":{"kind":"sin"},"outer":{"kind":"cos"}}'), {
    kind: "compose", inner: { kind: "sin" }, outer: { kind: "cos" },
  });
});

test("advanced editor rejects invalid JSON, unknown variants, and missing Compose children", () => {
  assert.throws(() => parseModulationFunctionJson("{"), SyntaxError);
  assert.throws(() => parseModulationFunctionJson('{"kind":"not_a_remote_function"}'), /Unsupported modulation function/);
  assert.throws(() => parseModulationFunctionJson('{"kind":"compose","inner":{"kind":"sin"}}'), TypeError);
  assert.throws(() => parseModulationFunctionJson('{"kind":"sin_pow","exponent":1e100}'), /finite f32 exponent/);
});

test("advanced editor enforces the same Compose depth limit as local preview", () => {
  let tree = { kind: "sin" };
  for (let index = 0; index < 9; index += 1) tree = { kind: "compose", inner: tree, outer: { kind: "sin" } };
  assert.throws(() => parseModulationFunctionJson(JSON.stringify(tree)), /depth exceeds 8/);
});

test("advanced editor stops at the Compose limit for deeply nested pasted JSON", () => {
  const deepTree = `${'{"kind":"compose","inner":'.repeat(4000)}{"kind":"sin"}${',"outer":{"kind":"sin"}}'.repeat(4000)}`;
  assert.throws(() => parseModulationFunctionJson(deepTree), /depth exceeds 8/);
});

test("advanced editor rejects function JSON larger than a Remote frame", () => {
  assert.throws(() => parseModulationFunctionJson(" ".repeat(256 * 1024 + 1)), /256 KiB/);
});

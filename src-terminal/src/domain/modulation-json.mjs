import { evaluateFunction } from "./preview-math.mjs";

const MAX_FUNCTION_JSON_BYTES = 256 * 1024;

function validateNode(value, depth = 0) {
  if (!value || typeof value !== "object" || Array.isArray(value) || typeof value.kind !== "string") {
    throw new TypeError("A modulation function must be an object with a string kind.");
  }
  if (value.kind === "compose") {
    if (depth >= 8) throw new RangeError("Modulation Compose depth exceeds 8");
    validateNode(value.inner, depth + 1);
    validateNode(value.outer, depth + 1);
    return;
  }
  if ((value.kind === "sin_pow" || value.kind === "cos_pow") && (typeof value.exponent !== "number" || !Number.isFinite(Math.fround(value.exponent)))) {
    throw new TypeError(`${value.kind} requires a finite f32 exponent.`);
  }
  evaluateFunction(value, 0);
}

export function parseModulationFunctionJson(source) {
  if (new TextEncoder().encode(source).byteLength > MAX_FUNCTION_JSON_BYTES) {
    throw new RangeError("Function JSON exceeds the 256 KiB Remote frame limit.");
  }
  const value = JSON.parse(source);
  validateNode(value);
  return value;
}

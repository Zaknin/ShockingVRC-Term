import { sampleModulation } from "./preview-math.mjs";
import hostSparklineReference from "./modulation-picker-sparklines.json" with { type: "json" };

export const HOST_FUNCTIONS = Object.freeze([
  { kind: "none" }, { kind: "sin" }, { kind: "cos" }, { kind: "tan" },
  { kind: "sin_cos" }, { kind: "sin2" }, { kind: "cos2" }, { kind: "sin_plus_cos" },
  { kind: "sinh" }, { kind: "cosh" }, { kind: "tanh" },
  { kind: "square" }, { kind: "cube" }, { kind: "pow4" }, { kind: "sqrt" },
  { kind: "cbrt" }, { kind: "abs" }, { kind: "sign" },
  { kind: "exp" }, { kind: "exp_neg" }, { kind: "pow2x" }, { kind: "pow10x" },
  { kind: "ln" }, { kind: "log2" }, { kind: "log10" },
  { kind: "triangle" }, { kind: "saw" }, { kind: "reverse_saw" }, { kind: "square_wave" },
  { kind: "pulse" }, { kind: "bounce" },
  { kind: "sigmoid" }, { kind: "smooth_step" }, { kind: "smoother_step" },
  { kind: "logistic" }, { kind: "soft_sign" },
  { kind: "perlin" }, { kind: "simplex" }, { kind: "fractal" }, { kind: "value_noise" },
  { kind: "sin_plus_noise" }, { kind: "sin_times_noise" },
  { kind: "triangle_plus_sin" }, { kind: "square_times_sigmoid" },
].map((func) => Object.freeze(func)));

const FUNCTION_NAMES = Object.freeze({
  none: "none", sin: "sin", cos: "cos", tan: "tan", asin: "asin", acos: "acos", atan: "atan",
  sin_cos: "sincos", sin2: "sin2", cos2: "cos2", sin_plus_cos: "sin+cos", sinh: "sinh",
  cosh: "cosh", tanh: "tanh", square: "x2", cube: "x3", pow4: "x4", sqrt: "sqrt",
  cbrt: "cbrt", abs: "abs", sign: "sign", exp: "exp", exp_neg: "exp-", pow2x: "2^x",
  pow10x: "10^x", ln: "ln", log2: "log2", log10: "log10", triangle: "triangle", saw: "saw",
  reverse_saw: "rsaw", square_wave: "square", pulse: "pulse", bounce: "bounce", sigmoid: "sigmoid",
  smooth_step: "smoothstep", smoother_step: "smootherstep", logistic: "logistic", soft_sign: "softsign",
  perlin: "perlin", simplex: "simplex", fractal: "fractal", value_noise: "valuenoise",
  sin_plus_noise: "sin+noise", sin_times_noise: "sin*noise", triangle_plus_sin: "triangle+sin",
  square_times_sigmoid: "square*sigmoid",
});

const GROUPS = Object.freeze([
  ["Off", new Set(["none"])],
  ["Trigonometric", new Set(["sin", "cos", "tan", "asin", "acos", "atan", "sin_cos", "sin2", "cos2", "sin_plus_cos", "sin_pow", "cos_pow"])],
  ["Hyperbolic", new Set(["sinh", "cosh", "tanh"])],
  ["Powers & roots", new Set(["square", "cube", "pow4", "sqrt", "cbrt", "abs", "sign"])],
  ["Exponential", new Set(["exp", "exp_neg", "pow2x", "pow10x"])],
  ["Logarithmic", new Set(["ln", "log2", "log10"])],
  ["Waveshapes", new Set(["triangle", "saw", "reverse_saw", "square_wave", "pulse", "bounce"])],
  ["Curves & steps", new Set(["sigmoid", "smooth_step", "smoother_step", "logistic", "soft_sign"])],
  ["Noise", new Set(["perlin", "simplex", "fractal", "value_noise"])],
  ["Blends", new Set(["sin_plus_noise", "sin_times_noise", "triangle_plus_sin", "square_times_sigmoid"])],
  ["Other", new Set(["compose"])],
]);

const SPARKLINE_CACHE_LIMIT = 128;
const sparklineCache = new Map();
const hostSparklineByName = new Map(hostSparklineReference.functions.map(({ name, normalized }) => [name, Object.freeze(normalized)]));

export function displayFunction(func) {
  if (func?.kind === "compose") return `${displayFunction(func.outer)}(${displayFunction(func.inner)})`;
  if (func?.kind === "sin_pow" || func?.kind === "cos_pow") {
    return `${func.kind === "sin_pow" ? "sin" : "cos"}^${String(func.exponent)}`;
  }
  return FUNCTION_NAMES[func?.kind] ?? String(func?.kind ?? "none");
}

export function functionGroup(func) {
  const group = GROUPS.find(([, kinds]) => kinds.has(func?.kind));
  return group?.[0] ?? "Other";
}

export function functionListFor(current) {
  const functions = [...HOST_FUNCTIONS];
  const currentName = displayFunction(current);
  if (!functions.some((func) => displayFunction(func) === currentName)) functions.splice(1, 0, current);
  return functions;
}

export function groupFunctions(functions, query = "") {
  const needle = String(query).trim().toLocaleLowerCase();
  const groups = [];
  for (const func of functions) {
    const name = displayFunction(func);
    if (needle && !name.toLocaleLowerCase().includes(needle)) continue;
    const label = functionGroup(func);
    let group = groups.at(-1);
    if (!group || group.label !== label) {
      group = { label, functions: [] };
      groups.push(group);
    }
    group.functions.push(func);
  }
  return groups;
}

export function functionSparkline(func) {
  const key = JSON.stringify(func);
  const cached = sparklineCache.get(key);
  if (cached) return cached;

  const hostReference = hostSparklineByName.get(displayFunction(func));
  let normalized;
  if (hostReference && HOST_FUNCTIONS.some((standard) => displayFunction(standard) === displayFunction(func))) {
    normalized = hostReference;
  } else {
    const points = sampleModulation({
      source: "Depth", function: func, base_speed: 1, sensitivity: 0, max_deviation: 1,
      phase: 0, frequency_multiplier: 1, offset: 0, power: 1, clamp_min: -10, clamp_max: 10,
    }, 0, 0, 64, 1 / 64);
    const low = Math.min(...points);
    const high = Math.max(...points);
    const span = Math.max(high - low, 1e-3);
    normalized = Object.freeze(points.map((value) => (value - low) / span));
  }
  if (sparklineCache.size >= SPARKLINE_CACHE_LIMIT) sparklineCache.delete(sparklineCache.keys().next().value);
  sparklineCache.set(key, normalized);
  return normalized;
}

const PARAMETER_LABELS = Object.freeze({
  base_speed: "Speed", sensitivity: "Sensitivity", max_deviation: "Deviation", phase: "Phase",
  frequency_multiplier: "Freq mul", offset: "Offset", power: "Power", clamp_min: "Clamp min", clamp_max: "Clamp max",
});

export function modulationParameterLabel(key) {
  return PARAMETER_LABELS[key] ?? key;
}

export function formatModulationParameter(key, value) {
  const precision = key === "base_speed" || key === "phase" ? 2
    : key === "max_deviation" || key === "offset" || key === "clamp_min" || key === "clamp_max" ? 1 : 3;
  const formatted = Number(value).toFixed(precision);
  if (key === "base_speed") return `${formatted} Hz`;
  if (key === "phase") return `${formatted} cyc`;
  if (key === "max_deviation") return `±${formatted}`;
  return formatted;
}

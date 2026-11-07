const TAU = Math.PI * 2;
const MAX_COMPOSE_DEPTH = 8;

function unitPhase(x) {
  const phase = x / TAU;
  return phase - Math.floor(phase);
}

function centeredPhase(x) {
  return 2 * unitPhase(x) - 1;
}

function hash1d(value) {
  let n = (value << 13) ^ value;
  const first = Math.imul(n, 15731) + 789221;
  const second = Math.imul(n, first) + 1376312589;
  const hashed = Math.imul(n, second);
  return 1 - (hashed & 0x7fffffff) / 1073741824;
}

function fade(value) {
  return value ** 3 * (value * (value * 6 - 15) + 10);
}

function perlin1d(x) {
  const xi = Math.floor(x);
  const xf = x - xi;
  const u = fade(xf);
  const left = hash1d(xi) * xf;
  const right = hash1d(xi + 1) * (xf - 1);
  return left + u * (right - left);
}

function simplex1d(x) {
  const i0 = Math.floor(x);
  const i1 = i0 + 1;
  const x0 = x - i0;
  const x1 = x0 - 1;
  const t0 = (1 - x0 * x0) ** 4;
  const t1 = (1 - x1 * x1) ** 4;
  return (t0 * hash1d(i0) * x0 + t1 * hash1d(i1) * x1) * 2.5;
}

function valueNoise1d(x) {
  const xi = Math.floor(x);
  const xf = x - xi;
  const u = fade(xf);
  const left = hash1d(xi);
  return left + u * (hash1d(xi + 1) - left);
}

function fractal1d(x) {
  let value = 0;
  let amplitude = 1;
  let frequency = 1;
  let maxAmplitude = 0;
  for (let octave = 0; octave < 4; octave += 1) {
    value += perlin1d(x * frequency) * amplitude;
    maxAmplitude += amplitude;
    amplitude *= 0.5;
    frequency *= 2;
  }
  return value / maxAmplitude;
}

function evalFunction(func, x, composeLevel) {
  if (!func || typeof func !== "object" || typeof func.kind !== "string") {
    throw new TypeError("Modulation function is not a valid Remote API v1 object");
  }
  const centered = () => centeredPhase(x);
  const unit = () => unitPhase(x);
  switch (func.kind) {
    case "none": return 0;
    case "sin": return Math.sin(x);
    case "cos": return Math.cos(x);
    case "tan": return Math.max(-1, Math.min(1, Math.tan(x)));
    case "asin": return Math.asin(Math.max(-1, Math.min(1, centered()))) / (Math.PI / 2);
    case "acos": return Math.acos(Math.max(-1, Math.min(1, centered()))) / Math.PI * 2 - 1;
    case "atan": return Math.atan(centered()) / (Math.PI / 2);
    case "sin_cos": return Math.sin(x) * Math.cos(x);
    case "sin2": return Math.sin(x) ** 2;
    case "cos2": return Math.cos(x) ** 2;
    case "sin_plus_cos": return (Math.sin(x) + Math.cos(x)) * 0.7071;
    case "sin_pow": return Math.sign(Math.sin(x)) * Math.abs(Math.sin(x)) ** func.exponent;
    case "cos_pow": return Math.sign(Math.cos(x)) * Math.abs(Math.cos(x)) ** func.exponent;
    case "sinh": return Math.max(-1, Math.min(1, Math.sinh(centered() * 3)));
    case "cosh": return Math.max(0, Math.min(1, Math.cosh(centered() * 3) - 1));
    case "tanh": return Math.tanh(centered() * 3);
    case "square": return centered() * centered() * Math.sign(centered());
    case "cube": return centered() ** 3;
    case "pow4": return centered() ** 4 * Math.sign(centered());
    case "sqrt": return Math.sqrt(unit()) * 2 - 1;
    case "cbrt": return Math.cbrt(centered());
    case "abs": return Math.abs(centered());
    case "sign": return Math.sign(centered());
    case "exp": {
      const s = centered() * 6;
      const e = Math.exp(s);
      return e / (1 + e) * 2 - 1;
    }
    case "exp_neg": {
      const e = Math.exp(-centered() * 6);
      return e / (1 + e) * 2 - 1;
    }
    case "pow2x": {
      const e = 2 ** (centered() * 4);
      return e / (1 + e) * 2 - 1;
    }
    case "pow10x": return (10 ** centered() - 1) / 9;
    case "ln": {
      const t = unit() * 0.99 + 0.01;
      return Math.max(0, Math.min(1, Math.log(t) / Math.log(0.01))) * 2 - 1;
    }
    case "log2": {
      const t = unit() * 0.99 + 0.01;
      return Math.max(0, Math.min(1, Math.log2(t) / Math.log2(0.01))) * 2 - 1;
    }
    case "log10": {
      const t = unit() * 0.99 + 0.01;
      return Math.max(0, Math.min(1, Math.log10(t) / Math.log10(0.01))) * 2 - 1;
    }
    case "triangle": return 2 * Math.abs(2 * unit() - 1) - 1;
    case "saw": return 2 * unit() - 1;
    case "reverse_saw": return 1 - 2 * unit();
    case "square_wave": return Math.sin(x) >= 0 ? 1 : -1;
    case "pulse": return unit() < 0.25 ? 1 : -1;
    case "bounce": return Math.abs(2 * unit() - 1) ** 2;
    case "sigmoid": return (1 / (1 + Math.exp(-centered() * 6))) * 2 - 1;
    case "smooth_step": {
      const t = unit();
      return t * t * (3 - 2 * t) * 2 - 1;
    }
    case "smoother_step": {
      const t = unit();
      return t ** 3 * (t * (t * 6 - 15) + 10) * 2 - 1;
    }
    case "logistic": return (1 / (1 + Math.exp(-centered() * 5))) * 2 - 1;
    case "soft_sign": {
      const s = centered() * 5;
      return s / (1 + Math.abs(s));
    }
    case "perlin": return perlin1d(x);
    case "simplex": return simplex1d(x);
    case "fractal": return fractal1d(x);
    case "value_noise": return valueNoise1d(x);
    case "sin_plus_noise": return (Math.sin(x) + perlin1d(x)) * 0.5;
    case "sin_times_noise": return Math.sin(x) * perlin1d(x * 3);
    case "triangle_plus_sin": return (evalFunction({ kind: "triangle" }, x, composeLevel) + Math.sin(x)) * 0.5;
    case "square_times_sigmoid": {
      const square = Math.sin(x) >= 0 ? 1 : -1;
      const sigmoid = (1 / (1 + Math.exp(-centered() * 6))) * 2 - 1;
      return square * Math.abs(sigmoid);
    }
    case "compose":
      if (composeLevel >= MAX_COMPOSE_DEPTH) throw new RangeError("Modulation Compose depth exceeds 8");
      return evalFunction(func.outer, evalFunction(func.inner, x, composeLevel + 1), composeLevel + 1);
    default: throw new TypeError(`Unsupported modulation function: ${func.kind}`);
  }
}

export function rawToHz(raw) {
  return 100 + (1 - 100) / (255 - 10) * (raw - 10);
}

export function hzToRaw(hz) {
  if (!Number.isFinite(hz)) return Number.NaN;
  const clampedHz = Math.max(1, Math.min(100, hz));
  return Math.round(10 + (255 - 10) / (1 - 100) * (clampedHz - 100));
}

export function modulationChartDomain(points) {
  const finitePoints = points.filter(Number.isFinite);
  if (finitePoints.length === 0) return { min: 0, max: 1 };

  const min = Math.min(...finitePoints);
  const max = Math.max(...finitePoints);
  if (min === max) {
    const padding = Math.max(Math.abs(min) * 0.04, 0.5);
    return { min: min - padding, max: max + padding };
  }

  const padding = (max - min) * 0.08;
  return { min: min - padding, max: max + padding };
}

export function modulationChartLabelPrecision(domain) {
  const span = Math.abs(domain.max - domain.min);
  if (!Number.isFinite(span) || span <= 0) return 2;
  return Math.min(6, Math.max(2, Math.ceil(-Math.log10(span))));
}

export function formatNumber(value, maxFraction = 3) {
  if (!Number.isFinite(value)) return "—";
  return new Intl.NumberFormat("en-US", {
    maximumFractionDigits: maxFraction,
    useGrouping: false,
  }).format(value);
}

export function defaultModulationConfig(kind) {
  return {
    source: "Depth",
    function: { kind: "sin" },
    base_speed: 1,
    sensitivity: 1,
    max_deviation: 20,
    phase: 0,
    frequency_multiplier: 1,
    offset: 0,
    power: 1,
    clamp_min: kind === "intensity" ? 0 : 10,
    clamp_max: kind === "intensity" ? 100 : 255,
  };
}

export function composeDepth(func) {
  if (!func || typeof func !== "object" || func.kind !== "compose") return 0;
  return 1 + Math.max(composeDepth(func.outer), composeDepth(func.inner));
}

export function evaluateFunction(func, x, depth = 0) {
  if (composeDepth(func) > MAX_COMPOSE_DEPTH) throw new RangeError("Modulation Compose depth exceeds 8");
  return evalFunction(func, x, depth);
}

export function sampleModulation(config, baseValue, sourceValue, sampleCount = 120, durationSeconds = 2) {
  if (composeDepth(config.function) > MAX_COMPOSE_DEPTH) {
    throw new RangeError("Modulation Compose depth exceeds 8");
  }
  if (!Number.isInteger(sampleCount) || sampleCount < 2 || sampleCount > 1200) {
    throw new RangeError("Preview sample count must be between 2 and 1200");
  }
  if (!Number.isFinite(durationSeconds) || durationSeconds <= 0) {
    throw new RangeError("Preview duration must be positive");
  }
  const points = [];
  let accumulator = 0;
  const step = durationSeconds / (sampleCount - 1);
  const speedHz = config.base_speed + sourceValue * config.sensitivity;
  for (let index = 0; index < sampleCount; index += 1) {
    if (index > 0) {
      accumulator += speedHz * step;
      if (Math.abs(accumulator) > 1000) accumulator -= Math.trunc(accumulator / 1000) * 1000;
    }
    const phase = accumulator * config.frequency_multiplier + config.phase;
    let modulation = evalFunction(config.function, phase * TAU, 0);
    if (config.power !== 1) modulation = Math.sign(modulation) * Math.abs(modulation) ** config.power;
    const output = baseValue + modulation * config.max_deviation + config.offset;
    points.push(Math.max(config.clamp_min, Math.min(config.clamp_max, output)));
  }
  return points;
}

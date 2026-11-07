export const HOST_THEMES = Object.freeze([
  { id: "terminal", name: "Terminal", base: [0.17, 0.012, 265], contrast: [0.86, 0.015, 265], accent: [0.8, 0.16, 158] },
  { id: "ember", name: "Ember", base: [0.16, 0.022, 40], contrast: [0.87, 0.02, 50], accent: [0.72, 0.18, 42] },
  { id: "flare", name: "Flare", base: [0.15, 0.016, 95], contrast: [0.88, 0.02, 95], accent: [0.86, 0.17, 96] },
  { id: "cobalt", name: "Cobalt", base: [0.16, 0.022, 250], contrast: [0.87, 0.02, 250], accent: [0.74, 0.15, 235] },
  { id: "orchid", name: "Orchid", base: [0.16, 0.022, 330], contrast: [0.88, 0.015, 330], accent: [0.75, 0.19, 340] },
  { id: "wire", name: "Wire", base: [0.13, 0, 0], contrast: [0.74, 0, 0], accent: [0.98, 0, 0] },
  { id: "void", name: "Void", base: [0.06, 0, 0], contrast: [0.92, 0.01, 300], accent: [0.72, 0.2, 300] },
  { id: "paper", name: "Paper", base: [0.97, 0.006, 95], contrast: [0.26, 0.012, 95], accent: [0.52, 0.14, 155] },
  { id: "blueprint", name: "Blueprint", base: [0.95, 0.012, 240], contrast: [0.28, 0.03, 250], accent: [0.5, 0.16, 245] },
]);

const f32 = Math.fround;
const add = (a, b) => f32(f32(a) + f32(b));
const sub = (a, b) => f32(f32(a) - f32(b));
const mul = (a, b) => f32(f32(a) * f32(b));
const div = (a, b) => f32(f32(a) / f32(b));

export function lerp(a, b, t) { return add(a, mul(sub(b, a), t)); }

export function lerpHue(a, b, t) {
  let d = f32(sub(b, a) % 360);
  d = f32(f32(d + 540) % 360 - 180);
  return add(a, mul(d, t));
}

export function mixOklch(a, b, aShare) {
  const t = sub(1, Math.max(0, Math.min(1, aShare)));
  return [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerpHue(a[2], b[2], t)];
}

export function oklchToRgb([lightness, chroma, hue]) {
  const radians = div(mul(hue, Math.PI), 180);
  const a = mul(chroma, f32(Math.cos(radians)));
  const b = mul(chroma, f32(Math.sin(radians)));
  const cube = (value) => mul(mul(value, value), value);
  const l = cube(add(add(lightness, mul(0.39633778, a)), mul(0.21580376, b)));
  const m = cube(sub(sub(lightness, mul(0.10556135, a)), mul(0.06385417, b)));
  const s = cube(sub(sub(lightness, mul(0.08948418, a)), mul(1.2914855, b)));
  const r = add(sub(mul(4.0767417, l), mul(3.3077116, m)), mul(0.23096994, s));
  const g = add(sub(mul(-1.268438, l), mul(-2.6097574, m)), mul(-0.34131938, s));
  const blue = add(add(mul(-0.0041960863, l), mul(-0.7034186, m)), mul(1.7076147, s));
  const encode = (channel) => {
    const value = channel <= 0.0031308
      ? mul(12.92, channel)
      : sub(mul(1.055, Math.max(0, channel) ** f32(1 / 2.4)), 0.055);
    return Math.round(Math.max(0, Math.min(1, value)) * 255);
  };
  return [encode(r), encode(g), encode(blue)];
}

const hex = (rgb) => `#${rgb.map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
const rgbMix = (color, base, amount) => color.map((channel, index) => Math.trunc(lerp(channel, base[index], amount)));
const faded = (color, base, amount) => hex(rgbMix(color, base, amount));

export function hostPalette(themeId) {
  const theme = HOST_THEMES.find(({ id }) => id === themeId) ?? HOST_THEMES[0];
  const light = theme.base[0] > theme.contrast[0];
  const base = oklchToRgb(theme.base);
  const contrast = oklchToRgb(theme.contrast);
  const accent = oklchToRgb(theme.accent);
  const baseOklch = theme.base;
  const contrastOklch = theme.contrast;
  const accentOklch = theme.accent;
  const statusLightness = light ? 0.55 : 0.78;
  const success = oklchToRgb([statusLightness, 0.16, 150]);
  const warning = oklchToRgb([statusLightness, 0.15, 80]);
  const error = oklchToRgb([statusLightness, 0.19, 25]);
  const tokens = {
    "--bg": hex(base),
    "--text": hex(contrast),
    "--contrast": hex(contrast),
    "--surface": hex(oklchToRgb(mixOklch(contrastOklch, baseOklch, light ? 0.03 : 0.05))),
    "--surface-raised": hex(oklchToRgb(mixOklch(contrastOklch, baseOklch, 0.2))),
    "--surface-deep": hex(oklchToRgb(mixOklch(contrastOklch, baseOklch, light ? 0.1 : 0.12))),
    "--surface-hover": faded(contrast, base, 0.82),
    "--line": hex(oklchToRgb(mixOklch(contrastOklch, baseOklch, 0.42))),
    "--line-soft": hex(oklchToRgb(mixOklch(contrastOklch, baseOklch, 0.58))),
    "--muted": hex(oklchToRgb(mixOklch(contrastOklch, baseOklch, 0.58))),
    "--accent": hex(accent),
    "--accent-soft": faded(accent, base, 0.18),
    "--accent-fill": faded(accent, base, 0.35),
    "--marker": hex(oklchToRgb(mixOklch(accentOklch, baseOklch, 0.26))),
    "--success": hex(success),
    "--warning": hex(warning),
    "--error": hex(error),
    "--success-soft": faded(success, base, 0.6),
    "--warning-soft": faded(warning, base, 0.6),
    "--error-soft": faded(error, base, 0.6),
    "--focus": hex(accent),
    "--disabled": faded(contrast, base, 0.65),
    "--chart-a": hex(accent),
    "--chart-b": hex(success),
    "--chart-c": hex(warning),
    "--thumb": hex(contrast),
    "--track": hex(oklchToRgb(mixOklch(contrastOklch, baseOklch, light ? 0.1 : 0.12))),
    "--button": hex(oklchToRgb(mixOklch(contrastOklch, baseOklch, 0.2))),
    "--button-text": hex(base),
    "--shadow": light ? "rgba(0, 0, 0, .14)" : "rgba(0, 0, 0, .1)",
  };
  return { id: theme.id, name: theme.name, light, base: hex(base), text: hex(contrast), accent: hex(accent), tokens };
}

export function applyHostPalette(themeId, root = document.documentElement) {
  const palette = hostPalette(themeId);
  for (const [name, value] of Object.entries(palette.tokens)) root.style.setProperty(name, value);
  root.dataset.theme = palette.id;
  return palette;
}

export function readSavedTheme(storage = globalThis.localStorage) {
  try {
    const id = storage?.getItem("terminal.theme");
    return HOST_THEMES.some((theme) => theme.id === id) ? id : "terminal";
  } catch { return "terminal"; }
}

export function saveTheme(id, storage = globalThis.localStorage) {
  const safeId = HOST_THEMES.some((theme) => theme.id === id) ? id : "terminal";
  try { storage?.setItem("terminal.theme", safeId); } catch { /* Storage may be unavailable in WebView policy contexts. */ }
  return safeId;
}

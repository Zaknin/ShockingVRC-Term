export const MIN_SCALE = 75;
export const MAX_SCALE = 175;
export const DEFAULT_SCALE = 100;
export const SCALE_STEPS = Object.freeze([75, 80, 90, 100, 110, 120, 125, 130, 140, 150, 160, 170, 175]);
export const SECTION_NAVIGATION = Object.freeze([
  Object.freeze({ name: "Connect", code: "0" }),
  Object.freeze({ name: "Status", code: "1" }),
  Object.freeze({ name: "Zones", code: "2" }),
  Object.freeze({ name: "Channels", code: "3" }),
  Object.freeze({ name: "Tuning", code: "4" }),
  Object.freeze({ name: "Modulation", code: "5" }),
  Object.freeze({ name: "Presets", code: "6" }),
  Object.freeze({ name: "Alarm", code: "7" }),
  Object.freeze({ name: "Setup", code: "8" }),
  Object.freeze({ name: "Logs", code: "9" }),
  Object.freeze({ name: "Graph", code: "G" }),
  Object.freeze({ name: "Theme", code: "T" }),
]);

export function normalizeScale(value) {
  if (value == null || value === "") return DEFAULT_SCALE;
  if (!Number.isFinite(Number(value))) return DEFAULT_SCALE;
  const bounded = Math.max(MIN_SCALE, Math.min(MAX_SCALE, Number(value)));
  return SCALE_STEPS.reduce((nearest, candidate) =>
    Math.abs(candidate - bounded) < Math.abs(nearest - bounded) ? candidate : nearest, SCALE_STEPS[0]);
}

export function changeScale(current, direction) {
  const value = normalizeScale(current);
  const index = SCALE_STEPS.indexOf(value);
  return SCALE_STEPS[Math.max(0, Math.min(SCALE_STEPS.length - 1, index + Math.sign(direction)))];
}

export function readSavedScale(storage = globalThis.localStorage) {
  try { return normalizeScale(storage?.getItem("terminal.scale")); } catch { return DEFAULT_SCALE; }
}

export function saveScale(value, storage = globalThis.localStorage) {
  const scale = normalizeScale(value);
  try { storage?.setItem("terminal.scale", String(scale)); } catch { /* WebView storage can be disabled by host policy. */ }
  return scale;
}

export function isTextEntryTarget(target) {
  if (!target) return false;
  const tag = String(target.tagName ?? "").toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || Boolean(target.isContentEditable);
}

export function shortcutAction(event) {
  if (event.ctrlKey || event.metaKey) {
    if (event.key === "+" || event.key === "=" || event.code === "NumpadAdd") return "zoom-in";
    if (event.key === "-" || event.code === "NumpadSubtract") return "zoom-out";
    if (event.key === "0" || event.code === "Numpad0") return "zoom-reset";
  }
  if (isTextEntryTarget(event.target)) return null;
  if (!event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey) {
    if (event.key.toLowerCase() === "g") return "graph";
    const sectionIndex = SECTION_NAVIGATION.findIndex((section) => section.code === event.key);
    if (sectionIndex >= 0 && sectionIndex < 10) return `section-${sectionIndex}`;
    if (event.key.toLowerCase() === "t") return "theme";
  }
  return null;
}

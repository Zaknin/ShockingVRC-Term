function record(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

export function entryIndex(entry, fallback) {
  const index = record(entry).index;
  return typeof index === "number" && Number.isInteger(index) ? index : fallback;
}

export function presetEntryName(entry) {
  const value = record(entry);
  const nested = record(value.preset);
  return typeof value.name === "string" ? value.name : typeof nested.name === "string" ? nested.name : "Host entry";
}

export function presetAuthorLabel(entry) {
  const value = record(entry);
  const nested = record(value.preset);
  return typeof value.author === "string" ? value.author : typeof nested.author === "string" ? nested.author : "Host catalog";
}

export function presetNumberArray(entry, key) {
  const nested = record(record(entry).preset);
  const value = nested[key];
  return Array.isArray(value) && value.length === 4 && value.every((item) => typeof item === "number" && Number.isFinite(item))
    ? value
    : null;
}

export function presetModulationCount(entry) {
  const nested = record(record(entry).preset);
  return [nested.freq_modulation, nested.intensity_modulation].reduce((total, value) =>
    total + (Array.isArray(value) ? value.filter((slot) => slot != null).length : 0), 0);
}

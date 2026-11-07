function record(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

export function entryIndex(entry, fallback) {
  const index = record(entry).index;
  return typeof index === "number" && Number.isInteger(index) ? index : fallback;
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

function catalogIdentity(entry) {
  const value = record(entry);
  const id = value.id;
  if ((typeof id === "string" && id.length > 0) || (typeof id === "number" && Number.isFinite(id))) return `id:${String(id)}`;
  const nested = record(value.preset);
  const identityFields = {
    name: presetEntryName(value),
    author: presetAuthorLabel(value),
    description: typeof value.description === "string" ? value.description : typeof nested.description === "string" ? nested.description : "",
    user: value.user === true,
    preset: value.preset ?? null,
  };
  return `entry:${stableJson(identityFields)}`;
}

export function projectCatalogEntries(entries) {
  if (!Array.isArray(entries)) return [];
  const occurrences = new Map();
  return entries.map((entry, fallback) => {
    const baseIdentity = catalogIdentity(entry);
    const occurrence = occurrences.get(baseIdentity) ?? 0;
    occurrences.set(baseIdentity, occurrence + 1);
    return { entry: record(entry), hostIndex: entryIndex(entry, fallback), identity: `${baseIdentity}#${occurrence}` };
  });
}

export function filterCatalogEntries(entries, query = "", scope = "ALL") {
  const normalizedQuery = typeof query === "string" ? query.trim().toLowerCase() : "";
  return (Array.isArray(entries) ? entries : []).filter(({ entry }) => {
    if (scope === "MINE" && entry.user !== true) return false;
    if (!normalizedQuery) return true;
    const nested = record(entry.preset);
    const searchable = [presetEntryName(entry), presetAuthorLabel(entry), entry.description, nested.description]
      .filter((value) => typeof value === "string").join(" ").toLowerCase();
    return searchable.includes(normalizedQuery);
  });
}

export function reconcileCatalogSelection(entries, selectedIdentity) {
  if (!Array.isArray(entries) || entries.length === 0) return null;
  if (typeof selectedIdentity === "string" && entries.some(({ identity }) => identity === selectedIdentity)) return selectedIdentity;
  return selectedIdentity == null ? entries[0].identity : null;
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

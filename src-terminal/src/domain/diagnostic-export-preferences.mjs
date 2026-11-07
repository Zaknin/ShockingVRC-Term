export const DIAGNOSTIC_EXPORT_PREFERENCES_KEY = "shockingvrc.diagnosticsExportPreferences.v1";

export const DEFAULT_DIAGNOSTIC_EXPORT_PREFERENCES = Object.freeze({
  source: "both",
  timeRange: "all",
  startLocal: "",
  endLocal: "",
  openFolderAfterExport: true,
});

export function loadDiagnosticExportPreferences(storage) {
  try {
    const parsed = JSON.parse(storage.getItem(DIAGNOSTIC_EXPORT_PREFERENCES_KEY) ?? "null");
    if (!parsed || typeof parsed !== "object") return { ...DEFAULT_DIAGNOSTIC_EXPORT_PREFERENCES };
    return {
      source: ["terminal", "host", "both"].includes(parsed.source) ? parsed.source : "both",
      timeRange: ["all", "last_1_hour", "last_6_hours", "last_24_hours", "custom"].includes(parsed.timeRange)
        ? parsed.timeRange : "all",
      startLocal: typeof parsed.startLocal === "string" ? parsed.startLocal : "",
      endLocal: typeof parsed.endLocal === "string" ? parsed.endLocal : "",
      openFolderAfterExport: typeof parsed.openFolderAfterExport === "boolean" ? parsed.openFolderAfterExport : true,
    };
  } catch {
    return { ...DEFAULT_DIAGNOSTIC_EXPORT_PREFERENCES };
  }
}

export function saveDiagnosticExportPreferences(storage, preferences) {
  const safe = {
    source: ["terminal", "host", "both"].includes(preferences.source) ? preferences.source : "both",
    timeRange: ["all", "last_1_hour", "last_6_hours", "last_24_hours", "custom"].includes(preferences.timeRange)
      ? preferences.timeRange : "all",
    startLocal: typeof preferences.startLocal === "string" ? preferences.startLocal : "",
    endLocal: typeof preferences.endLocal === "string" ? preferences.endLocal : "",
    openFolderAfterExport: preferences.openFolderAfterExport === true,
  };
  try {
    storage.setItem(DIAGNOSTIC_EXPORT_PREFERENCES_KEY, JSON.stringify(safe));
    return true;
  } catch {
    return false;
  }
}

import test from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_DIAGNOSTIC_EXPORT_PREFERENCES,
  DIAGNOSTIC_EXPORT_PREFERENCES_KEY,
  loadDiagnosticExportPreferences,
  saveDiagnosticExportPreferences,
} from "../src/domain/diagnostic-export-preferences.mjs";

function memoryStorage(initial = {}) {
  const values = new Map(Object.entries(initial));
  return {
    getItem(key) { return values.get(key) ?? null; },
    setItem(key, value) { values.set(key, value); },
  };
}

test("export preferences default to Both, All retained, and opening the folder", () => {
  assert.deepEqual(loadDiagnosticExportPreferences(memoryStorage()), DEFAULT_DIAGNOSTIC_EXPORT_PREFERENCES);
});

test("export source, range, custom fields, and Explorer preference persist between openings", () => {
  const storage = memoryStorage();
  const preferences = {
    source: "host",
    timeRange: "custom",
    startLocal: "2026-10-24T09:30",
    endLocal: "2026-10-24T10:30",
    openFolderAfterExport: false,
  };
  assert.equal(saveDiagnosticExportPreferences(storage, preferences), true);
  assert.deepEqual(loadDiagnosticExportPreferences(storage), preferences);
  assert.ok(storage.getItem(DIAGNOSTIC_EXPORT_PREFERENCES_KEY));
});

test("malformed stored values fall back safely without throwing", () => {
  const storage = memoryStorage({ [DIAGNOSTIC_EXPORT_PREFERENCES_KEY]: "{" });
  assert.deepEqual(loadDiagnosticExportPreferences(storage), DEFAULT_DIAGNOSTIC_EXPORT_PREFERENCES);
});

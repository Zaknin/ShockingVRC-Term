export type DiagnosticExportPreferences = {
  source: "terminal" | "host" | "both";
  timeRange: "all" | "last_1_hour" | "last_6_hours" | "last_24_hours" | "custom";
  startLocal: string;
  endLocal: string;
  openFolderAfterExport: boolean;
};

export const DIAGNOSTIC_EXPORT_PREFERENCES_KEY: string;
export const DEFAULT_DIAGNOSTIC_EXPORT_PREFERENCES: Readonly<DiagnosticExportPreferences>;
export function loadDiagnosticExportPreferences(storage: Pick<Storage, "getItem">): DiagnosticExportPreferences;
export function saveDiagnosticExportPreferences(storage: Pick<Storage, "setItem">, preferences: DiagnosticExportPreferences): boolean;

import type { HostLogLine } from "../protocol/types";

export interface ParsedLogEntry {
  seq: number;
  text: string;
  level: "ERROR" | "WARN" | "INFO" | "DEBUG";
  timestamp: string | null;
}

export function parseLogEntry(line: HostLogLine | string, fallbackSequence?: number): ParsedLogEntry;
export function filterLogEntries(lines: readonly (HostLogLine | string)[], level?: "ALL" | ParsedLogEntry["level"], query?: string): ParsedLogEntry[];

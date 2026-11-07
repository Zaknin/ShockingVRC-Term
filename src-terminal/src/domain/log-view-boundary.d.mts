import type { HostLogLine } from "../protocol/types";

export type LogViewLine = HostLogLine | string;

export interface LogViewWatermark {
  version: 1;
  maxSequence: number | null;
  latestTimestamp: string | null;
  latestTimestampCount: number;
  untimedTextCounts: Record<string, number>;
}

export interface LogViewStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export function createLogViewWatermark(lines: readonly LogViewLine[]): LogViewWatermark;
export function filterAfterLogViewWatermark(lines: readonly LogViewLine[], watermark: LogViewWatermark | null): LogViewLine[];
export function readLogViewWatermark(storage: LogViewStorage | null | undefined, sourceKey: string): LogViewWatermark | null;
export function writeLogViewWatermark(storage: LogViewStorage | null | undefined, sourceKey: string, watermark: LogViewWatermark): boolean;
export function clearLogViewWatermark(storage: LogViewStorage | null | undefined, sourceKey: string): boolean;

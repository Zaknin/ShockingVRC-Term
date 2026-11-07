import type { HostLogLine, HostLogs } from "../protocol/types";

export const HOST_LOG_PAGE_SIZE: number;
export function isCurrentHostLogRequest(requestRevision: number, currentRevision: number): boolean;
export function olderHostLogCursor(firstSequence: number, pageSize?: number): string | null;
export function hasOlderHostLogRecords(lines: HostLogLine[]): boolean;
export function prependOlderHostLogPage(current: HostLogs, older: HostLogs): HostLogs;

/** @typedef {import("../protocol/types").HostLogLine} HostLogLine */
/** @typedef {import("../protocol/types").HostLogs} HostLogs */

export const HOST_LOG_PAGE_SIZE = 100;

/** @param {number} requestRevision @param {number} currentRevision @returns {boolean} */
export function isCurrentHostLogRequest(requestRevision, currentRevision) {
  return Number.isSafeInteger(requestRevision)
    && Number.isSafeInteger(currentRevision)
    && requestRevision === currentRevision;
}

/** @param {number} firstSequence @param {number} [pageSize] @returns {string | null} */
export function olderHostLogCursor(firstSequence, pageSize = HOST_LOG_PAGE_SIZE) {
  if (!Number.isSafeInteger(firstSequence) || firstSequence <= 1 || !Number.isSafeInteger(pageSize) || pageSize < 1) {
    return null;
  }
  return String(Math.max(0, firstSequence - pageSize - 1));
}

/** @param {HostLogLine[]} lines @returns {boolean} */
export function hasOlderHostLogRecords(lines) {
  return Number.isSafeInteger(lines[0]?.seq) && Number(lines[0]?.seq) > 1;
}

/** @param {HostLogs} current @param {HostLogs} older @returns {HostLogs} */
export function prependOlderHostLogPage(current, older) {
  const currentBySequence = new Map(current.lines.map((line) => [line.seq, line]));
  const combined = [
    ...older.lines.filter((line) => !currentBySequence.has(line.seq)),
    ...current.lines,
  ].sort((left, right) => left.seq - right.seq);
  return {
    cursor: older.cursor,
    next_cursor: current.next_cursor,
    lines: combined,
  };
}

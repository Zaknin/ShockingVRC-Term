const timestampPattern = /\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?\b/;

export function parseLogEntry(line, fallbackSequence = 1) {
  const seq = typeof line === "string" ? fallbackSequence : line.seq;
  const text = typeof line === "string" ? line : line.text;
  const taggedLevel = /^\[(ERROR|WARN|INFO|DEBUG|TRACE)\s*\]/i.exec(text)?.[1]?.toUpperCase();
  return {
    seq,
    text,
    level: taggedLevel === "TRACE" ? "DEBUG" : taggedLevel ?? "INFO",
    timestamp: timestampPattern.exec(text)?.[0] ?? null,
  };
}

export function filterLogEntries(lines, level = "ALL", query = "") {
  const term = query.trim().toLowerCase();
  return lines.map((line, index) => parseLogEntry(line, index + 1))
    .filter((line) => (level === "ALL" || line.level === level) && line.text.toLowerCase().includes(term));
}

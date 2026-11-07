import { parseLogEntry } from "./log-format.mjs";

const STORAGE_PREFIX = "shockingvrc.log-view-watermark.v1:";

export function createLogViewWatermark(lines) {
  const parsed = lines.map((line, index) => parseLogEntry(line, index + 1));
  const sequenceValues = lines
    .filter((line) => typeof line !== "string" && Number.isSafeInteger(line.seq))
    .map((line) => line.seq);
  const timestamps = parsed.map((line) => line.timestamp).filter(Boolean).sort();
  const latestTimestamp = timestamps.at(-1) ?? null;
  const latestTimestampCount = latestTimestamp
    ? parsed.filter((line) => line.timestamp === latestTimestamp).length
    : 0;
  const untimedTextCounts = {};
  for (let index = 0; index < parsed.length; index += 1) {
    const line = parsed[index];
    const raw = lines[index];
    if (line.timestamp || (typeof raw !== "string" && Number.isSafeInteger(raw.seq))) continue;
    untimedTextCounts[line.text] = (untimedTextCounts[line.text] ?? 0) + 1;
  }
  return {
    version: 1,
    maxSequence: sequenceValues.length ? Math.max(...sequenceValues) : null,
    latestTimestamp,
    latestTimestampCount,
    untimedTextCounts,
  };
}

export function filterAfterLogViewWatermark(lines, watermark) {
  if (!watermark || watermark.version !== 1) return [...lines];
  const seenLatestTimestamp = new Map();
  const seenUntimedText = new Map();
  return lines.filter((raw, index) => {
    const line = parseLogEntry(raw, index + 1);
    const sequence = typeof raw === "string" ? null : raw.seq;
    if (Number.isSafeInteger(sequence) && Number.isSafeInteger(watermark.maxSequence)) {
      return sequence > watermark.maxSequence || Boolean(line.timestamp && watermark.latestTimestamp && line.timestamp > watermark.latestTimestamp);
    }
    if (line.timestamp && watermark.latestTimestamp) {
      if (line.timestamp > watermark.latestTimestamp) return true;
      if (line.timestamp < watermark.latestTimestamp) return false;
      const count = (seenLatestTimestamp.get(line.timestamp) ?? 0) + 1;
      seenLatestTimestamp.set(line.timestamp, count);
      return count > watermark.latestTimestampCount;
    }
    const count = (seenUntimedText.get(line.text) ?? 0) + 1;
    seenUntimedText.set(line.text, count);
    return count > (watermark.untimedTextCounts?.[line.text] ?? 0);
  });
}

export function readLogViewWatermark(storage, sourceKey) {
  try {
    const value = storage?.getItem(`${STORAGE_PREFIX}${sourceKey}`);
    return value ? JSON.parse(value) : null;
  } catch {
    return null;
  }
}

export function writeLogViewWatermark(storage, sourceKey, watermark) {
  try {
    storage?.setItem(`${STORAGE_PREFIX}${sourceKey}`, JSON.stringify(watermark));
    return true;
  } catch {
    return false;
  }
}

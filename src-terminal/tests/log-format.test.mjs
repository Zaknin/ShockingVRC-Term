import test from "node:test";
import assert from "node:assert/strict";
import { filterLogEntries, parseLogEntry } from "../src/domain/log-format.mjs";

test("log projection exposes Host timestamp and severity embedded in the original line", () => {
  const parsed = parseLogEntry({ seq: 14, text: "[WARN ] 2026-10-16T19:33:08.649 [ble] scan error" });
  assert.equal(parsed.seq, 14);
  assert.equal(parsed.level, "WARN");
  assert.equal(parsed.timestamp, "2026-10-16T19:33:08.649");
  assert.equal(parsed.text, "[WARN ] 2026-10-16T19:33:08.649 [ble] scan error");
});

test("log filtering handles levels and search without rewriting source lines", () => {
  const lines = [
    { seq: 1, text: "[ERROR] socket closed" },
    { seq: 2, text: "[INFO ] socket open" },
    { seq: 3, text: "[DEBUG] selected candidate pair" },
  ];
  assert.deepEqual(filterLogEntries(lines, "ERROR", "socket").map((line) => line.seq), [1]);
  assert.deepEqual(filterLogEntries(lines, "ALL", "candidate").map((line) => line.seq), [3]);
});

test("log projection leaves absent timestamps absent instead of inventing event time", () => {
  assert.equal(parseLogEntry("transport failed").timestamp, null);
});

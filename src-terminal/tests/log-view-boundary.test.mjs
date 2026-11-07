import test from "node:test";
import assert from "node:assert/strict";
import {
  createLogViewWatermark,
  filterAfterLogViewWatermark,
  readLogViewWatermark,
  writeLogViewWatermark,
} from "../src/domain/log-view-boundary.mjs";

test("Host clear-view watermark hides replayed pages and reveals higher sequence lines", () => {
  const page = [
    { seq: 40, text: "[INFO] 2026-10-20T10:00:00Z old A" },
    { seq: 41, text: "[INFO] 2026-10-20T10:00:01Z old B" },
  ];
  const watermark = createLogViewWatermark(page);
  assert.deepEqual(filterAfterLogViewWatermark(page, watermark), []);
  assert.deepEqual(
    filterAfterLogViewWatermark([...page, { seq: 42, text: "[INFO] 2026-10-20T10:00:02Z new" }], watermark)
      .map((line) => line.seq),
    [42],
  );
});

test("Host restart can reveal new timestamped events after its sequence resets", () => {
  const watermark = createLogViewWatermark([
    { seq: 900, text: "[INFO] 2026-10-20T10:00:00Z before restart" },
  ]);
  assert.deepEqual(
    filterAfterLogViewWatermark([
      { seq: 1, text: "[INFO] 2026-10-20T10:01:00Z after restart" },
    ], watermark).map((line) => line.seq),
    [1],
  );
});

test("Terminal watermark hides stored timestamps while allowing later and same-timestamp appended lines", () => {
  const before = [
    "[INFO] 2026-10-20T10:00:00.000Z old A",
    "[INFO] 2026-10-20T10:00:01.000Z old B",
  ];
  const watermark = createLogViewWatermark(before);
  const current = [
    ...before,
    "[WARN] 2026-10-20T10:00:01.000Z appended at boundary",
    "[INFO] 2026-10-20T10:00:02.000Z new",
  ];
  assert.deepEqual(filterAfterLogViewWatermark(current, watermark), current.slice(2));
});

test("watermarks persist by source key across component remounts", () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  const watermark = createLogViewWatermark(["[INFO] 2026-10-20T10:00:00Z old"]);
  assert.equal(writeLogViewWatermark(storage, "host:profile-a:host-a", watermark), true);
  assert.deepEqual(readLogViewWatermark(storage, "host:profile-a:host-a"), watermark);
  assert.equal(readLogViewWatermark(storage, "host:profile-b:host-b"), null);
});

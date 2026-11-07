import test from "node:test";
import assert from "node:assert/strict";
import { formatTimestamp } from "../src/domain/timestamp-format.mjs";

test("formats Unix seconds as a local timestamp", () => {
  const seconds = "1792231932";
  assert.equal(formatTimestamp(seconds), new Date(Number(seconds) * 1000).toLocaleString());
});

test("formats Unix milliseconds as a local timestamp", () => {
  const milliseconds = "1792231932000";
  assert.equal(formatTimestamp(milliseconds), new Date(Number(milliseconds)).toLocaleString());
});

test("formats ISO timestamps and preserves invalid text", () => {
  const iso = "2026-10-17T12:00:00.000Z";
  assert.equal(formatTimestamp(iso), new Date(iso).toLocaleString());
  assert.equal(formatTimestamp("not-a-timestamp"), "not-a-timestamp");
  assert.equal(formatTimestamp(null), "—");
});

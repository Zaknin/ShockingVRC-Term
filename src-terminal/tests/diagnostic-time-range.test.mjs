import test from "node:test";
import assert from "node:assert/strict";
import { resolveLocalDateTime, validateCustomRange } from "../src/domain/diagnostic-time-range.mjs";

test("custom local time resolves using its named timezone and converts to UTC", () => {
  const resolved = resolveLocalDateTime("2026-03-08T03:30", "America/New_York");
  assert.equal(resolved.ok, true);
  assert.equal(resolved.utc, "2026-03-08T07:30:00.000Z");
  assert.equal(resolved.offsetMinutes, -240);
});

test("nonexistent and ambiguous daylight-saving wall times are rejected", () => {
  assert.match(resolveLocalDateTime("2026-03-08T02:30", "America/New_York").error, /does not exist/i);
  assert.match(resolveLocalDateTime("2026-11-01T01:30", "America/New_York").error, /ambiguous/i);
});

test("custom ranges span midnight and enforce start-inclusive end-exclusive order", () => {
  const result = validateCustomRange("2026-10-24T23:30", "2026-10-25T00:15", "Asia/Baku");
  assert.equal(result.ok, true);
  assert.equal(result.startUtc, "2026-10-24T19:30:00.000Z");
  assert.equal(result.endUtc, "2026-10-24T20:15:00.000Z");
  assert.equal(result.boundary, "[start_utc,end_utc)");
  assert.match(validateCustomRange("2026-10-25T00:15", "2026-10-24T23:30", "Asia/Baku").error, /earlier/i);
});

test("invalid calendar input and unknown timezones return actionable errors", () => {
  assert.equal(resolveLocalDateTime("2026-02-30T12:00", "UTC").ok, false);
  assert.match(resolveLocalDateTime("2026-10-24T12:00", "No/Such_Zone").error, /timezone/i);
});

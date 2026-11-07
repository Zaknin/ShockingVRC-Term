import test from "node:test";
import assert from "node:assert/strict";
import { hasOlderHostLogRecords, isCurrentHostLogRequest, olderHostLogCursor, prependOlderHostLogPage } from "../src/domain/host-log-pages.mjs";

test("older Host log cursor requests the immediately preceding page", () => {
  assert.equal(olderHostLogCursor(1), null);
  assert.equal(olderHostLogCursor(100), "0");
  assert.equal(olderHostLogCursor(101), "0");
  assert.equal(olderHostLogCursor(102), "1");
  assert.equal(olderHostLogCursor(1_102), "1001");
});

test("older Host log pages prepend in sequence order without duplicates", () => {
  const current = {
    cursor: "1099",
    next_cursor: null,
    lines: [
      { seq: 1_100, text: "current 1100" },
      { seq: 1_101, text: "current 1101" },
    ],
  };
  const older = {
    cursor: "1097",
    next_cursor: "1099",
    lines: [
      { seq: 1_098, text: "older 1098" },
      { seq: 1_099, text: "older 1099" },
      { seq: 1_100, text: "duplicate 1100" },
    ],
  };
  assert.deepEqual(prependOlderHostLogPage(current, older), {
    cursor: "1097",
    next_cursor: null,
    lines: [
      { seq: 1_098, text: "older 1098" },
      { seq: 1_099, text: "older 1099" },
      { seq: 1_100, text: "current 1100" },
      { seq: 1_101, text: "current 1101" },
    ],
  });
});

test("older-history affordance ends at the first retained sequence", () => {
  assert.equal(hasOlderHostLogRecords([{ seq: 2, text: "second" }]), true);
  assert.equal(hasOlderHostLogRecords([{ seq: 1, text: "first" }]), false);
  assert.equal(hasOlderHostLogRecords([]), false);
});

test("Host/profile changes fence responses from an earlier log request", () => {
  const requestRevision = 4;
  const revisionAfterHostProfileChange = requestRevision + 1;
  assert.equal(isCurrentHostLogRequest(requestRevision, requestRevision), true);
  assert.equal(isCurrentHostLogRequest(requestRevision, revisionAfterHostProfileChange), false);
});

test("a newer log request fences a delayed response from an older page request", () => {
  const olderPageRevision = 8;
  const latestRequestRevision = 9;
  assert.equal(isCurrentHostLogRequest(olderPageRevision, latestRequestRevision), false);
  assert.equal(isCurrentHostLogRequest(latestRequestRevision, latestRequestRevision), true);
});

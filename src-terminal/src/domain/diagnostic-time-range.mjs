const LOCAL_VALUE = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/;
const HOUR_MS = 60 * 60 * 1000;

function localParts(utcMilliseconds, timeZone) {
  const formatter = new Intl.DateTimeFormat("en-GB", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  });
  const values = Object.fromEntries(formatter.formatToParts(new Date(utcMilliseconds)).map(({ type, value }) => [type, value]));
  return {
    year: Number(values.year),
    month: Number(values.month),
    day: Number(values.day),
    hour: Number(values.hour),
    minute: Number(values.minute),
  };
}

function dateTimeKey(parts) {
  return `${String(parts.year).padStart(4, "0")}-${String(parts.month).padStart(2, "0")}-${String(parts.day).padStart(2, "0")}T${String(parts.hour).padStart(2, "0")}:${String(parts.minute).padStart(2, "0")}`;
}

function wallClockEpoch(parts) {
  const value = new Date(0);
  value.setUTCFullYear(parts.year, parts.month - 1, parts.day);
  value.setUTCHours(parts.hour, parts.minute, 0, 0);
  return value.getTime();
}

function validCalendarParts(parts) {
  if (parts.year < 1900 || parts.year > 9999 || parts.month < 1 || parts.month > 12 || parts.hour > 23 || parts.minute > 59) {
    return false;
  }
  const value = new Date(0);
  value.setUTCFullYear(parts.year, parts.month - 1, parts.day);
  value.setUTCHours(parts.hour, parts.minute, 0, 0);
  return value.getUTCFullYear() === parts.year
    && value.getUTCMonth() === parts.month - 1
    && value.getUTCDate() === parts.day;
}

function timezoneOffsetAt(utcMilliseconds, timeZone) {
  const minute = Math.floor(utcMilliseconds / 60_000) * 60_000;
  const parts = localParts(minute, timeZone);
  return (wallClockEpoch(parts) - minute) / 60_000;
}

/** Resolve a datetime-local wall time without silently guessing at DST gaps or folds. */
export function resolveLocalDateTime(value, timeZone) {
  const match = LOCAL_VALUE.exec(value ?? "");
  if (!match) return { ok: false, error: "Enter a complete local date and time." };
  const parts = {
    year: Number(match[1]), month: Number(match[2]), day: Number(match[3]),
    hour: Number(match[4]), minute: Number(match[5]),
  };
  if (!validCalendarParts(parts)) return { ok: false, error: "Enter a valid calendar date and time." };
  if (!timeZone?.trim()) return { ok: false, error: "The Terminal timezone is unavailable." };

  let offsets;
  try {
    const approximate = wallClockEpoch(parts);
    offsets = new Set([-36, -24, -12, 0, 12, 24, 36].map((hours) => timezoneOffsetAt(approximate + hours * HOUR_MS, timeZone)));
  } catch {
    return { ok: false, error: "The Terminal timezone is invalid or unavailable." };
  }

  const key = dateTimeKey(parts);
  const candidates = [...offsets]
    .map((offsetMinutes) => ({ utcMilliseconds: wallClockEpoch(parts) - offsetMinutes * 60_000, offsetMinutes }))
    .filter(({ utcMilliseconds }) => dateTimeKey(localParts(utcMilliseconds, timeZone)) === key)
    .filter((candidate, index, values) => values.findIndex((other) => other.utcMilliseconds === candidate.utcMilliseconds) === index);
  if (candidates.length === 0) return { ok: false, error: "This local date and time does not exist because of a daylight-saving transition." };
  if (candidates.length > 1) return { ok: false, error: "This local date and time is ambiguous because of a daylight-saving transition." };
  const candidate = candidates[0];
  return {
    ok: true,
    utc: new Date(candidate.utcMilliseconds).toISOString(),
    offsetMinutes: candidate.offsetMinutes,
  };
}

export function validateCustomRange(startLocal, endLocal, timeZone) {
  const start = resolveLocalDateTime(startLocal, timeZone);
  if (!start.ok) return { ok: false, error: `From: ${start.error}` };
  const end = resolveLocalDateTime(endLocal, timeZone);
  if (!end.ok) return { ok: false, error: `To: ${end.error}` };
  if (Date.parse(start.utc) >= Date.parse(end.utc)) {
    return { ok: false, error: "From must be earlier than To." };
  }
  return {
    ok: true,
    startUtc: start.utc,
    endUtc: end.utc,
    boundary: "[start_utc,end_utc)",
  };
}

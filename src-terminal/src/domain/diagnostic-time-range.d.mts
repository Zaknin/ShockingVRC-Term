export type LocalDateTimeResolution =
  | { ok: true; utc: string; offsetMinutes: number }
  | { ok: false; error: string };

export type CustomRangeValidation =
  | { ok: true; startUtc: string; endUtc: string; boundary: "[start_utc,end_utc)" }
  | { ok: false; error: string };

export function resolveLocalDateTime(value: string, timeZone: string): LocalDateTimeResolution;
export function validateCustomRange(startLocal: string, endLocal: string, timeZone: string): CustomRangeValidation;

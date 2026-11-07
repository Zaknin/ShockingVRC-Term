export function formatTimestamp(value) {
  if (value === null || value === undefined || value === "") return "—";

  const text = String(value);
  const epochMilliseconds = /^\d{10}$/.test(text)
    ? Number(text) * 1000
    : /^\d{13}$/.test(text)
      ? Number(text)
      : Number.NaN;
  const timestamp = Number.isFinite(epochMilliseconds) ? epochMilliseconds : Date.parse(text);

  return Number.isFinite(timestamp) ? new Date(timestamp).toLocaleString() : text;
}

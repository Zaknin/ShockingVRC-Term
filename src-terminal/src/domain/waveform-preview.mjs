import { formatNumber, rawToHz } from "./preview-math.mjs";

export function buildWaveformSegments(frequency, intensity) {
  return Array.from({ length: 4 }, (_, index) => {
    const current = intensity[index];
    const readout = Number.isFinite(current) ? current : null;
    return {
      index,
      basis: "25%",
      strength: readout == null ? 0 : Math.max(0, Math.min(100, readout)),
      hz: formatNumber(rawToHz(Number.isFinite(frequency[index]) ? frequency[index] : 10), 0),
      intensityLabel: readout == null ? "—" : `${formatNumber(readout, 2)}%`,
    };
  });
}

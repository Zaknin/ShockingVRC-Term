export interface WaveformSegmentPreview {
  index: number;
  basis: string;
  strength: number;
  hz: string;
  intensityLabel: string;
}

export function buildWaveformSegments(frequency: readonly number[], intensity: readonly number[]): WaveformSegmentPreview[];

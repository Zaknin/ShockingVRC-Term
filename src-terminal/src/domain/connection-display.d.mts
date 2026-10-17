export type ConnectionIndicator = "ok" | "warn" | "error" | "idle";

export function connectionStatusLabel(phase: string): string;
export function connectionIndicator(phase: string): ConnectionIndicator;
export function transportLabel(transport: string | null, candidatePair: string | null, phase: string): string;

export function canBeginConnection(phase: string): boolean;
export function canManuallyReconnect(phase: string): boolean;
export function clearsSnapshotForPhase(phase: string): boolean;
export const DEFAULT_TERMINAL_SECTION: string;
export function sectionAfterConnectionUpdate(currentSection: string, phase: string): string;

export function canBeginConnection(phase: string): boolean;
export function canStartProfileConnection(phase: string, hasPendingPairingInvitation: boolean): boolean;
export function canManuallyReconnect(phase: string): boolean;
export function clearsSnapshotForPhase(phase: string): boolean;
export const DEFAULT_TERMINAL_SECTION: string;
export function sectionAfterConnectionUpdate(currentSection: string, phase: string): string;
export function selectedProfileIdAfterListChange(currentId: string, savedProfileIds: string[], pendingProfileId: string | null): string;
export function canApplyConnectionResult(attemptRevision: number, currentRevision: number, attemptProfileId: string, pendingProfileId: string | null): boolean;
export function shouldAcceptConnectionEvent(current: any, incoming: any, highestSessionGeneration?: number): boolean;

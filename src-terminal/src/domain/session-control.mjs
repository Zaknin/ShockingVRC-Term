const reconnectablePhases = new Set([
  "user_disconnected",
  "host_kicked",
  "host_unavailable",
  "transport_lost",
]);
const activePhases = new Set(["connecting", "identity_verified", "online", "reconnecting"]);
const terminalEndPhases = new Set(["user_disconnected", "host_kicked", "session_revoked", "host_unavailable", "transport_lost"]);

export const DEFAULT_TERMINAL_SECTION = "Connect";

export function canBeginConnection(phase) {
  return !activePhases.has(phase);
}

export function canStartProfileConnection(phase, hasPendingPairingInvitation) {
  return canBeginConnection(phase)
    && (phase !== "session_revoked" || hasPendingPairingInvitation);
}

export function canManuallyReconnect(phase) {
  return reconnectablePhases.has(phase);
}

export function clearsSnapshotForPhase(phase) {
  return phase !== "online" && phase !== "reconnecting";
}

export function sectionAfterConnectionUpdate(currentSection, phase) {
  return phase === "online" || terminalEndPhases.has(phase) ? "Status" : currentSection;
}

export function selectedProfileIdAfterListChange(currentId, savedProfileIds, pendingProfileId) {
  if (!currentId || currentId === pendingProfileId || savedProfileIds.includes(currentId)) return currentId;
  return "";
}

export function canApplyConnectionResult(attemptRevision, currentRevision, attemptProfileId, pendingProfileId) {
  return attemptRevision === currentRevision
    && (pendingProfileId === null || attemptProfileId === pendingProfileId);
}

const reconnectablePhases = new Set([
  "user_disconnected",
  "host_kicked",
  "host_unavailable",
  "transport_lost",
]);
const activePhases = new Set(["connecting", "identity_verified", "online", "reconnecting"]);
const terminalEndPhases = new Set(["user_disconnected", "host_kicked", "session_revoked", "host_unavailable", "transport_lost"]);
const activeConnectionPhases = new Set(["connecting", "identity_verified", "online", "reconnecting"]);

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

export function shouldAcceptConnectionEvent(current, incoming, highestSessionGeneration = 0) {
  const incomingGeneration = Number.isSafeInteger(incoming?.session_generation) ? incoming.session_generation : null;
  const currentGeneration = Number.isSafeInteger(current?.session_generation) ? current.session_generation : null;
  const incomingActive = activeConnectionPhases.has(incoming?.phase);
  const currentActive = activeConnectionPhases.has(current?.phase);

  if (incomingActive && incomingGeneration === null) return false;
  if (incomingGeneration !== null && incomingGeneration < highestSessionGeneration) return false;
  if (currentGeneration !== null && incomingGeneration !== null && incomingGeneration < currentGeneration) return false;
  if (!currentActive && incomingActive) {
    if (currentGeneration !== null && (incomingGeneration === null || incomingGeneration <= currentGeneration)) return false;
    if (currentGeneration === null && (incomingGeneration === null || incomingGeneration < highestSessionGeneration)) return false;
  }
  if (current?.server_epoch === incoming?.server_epoch
      && Number.isSafeInteger(current?.transport_generation)
      && Number.isSafeInteger(incoming?.transport_generation)
      && incoming.transport_generation < current.transport_generation) return false;
  return true;
}

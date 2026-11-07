const labels = {
  disconnected: "DISCONNECTED",
  connecting: "VERIFYING HOST",
  identity_verified: "IDENTITY VERIFIED",
  online: "ONLINE",
  reconnecting: "RECONNECTING",
  revoked: "SESSION REVOKED",
};

export function connectionStatusLabel(phase) {
  return labels[phase] ?? String(phase).toUpperCase();
}

export function connectionIndicator(phase) {
  if (phase === "online") return "ok";
  if (phase === "connecting" || phase === "reconnecting") return "warn";
  if (phase === "revoked") return "error";
  return "idle";
}

export function transportLabel(transport, candidatePair, phase) {
  if (phase === "reconnecting") return "Reconnecting";
  if (phase === "connecting") return "Connecting";
  if (transport === "direct_wss") return "Direct";
  if (transport === "webrtc_datachannel") {
    if (!candidatePair) return "Path unknown";
    return /\b(?:TURN|RELAY)\b/i.test(candidatePair) ? "Relay" : "P2P";
  }
  if (phase === "identity_verified") return "Ready";
  if (phase === "online") return "Path unknown";
  return "Not connected";
}

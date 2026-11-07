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
  if (transport === "direct_wss") return "DIRECT WSS";
  if (transport === "webrtc_datachannel") {
    if (!candidatePair) return "ICE PATH UNKNOWN";
    return candidatePair.toUpperCase().includes("RELAY") ? "TURN RELAY" : "ICE P2P";
  }
  if (phase === "identity_verified") return "HOST IDENTITY VERIFIED";
  if (phase === "connecting") return "TRANSPORT NEGOTIATING";
  if (phase === "reconnecting") return "RECONNECTING · TRANSPORT PENDING";
  return "NO TRANSPORT SELECTED";
}

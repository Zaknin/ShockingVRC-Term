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
    if (/\bRELAY\/UDP\b/i.test(candidatePair)) return "TURN relay · UDP";
    if (/\bRELAY\/TCP\b/i.test(candidatePair)) return "TURN relay · TCP";
    const protocols = [...candidatePair.matchAll(/\b(?:LOCAL|REMOTE)\s+(?:HOST|SRFLX|PRFLX)\/(UDP|TCP)\b/gi)]
      .map((match) => match[1].toUpperCase());
    const protocol = protocols.length && protocols.every((value) => value === protocols[0]) ? ` · ${protocols[0]}` : "";
    return `ICE peer path${protocol}`;
  }
  if (phase === "identity_verified") return "Ready";
  if (phase === "online") return "Path unknown";
  return "Not connected";
}

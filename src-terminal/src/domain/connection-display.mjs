const labels = {
  disconnected: "DISCONNECTED",
  connecting: "VERIFYING HOST",
  identity_verified: "IDENTITY VERIFIED",
  online: "ONLINE",
  reconnecting: "RECONNECTING",
  revoked: "SESSION REVOKED",
  user_disconnected: "DISCONNECTED BY YOU",
  host_kicked: "HOST ENDED SESSION",
  session_revoked: "ACCESS REVOKED",
  host_unavailable: "HOST UNAVAILABLE",
  transport_lost: "CONNECTION LOST",
};

const emptyStateGuidanceByPhase = {
  user_disconnected: "You disconnected from the Host. Choose the saved Host and reconnect when ready.",
  host_kicked: "The Host ended this session. Choose the saved Host to reconnect; pairing remains valid.",
  session_revoked: "The Host revoked this Terminal. Open a new Host invitation to pair this profile again.",
};

export function emptyConnectionGuidance(phase) {
  return emptyStateGuidanceByPhase[phase]
    ?? "Open Connect, confirm the saved Host fingerprint, and sign in to read this section.";
}

export function connectionStatusLabel(phase) {
  return labels[phase] ?? String(phase).toUpperCase();
}

export function connectionIndicator(phase) {
  if (phase === "online") return "ok";
  if (phase === "connecting" || phase === "reconnecting") return "warn";
  if (["revoked", "host_kicked", "session_revoked", "host_unavailable", "transport_lost", "user_disconnected"].includes(phase)) return "error";
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

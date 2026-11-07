export function telemetryBarPresentation(previous, value, smooth = false) {
  const clamp = (input) => input == null ? null : Math.max(0, Math.min(1, input));
  const next = clamp(value);
  const prior = clamp(previous);
  const animate = smooth && prior != null && prior > 0 && next != null && next > 0;
  return { value: next, transitionMs: animate ? 80 : 0 };
}

export function telemetryResetIdentity(connection, snapshot) {
  const contactIds = (Array.isArray(snapshot?.avatar_zones) ? snapshot.avatar_zones : [])
    .map((zone) => `${zone.zone_type}:${zone.id}`)
    .sort()
    .join(",");
  const activeDevice = snapshot?.devices?.[snapshot.active_device]?.mac ?? "none";
  return `${connection?.session_generation ?? "none"}:${connection?.server_epoch ?? "none"}:${connection?.transport_generation ?? "none"}:${activeDevice}:${contactIds}`;
}

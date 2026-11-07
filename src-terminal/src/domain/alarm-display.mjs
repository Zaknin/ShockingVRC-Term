export function alarmDisplayState(config, status) {
  const phase = typeof status?.phase === "string" ? status.phase.toLowerCase() : "";
  if (phase === "ringing") return "RINGING";
  if (phase === "snoozed") return "SNOOZED";
  if (phase === "idle" && config?.enabled === true) return "ARMED";
  if (phase === "idle" && config?.enabled === false) return "OFF / DISARMED";
  return "NOT REPORTED";
}

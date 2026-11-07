export function displayHostName({ localAlias, hostDisplayName, serverId }) {
  const alias = String(localAlias ?? "").trim();
  const remoteName = String(hostDisplayName ?? "").trim();
  const defaultAlias = serverId ? `Host ${String(serverId).slice(0, 8)}` : "";
  if (remoteName && (!alias || alias === defaultAlias)) return remoteName;
  return alias || remoteName || "—";
}

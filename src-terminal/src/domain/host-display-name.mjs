export function displayHostName({ localAlias, hasLocalAlias, hostDisplayName, serverId }) {
  const alias = String(localAlias ?? "").trim();
  const remoteName = String(hostDisplayName ?? "").trim();
  const defaultAlias = serverId ? `Host ${String(serverId).slice(0, 8)}` : "";
  const aliasIsExplicit = hasLocalAlias === true || (hasLocalAlias == null && alias && alias !== defaultAlias);
  if (aliasIsExplicit && alias) return alias;
  if (remoteName) return remoteName;
  return defaultAlias || (aliasIsExplicit ? alias : "—");
}

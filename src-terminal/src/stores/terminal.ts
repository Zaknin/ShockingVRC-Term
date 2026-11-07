import { Channel, invoke } from "@tauri-apps/api/core";
import { defineStore } from "pinia";
import { computed, nextTick, ref } from "vue";
import { canApplyConnectionResult, clearsSnapshotForPhase, shouldAcceptConnectionEvent } from "../domain/session-control.mjs";
import { hasOlderHostLogRecords, HOST_LOG_PAGE_SIZE, isCurrentHostLogRequest, olderHostLogCursor, prependOlderHostLogPage } from "../domain/host-log-pages.mjs";
import {
  CONFIGURATION_MUTATION_TIMEOUT_MS,
  acceptConfigurationSnapshot,
  beginConfigurationMutation,
  configurationMutationDescriptors,
  createConfigurationSyncState,
  expireConfigurationMutations,
  markConfigurationCommandQueued,
  markConfigurationCommandRejected,
  markConfigurationOutcomeUnknown,
  reconcileConfigurationSession,
  sameSnapshotContext,
  type ConfigurationMutation,
  type SnapshotContext,
  type SnapshotCursor,
} from "../domain/configuration-mutations.mjs";
import type { Capabilities, CommandAck, ConnectionState, HostCatalog, HostIdentity, HostLogs, HostProfile, HostSnapshot, TerminalDiagnostics, UiEvent } from "../protocol/types";
import type { AllowedOperation } from "../protocol/operations";

const disconnected: ConnectionState = {
  phase: "disconnected",
  transport: null,
  ice_candidate_pair: null,
  ice_servers_configured: null,
  rendezvous_stun_received: null,
  turn_credentials_received: null,
  local_candidates_gathered: null,
  profile_id: null,
  host_label: null,
  fingerprint: null,
  server_id: null,
  last_heartbeat: null,
  message: null,
  session_generation: null,
  server_epoch: null,
  transport_generation: null,
};

export const useTerminalStore = defineStore("terminal", () => {
  const profiles = ref<HostProfile[]>([]);
  const connection = ref<ConnectionState>({ ...disconnected });
  const snapshot = ref<HostSnapshot | null>(null);
  const snapshotContext = ref<(SnapshotContext & { seq: number }) | null>(null);
  const configurationSync = ref(createConfigurationSyncState());
  const snapshotReceivedAt = ref<number | null>(null);
  const capabilities = ref<Capabilities | null>(null);
  const catalogs = ref<Record<string, HostCatalog>>({});
  const logs = ref<HostLogs | null>(null);
  const hostLogLoadState = ref<"idle" | "loading" | "loaded" | "error">("idle");
  const hostLogLoadError = ref("");
  const hostLogHasOlder = ref(false);
  const terminalDiagnostics = ref<TerminalDiagnostics | null>(null);
  const error = ref("");
  const verifiedIdentity = ref<HostIdentity | null>(null);
  const lastSessionEndPhase = ref<string | null>(null);
  const pendingPairingProfile = ref<HostProfile | null>(null);
  const pendingPairingProfileId = ref<string | null>(null);
  const pairingRequestRevision = ref(0);
  const connected = computed(() => connection.value.phase === "online");
  const hostNameCacheOperations = new Map<string, Promise<void>>();
  let profileLoadRevision = 0;
  let invitationOpenRevision = 0;
  let hostLogRequestRevision = 0;
  let configurationOperationId = 0;
  let highestSessionGeneration = 0;
  let latestFrameSeq = 0;
  let renderWindowStartedAt = performance.now();
  let renderSamples = 0;
  let renderTotalMs = 0;
  let renderMaxMs = 0;
  let renderFlushPending = false;
  const configurationMutationTimers = new Map<number, ReturnType<typeof setTimeout>>();

  function activeSnapshotContext(value: ConnectionState): SnapshotContext | null {
    if (value.session_generation == null || !value.server_epoch || value.transport_generation == null) return null;
    return {
      session_generation: value.session_generation,
      server_epoch: value.server_epoch,
      transport_generation: value.transport_generation,
    };
  }

  function clearMutationTimer(operationId: number) {
    const timer = configurationMutationTimers.get(operationId);
    if (timer !== undefined) clearTimeout(timer);
    configurationMutationTimers.delete(operationId);
  }

  function scheduleMutationCleanup(operationId: number) {
    clearMutationTimer(operationId);
    configurationMutationTimers.set(operationId, setTimeout(() => {
      const mutations = { ...configurationSync.value.mutations };
      let changed = false;
      for (const [key, mutation] of Object.entries(mutations)) {
        if (mutation.operationId === operationId && mutation.status !== "pending") {
          delete mutations[key];
          changed = true;
        }
      }
      if (changed) configurationSync.value = { ...configurationSync.value, mutations };
      configurationMutationTimers.delete(operationId);
    }, 4_500));
  }

  function settleChangedMutations(previous: typeof configurationSync.value) {
    for (const [key, mutation] of Object.entries(configurationSync.value.mutations)) {
      if (previous.mutations[key]?.status === "pending" && mutation.status !== "pending") {
        scheduleMutationCleanup(mutation.operationId);
      }
    }
  }

  function scheduleMutationTimeout(operationId: number) {
    clearMutationTimer(operationId);
    configurationMutationTimers.set(operationId, setTimeout(() => {
      const previous = configurationSync.value;
      configurationSync.value = expireConfigurationMutations(previous, Date.now());
      settleChangedMutations(previous);
      if (Object.values(configurationSync.value.mutations).some((value) => value.operationId === operationId && value.status === "unknown")) {
        scheduleMutationCleanup(operationId);
      } else {
        configurationMutationTimers.delete(operationId);
      }
    }, CONFIGURATION_MUTATION_TIMEOUT_MS));
  }

  function configurationMutation(key: string): ConfigurationMutation | undefined {
    return configurationSync.value.mutations[key];
  }

  function recordTelemetryRender(durationMs: number) {
    renderSamples = Math.min(5_000, renderSamples + 1);
    renderTotalMs += Math.max(0, durationMs);
    renderMaxMs = Math.max(renderMaxMs, durationMs);
    if (performance.now() - renderWindowStartedAt < 5_000 || renderFlushPending || renderSamples === 0) return;
    const batch = { samples: renderSamples, totalMs: Math.round(renderTotalMs), maxMs: Math.ceil(renderMaxMs) };
    renderSamples = 0;
    renderTotalMs = 0;
    renderMaxMs = 0;
    renderWindowStartedAt = performance.now();
    renderFlushPending = true;
    void invoke("record_telemetry_render_metrics", batch)
      .catch(() => undefined)
      .finally(() => { renderFlushPending = false; });
  }

  async function onEvent(event: UiEvent) {
    switch (event.kind) {
      case "connection": {
        const previousHostLogKey = `${connection.value.profile_id ?? ""}:${connection.value.server_id ?? ""}`;
        const nextHostLogKey = `${event.data.profile_id ?? ""}:${event.data.server_id ?? ""}`;
        const oldContext = activeSnapshotContext(connection.value);
        const nextContext = activeSnapshotContext(event.data);
        if (!shouldAcceptConnectionEvent(connection.value, event.data, highestSessionGeneration)) return;
        if (event.data.session_generation != null) highestSessionGeneration = Math.max(highestSessionGeneration, event.data.session_generation);
        if (previousHostLogKey !== nextHostLogKey) {
          hostLogRequestRevision++;
          logs.value = null;
          hostLogLoadState.value = "idle";
          hostLogLoadError.value = "";
          hostLogHasOlder.value = false;
        }
        const contextChanged = !sameSnapshotContext(oldContext, nextContext);
        connection.value = event.data;
        if (contextChanged || clearsSnapshotForPhase(event.data.phase)) {
          const previous = configurationSync.value;
          configurationSync.value = reconcileConfigurationSession(previous,
            event.data.phase === "online" || event.data.phase === "reconnecting" ? nextContext : null,
            Date.now());
          settleChangedMutations(previous);
          snapshot.value = null;
          snapshotContext.value = null;
          latestFrameSeq = 0;
        }
        if (["user_disconnected", "host_kicked", "session_revoked"].includes(event.data.phase)) {
          lastSessionEndPhase.value = event.data.phase;
        } else if (event.data.phase === "online") {
          lastSessionEndPhase.value = null;
        }
        if (clearsSnapshotForPhase(event.data.phase)) {
          snapshot.value = null;
          snapshotContext.value = null;
          snapshotReceivedAt.value = null;
          capabilities.value = null;
          catalogs.value = {};
          logs.value = null;
          latestFrameSeq = 0;
        }
        break;
      }
      case "pairing_invitation": {
        invitationOpenRevision += 1;
        const profile = event.data;
        pendingPairingProfile.value = profile;
        pendingPairingProfileId.value = profile.id;
        pairingRequestRevision.value += 1;
        break;
      }
      case "snapshot":
        if (connection.value.phase === "online" || connection.value.phase === "reconnecting") {
          const active = activeSnapshotContext(connection.value);
          if (!active) return;
          if (event.data.seq <= latestFrameSeq) return;
          const previous = configurationSync.value;
          const result = acceptConfigurationSnapshot(previous, event.data as unknown as SnapshotCursor & Record<string, unknown>, active, Date.now());
          if (!result.accepted) return;
          configurationSync.value = result.state;
          settleChangedMutations(previous);
          snapshot.value = event.data.state;
          snapshotContext.value = {
            session_generation: event.data.session_generation,
            server_epoch: event.data.server_epoch,
            transport_generation: event.data.transport_generation,
            seq: event.data.seq,
          };
          latestFrameSeq = event.data.seq;
          snapshotReceivedAt.value = Date.now();
        }
        break;
      case "telemetry": {
        if (connection.value.phase !== "online" && connection.value.phase !== "reconnecting") return;
        const active = activeSnapshotContext(connection.value);
        const currentContext = snapshotContext.value;
        const currentSnapshot = snapshot.value;
        if (!active || !currentContext || !currentSnapshot || event.data.seq <= latestFrameSeq
          || !sameSnapshotContext(active, event.data)
          || currentContext.session_generation !== event.data.session_generation
          || currentContext.server_epoch !== event.data.server_epoch
          || currentContext.transport_generation !== event.data.transport_generation) return;
        const receiveAt = performance.now();
        latestFrameSeq = event.data.seq;
        snapshot.value = {
          ...currentSnapshot,
          avatar_zones: event.data.avatar_zones,
          status: event.data.status,
        };
        snapshotReceivedAt.value = Date.now();
        await nextTick();
        recordTelemetryRender(performance.now() - receiveAt);
        break;
      }
      case "capabilities":
        if (connection.value.phase === "online" || connection.value.phase === "reconnecting") capabilities.value = event.data;
        break;
      case "catalog": catalogs.value[event.data.catalog] = event.data; break;
      case "error": error.value = event.data; break;
    }
  }

  async function subscribe() {
    const channel = new Channel<UiEvent>();
    channel.onmessage = onEvent;
    await invoke("subscribe_state", { channel });
  }

  async function loadProfiles() {
    const revision = ++profileLoadRevision;
    const nextProfiles = await invoke<HostProfile[]>("list_profiles");
    if (revision === profileLoadRevision) profiles.value = nextProfiles;
  }

  async function deleteProfile(profileId: string) {
    await invoke("delete_profile", { profileId });
    profileLoadRevision += 1;
    invitationOpenRevision += 1;
    if (connection.value.profile_id === profileId) {
      connection.value = { ...disconnected, session_generation: connection.value.session_generation };
      verifiedIdentity.value = null;
      error.value = "";
    }
    if (pendingPairingProfileId.value === profileId) {
      pendingPairingProfile.value = null;
      pendingPairingProfileId.value = null;
      pairingRequestRevision.value += 1;
    }
    await loadProfiles();
  }

  async function renameProfile(profileId: string, label: string) {
    await invoke("rename_profile", { profileId, label });
    await loadProfiles();
  }

  async function cacheHostDisplayName(profileId: string, hostDisplayName: string | null | undefined) {
    const value = hostDisplayName?.trim() || null;
    const key = `${profileId}:${value ?? ""}`;
    const pending = hostNameCacheOperations.get(key);
    if (pending) return pending;
    const operation = (async () => {
      await invoke("cache_host_display_name", { profileId, hostDisplayName: value });
      await loadProfiles();
    })();
    hostNameCacheOperations.set(key, operation);
    try { await operation; }
    finally { hostNameCacheOperations.delete(key); }
  }

  async function saveProfileConnectionSettings(input: {
    profileId: string;
    host: string;
    port: number;
    connectionMode: string;
    rendezvousUrl: string;
    iceServerUrls: string[];
    forceRelay: boolean;
  }) {
    await invoke("save_profile_connection_settings", {
      profileId: input.profileId,
      host: input.host,
      port: input.port,
      connectionMode: input.connectionMode,
      rendezvousUrl: input.rendezvousUrl,
      iceServerUrls: input.iceServerUrls,
      forceRelay: input.forceRelay,
    });
    await loadProfiles();
  }

  async function openPairingInvitation(invitation: string) {
    error.value = "";
    const revision = ++invitationOpenRevision;
    const profile = await invoke<HostProfile>("open_pairing_invitation", { invitation });
    if (revision !== invitationOpenRevision) return profile;
    pendingPairingProfile.value = profile;
    pendingPairingProfileId.value = profile.id;
    pairingRequestRevision.value += 1;
    return profile;
  }

  async function connectProfile(profileId: string) {
    error.value = "";
    const pairingRevision = pairingRequestRevision.value;
    latestFrameSeq = 0;
    snapshot.value = null;
    snapshotContext.value = null;
    snapshotReceivedAt.value = null;
    capabilities.value = null;
    catalogs.value = {};
    logs.value = null;
    const identity = await invoke<HostIdentity>("connect_profile", { profileId });
    await loadProfiles();
    if (!canApplyConnectionResult(
      pairingRevision,
      pairingRequestRevision.value,
      profileId,
      pendingPairingProfileId.value,
    )) return identity;
    verifiedIdentity.value = identity;
    if (pendingPairingProfileId.value === profileId) {
      pendingPairingProfile.value = null;
      pendingPairingProfileId.value = null;
    }
    return identity;
  }

  async function disconnect() {
    const profileId = connection.value.profile_id;
    const hostDisplayName = snapshot.value?.host_display_name?.trim();
    if (connection.value.phase === "online" && profileId && hostDisplayName) {
      try {
        await cacheHostDisplayName(profileId, hostDisplayName);
      } catch (cacheError) {
        if (connection.value.phase === "online" && connection.value.profile_id === profileId) {
          error.value = String(cacheError);
        }
      }
    }
    await invoke("disconnect", { hostDisplayName: snapshot.value?.host_display_name ?? null });
    verifiedIdentity.value = null;
    latestFrameSeq = 0;
    snapshot.value = null;
    snapshotContext.value = null;
    snapshotReceivedAt.value = null;
    capabilities.value = null;
  }

  const fetchState = () => invoke("fetch_state");
  const fetchCapabilities = () => invoke("fetch_capabilities");
  const fetchCatalog = (kind: "presets" | "zone_presets") => invoke("fetch_catalog", { kind });
  async function fetchLogs(cursor?: string | null) {
    const revision = ++hostLogRequestRevision;
    const loadingOlder = cursor != null && logs.value != null;
    hostLogLoadState.value = "loading";
    hostLogLoadError.value = "";
    try {
      const page = await invoke<HostLogs>("fetch_logs", { cursor: cursor ?? null, limit: HOST_LOG_PAGE_SIZE });
      if (!isCurrentHostLogRequest(revision, hostLogRequestRevision)) return;
      const existing = logs.value;
      if (!loadingOlder || existing == null) {
        logs.value = page;
        hostLogHasOlder.value = hasOlderHostLogRecords(page.lines);
      } else if (!page.lines.length) {
        hostLogHasOlder.value = false;
      } else {
        logs.value = prependOlderHostLogPage(existing, page);
        hostLogHasOlder.value = page.lines.length === HOST_LOG_PAGE_SIZE
          && hasOlderHostLogRecords(logs.value.lines);
      }
      hostLogLoadState.value = "loaded";
    } catch (cause) {
      if (isCurrentHostLogRequest(revision, hostLogRequestRevision)) {
        hostLogLoadState.value = "error";
        hostLogLoadError.value = String(cause);
      }
    }
  }
  async function fetchOlderHostLogs() {
    const firstSequence = logs.value?.lines[0]?.seq ?? 0;
    const cursor = olderHostLogCursor(firstSequence);
    if (cursor == null) {
      hostLogHasOlder.value = false;
      return;
    }
    await fetchLogs(cursor);
  }
  const fetchTerminalDiagnostics = async () => { terminalDiagnostics.value = await invoke<TerminalDiagnostics>("get_terminal_diagnostics"); };
  const clearTerminalDiagnostics = async () => {
    await invoke("clear_terminal_diagnostics");
    await fetchTerminalDiagnostics();
  };
  const exportDiagnostics = (request: import("../protocol/types").DiagnosticExportRequest) =>
    invoke<import("../protocol/types").DiagnosticExportResult>("export_diagnostics", { request });
  const diagnosticExportProgress = () =>
    invoke<import("../protocol/types").DiagnosticExportProgress>("diagnostic_export_progress");
  const cancelDiagnosticExport = () => invoke("cancel_diagnostic_export");
  async function submitOperation(operation: AllowedOperation) {
    const descriptors = configurationMutationDescriptors(operation, snapshot.value as Record<string, unknown> | null);
    const context = snapshotContext.value;
    const operationId = ++configurationOperationId;
    if (descriptors.length && context) {
      for (const descriptor of descriptors) {
        configurationSync.value = beginConfigurationMutation(configurationSync.value, {
          ...descriptor,
          operationId,
          context,
          now: Date.now(),
        });
      }
      if (Object.values(configurationSync.value.mutations).some((mutation) => mutation.operationId === operationId)) {
        scheduleMutationTimeout(operationId);
      }
    }
    try {
      const ack = await invoke<CommandAck>("submit_operation", { operation });
      if (ack.queued && context) {
        const previous = configurationSync.value;
        configurationSync.value = markConfigurationCommandQueued(configurationSync.value, {
          operationId,
          requestId: ack.request_id,
          context: {
            session_generation: ack.session_generation,
            server_epoch: ack.server_epoch,
            transport_generation: ack.transport_generation,
            seq: ack.snapshot_seq,
          },
        });
        settleChangedMutations(previous);
      }
      return ack;
    } catch (submitError) {
      if (descriptors.length && context) {
        const previous = configurationSync.value;
        const message = String(submitError);
        configurationSync.value = /outcome is unknown|timed out|interrupted|session ended/i.test(message)
          ? markConfigurationOutcomeUnknown(configurationSync.value, { operationId, now: Date.now() })
          : markConfigurationCommandRejected(configurationSync.value, { operationId, now: Date.now() });
        settleChangedMutations(previous);
        scheduleMutationCleanup(operationId);
      }
      return Promise.reject(submitError);
    }
  }

  return {
    profiles, connection, snapshot, snapshotContext, snapshotReceivedAt, capabilities, catalogs, logs, hostLogLoadState, hostLogLoadError, hostLogHasOlder, terminalDiagnostics, error, verifiedIdentity, lastSessionEndPhase, pendingPairingProfile, pendingPairingProfileId, pairingRequestRevision, connected,
    subscribe, loadProfiles, deleteProfile, renameProfile, cacheHostDisplayName, saveProfileConnectionSettings,
    openPairingInvitation, connectProfile, disconnect,
    fetchState, fetchCapabilities, fetchCatalog, fetchLogs, fetchOlderHostLogs, fetchTerminalDiagnostics, clearTerminalDiagnostics, exportDiagnostics, diagnosticExportProgress, cancelDiagnosticExport, submitOperation,
    configurationMutation,
  };
});

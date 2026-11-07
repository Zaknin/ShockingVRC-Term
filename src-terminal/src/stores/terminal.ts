import { Channel, invoke } from "@tauri-apps/api/core";
import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { Capabilities, ConnectionState, HostCatalog, HostIdentity, HostLogs, HostProfile, HostSnapshot, TerminalDiagnostics, UiEvent } from "../protocol/types";
import type { AllowedOperation } from "../protocol/operations";

const disconnected: ConnectionState = {
  phase: "disconnected",
  transport: null,
  ice_candidate_pair: null,
  profile_id: null,
  host_label: null,
  fingerprint: null,
  server_id: null,
  last_heartbeat: null,
  message: null,
};

export const useTerminalStore = defineStore("terminal", () => {
  const profiles = ref<HostProfile[]>([]);
  const connection = ref<ConnectionState>({ ...disconnected });
  const snapshot = ref<HostSnapshot | null>(null);
  const capabilities = ref<Capabilities | null>(null);
  const catalogs = ref<Record<string, HostCatalog>>({});
  const logs = ref<HostLogs | null>(null);
  const terminalDiagnostics = ref<TerminalDiagnostics | null>(null);
  const error = ref("");
  const verifiedIdentity = ref<HostIdentity | null>(null);
  const pendingPairingProfile = ref<HostProfile | null>(null);
  const pendingPairingProfileId = ref<string | null>(null);
  const pairingRequestRevision = ref(0);
  const connected = computed(() => connection.value.phase === "online");

  function onEvent(event: UiEvent) {
    switch (event.kind) {
      case "connection": connection.value = event.data; break;
      case "pairing_invitation": {
        const profile = event.data;
        pendingPairingProfile.value = profile;
        pendingPairingProfileId.value = profile.id;
        pairingRequestRevision.value += 1;
        break;
      }
      case "snapshot": snapshot.value = event.data; break;
      case "capabilities": capabilities.value = event.data; break;
      case "catalog": catalogs.value[event.data.catalog] = event.data; break;
      case "logs": logs.value = event.data; break;
      case "error": error.value = event.data; break;
    }
  }

  async function subscribe() {
    const channel = new Channel<UiEvent>();
    channel.onmessage = onEvent;
    await invoke("subscribe_state", { channel });
  }

  async function loadProfiles() {
    profiles.value = await invoke<HostProfile[]>("list_profiles");
  }

  async function deleteProfile(profileId: string) {
    await invoke("delete_profile", { profileId });
    if (pendingPairingProfileId.value === profileId) {
      pendingPairingProfile.value = null;
      pendingPairingProfileId.value = null;
    }
    await loadProfiles();
  }

  async function openPairingInvitation(invitation: string) {
    error.value = "";
    const profile = await invoke<HostProfile>("open_pairing_invitation", { invitation });
    pendingPairingProfile.value = profile;
    pendingPairingProfileId.value = profile.id;
    pairingRequestRevision.value += 1;
    return profile;
  }

  async function connectProfile(profileId: string) {
    error.value = "";
    snapshot.value = null;
    capabilities.value = null;
    catalogs.value = {};
    logs.value = null;
    verifiedIdentity.value = await invoke<HostIdentity>("connect_profile", { profileId });
    await loadProfiles();
    pendingPairingProfile.value = null;
    pendingPairingProfileId.value = null;
  }

  async function disconnect() {
    await invoke("disconnect");
    verifiedIdentity.value = null;
    snapshot.value = null;
    capabilities.value = null;
  }

  const fetchState = () => invoke("fetch_state");
  const fetchCapabilities = () => invoke("fetch_capabilities");
  const fetchCatalog = (kind: "presets" | "zone_presets") => invoke("fetch_catalog", { kind });
  const fetchLogs = (cursor?: string | null) => invoke("fetch_logs", { cursor: cursor ?? null, limit: 100 });
  const fetchTerminalDiagnostics = async () => { terminalDiagnostics.value = await invoke<TerminalDiagnostics>("get_terminal_diagnostics"); };
  const clearTerminalDiagnostics = async () => {
    await invoke("clear_terminal_diagnostics");
    await fetchTerminalDiagnostics();
  };
  async function submitOperation(operation: AllowedOperation) {
    return invoke<{ request_id: string; queued: boolean }>("submit_operation", { operation });
  }

  return {
    profiles, connection, snapshot, capabilities, catalogs, logs, terminalDiagnostics, error, verifiedIdentity, pendingPairingProfile, pendingPairingProfileId, pairingRequestRevision, connected,
    subscribe, loadProfiles, deleteProfile, openPairingInvitation, connectProfile, disconnect,
    fetchState, fetchCapabilities, fetchCatalog, fetchLogs, fetchTerminalDiagnostics, clearTerminalDiagnostics, submitOperation,
  };
});

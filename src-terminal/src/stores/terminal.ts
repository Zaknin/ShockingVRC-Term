import { Channel, invoke } from "@tauri-apps/api/core";
import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { Capabilities, ConnectionState, HostCatalog, HostIdentity, HostLogs, HostProfile, HostSnapshot, UiEvent } from "../protocol/types";
import type { AllowedOperation } from "../protocol/operations";

const disconnected: ConnectionState = {
  phase: "disconnected",
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
  const error = ref("");
  const verifiedIdentity = ref<HostIdentity | null>(null);
  const savedCredentialsAvailable = ref(false);
  const connected = computed(() => connection.value.phase === "online");

  function onEvent(event: UiEvent) {
    switch (event.kind) {
      case "connection": connection.value = event.data; break;
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

  async function saveProfile(profile: Omit<HostProfile, "id"> & { id?: string }) {
    const saved = await invoke<HostProfile>("save_profile", { profile });
    await loadProfiles();
    return saved;
  }

  async function deleteProfile(profileId: string) {
    await invoke("delete_profile", { profileId });
    await loadProfiles();
  }

  async function checkSavedCredentials(profileId: string) {
    savedCredentialsAvailable.value = await invoke<boolean>("has_saved_credentials", { profileId });
  }

  async function forgetCredentials(profileId: string) {
    await invoke("forget_credentials", { profileId });
    savedCredentialsAvailable.value = false;
  }

  async function connectProfile(profileId: string) {
    error.value = "";
    snapshot.value = null;
    capabilities.value = null;
    catalogs.value = {};
    logs.value = null;
    verifiedIdentity.value = await invoke<HostIdentity>("connect_profile", { profileId });
  }

  async function login(profileId: string, username: string, password: string, rememberPassword: boolean) {
    error.value = "";
    try {
      await invoke("login", { profileId, username, password, rememberPassword });
    } finally {
      verifiedIdentity.value = null;
    }
  }

  async function loginSaved(profileId: string) {
    error.value = "";
    try {
      await invoke("login_saved", { profileId });
    } finally {
      verifiedIdentity.value = null;
    }
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
  async function submitOperation(operation: AllowedOperation) {
    return invoke<{ request_id: string; queued: boolean }>("submit_operation", { operation });
  }

  return {
    profiles, connection, snapshot, capabilities, catalogs, logs, error, verifiedIdentity, savedCredentialsAvailable, connected,
    subscribe, loadProfiles, saveProfile, deleteProfile, checkSavedCredentials, forgetCredentials, connectProfile, login, loginSaved, disconnect,
    fetchState, fetchCapabilities, fetchCatalog, fetchLogs, submitOperation,
  };
});

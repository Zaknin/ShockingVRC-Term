<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { useTerminalStore } from "../stores/terminal";
import TerminalButton from "./TerminalButton.vue";
import TerminalPanel from "./TerminalPanel.vue";
import { displayHostName } from "../domain/host-display-name.mjs";
import type { HostProfile } from "../protocol/types";

const store = useTerminalStore();
const selectedId = ref("");
const invitationText = ref("");
const busy = ref(false);
const openingInvitation = ref(false);
const automaticPairingRequestRevision = ref(0);
const renameOpen = ref(false);
const renameDraft = ref("");
const networkDraft = reactive({
  host: "",
  port: 4318,
  connectionMode: "auto",
  rendezvousUrl: "",
  iceServerUrls: "",
  forceRelay: false,
});

const selectedProfile = computed(() => (store.pendingPairingProfile?.id === selectedId.value ? store.pendingPairingProfile : null)
  ?? store.profiles.find((profile) => profile.id === selectedId.value) ?? null);
function profileDisplayName(profile: HostProfile) {
  return displayHostName({
    localAlias: profile.label,
    hasLocalAlias: profile.label_is_custom,
    hostDisplayName: profile.host_display_name,
    serverId: profile.server_id,
  });
}

watch(() => store.pairingRequestRevision, (revision) => { void startPairing(revision); });
watch(selectedProfile, (profile) => {
  if (!profile) return;
  renameDraft.value = profileDisplayName(profile);
  networkDraft.host = profile.host;
  networkDraft.port = profile.port;
  networkDraft.connectionMode = profile.connection_mode;
  networkDraft.rendezvousUrl = profile.rendezvous_url;
  networkDraft.iceServerUrls = profile.ice_server_urls.join("\n");
  networkDraft.forceRelay = profile.force_relay;
  renameOpen.value = false;
});

async function startPairing(revision: number) {
  const profileId = store.pendingPairingProfileId;
  if (!profileId || revision === 0 || automaticPairingRequestRevision.value === revision) return;
  const profile = (store.pendingPairingProfile?.id === profileId ? store.pendingPairingProfile : null)
    ?? store.profiles.find((item) => item.id === profileId);
  if (!profile) return;
  selectProfile(profile);
  automaticPairingRequestRevision.value = revision;
  await connect(profileId);
}

onMounted(async () => {
  await store.loadProfiles();
  const preferredId = store.pendingPairingProfileId ?? store.connection.profile_id;
  const preferred = store.profiles.find((profile) => profile.id === preferredId)
    ?? (store.pendingPairingProfile?.id === preferredId ? store.pendingPairingProfile : undefined);
  if (preferred) selectProfile(preferred);
  else if (store.profiles.length) selectProfile(store.profiles[0]);
  await startPairing(store.pairingRequestRevision);
});

function selectProfile(profile: HostProfile) {
  selectedId.value = profile.id;
  store.verifiedIdentity = null;
}

function identityIsVerified() {
  return Boolean(store.verifiedIdentity ||
    (store.connection.phase === "online" && store.connection.profile_id === selectedId.value));
}

async function openInvitation() {
  if (!invitationText.value.trim()) return;
  openingInvitation.value = true;
  try {
    await store.openPairingInvitation(invitationText.value.trim());
    invitationText.value = "";
  } catch (error) {
    store.error = String(error);
  } finally {
    openingInvitation.value = false;
  }
}

async function connect(profileId = selectedId.value) {
  if (!profileId) return;
  busy.value = true;
  try {
    await store.connectProfile(profileId);
  } catch (error) {
    store.error = String(error);
  } finally {
    busy.value = false;
  }
}

async function renameConnection() {
  if (!selectedId.value) return;
  busy.value = true;
  try {
    await store.renameProfile(selectedId.value, renameDraft.value);
    renameOpen.value = false;
  } catch (error) {
    store.error = String(error);
  } finally {
    busy.value = false;
  }
}

async function removeConnection() {
  if (!selectedId.value || !window.confirm("Remove this saved connection and its local device grant? The Host will still trust this Terminal until access is revoked from the Host.")) return;
  try {
    await store.deleteProfile(selectedId.value);
    selectedId.value = store.profiles[0]?.id ?? "";
  } catch (error) {
    store.error = String(error);
  }
}

async function saveAdvancedSettings() {
  const profile = selectedProfile.value;
  if (!profile) return;
  busy.value = true;
  try {
    await store.saveProfileConnectionSettings({
      profileId: profile.id,
      host: networkDraft.host,
      port: Number(networkDraft.port),
      connectionMode: networkDraft.connectionMode,
      rendezvousUrl: networkDraft.rendezvousUrl,
      iceServerUrls: networkDraft.iceServerUrls.split(/\r?\n/).map((item) => item.trim()).filter(Boolean),
      forceRelay: networkDraft.forceRelay,
    });
  } catch (error) {
    store.error = String(error);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="connect-grid pairing-grid">
    <TerminalPanel class="profile-panel" kicker="CONNECTIONS" title="Saved connections" :suffix="`${store.profiles.length} SAVED`">
      <div v-for="profile in store.profiles" :key="profile.id" class="saved-connection">
        <button class="profile-row" :class="{ selected: selectedId === profile.id }" @click="selectProfile(profile)">
          <span class="profile-info"><strong>{{ profileDisplayName(profile) }}</strong><small>{{ profile.host }}:{{ profile.port }}</small></span>
          <small class="saved-state" :class="{ offline: store.connection.phase !== 'online' || store.connection.profile_id !== profile.id }">{{ store.connection.phase === 'online' && store.connection.profile_id === profile.id ? 'ONLINE' : 'OFFLINE' }}</small>
        </button>
      </div>
      <div v-if="!store.profiles.length" class="empty-note">Add a connection by opening a Host invitation.</div>

      <div class="selected-host" v-if="selectedProfile">
        <div class="connection-heading"><div><span class="eyebrow">SELECTED CONNECTION</span><strong>{{ profileDisplayName(selectedProfile) }}</strong></div>
          <span class="verified-state" :class="{ verified: identityIsVerified() }">{{ identityIsVerified() ? 'VERIFIED' : 'PENDING' }}</span></div>

        <div v-if="renameOpen" class="inline-edit">
          <label for="connection-name">Connection name</label>
          <input id="connection-name" v-model="renameDraft" maxlength="80" autocomplete="off" />
          <div class="button-row"><TerminalButton variant="primary" :disabled="busy || !renameDraft.trim()" @click="renameConnection">SAVE NAME</TerminalButton>
            <TerminalButton variant="subtle" @click="renameOpen = false">CANCEL</TerminalButton></div>
        </div>

        <div class="selected-host-actions">
          <TerminalButton v-if="store.connection.phase === 'online' && store.connection.profile_id === selectedId" variant="danger" :disabled="busy" @click="store.disconnect">DISCONNECT</TerminalButton>
          <TerminalButton v-else variant="primary" :disabled="busy || store.connection.phase !== 'disconnected'" @click="connect()">{{ store.pendingPairingProfileId === selectedId ? 'RETRY PAIRING' : 'CONNECT' }}</TerminalButton>
          <TerminalButton variant="subtle" :disabled="busy || store.connection.phase !== 'disconnected'" @click="renameOpen = !renameOpen">RENAME</TerminalButton>
          <TerminalButton variant="danger" :disabled="busy || store.connection.phase !== 'disconnected'" @click="removeConnection">REMOVE CONNECTION</TerminalButton>
        </div>

        <details class="connection-details">
          <summary>Connection details and advanced settings</summary>
          <div class="detail-grid">
            <div><span>Host identity</span><code>{{ selectedProfile.server_id || 'Verified when connecting' }}</code></div>
            <div><span>TLS fingerprint</span><code>{{ selectedProfile.fingerprint }}</code></div>
            <div><span>Connection mode</span><code>{{ selectedProfile.connection_mode }}</code></div>
            <div><span>Active transport</span><code>{{ store.connection.transport ?? 'Not connected' }}</code></div>
            <div class="wide-field"><span>ICE candidate pair</span><code>{{ store.connection.ice_candidate_pair ?? 'Not reported' }}</code></div>
          </div>
          <div class="advanced-grid">
            <label>Direct address<input v-model="networkDraft.host" autocomplete="off" /></label>
            <label>Port<input v-model.number="networkDraft.port" type="number" min="1" max="65535" /></label>
            <label>Mode<select v-model="networkDraft.connectionMode"><option value="auto">Auto</option><option value="direct_only">Direct</option><option value="traversal_only">ICE traversal</option></select></label>
            <label>Rendezvous URL<input v-model="networkDraft.rendezvousUrl" autocomplete="off" placeholder="wss://…" /></label>
            <label class="wide-field">STUN/TURN URIs<textarea v-model="networkDraft.iceServerUrls" rows="2" placeholder="stun:host:3478" /></label>
            <label class="check-field"><input v-model="networkDraft.forceRelay" type="checkbox" /> Force relay</label>
          </div>
          <TerminalButton variant="primary" :disabled="busy || store.connection.phase !== 'disconnected'" @click="saveAdvancedSettings">SAVE CONNECTION SETTINGS</TerminalButton>
        </details>
        <p class="identity-note">{{ store.connection.phase === 'online' && store.connection.profile_id === selectedId ? 'Connected and verified.' : 'Host identity and TLS fingerprint are verified before device authentication.' }}</p>
        <p class="microcopy">Remove connection clears this Terminal's saved profile and grant. To end trust on a Host, use Revoke access in the Host's paired devices list.</p>
      </div>
      <div v-else class="empty-note selected-empty">Select a saved connection or open a new invitation.</div>
    </TerminalPanel>

    <TerminalPanel class="connection-panel pairing-panel" kicker="ADD CONNECTION" title="Connect to Host">
      <div class="invitation-card">
        <div class="invitation-step"><span>+</span><div><strong>Add connection</strong><small>Open a one-use Host invitation. Pairing starts automatically; pasting is available as a fallback.</small></div></div>
        <label class="invitation-label" for="pairing-invitation">Host invitation</label>
        <textarea id="pairing-invitation" v-model="invitationText" class="invitation-input mono" rows="3"
          autocomplete="off" spellcheck="false" placeholder="shockingvrc://pair/…" @keydown.ctrl.enter="openInvitation" />
        <div class="button-row">
          <TerminalButton variant="primary" :disabled="busy || openingInvitation || !invitationText.trim()" @click="openInvitation">OPEN & PAIR</TerminalButton>
          <span class="invitation-hint">Invitations expire after 15 minutes and work once.</span>
        </div>
      </div>

    </TerminalPanel>
  </div>
</template>

<style scoped>
.pairing-panel { min-height: 0; }
.saved-connection { min-width: 0; }
.saved-state, .verified-state { flex: 0 0 auto; color: var(--success); font: var(--type-micro)/var(--line-micro) var(--mono); letter-spacing: .06em; }
.saved-state.offline { color: var(--muted); }
.invitation-card { display: grid; gap: 10px; padding: 12px; border: 1px solid var(--line); background: var(--surface); }
.invitation-step { display: flex; align-items: center; gap: 10px; }
.invitation-step > span { width: 26px; height: 26px; display: grid; place-items: center; border: 1px solid var(--line); color: var(--accent); font: var(--type-caption)/var(--line-caption) var(--mono); }
.invitation-step > div { min-width: 0; display: grid; gap: 4px; }
.invitation-step strong { font-size: var(--type-caption); line-height: var(--line-caption); font-weight: 550; }
.invitation-step small { color: var(--muted); font-size: var(--type-micro); line-height: var(--line-micro); line-height: 1.45; overflow-wrap: anywhere; }
.invitation-label { margin-top: 4px; color: var(--muted); font-size: var(--type-micro); line-height: var(--line-micro); }
.invitation-input { width: 100%; min-height: 72px; padding: 8px 9px; resize: vertical; border: 1px solid var(--line); background: var(--surface-deep); color: var(--text); outline: none; font-size: var(--type-micro); line-height: var(--line-micro); line-height: 1.5; overflow-wrap: anywhere; }
.invitation-input:focus { border-color: var(--focus); box-shadow: 0 0 0 2px var(--accent-soft); }
.invitation-hint { color: var(--muted); font-size: var(--type-micro); line-height: var(--line-micro); }
.selected-host { display: grid; gap: 11px; margin-top: 10px; padding-top: 14px; border-top: 1px solid var(--line-soft); }
.connection-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.connection-heading > div { display: grid; gap: 4px; }.connection-heading strong { font-size: var(--type-secondary); line-height: var(--line-secondary); }
.selected-host-actions { display: flex; flex-wrap: wrap; gap: 7px; }
.inline-edit { display: grid; gap: 7px; padding: 9px; border: 1px solid var(--line-soft); background: var(--surface); }
.inline-edit label, .advanced-grid label { display: grid; gap: 5px; color: var(--muted); font-size: var(--type-micro); line-height: var(--line-micro); }
.inline-edit input, .advanced-grid input:not([type=checkbox]), .advanced-grid select, .advanced-grid textarea { width: 100%; min-width: 0; min-height: 28px; padding: 5px 7px; border: 1px solid var(--line); background: var(--surface-deep); color: var(--text); font: var(--type-micro)/var(--line-micro) var(--mono); }
.connection-details { display: grid; gap: 10px; border-top: 1px solid var(--line-soft); padding-top: 9px; }
.connection-details summary { cursor: pointer; color: var(--muted); font: var(--type-micro)/var(--line-micro) var(--mono); }
.detail-grid, .advanced-grid { display: grid; grid-template-columns: repeat(2,minmax(0,1fr)); gap: 8px; }
.detail-grid > div { display: grid; gap: 4px; min-width: 0; }.detail-grid span { color: var(--muted); font-size: var(--type-micro); line-height: var(--line-micro); }.detail-grid code { overflow-wrap: anywhere; font: var(--type-micro)/var(--line-micro) var(--mono); }
.wide-field { grid-column: 1 / -1; }.advanced-grid textarea { resize: vertical; }.check-field { display: flex !important; align-items: center; }.check-field input { accent-color: var(--accent); }
.identity-note { margin: 0; color: var(--muted); font-size: var(--type-micro); line-height: var(--line-micro); line-height: 1.5; }
.selected-empty { padding: 14px 2px; }
</style>

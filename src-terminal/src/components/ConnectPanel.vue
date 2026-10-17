<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { useTerminalStore } from "../stores/terminal";
import TerminalButton from "./TerminalButton.vue";
import TerminalPanel from "./TerminalPanel.vue";
import type { HostProfile } from "../protocol/types";

const store = useTerminalStore();
const selectedId = ref("");
const invitationText = ref("");
const busy = ref(false);
const openingInvitation = ref(false);
const automaticPairingRequestRevision = ref(0);

watch(() => store.pairingRequestRevision, (revision) => { void startPairing(revision); });

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
  return Boolean(
    store.verifiedIdentity ||
    (store.connection.phase === "online" && store.connection.profile_id === selectedId.value),
  );
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

async function forgetHost() {
  if (!selectedId.value) return;
  try {
    await store.deleteProfile(selectedId.value);
    selectedId.value = store.profiles[0]?.id ?? "";
  } catch (error) {
    store.error = String(error);
  }
}

const selectedProfile = () => (store.pendingPairingProfile?.id === selectedId.value ? store.pendingPairingProfile : null)
  ?? store.profiles.find((profile) => profile.id === selectedId.value);
</script>

<template>
  <div class="connect-grid pairing-grid">
    <TerminalPanel class="profile-panel" kicker="SAVED HOSTS" title="Paired devices" :suffix="`${store.profiles.length} SAVED`">
      <button v-for="profile in store.profiles" :key="profile.id"
        class="profile-row" :class="{ selected: selectedId === profile.id }" @click="selectProfile(profile)">
        <span class="profile-info"><strong>{{ profile.label }}</strong><small>{{ profile.host }}:{{ profile.port }}</small></span>
      </button>
      <div v-if="!store.profiles.length" class="empty-note">Open a Host invitation to pair your first device.</div>
      <p class="microcopy">Each Terminal has a device key stored in Windows Credential Manager. Hosts remember paired devices and can revoke them at any time.</p>
    </TerminalPanel>

    <TerminalPanel class="connection-panel pairing-panel" kicker="PASSWORDLESS PAIRING" title="Connect securely" suffix="HOST SIGNED · ONE USE">

      <div class="invitation-card">
        <div class="invitation-step"><span>01</span><div><strong>Open a Host invitation</strong><small>Opening the Host's link starts pairing automatically. You can also paste the link below.</small></div></div>
        <label class="invitation-label" for="pairing-invitation">Paste invitation</label>
        <textarea id="pairing-invitation" v-model="invitationText" class="invitation-input mono" rows="3"
          autocomplete="off" spellcheck="false" placeholder="shockingvrc://pair/…" @keydown.ctrl.enter="openInvitation" />
        <div class="button-row">
          <TerminalButton variant="primary" :disabled="busy || openingInvitation || !invitationText.trim()" @click="openInvitation">OPEN & PAIR</TerminalButton>
          <span class="invitation-hint">Links expire after 15 minutes and can be used once.</span>
        </div>
      </div>

      <div class="pairing-divider"><span>OR RECONNECT</span></div>

      <div class="selected-host" v-if="selectedProfile()">
        <div class="invitation-step"><span>02</span><div><strong>{{ selectedProfile()?.label }}</strong><small>{{ selectedProfile()?.server_id || 'Host identity is checked against the saved invitation pin.' }}</small></div></div>
        <div class="selected-host-actions">
          <TerminalButton variant="primary" :disabled="busy || !selectedId || store.connection.phase !== 'disconnected'" @click="connect()">{{ store.pendingPairingProfileId === selectedId ? 'RETRY PAIRING' : 'CONNECT' }}</TerminalButton>
          <TerminalButton v-if="selectedProfile()" variant="subtle" :disabled="busy || store.connection.phase !== 'disconnected'" @click="forgetHost">FORGET HOST</TerminalButton>
        </div>
      </div>
      <div v-else class="empty-note selected-empty">Select a saved Host or open a new invitation.</div>

      <div class="identity-result" :class="{ verified: identityIsVerified() }">
        <span class="identity-label">{{ identityIsVerified() ? 'VERIFIED' : 'PENDING' }}</span>
        <div>
          <strong>{{ identityIsVerified() ? 'Host identity verified' : 'Invitation verification is automatic' }}</strong>
          <small>{{ store.verifiedIdentity?.server_id ?? (identityIsVerified() ? 'Verified for this active Host session.' : 'The signed link pins the Host identity before device proof is sent.') }}</small>
        </div>
      </div>
    </TerminalPanel>
  </div>
</template>

<style scoped>
.pairing-panel { min-height: 0; }
.invitation-card { display: grid; gap: 10px; padding: 15px; border: 1px solid #444149; border-radius: 4px; background: #19181c; }
.invitation-step { display: flex; align-items: center; gap: 10px; }
.invitation-step > span { width: 26px; height: 26px; display: grid; place-items: center; border: 1px solid #4c4852; color: #8fe0b9; font: 9px var(--mono); }
.invitation-step > div { min-width: 0; display: grid; gap: 4px; }
.invitation-step strong { color: #d3d0d7; font-size: 11px; font-weight: 550; }
.invitation-step small { color: var(--muted); font-size: 9px; line-height: 1.45; overflow-wrap: anywhere; }
.invitation-label { margin-top: 4px; color: #aaa7af; font-size: 9px; }
.invitation-input { width: 100%; min-height: 72px; padding: 8px 9px; resize: vertical; border: 1px solid #3a393d; border-radius: 3px; background: #111014; color: var(--text); outline: none; font-size: 9px; line-height: 1.5; overflow-wrap: anywhere; }
.invitation-input:focus { border-color: #888098; box-shadow: 0 0 0 2px #4cd4971a; }
.invitation-input::placeholder { color: #69686c; }
.invitation-hint { color: #8e8b93; font-size: 8px; }
.pairing-divider { display: flex; align-items: center; gap: 10px; margin: 17px 0 12px; color: #77747d; font: 8px var(--mono); letter-spacing: .08em; }
.pairing-divider::before, .pairing-divider::after { content: ""; height: 1px; flex: 1; background: var(--line-soft); }
.selected-host { display: grid; gap: 13px; }
.selected-host-actions { display: flex; flex-wrap: wrap; gap: 8px; padding-left: 36px; }
.selected-empty { padding: 14px 2px; }
</style>

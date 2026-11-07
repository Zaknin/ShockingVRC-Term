<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { useTerminalStore } from "../stores/terminal";
import type { HostProfile } from "../protocol/types";

const store = useTerminalStore();
const selectedId = ref("");
const label = ref("");
const host = ref("");
const port = ref(4318);
const fingerprint = ref("");
const serverId = ref("");
const connectionMode = ref("auto");
const rendezvousUrl = ref("");
const iceServerUris = ref("");
const inviteToken = ref("");
const username = ref("");
const password = ref("");
const rememberPassword = ref(false);
const busy = ref(false);

watch([selectedId, label, host, port, fingerprint, serverId, connectionMode, rendezvousUrl, iceServerUris], () => {
  store.verifiedIdentity = null;
}, { flush: "sync" });

watch(selectedId, (next, previous) => {
  if (previous && next !== previous) {
    username.value = "";
    password.value = "";
    rememberPassword.value = false;
  }
});

watch(() => store.profiles, (profiles) => {
  if (!selectedId.value && profiles.length) selectProfile(profiles[0]);
}, { deep: true });

onMounted(async () => {
  await store.loadProfiles();
  if (store.profiles.length) selectProfile(store.profiles[0]);
});

function selectProfile(profile: HostProfile) {
  selectedId.value = profile.id;
  label.value = profile.label;
  host.value = profile.host;
  port.value = profile.port;
  fingerprint.value = profile.fingerprint;
  serverId.value = profile.server_id ?? "";
  connectionMode.value = profile.connection_mode ?? "auto";
  rendezvousUrl.value = profile.rendezvous_url ?? "";
  iceServerUris.value = (profile.ice_server_urls ?? []).join("\n");
  store.checkSavedCredentials(profile.id).catch((error) => { store.error = String(error); });
  store.checkSavedInviteToken(profile.id).catch((error) => { store.error = String(error); });
}

function blankProfile() {
  selectedId.value = "";
  label.value = "";
  host.value = "";
  port.value = 4318;
  fingerprint.value = "";
  serverId.value = "";
  connectionMode.value = "auto";
  rendezvousUrl.value = "";
  iceServerUris.value = "";
  inviteToken.value = "";
  username.value = "";
  password.value = "";
  rememberPassword.value = false;
  store.savedCredentialsAvailable = false;
  store.savedInviteTokenAvailable = false;
}

async function persistProfile() {
  const saved = await store.saveProfile({
    ...(selectedId.value ? { id: selectedId.value } : {}),
    label: label.value,
    host: host.value,
    port: Number(port.value),
    fingerprint: fingerprint.value,
    server_id: serverId.value,
    connection_mode: connectionMode.value,
    rendezvous_url: rendezvousUrl.value,
    ice_server_urls: iceServerUris.value.split(/\r?\n/).map((line) => line.trim()).filter(Boolean),
  });
  selectProfile(saved);
  return saved;
}

async function saveProfile() {
  busy.value = true;
  try {
    await persistProfile();
  } catch (error) {
    store.error = String(error);
  } finally {
    busy.value = false;
  }
}

async function ensureVerifiedProfile(profileId: string) {
  if (store.verifiedIdentity?.profile_id === profileId) return store.verifiedIdentity;
  await store.connectProfile(profileId);
  const identity = store.verifiedIdentity;
  if (!identity || identity.profile_id !== profileId) {
    throw new Error("Pinned Host identity verification did not complete");
  }
  serverId.value = identity.server_id;
  return identity;
}

async function saveInviteToken() {
  if (!selectedId.value || !inviteToken.value) return;
  busy.value = true;
  try {
    const profile = await persistProfile();
    await store.saveInviteToken(profile.id, inviteToken.value);
  } catch (error) {
    store.error = String(error);
  } finally {
    inviteToken.value = "";
    busy.value = false;
  }
}

async function forgetInviteToken() {
  if (!selectedId.value) return;
  try { await store.forgetInviteToken(selectedId.value); } catch (error) { store.error = String(error); }
}

async function verifyHost() {
  busy.value = true;
  try {
    const profile = await persistProfile();
    await ensureVerifiedProfile(profile.id);
    await store.checkSavedCredentials(profile.id);
  } catch (error) {
    store.error = String(error);
  } finally {
    busy.value = false;
  }
}

async function signIn() {
  if (!selectedId.value) return;
  busy.value = true;
  try {
    const profile = await persistProfile();
    const identity = await ensureVerifiedProfile(profile.id);
    await store.login(identity.profile_id, username.value, password.value, rememberPassword.value);
    password.value = "";
    if (rememberPassword.value) await store.checkSavedCredentials(profile.id);
  } catch (error) {
    store.error = String(error);
  } finally {
    password.value = "";
    busy.value = false;
  }
}

async function signInSaved() {
  if (!selectedId.value) return;
  busy.value = true;
  try {
    const profile = await persistProfile();
    await ensureVerifiedProfile(profile.id);
    await store.loginSaved(profile.id);
  } catch (error) {
    store.error = String(error);
  } finally {
    busy.value = false;
  }
}

async function forgetSavedCredentials() {
  if (!selectedId.value) return;
  try { await store.forgetCredentials(selectedId.value); } catch (error) { store.error = String(error); }
}

async function removeProfile() {
  if (!selectedId.value) return;
  try {
    await store.deleteProfile(selectedId.value);
    blankProfile();
  } catch (error) {
    store.error = String(error);
  }
}
</script>

<template>
  <div class="connect-grid">
    <section class="panel profile-panel">
      <div class="panel-heading">
        <div><span class="eyebrow">DIRECT CONNECTION</span><h2>Host profiles</h2></div>
        <button class="button subtle" @click="blankProfile">NEW</button>
      </div>
      <button v-for="profile in store.profiles" :key="profile.id"
        class="profile-row" :class="{ selected: selectedId === profile.id }" @click="selectProfile(profile)">
        <span class="profile-dot"></span>
        <span class="profile-info"><strong>{{ profile.label }}</strong><small>{{ profile.host }}:{{ profile.port }}</small></span>
        <span class="profile-arrow">›</span>
      </button>
      <div v-if="!store.profiles.length" class="empty-note">No saved Host profiles yet.</div>
      <p class="microcopy">Profiles store Host addresses and the confirmed fingerprint. Passwords, invite tokens, access tokens and resume tickets stay out of the profile file.</p>
    </section>

    <section class="panel connection-panel">
      <div class="panel-heading">
        <div><span class="eyebrow">HOST PAIRING</span><h2>{{ selectedId ? 'Connection profile' : 'Add a Host' }}</h2></div>
        <span class="tag">{{ connectionMode === 'traversal_only' ? 'ICE · WEBRTC' : connectionMode === 'direct_only' ? 'DIRECT · HTTPS/WSS' : 'DIRECT → ICE' }}</span>
      </div>
      <div class="form-grid">
        <label>Profile name<input v-model="label" autocomplete="off" placeholder="Desk Host" /></label>
        <div class="field-pair">
          <label>Host name or IP<input v-model="host" autocomplete="off" placeholder="192.168.1.20" /></label>
          <label class="port-field">Port<input v-model.number="port" type="number" min="1" max="65535" /></label>
        </div>
        <label>Confirmed SPKI fingerprint
          <input v-model="fingerprint" class="mono" autocomplete="off" spellcheck="false" placeholder="64 hex characters from Host Setup" />
        </label>
        <p class="field-help">Enter the fingerprint shown by the Host. Terminal checks this pin before sending credentials and blocks HTTPS/WSS if it does not match.</p>
        <label>Host ID
          <input v-model="serverId" class="mono" autocomplete="off" spellcheck="false" placeholder="Stable ID shown in Host Setup" />
        </label>
        <div class="field-pair">
          <label>Connection mode
            <select v-model="connectionMode">
              <option value="auto">Auto · Direct then ICE</option>
              <option value="direct_only">Direct only</option>
              <option value="traversal_only">ICE only</option>
            </select>
          </label>
          <label>Rendezvous WSS URL
            <input v-model="rendezvousUrl" autocomplete="off" placeholder="wss://your-rendezvous.example/ws" />
          </label>
        </div>
        <label>Additional STUN server URLs
          <textarea v-model="iceServerUris" class="ice-uris mono" rows="2" spellcheck="false" placeholder="stun:your-stun.example:3478" />
        </label>
        <p class="field-help">TURN credentials are requested per room from the configured rendezvous. Leave endpoint fields empty for direct-only use.</p>
        <div class="field-pair invite-token-row">
          <label>Host invite token
            <input v-model="inviteToken" type="password" autocomplete="new-password" placeholder="Stored in Windows Credential Manager" />
          </label>
          <div class="invite-actions">
            <button class="button" :disabled="busy || !selectedId || !inviteToken" @click="saveInviteToken">SAVE INVITE TOKEN</button>
            <button v-if="store.savedInviteTokenAvailable" class="button subtle" :disabled="busy" @click="forgetInviteToken">FORGET TOKEN</button>
            <small>{{ store.savedInviteTokenAvailable ? 'Invite token saved in Windows Credential Manager.' : 'No saved invite token.' }}</small>
          </div>
        </div>
        <div class="button-row">
          <button class="button primary" :disabled="busy || !label || !host || !fingerprint" @click="saveProfile">SAVE PROFILE</button>
          <button class="button" :disabled="busy || !selectedId" @click="verifyHost">VERIFY HOST</button>
          <button v-if="selectedId" class="button danger" :disabled="busy" @click="removeProfile">REMOVE</button>
        </div>
      </div>

      <div class="identity-result" :class="{ verified: store.verifiedIdentity }">
        <span class="identity-mark">{{ store.verifiedIdentity ? '✓' : '◇' }}</span>
        <div>
          <strong>{{ store.verifiedIdentity ? 'Host identity verified' : 'Pinned verification runs on sign-in' }}</strong>
          <small>{{ store.verifiedIdentity?.server_id ?? 'The saved SPKI pin is checked before credentials are sent.' }}</small>
        </div>
      </div>

      <div class="login-form">
        <span class="eyebrow">REMOTE SESSION</span>
        <div class="field-pair">
          <label>Username<input v-model="username" autocomplete="username" placeholder="Host username" /></label>
          <label>Password<input v-model="password" type="password" autocomplete="current-password" placeholder="Entered for this sign-in only" /></label>
        </div>
        <label class="check-row"><input v-model="rememberPassword" type="checkbox" /> Save this password in Windows Credential Manager after successful sign-in</label>
        <div class="button-row">
          <button class="button primary" :disabled="busy || !selectedId || !username || !password" @click="signIn">{{ store.verifiedIdentity ? 'SIGN IN' : 'VERIFY PIN & SIGN IN' }}</button>
          <button v-if="store.savedCredentialsAvailable" class="button" :disabled="busy" @click="signInSaved">{{ store.verifiedIdentity ? 'SIGN IN WITH SAVED CREDENTIAL' : 'VERIFY PIN & USE SAVED CREDENTIAL' }}</button>
          <button v-if="store.savedCredentialsAvailable" class="button subtle" :disabled="busy" @click="forgetSavedCredentials">FORGET SAVED CREDENTIAL</button>
          <button class="button danger" :disabled="busy || store.connection.phase === 'disconnected'" @click="store.disconnect">DISCONNECT</button>
        </div>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { getVersion } from "@tauri-apps/api/app";
import { computed, ref, watch } from "vue";
import ConnectPanel from "./components/ConnectPanel.vue";
import SectionView from "./components/SectionView.vue";
import { sectionAfterPairingInvitation } from "./pairing-navigation.mjs";
import { useTerminalStore } from "./stores/terminal";

const store = useTerminalStore();
const clientVersion = ref("…");
const activeSection = ref("Connect");
const sections = [
  { name: "Connect", code: "00", mark: "⌁" },
  { name: "Status", code: "01", mark: "◉" },
  { name: "Zones", code: "02", mark: "⌖" },
  { name: "Channels", code: "03", mark: "◫" },
  { name: "Tuning", code: "04", mark: "⌘" },
  { name: "Modulation", code: "05", mark: "∿" },
  { name: "Presets", code: "06", mark: "▤" },
  { name: "Alarm", code: "07", mark: "◷" },
  { name: "Setup", code: "08", mark: "⚙" },
  { name: "Logs", code: "09", mark: "≡" },
];
watch(() => store.pairingRequestRevision, (revision) => {
  activeSection.value = sectionAfterPairingInvitation(
    activeSection.value,
    revision,
    store.pendingPairingProfileId,
  );
});
const statusLabel = computed(() => ({
  disconnected: "DISCONNECTED",
  connecting: "VERIFYING HOST",
  identity_verified: "IDENTITY VERIFIED",
  online: "ONLINE",
  reconnecting: "RECONNECTING",
  revoked: "SESSION REVOKED",
}[store.connection.phase] ?? store.connection.phase.toUpperCase()));
const isStale = computed(() => store.connection.phase === "reconnecting");
const transportLabel = computed(() => {
  switch (store.connection.transport) {
    case "direct_wss": return "DIRECT WSS · TLS PINNED";
    case "webrtc_datachannel": return "ICE DATA CHANNEL · DTLS PINNED";
    default:
      if (store.connection.phase === "identity_verified") return "HOST IDENTITY VERIFIED";
      if (store.connection.phase === "connecting") return "TRANSPORT NEGOTIATING";
      if (store.connection.phase === "reconnecting") return "RECONNECTING · TRANSPORT PENDING";
      return "NO TRANSPORT SELECTED";
  }
});

watch(() => store.connected, (connected) => {
  if (connected && activeSection.value === "Connect") activeSection.value = "Status";
});

store.subscribe().catch((error) => { store.error = String(error); });
getVersion().then((version) => { clientVersion.value = version; }).catch(() => { clientVersion.value = "unknown"; });
</script>

<template>
  <div class="app-shell">
    <aside class="sidebar">
      <div class="brand-lockup">
        <div class="brand-mark"><span></span><span></span><span></span></div>
        <div><strong>SHOCKING<span>VRC</span></strong><small>REMOTE TERMINAL</small></div>
      </div>

      <div class="nav-label">WORKSPACE</div>
      <nav class="section-nav" aria-label="Terminal sections">
        <button v-for="section in sections" :key="section.name" class="nav-item" :class="{ active: activeSection === section.name }" @click="activeSection = section.name">
          <span class="nav-mark">{{ section.mark }}</span><span>{{ section.name }}</span><small>{{ section.code }}</small>
        </button>
      </nav>

      <div class="sidebar-bottom">
        <div class="sidebar-rule"></div>
        <span class="eyebrow">REMOTE HOST</span>
        <strong class="sidebar-host">{{ store.connection.host_label || 'No active Host' }}</strong>
        <small>{{ store.connection.fingerprint ? store.connection.fingerprint.slice(0, 16) + '…' : 'Pair a confirmed identity' }}</small>
        <div class="build-label"><span class="status-dot" :class="{ online: store.connected }"></span> CLIENT {{ clientVersion }} · API V1</div>
      </div>
    </aside>

    <main class="main-area">
      <header class="topbar">
        <div class="breadcrumb"><span>TERMINAL</span><b>/</b><strong>{{ activeSection.toUpperCase() }}</strong></div>
        <div class="topbar-right">
          <span class="transport-label">{{ transportLabel }}</span>
          <span v-if="store.connection.ice_candidate_pair" class="transport-label">ICE PAIR · {{ store.connection.ice_candidate_pair }}</span>
          <span class="status-pill" :class="`state-${store.connection.phase}`"><i class="status-dot"></i>{{ statusLabel }}</span>
          <button v-if="store.connection.phase !== 'disconnected' && store.connection.phase !== 'identity_verified'" class="button danger small-button" @click="store.disconnect">DISCONNECT</button>
        </div>
      </header>

      <section class="content-scroll">
        <div class="page-heading">
          <div><span class="eyebrow">REMOTE CONTROL / {{ activeSection.toUpperCase() }}</span><h1>{{ activeSection === 'Connect' ? 'Connect to Host' : activeSection }}</h1></div>
          <div v-if="store.connected" class="host-chip"><span class="status-dot online"></span><div><strong>{{ store.connection.host_label }}</strong><small>{{ store.connection.server_id }}</small></div></div>
        </div>
        <div v-if="isStale" class="session-banner"><span>↻</span><div><strong>Reconnecting to the same Host session</strong><small>Cached state is stale. Controls remain unavailable until a fresh snapshot arrives.</small></div></div>
        <div v-if="store.error" class="error-banner"><span>!</span><div><strong>Connection message</strong><small>{{ store.error }}</small></div><button class="close-error" @click="store.error = ''">×</button></div>
        <ConnectPanel v-if="activeSection === 'Connect'" />
        <SectionView v-else :section="activeSection" />
      </section>
      <footer class="statusbar"><span>SHOCKINGVRC REMOTE API V1</span><span><i class="status-dot" :class="{ online: store.connected }"></i>{{ store.connected ? 'HOST STATE LIVE' : 'WAITING FOR HOST' }}</span><span v-if="store.snapshot">REV {{ store.snapshot.bridge_revision }}</span></footer>
    </main>
  </div>
</template>

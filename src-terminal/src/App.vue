<script setup lang="ts">
import { getVersion } from "@tauri-apps/api/app";
import { computed, ref, watch } from "vue";
import ConnectPanel from "./components/ConnectPanel.vue";
import SectionView from "./components/SectionView.vue";
import TerminalButton from "./components/TerminalButton.vue";
import TerminalStatusIndicator from "./components/TerminalStatusIndicator.vue";
import { connectionIndicator, connectionStatusLabel, transportLabel } from "./domain/connection-display.mjs";
import { sectionAfterPairingInvitation } from "./pairing-navigation.mjs";
import { useTerminalStore } from "./stores/terminal";

const store = useTerminalStore();
const clientVersion = ref("…");
const activeSection = ref("Connect");
const sections = [
  { name: "Connect", code: "00" },
  { name: "Status", code: "01" },
  { name: "Zones", code: "02" },
  { name: "Channels", code: "03" },
  { name: "Tuning", code: "04" },
  { name: "Modulation", code: "05" },
  { name: "Presets", code: "06" },
  { name: "Alarm", code: "07" },
  { name: "Setup", code: "08" },
  { name: "Logs", code: "09" },
];
watch(() => store.pairingRequestRevision, (revision) => {
  activeSection.value = sectionAfterPairingInvitation(
    activeSection.value,
    revision,
    store.pendingPairingProfileId,
  );
});
const statusLabel = computed(() => connectionStatusLabel(store.connection.phase));
const isStale = computed(() => store.connection.phase === "reconnecting");
const currentTransportLabel = computed(() => transportLabel(store.connection.transport, store.connection.ice_candidate_pair, store.connection.phase));
const currentConnectionIndicator = computed(() => connectionIndicator(store.connection.phase));

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
        <div><strong>SHOCKING<span>VRC</span></strong><small>REMOTE TERMINAL</small></div>
      </div>

      <div class="nav-label">WORKSPACE</div>
      <nav class="section-nav" aria-label="Terminal sections">
        <button v-for="section in sections" :key="section.name" class="nav-item" :class="{ active: activeSection === section.name }" @click="activeSection = section.name">
          <span>{{ section.name }}</span><small>{{ section.code }}</small>
        </button>
      </nav>

      <div class="sidebar-bottom">
        <div class="sidebar-rule"></div>
        <span class="eyebrow">REMOTE HOST</span>
        <strong class="sidebar-host">{{ store.connection.host_label || 'No active Host' }}</strong>
        <small>{{ store.connection.fingerprint ? store.connection.fingerprint.slice(0, 16) + '…' : 'Pair a confirmed identity' }}</small>
        <div class="build-label">CLIENT {{ clientVersion }} · API V1 · {{ store.connected ? 'HOST ONLINE' : 'HOST IDLE' }}</div>
      </div>
    </aside>

    <main class="main-area">
      <header class="topbar">
        <div class="breadcrumb"><span>TERMINAL</span><b>/</b><strong>{{ activeSection.toUpperCase() }}</strong></div>
        <div class="topbar-right">
          <div class="header-host"><span class="eyebrow">HOST</span><strong>{{ store.connection.host_label || '—' }}</strong></div>
          <span class="transport-label">{{ currentTransportLabel }}</span>
          <span v-if="store.connection.ice_candidate_pair" class="transport-label">ICE PAIR · {{ store.connection.ice_candidate_pair }}</span>
          <TerminalStatusIndicator :state="currentConnectionIndicator" :label="statusLabel" class="header-status" />
          <TerminalButton v-if="store.connection.phase !== 'disconnected' && store.connection.phase !== 'identity_verified'" variant="danger" size="small" @click="store.disconnect">DISCONNECT</TerminalButton>
        </div>
      </header>

      <section class="content-scroll">
        <div class="page-heading">
          <div><span class="eyebrow">REMOTE CONTROL / {{ activeSection.toUpperCase() }}</span><h1>{{ activeSection === 'Connect' ? 'Connect to Host' : activeSection }}</h1></div>
          <div v-if="store.connected" class="host-chip"><div><strong>{{ store.connection.host_label }}</strong><small>{{ store.connection.server_id }}</small></div><small class="host-chip__state">ONLINE</small></div>
        </div>
        <div v-if="isStale" class="session-banner"><span class="banner-label">RECONNECTING</span><div><strong>Reconnecting to the same Host session</strong><small>Cached state is stale. Controls remain unavailable until a fresh snapshot arrives.</small></div></div>
        <div v-if="store.error" class="error-banner"><span class="banner-label">ERROR</span><div><strong>Connection message</strong><small>{{ store.error }}</small></div><button class="close-error" aria-label="Clear connection message" @click="store.error = ''">CLEAR</button></div>
        <ConnectPanel v-if="activeSection === 'Connect'" />
        <SectionView v-else :section="activeSection" />
      </section>
      <footer class="statusbar"><span>SHOCKINGVRC REMOTE API V1</span><span>{{ store.connected ? 'HOST STATE LIVE' : 'WAITING FOR HOST' }}</span><span v-if="store.snapshot">REV {{ store.snapshot.bridge_revision }}</span></footer>
    </main>
  </div>
</template>

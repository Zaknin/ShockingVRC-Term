<script setup lang="ts">
import { getVersion } from "@tauri-apps/api/app";
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import ConnectPanel from "./components/ConnectPanel.vue";
import SectionView from "./components/SectionView.vue";
import TerminalButton from "./components/TerminalButton.vue";
import TerminalStatusIndicator from "./components/TerminalStatusIndicator.vue";
import ThemePage from "./components/ThemePage.vue";
import { connectionIndicator, connectionStatusLabel, transportLabel } from "./domain/connection-display.mjs";
import { displayHostName } from "./domain/host-display-name.mjs";
import { sectionAfterPairingInvitation } from "./pairing-navigation.mjs";
import { applyHostPalette, readSavedTheme, saveTheme } from "./domain/theme-palette.mjs";
import { changeScale, readSavedScale, saveScale, shortcutAction } from "./domain/ui-preferences.mjs";
import { useTerminalStore } from "./stores/terminal";

const store = useTerminalStore();
const clientVersion = ref("…");
const activeSection = ref("Connect");
const selectedTheme = ref(readSavedTheme());
const uiScale = ref(readSavedScale());
const sections = [
  { name: "Connect", code: "1" },
  { name: "Status", code: "2" },
  { name: "Zones", code: "3" },
  { name: "Channels", code: "4" },
  { name: "Tuning", code: "5" },
  { name: "Modulation", code: "6" },
  { name: "Presets", code: "7" },
  { name: "Alarm", code: "8" },
  { name: "Setup", code: "9" },
  { name: "Logs", code: "10" },
  { name: "Theme", code: "11" },
];
const visibleSections = computed(() => sections.filter((section) =>
  section.name !== "Alarm" || store.snapshot?.prefs?.alarm_tab_visible !== false,
));
const activeProfile = computed(() => store.profiles.find((profile) => profile.id === store.connection.profile_id)
  ?? (store.pendingPairingProfile?.id === store.connection.profile_id ? store.pendingPairingProfile : null));
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
const currentHostLabel = computed(() => displayHostName({
  localAlias: activeProfile.value?.label ?? store.connection.host_label,
  hasLocalAlias: activeProfile.value?.label_is_custom,
  hostDisplayName: store.snapshot?.host_display_name ?? activeProfile.value?.host_display_name,
  serverId: store.connection.server_id,
}));
watch(() => [store.connection.profile_id, store.snapshot?.host_display_name] as const, ([profileId, hostDisplayName]) => {
  if (!profileId || !store.snapshot) return;
  store.cacheHostDisplayName(profileId, hostDisplayName).catch((error) => { store.error = String(error); });
});
watch(() => [store.snapshot?.prefs?.alarm_tab_visible, activeSection.value] as const, ([visible, section]) => {
  if (visible === false && section === "Alarm") activeSection.value = "Status";
});

async function applyScale(value: number) {
  uiScale.value = saveScale(value);
  try { await getCurrentWebview().setZoom(uiScale.value / 100); } catch { /* WebView zoom is available in the native Terminal. */ }
}

function handleShortcut(event: KeyboardEvent) {
  const action = shortcutAction(event);
  if (!action) return;
  if (action.startsWith("zoom-")) {
    event.preventDefault();
    if (action === "zoom-reset") void applyScale(100);
    else void applyScale(changeScale(uiScale.value, action === "zoom-in" ? 1 : -1));
    return;
  }
  if (action === "theme") { activeSection.value = "Theme"; return; }
  const sectionIndex = Number(action.slice("section-".length));
  const target = sections[sectionIndex];
  if (Number.isInteger(sectionIndex) && target && visibleSections.value.some((section) => section.name === target.name)) activeSection.value = target.name;
}

watch(selectedTheme, (themeId) => {
  selectedTheme.value = saveTheme(themeId);
  document.documentElement.classList.add("theme-transition");
  applyHostPalette(selectedTheme.value);
  window.setTimeout(() => document.documentElement.classList.remove("theme-transition"), 300);
});

watch(() => store.connected, (connected) => {
  if (connected && activeSection.value === "Connect") activeSection.value = "Status";
});

onMounted(() => {
  applyHostPalette(selectedTheme.value);
  void applyScale(uiScale.value);
  window.addEventListener("keydown", handleShortcut, true);
});
onUnmounted(() => window.removeEventListener("keydown", handleShortcut, true));
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
        <button v-for="section in visibleSections" :key="section.name" class="nav-item" :class="{ active: activeSection === section.name }" @click="activeSection = section.name">
          <span>{{ section.name }}</span><small>{{ section.code }}</small>
        </button>
      </nav>

      <div class="sidebar-bottom">
        <div class="sidebar-rule"></div>
        <span class="eyebrow">REMOTE HOST</span>
        <strong class="sidebar-host">{{ currentHostLabel === '—' ? 'No active Host' : currentHostLabel }}</strong>
        <small>{{ store.connected ? 'ONLINE' : statusLabel }}</small>
        <div class="build-label">CLIENT {{ clientVersion }}</div>
      </div>
    </aside>

    <main class="main-area">
      <header class="topbar">
        <div class="breadcrumb"><span>TERMINAL</span><b>/</b><strong>{{ activeSection.toUpperCase() }}</strong></div>
        <div class="topbar-right">
          <div class="header-host"><span class="eyebrow">HOST</span><strong>{{ currentHostLabel }}</strong></div>
          <span class="transport-label">{{ currentTransportLabel }}</span>
          <TerminalStatusIndicator :state="currentConnectionIndicator" :label="statusLabel" class="header-status" />
          <div class="zoom-control" role="group" aria-label="Interface scale">
            <button aria-label="Decrease interface scale" :disabled="uiScale <= 75" @click="applyScale(changeScale(uiScale, -1))">−</button>
            <button aria-label="Reset interface scale to 100 percent" @click="applyScale(100)">{{ uiScale }}%</button>
            <button aria-label="Increase interface scale" :disabled="uiScale >= 175" @click="applyScale(changeScale(uiScale, 1))">+</button>
          </div>
          <TerminalButton v-if="store.connection.phase !== 'disconnected' && store.connection.phase !== 'identity_verified'" variant="danger" size="small" @click="store.disconnect">DISCONNECT</TerminalButton>
        </div>
      </header>

      <section class="content-scroll">
        <div class="page-heading">
          <div><span class="eyebrow">REMOTE CONTROL / {{ activeSection.toUpperCase() }}</span><h1>{{ activeSection === 'Connect' ? 'Connect to Host' : activeSection }}</h1></div>
        </div>
        <div v-if="isStale" class="session-banner"><span class="banner-label">RECONNECTING</span><div><strong>Reconnecting to the same Host session</strong><small>Cached state is stale. Controls remain unavailable until a fresh snapshot arrives.</small></div></div>
        <div v-if="store.error" class="error-banner"><span class="banner-label">ERROR</span><div><strong>Connection message</strong><small>{{ store.error }}</small></div><button class="close-error" aria-label="Clear connection message" @click="store.error = ''">CLEAR</button></div>
        <ConnectPanel v-if="activeSection === 'Connect'" />
        <ThemePage v-else-if="activeSection === 'Theme'" :selected-theme="selectedTheme" @select="selectedTheme = $event" />
        <SectionView v-else :section="activeSection" />
      </section>
      <footer class="statusbar"><span>TERMINAL</span><span>{{ store.connected ? 'ONLINE' : 'OFFLINE' }}</span><span v-if="store.snapshot">REV {{ store.snapshot.bridge_revision }}</span></footer>
    </main>
  </div>
</template>

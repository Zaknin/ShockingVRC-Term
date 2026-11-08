<script setup lang="ts">
import { getVersion } from "@tauri-apps/api/app";
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import ConnectPanel from "./components/ConnectPanel.vue";
import GraphWorkspace from "./components/GraphWorkspace.vue";
import SectionView from "./components/SectionView.vue";
import TerminalButton from "./components/TerminalButton.vue";
import TerminalStatusIndicator from "./components/TerminalStatusIndicator.vue";
import ThemePage from "./components/ThemePage.vue";
import { connectionIndicator, connectionStatusLabel, transportLabel } from "./domain/connection-display.mjs";
import { displayHostName } from "./domain/host-display-name.mjs";
import { sectionAfterPairingInvitation } from "./pairing-navigation.mjs";
import { canManuallyReconnect, DEFAULT_TERMINAL_SECTION, sectionAfterConnectionUpdate } from "./domain/session-control.mjs";
import { applyHostPalette, readSavedTheme, saveTheme } from "./domain/theme-palette.mjs";
import { changeScale, readSavedScale, saveScale, SECTION_NAVIGATION, shortcutAction } from "./domain/ui-preferences.mjs";
import { useTerminalStore } from "./stores/terminal";

const store = useTerminalStore();
const clientVersion = ref("…");
const activeSection = ref(DEFAULT_TERMINAL_SECTION);
const selectedTheme = ref(readSavedTheme());
const uiScale = ref(readSavedScale());
const reconnectBusy = ref(false);
const sections = SECTION_NAVIGATION;
const visibleSections = sections;
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
const terminalEndPhases = new Set(["user_disconnected", "host_kicked", "session_revoked", "host_unavailable", "transport_lost"]);
const showSessionEnd = computed(() => terminalEndPhases.has(store.connection.phase));
const showReconnect = computed(() => canManuallyReconnect(store.connection.phase));
const showDisconnect = computed(() => ["connecting", "identity_verified", "online", "reconnecting"].includes(store.connection.phase));
const currentTransportLabel = computed(() => transportLabel(store.connection.transport, store.connection.ice_candidate_pair, store.connection.phase));
const currentConnectionIndicator = computed(() => connectionIndicator(store.connection.phase));
const currentHostLabel = computed(() => displayHostName({
  localAlias: activeProfile.value?.label ?? store.connection.host_label,
  hasLocalAlias: activeProfile.value?.label_is_custom,
  hostDisplayName: store.snapshot?.host_display_name ?? activeProfile.value?.host_display_name,
  serverId: store.connection.server_id,
}));
watch(() => [store.connection.phase, store.connection.profile_id, store.snapshot?.host_display_name] as const,
  ([phase, profileId, hostDisplayName]) => {
    if (phase !== "online" || !profileId || !store.snapshot || !hostDisplayName?.trim()) return;
    store.cacheHostDisplayName(profileId, hostDisplayName).catch((error) => {
      const message = String(error);
      if (message.includes("A Host display name can only be cached from its authenticated active session")) return;
      if (store.connection.phase === "online" && store.connection.profile_id === profileId) store.error = message;
    });
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
  if (action === "graph") { activeSection.value = "Graph"; return; }
  const sectionIndex = Number(action.slice("section-".length));
  const target = sections[sectionIndex];
  if (Number.isInteger(sectionIndex) && target && visibleSections.some((section) => section.name === target.name)) activeSection.value = target.name;
}

watch(selectedTheme, (themeId) => {
  selectedTheme.value = saveTheme(themeId);
  document.documentElement.classList.add("theme-transition");
  applyHostPalette(selectedTheme.value);
  window.setTimeout(() => document.documentElement.classList.remove("theme-transition"), 300);
});

watch(() => store.connection.phase, (phase) => {
  activeSection.value = sectionAfterConnectionUpdate(activeSection.value, phase);
});

watch(() => [activeSection.value, store.connection.phase, store.snapshotContext?.session_generation,
  store.snapshotContext?.transport_generation, store.capabilities?.features.join("|")] as const,
([section, phase]) => {
  if (section === "Graph" && phase === "online") void store.fetchGraphWorkspace();
}, { immediate: true });

async function reconnectCurrentHost() {
  const profileId = store.connection.profile_id;
  if (!profileId || reconnectBusy.value || !canManuallyReconnect(store.connection.phase)) return;
  reconnectBusy.value = true;
  try {
    await store.disconnect();
    await store.connectProfile(profileId);
    activeSection.value = "Status";
  } catch (error) {
    store.error = String(error);
  } finally {
    reconnectBusy.value = false;
  }
}

function handleGraphOperation(result: Promise<unknown>) {
  void result.catch((cause) => { store.graphDraftError = String(cause); });
}

function applyGraphPreset(presetId: string, mode: "replace" | "insert", position: [number, number]) {
  handleGraphOperation(store.applyGraphPreset(presetId, mode, position));
}

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
          <TerminalButton v-if="showDisconnect" variant="danger" size="small" @click="store.disconnect">DISCONNECT</TerminalButton>
        </div>
      </header>

      <section class="content-scroll">
        <div class="page-heading">
          <div><span class="eyebrow">REMOTE CONTROL / {{ activeSection.toUpperCase() }}</span><h1>{{ activeSection === 'Connect' ? 'Connect to Host' : activeSection }}</h1></div>
        </div>
        <div v-if="store.snapshot?.configuration_state === 'preview_only'" class="session-banner">
          <span class="banner-label">DEVICE OFFLINE</span>
          <div><strong>PREVIEW ONLY · NOT APPLIED TO HARDWARE</strong><small>Host changes remain staged in memory and will not auto-apply when a Coyote connects.</small></div>
        </div>
        <div v-if="store.snapshot?.engine_mode === 'graph'" class="session-banner">
          <span class="banner-label">GRAPH MODE</span>
          <div><strong>Host Graph output is active</strong><small>Terminal does not edit Graphs or send Graph output commands. Classic configuration controls are disabled; alarm and Host preference operations remain available.</small></div>
        </div>
        <div v-if="isStale" class="session-banner"><span class="banner-label">RECONNECTING</span><div><strong>Reconnecting to the same Host session</strong><small>Cached state is stale. Controls remain unavailable until a fresh snapshot arrives.</small></div></div>
        <div v-if="showSessionEnd" class="session-banner session-end-banner">
          <span class="banner-label">{{ store.connection.phase.replaceAll('_', ' ').toUpperCase() }}</span>
          <div class="session-end-content">
            <strong>{{ store.connection.message || 'The Host session ended.' }}</strong>
            <small v-if="store.connection.phase === 'session_revoked'">Pair this Host again with a new invitation to restore access.</small>
          </div>
          <div class="session-actions">
            <TerminalButton v-if="showReconnect" variant="primary" size="small" :disabled="reconnectBusy" @click="reconnectCurrentHost">RECONNECT</TerminalButton>
            <TerminalButton v-else-if="store.connection.phase === 'session_revoked'" variant="primary" size="small" @click="activeSection = 'Connect'">PAIR THIS HOST</TerminalButton>
            <TerminalButton variant="subtle" size="small" @click="activeSection = 'Connect'">CHOOSE ANOTHER HOST</TerminalButton>
          </div>
        </div>
        <div v-if="store.error" class="error-banner"><span class="banner-label">ERROR</span><div><strong>Connection message</strong><small>{{ store.error }}</small></div><button class="close-error" aria-label="Clear connection message" @click="store.error = ''">CLEAR</button></div>
        <ConnectPanel v-if="activeSection === 'Connect'" />
        <ThemePage v-else-if="activeSection === 'Theme'" :selected-theme="selectedTheme" @select="selectedTheme = $event" />
        <GraphWorkspace v-else-if="activeSection === 'Graph'"
          :engine-mode="store.snapshot?.engine_mode"
          :mode-change="store.graphSession.modeChange"
          :connected="store.connected"
          :capabilities="store.capabilities?.features ?? []"
          :document="store.graphDocument"
          :draft="store.graphDraft"
          :node-catalog="store.graphNodeCatalog"
          :presets="store.graphPresets"
          :load-status="store.graphLoadStatus"
          :load-error="store.graphLoadError"
          :draft-error="store.graphDraftError"
          :apply-result="store.graphApplyResult"
          :draft-loading="store.graphDraftLoading"
          :can-undo="store.graphCanUndo"
          :can-redo="store.graphCanRedo"
          @refresh="store.fetchGraphWorkspace(true)"
          @request-mode="store.setEngineMode"
          @update-draft="store.setGraphDraft"
          @save-draft="handleGraphOperation(store.saveGraphDraft())"
          @reload-draft="handleGraphOperation(store.reloadSavedGraphDraft())"
          @discard-draft="store.discardGraphDraftAndReloadHost"
          @import-draft="handleGraphOperation(store.importGraphDraft($event))"
          @transform-draft="handleGraphOperation(store.transformGraphDraft($event))"
          @apply-preset="applyGraphPreset"
          @apply-draft="handleGraphOperation(store.applyGraphDraft())"
          @validate-draft="handleGraphOperation(store.validateGraphDraft())"
          @undo="store.restoreGraphHistory('undo')"
          @redo="store.restoreGraphHistory('redo')" />
        <SectionView v-else :section="activeSection" :host-label="currentHostLabel" />
      </section>
      <footer class="statusbar"><span>TERMINAL</span><span>{{ store.connected ? 'ONLINE' : 'OFFLINE' }}</span></footer>
    </main>
  </div>
</template>

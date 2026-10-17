<script setup lang="ts">
import { computed, reactive, ref, watch } from "vue";
import KeyValueTable from "./KeyValueTable.vue";
import ChannelEditor from "./ChannelEditor.vue";
import ModulationWorkspace from "./ModulationWorkspace.vue";
import TuningEditor from "./TuningEditor.vue";
import TerminalButton from "./TerminalButton.vue";
import TerminalLogViewer from "./TerminalLogViewer.vue";
import TerminalPanel from "./TerminalPanel.vue";
import TerminalSegmentedControl from "./TerminalSegmentedControl.vue";
import TerminalSlider from "./TerminalSlider.vue";
import TerminalWaveform from "./TerminalWaveform.vue";
import TerminalProgressBar from "./TerminalProgressBar.vue";
import TerminalStatusIndicator from "./TerminalStatusIndicator.vue";
import { entryIndex, presetAuthorLabel, presetEntryName, presetModulationCount, presetNumberArray } from "../domain/catalog-projection.mjs";
import { connectionIndicator, connectionStatusLabel, transportLabel } from "../domain/connection-display.mjs";
import { formatNumber } from "../domain/preview-math.mjs";
import { formatTimestamp } from "../domain/timestamp-format.mjs";
import { resetCalibrationOperations, zoneThresholdUiToWire, zoneThresholdWireToUi } from "../domain/zone-calibration.mjs";
import { useTerminalStore } from "../stores/terminal";
import type { JsonValue } from "../protocol/types";
import type { AlarmConfigInput, AllowedOperation, ChannelId, ZoneType } from "../protocol/operations";

const props = defineProps<{ section: string }>();
const store = useTerminalStore();
const catalogKind = ref<"presets" | "zone_presets">("presets");
const presetSearch = ref("");
const catalogScope = ref<"ALL" | "MINE">("ALL");
const savePresetChannel = ref<ChannelId>("A");
const presetName = ref("");
const presetAuthor = ref("");
const zonePresetName = ref("");
const lastQueued = ref("");
const snapshot = computed(() => store.snapshot);
const channelItems = computed(() => snapshot.value ? [
  { id: "A" as const, channel: snapshot.value.config.channel_a },
  { id: "B" as const, channel: snapshot.value.config.channel_b },
] : []);
const statusObject = computed(() => asRecord(snapshot.value?.status));
const alarmObject = computed(() => asRecord(snapshot.value?.alarm));
const alarmConfig = computed(() => asRecord(alarmObject.value.config));
const alarmStatus = computed(() => alarmObject.value.status ?? null);
const catalog = computed(() => store.catalogs[catalogKind.value]);
const filteredCatalogEntries = computed(() => (catalog.value?.entries ?? []).filter((entry, index) => {
  const query = presetSearch.value.trim().toLowerCase();
  const payload = asRecord(entry.preset);
  const text = [presetEntryName(entry), entry.author, payload.author, entry.description, payload.description]
    .filter((value) => typeof value === "string").join(" ").toLowerCase();
  return (catalogScope.value === "ALL" || entry.user === true) && (!query || text.includes(query)) && entryIndex(entry, index) >= 0;
}));
const logs = computed(() => store.logs);
const terminalDiagnostics = computed(() => store.terminalDiagnostics);
const writeEnabled = computed(() => store.connected && !!snapshot.value);
const snapshotIdentity = computed(() => store.verifiedIdentity);
const snapshotReceivedLabel = computed(() => store.snapshotReceivedAt == null ? "NOT RECEIVED" : new Date(store.snapshotReceivedAt).toLocaleTimeString());

const alarmDraft = reactive<AlarmConfigInput>({
  enabled: false,
  hour: 0,
  minute: 0,
  channels: "Both",
  start_strength: 0,
  peak_strength: 0,
  ramp_secs: 0,
  max_duration_secs: 30,
  repeats: 1,
  pulse_on_ms: 0,
  pulse_off_ms: 0,
  snooze_mins: 1,
});

watch(() => JSON.stringify(alarmConfig.value), () => {
  const value = alarmConfig.value;
  Object.assign(alarmDraft, value);
}, { immediate: true });
function asRecord(value: JsonValue | undefined | null): Record<string, JsonValue> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, JsonValue> : {};
}

function channelStatus(key: "channel_a" | "channel_b") {
  return asRecord(statusObject.value[key]);
}

function numberValue(value: JsonValue | undefined): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

const activeDevice = computed(() => snapshot.value?.devices[snapshot.value.active_device] ?? null);
const deviceLink = computed(() => typeof statusObject.value.device_connected === "boolean"
  ? statusObject.value.device_connected : activeDevice.value?.connected ?? null);
const currentTransport = computed(() => transportLabel(store.connection.transport, store.connection.ice_candidate_pair, store.connection.phase));
const coyoteLabel = computed(() => deviceLink.value === true ? "CONNECTED" : activeDevice.value?.reconnecting ? "RECONNECTING" : deviceLink.value === false ? "OFFLINE" : "NOT REPORTED");
const coyoteIndicator = computed(() => deviceLink.value === true ? "ok" : activeDevice.value?.reconnecting ? "warn" : deviceLink.value === false ? "error" : "idle");

function numberFrom(event: Event): number {
  return Number((event.target as HTMLInputElement | HTMLSelectElement).value);
}

function boolFrom(event: Event): boolean {
  return (event.target as HTMLInputElement).checked;
}

function supports(op: AllowedOperation["op"]): boolean {
  return store.capabilities?.operations.includes(op) ?? false;
}

async function send(operation: AllowedOperation) {
  if (!writeEnabled.value) {
    store.error = "Connect and synchronize with the Host before sending an operation.";
    return;
  }
  if (!supports(operation.op)) {
    store.error = `The connected Host does not advertise ${operation.op}.`;
    return;
  }
  lastQueued.value = "";
  try {
    const ack = await store.submitOperation(operation);
    lastQueued.value = `Host accepted request ${ack.request_id.slice(0, 8)} into its command queue. This does not confirm device execution; the next Host snapshot remains authoritative.`;
  } catch (error) {
    store.error = String(error);
  }
}

function addAvatarZone(channel: ChannelId, zoneType: string, name: string) {
  const accepted: ZoneType[] = ["Any", "Pen", "Orf", "Touch", "DGB"];
  if (!accepted.includes(zoneType as ZoneType)) {
    store.error = "Host reported an unsupported avatar zone type.";
    return;
  }
  void send({ op: "zones.add", args: { channel, zone_id: { zone_type: zoneType as ZoneType, name } } });
}

async function resetCalibration(channel: ChannelId, index: number) {
  for (const operation of resetCalibrationOperations(channel, index)) await send(operation);
}

async function loadCatalog(kind = catalogKind.value) {
  try { await store.fetchCatalog(kind); } catch (error) { store.error = String(error); }
}

async function refreshCatalog() {
  if (catalogKind.value === "presets") await send({ op: "presets.refresh", args: {} });
  else await send({ op: "zone_presets.reload", args: {} });
}

function saveUserPreset() {
  void send({ op: "presets.save_user", args: { channel: savePresetChannel.value, name: presetName.value, author: presetAuthor.value } });
}

function saveZonePreset() {
  void send({ op: "zone_presets.save", args: { name: zonePresetName.value } });
}

function setPreference(op: "prefs.set_auto_save" | "prefs.set_alarm_tab_visible", event: Event) {
  void send({ op, args: { enabled: boolFrom(event) } });
}

function alarmNumber(key: keyof AlarmConfigInput, event: Event) {
  (alarmDraft[key] as number) = numberFrom(event);
}

function channelStrength(key: "channel_a" | "channel_b"): number | null {
  const value = channelStatus(key).strength;
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function activeZoneCount(key: "channel_a" | "channel_b"): number | null {
  const zones = channelStatus(key).active_zones;
  return Array.isArray(zones) ? zones.length : null;
}

function kinematicsValue(key: "channel_a" | "channel_b", field: "depth" | "speed" | "acc" | "recoil"): number | null {
  return numberValue(asRecord(channelStatus(key).kinematics)[field]);
}

function hasKinematics(key: "channel_a" | "channel_b"): boolean {
  return (["depth", "speed", "acc", "recoil"] as const).some((field) => kinematicsValue(key, field) != null);
}

async function loadLogs(cursor?: string | null) {
  try { await store.fetchLogs(cursor); } catch (error) { store.error = String(error); }
}

async function loadTerminalDiagnostics() {
  try { await store.fetchTerminalDiagnostics(); } catch (error) { store.error = String(error); }
}

async function clearTerminalDiagnostics() {
  try { await store.clearTerminalDiagnostics(); } catch (error) { store.error = String(error); }
}

async function copyLogLines(lines: string[]) {
  if (!lines.length) return;
  try {
    await navigator.clipboard.writeText(lines.join("\n"));
  } catch (error) {
    store.error = `Could not copy log lines: ${String(error)}`;
  }
}

watch(() => props.section, (section) => {
  if (section === "Logs") void loadTerminalDiagnostics();
  if (section === "Zones") void loadCatalog("zone_presets");
  if (section === "Presets") void loadCatalog();
}, { immediate: true });
watch(catalogKind, (kind) => { if (props.section === "Presets") void loadCatalog(kind); });

async function refreshState() {
  try { await store.fetchState(); } catch (error) { store.error = String(error); }
}
</script>

<template>
  <template v-if="section === 'Logs'">
    <TerminalPanel kicker="HOST LOG BUFFER" title="Sanitized Host events" :suffix="logs ? `${logs.lines.length} LINES` : 'NOT LOADED'">
      <div class="button-row compact-row section-gap-small">
        <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled" @click="loadLogs()">LOAD LATEST</TerminalButton>
        <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !logs?.next_cursor" @click="loadLogs(logs?.next_cursor)">NEXT PAGE</TerminalButton>
      </div>
      <TerminalLogViewer :lines="logs?.lines ?? []" :empty-label="logs?.lines.length ? 'No Host lines match this filter.' : 'Load a page to read sanitized Host logs.'" title="HOST / FILTERED VIEW" @copy="copyLogLines" />
      <div v-if="logs" class="state-footer">Cursor {{ logs.cursor }} · {{ logs.lines.length }} returned · timestamps appear when present in the Host line.</div>
    </TerminalPanel>
    <TerminalPanel class="section-gap" kicker="TERMINAL ICE / TRANSPORT" title="Local diagnostic log" :suffix="`${terminalDiagnostics?.lines.length ?? 0} LINES`">
      <div class="button-row compact-row section-gap-small">
        <TerminalButton variant="danger" size="small" :disabled="!terminalDiagnostics?.lines.length" @click="clearTerminalDiagnostics">CLEAR LOCAL BUFFER</TerminalButton>
        <TerminalButton variant="subtle" size="small" @click="loadTerminalDiagnostics">REFRESH</TerminalButton>
      </div>
      <TerminalLogViewer :lines="terminalDiagnostics?.lines ?? []" title="TERMINAL / ICE & RELAY" @copy="copyLogLines" />
      <div class="state-footer">Detailed ICE diagnostics {{ terminalDiagnostics?.detailed_ice_enabled ? 'enabled' : 'disabled' }}. The local buffer can be cleared independently of Host logs.</div>
      <div v-if="!terminalDiagnostics?.lines.length" class="empty-note">No Terminal transport events have been recorded. Set SHOCKINGVRC_ICE_DIAGNOSTICS=1 before launching Terminal for candidate-level details.</div>
    </TerminalPanel>
  </template>

  <div v-else-if="!snapshot" class="panel empty-state">
    <h2>{{ store.connection.phase === 'reconnecting' ? 'Resuming Host session' : 'Host state is not connected' }}</h2>
    <p>Open Connect, verify the saved SPKI fingerprint, and sign in to read this section from the Host.</p>
  </div>

  <template v-else>
    <div v-if="!writeEnabled && store.connection.phase !== 'reconnecting'" class="session-banner"><span class="banner-label">CACHED</span><div><strong>Showing the last Host snapshot</strong><small>Controls are disabled until the Terminal reconnects and receives a fresh snapshot.</small></div></div>
    <div v-if="lastQueued" class="queue-banner"><span class="banner-label">QUEUED</span><small>{{ lastQueued }}</small></div>

    <template v-if="section === 'Status'">
      <div class="status-page">
        <div class="status-page__top">
          <TerminalPanel kicker="REMOTE SESSION" title="Host connection" :suffix="`REV ${snapshot.bridge_revision}`">
            <div class="status-connection-row">
              <TerminalStatusIndicator :state="connectionIndicator(store.connection.phase)" :label="connectionStatusLabel(store.connection.phase)" />
              <span class="term-tag">{{ currentTransport }}</span>
              <span v-if="store.connection.ice_candidate_pair" class="status-pair">{{ store.connection.ice_candidate_pair }}</span>
            </div>
            <div class="status-facts">
              <div><span>HOST</span><b>{{ store.connection.host_label ?? snapshotIdentity?.host_label ?? '—' }}</b></div>
              <div><span>SERVER ID</span><code>{{ store.connection.server_id ?? snapshotIdentity?.server_id ?? '—' }}</code></div>
              <div><span>SESSION PROFILE</span><code>{{ store.connection.profile_id ?? snapshotIdentity?.profile_id ?? '—' }}</code></div>
              <div><span>LAST HEARTBEAT</span><code>{{ formatTimestamp(store.connection.last_heartbeat) }}</code></div>
              <div><span>SNAPSHOT RECEIVED</span><code>{{ snapshotReceivedLabel }}</code></div>
              <div><span>BRIDGE REVISION</span><code>{{ snapshot.bridge_revision }}</code></div>
            </div>
          </TerminalPanel>

          <TerminalPanel kicker="HOST RUNTIME" title="Coyote · VRChat · OSC">
            <div class="status-runtime-grid">
              <div>
                <span class="eyebrow">COYOTE DEVICE LINK</span>
                <TerminalStatusIndicator :state="coyoteIndicator" :label="coyoteLabel" :detail="activeDevice?.name || snapshot.active_device_mac || 'No active device'" />
                <small v-if="activeDevice">{{ activeDevice.mac }} · {{ activeDevice.battery == null ? 'battery —' : `battery ${activeDevice.battery}%` }}</small>
              </div>
              <div>
                <span class="eyebrow">VRCHAT / OSC</span>
                <TerminalStatusIndicator :state="snapshot.vrchat.connected ? 'ok' : 'idle'" :label="snapshot.vrchat.connected ? 'CONNECTED' : 'NOT DETECTED'" />
                <small>OSC {{ snapshot.vrchat.osc_port || '—' }} · OSCQuery {{ snapshot.vrchat.oscquery_port ?? '—' }}</small>
              </div>
            </div>
            <div class="button-row section-gap-small">
              <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled" @click="refreshState">REFRESH SNAPSHOT</TerminalButton>
            </div>
          </TerminalPanel>
        </div>

        <TerminalPanel class="section-gap" kicker="LIVE OUTPUT STATE" title="Channels A / B" suffix="HOST SNAPSHOT">
          <div class="status-channel-grid">
            <article v-for="channel in [{id:'A',key:'channel_a'},{id:'B',key:'channel_b'}] as const" :key="channel.id" class="status-channel">
              <div class="status-channel__head"><strong>CHANNEL {{ channel.id }}</strong><TerminalStatusIndicator :state="deviceLink === true ? 'ok' : deviceLink === false ? 'warn' : 'idle'" :label="`STRENGTH ${channelStrength(channel.key) ?? '—'}`" /></div>
              <div class="status-channel__metrics">
                <div><span>RAW LEVEL</span><b>{{ numberValue(channelStatus(channel.key).raw_level) == null ? '—' : formatNumber(numberValue(channelStatus(channel.key).raw_level)!, 3) }}</b></div>
                <div><span>ACTIVE ZONES</span><b>{{ activeZoneCount(channel.key) ?? '—' }}</b></div>
              </div>
              <div class="status-channel__kinematics">
                <TerminalProgressBar v-for="field in ['depth','speed','acc','recoil'] as const" :key="field" :label="field.toUpperCase()" :value="kinematicsValue(channel.key, field)" />
              </div>
              <p v-if="!hasKinematics(channel.key)" class="empty-note">No channel kinematics in the Host status payload.</p>
            </article>
          </div>
        </TerminalPanel>

        <TerminalPanel class="section-gap" kicker="DEVICE INVENTORY" title="Host devices" :suffix="`${snapshot.devices.length} DEVICES`">
          <div v-if="snapshot.devices.length" class="device-list">
            <article v-for="(device, index) in snapshot.devices" :key="device.mac" class="device-row">
              <TerminalStatusIndicator :state="device.connected ? 'ok' : device.reconnecting ? 'warn' : 'idle'" :label="device.reconnecting ? 'RECONNECTING' : device.connected ? 'CONNECTED' : 'OFFLINE'" />
              <div class="grow"><strong>{{ device.name || 'Unnamed device' }}</strong><small>{{ device.mac }}</small></div>
              <span class="battery">{{ device.battery == null ? '—' : `${device.battery}%` }}</span>
              <span v-if="index === snapshot.active_device" class="term-tag">ACTIVE</span>
              <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !supports('devices.switch')" @click="send({ op: 'devices.switch', args: { index } })">SELECT</TerminalButton>
            </article>
          </div><div v-else class="empty-note">The Host currently reports no devices.</div>
        </TerminalPanel>
      </div>
    </template>

    <template v-else-if="section === 'Zones'">
      <div class="zone-editor-grid">
        <TerminalPanel v-for="item in channelItems" :key="item.id" :kicker="`CHANNEL ${item.id} MAPPINGS`" title="Configured zones" :suffix="`${item.channel.zones.length} ZONES`">
          <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !supports('zones.add_all_from_avatar')" @click="send({ op: 'zones.add_all_from_avatar', args: { channel: item.id } })">ADD ALL DISCOVERED TO {{ item.id }}</TerminalButton>
          <div v-if="item.channel.zones.length" class="zone-list section-gap-small">
            <article v-for="(zone, index) in item.channel.zones" :key="`${zone.zone_type}-${zone.name}`" class="zone-calibration">
              <div class="grow"><strong :title="zone.name">{{ zone.name }}</strong><small>{{ zone.zone_type }} · mapping {{ index + 1 }}</small>
                <TerminalSegmentedControl :value="zone.mode" :options="['Depth','Speed','Acc','Recoil']" :disabled="!writeEnabled || !supports('zones.set_mode')" label="Zone input mode"
                  @change="send({ op: 'zones.set_mode', args: { channel: item.id, index, mode: $event as 'Depth' | 'Speed' | 'Acc' | 'Recoil' } })" />
              </div>
              <TerminalSlider label="Scale" :value="zone.scale" :min="0" :max="100" :step="1" :revision="snapshot.bridge_revision" units="%" :disabled="!writeEnabled || !supports('zones.set_scale')"
                @commit="send({ op: 'zones.set_scale', args: { channel: item.id, index, value: Math.round($event) } })" />
              <TerminalSlider label="Min threshold" :value="zoneThresholdWireToUi(zone.min_threshold)" :min="0.01" :max="1" :step="0.01" :precision="2" :fixed-precision="true" :revision="snapshot.bridge_revision" :disabled="!writeEnabled || !supports('zones.set_threshold_min')"
                @commit="send({ op: 'zones.set_threshold_min', args: { channel: item.id, index, value: zoneThresholdUiToWire($event) } })" />
              <TerminalSlider label="Max threshold" :value="zoneThresholdWireToUi(zone.max_threshold)" :min="0.01" :max="1" :step="0.01" :precision="2" :fixed-precision="true" :revision="snapshot.bridge_revision" :disabled="!writeEnabled || !supports('zones.set_threshold_max')"
                @commit="send({ op: 'zones.set_threshold_max', args: { channel: item.id, index, value: zoneThresholdUiToWire($event) } })" />
              <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !supports('zones.set_scale') || !supports('zones.set_threshold_min') || !supports('zones.set_threshold_max')" @click="resetCalibration(item.id, index)">RESET CALIBRATION</TerminalButton>
              <TerminalButton variant="danger" size="small" :disabled="!writeEnabled || !supports('zones.remove')" @click="send({ op: 'zones.remove', args: { channel: item.id, index } })">REMOVE</TerminalButton>
            </article>
          </div><div v-else class="empty-note">The Host reports no configured zones for channel {{ item.id }}.</div>
        </TerminalPanel>
      </div>
      <TerminalPanel class="section-gap" kicker="VRCHAT / OSC DISCOVERY" title="Detected avatar contacts" :suffix="`${snapshot.avatar_zones.length} CONTACTS`">
        <div v-if="snapshot.avatar_zones.length" class="zone-list">
          <article v-for="zone in snapshot.avatar_zones" :key="`${zone.zone_type}-${zone.id}`" class="zone-row">
            <div class="grow"><strong :title="zone.id">{{ zone.id }}</strong><small>{{ zone.zone_type }}{{ zone.is_tps ? ' · TPS' : '' }}</small></div>
            <span class="telemetry-inline">L {{ formatNumber(zone.level, 2) }} · V {{ formatNumber(zone.velocity, 2) }} · A {{ formatNumber(zone.acceleration, 2) }} · R {{ formatNumber(zone.recoil, 2) }}</span>
            <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !supports('zones.add')" @click="addAvatarZone('A', zone.zone_type, zone.id)">ADD TO A</TerminalButton>
            <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !supports('zones.add')" @click="addAvatarZone('B', zone.zone_type, zone.id)">ADD TO B</TerminalButton>
          </article>
        </div><div v-else class="empty-note">The Host has not reported discovered avatar zones.</div>
      </TerminalPanel>
      <TerminalPanel class="section-gap" kicker="HOST CATALOG" title="Zone sets" :suffix="`${store.catalogs.zone_presets?.entries.length ?? 0} SAVED`">
        <div class="button-row"><TerminalButton variant="subtle" size="small" :disabled="!writeEnabled" @click="loadCatalog('zone_presets')">LOAD ZONE SETS</TerminalButton><TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !supports('zone_presets.reload')" @click="send({ op: 'zone_presets.reload', args: {} })">RELOAD HOST FILE</TerminalButton></div>
        <div v-if="store.catalogs.zone_presets?.entries.length" class="catalog-list section-gap-small">
          <article v-for="(entry, index) in store.catalogs.zone_presets.entries" :key="String(entry.id ?? index)" class="catalog-row">
            <span class="catalog-index">{{ entryIndex(entry, index) }}</span><div class="grow"><strong>{{ presetEntryName(entry) }}</strong><small>{{ entry.description ?? asRecord(entry.preset).description ?? 'Host zone set' }}</small></div>
            <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !supports('zone_presets.apply')" @click="send({ op: 'zone_presets.apply', args: { index: entryIndex(entry, index) } })">APPLY SET</TerminalButton>
            <TerminalButton variant="danger" size="small" :disabled="!writeEnabled || !supports('zone_presets.delete')" @click="send({ op: 'zone_presets.delete', args: { index: entryIndex(entry, index) } })">DELETE</TerminalButton>
          </article>
        </div><div v-else class="empty-note">No zone sets were returned by the Host catalog.</div>
        <div class="field-pair section-gap-small"><label>Save current mappings<input v-model="zonePresetName" maxlength="256" placeholder="Zone set name" /></label><TerminalButton variant="primary" :disabled="!writeEnabled || !supports('zone_presets.save') || !zonePresetName" @click="saveZonePreset">SAVE ZONE SET</TerminalButton></div>
      </TerminalPanel>
    </template>

    <template v-else-if="section === 'Channels'">
      <div class="channel-editor-grid">
        <ChannelEditor v-for="item in channelItems" :key="item.id" :channel="item.id" :config="item.channel" :snapshot="snapshot"
          :write-enabled="writeEnabled" :supports="supports" :strength="channelStrength(item.id === 'A' ? 'channel_a' : 'channel_b')" @send="send" />
      </div>
    </template>

    <template v-else-if="section === 'Tuning'">
      <TuningEditor :snapshot="snapshot" :status="snapshot.status" :write-enabled="writeEnabled" :supports="supports" @send="send" />
    </template>

    <template v-else-if="section === 'Modulation'">
      <ModulationWorkspace :snapshot="snapshot" :write-enabled="writeEnabled" :supports="supports" @send="send" />
    </template>

    <template v-else-if="section === 'Presets'">
      <TerminalPanel kicker="HOST-OWNED ORDER / INDEX" :title="catalogKind === 'presets' ? 'Waveform preset catalog' : 'Zone set catalog'" :suffix="`${catalog?.entries.length ?? 0} ENTRIES`">
        <div class="catalog-controls">
          <select v-model="catalogKind" aria-label="Preset catalog"><option value="presets">WAVEFORMS</option><option value="zone_presets">ZONE SETS</option></select>
          <input v-model="presetSearch" aria-label="Search presets" placeholder="Search name, author, description…" />
          <TerminalSegmentedControl :value="catalogScope" :options="['ALL','MINE']" label="Preset ownership filter" @change="catalogScope = $event as 'ALL' | 'MINE'" />
          <TerminalButton variant="subtle" :disabled="!writeEnabled" @click="loadCatalog()">LOAD FROM HOST</TerminalButton>
          <TerminalButton variant="subtle" :disabled="!writeEnabled || !supports(catalogKind === 'presets' ? 'presets.refresh' : 'zone_presets.reload')" @click="refreshCatalog">REFRESH HOST</TerminalButton>
        </div>
        <p class="microcopy">Catalog order and operation indices come from the Host. Applying a waveform changes frequency, intensity, and modulation only; Host limits and zone mappings are untouched.</p>
        <div v-if="filteredCatalogEntries.length" class="catalog-list">
          <article v-for="(entry, index) in filteredCatalogEntries" :key="String(entry.id ?? entryIndex(entry, index))" class="catalog-card">
            <div class="catalog-card__info"><strong>{{ entryIndex(entry, index) }} · {{ presetEntryName(entry) }} <span v-if="entry.user" class="term-tag">MINE</span></strong>
              <small>BY {{ presetAuthorLabel(entry) }} · {{ entry.description ?? asRecord(entry.preset).description ?? '' }}</small>
              <small v-if="catalogKind === 'presets'">{{ presetModulationCount(entry) }} modulation slots</small>
            </div>
            <template v-if="catalogKind === 'presets'">
              <TerminalWaveform v-if="presetNumberArray(entry,'frequency') && presetNumberArray(entry,'intensity')" :frequency="presetNumberArray(entry,'frequency')!" :intensity="presetNumberArray(entry,'intensity')!" :label="`${presetEntryName(entry)} waveform preview`" />
              <div v-else class="empty-note">Waveform preview not supplied by Host.</div>
            </template>
            <div class="button-row compact-row">
              <template v-if="catalogKind === 'presets'">
                <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !supports('presets.apply')" @click="send({ op: 'presets.apply', args: { channel: 'A', index: entryIndex(entry, index) } })">APPLY A</TerminalButton>
                <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !supports('presets.apply')" @click="send({ op: 'presets.apply', args: { channel: 'B', index: entryIndex(entry, index) } })">APPLY B</TerminalButton>
                <TerminalButton v-if="entry.user" variant="danger" size="small" :disabled="!writeEnabled || !supports('presets.delete_user')" @click="send({ op: 'presets.delete_user', args: { index: entryIndex(entry, index) } })">DELETE</TerminalButton>
              </template>
              <template v-else>
                <TerminalButton variant="subtle" size="small" :disabled="!writeEnabled || !supports('zone_presets.apply')" @click="send({ op: 'zone_presets.apply', args: { index: entryIndex(entry, index) } })">APPLY SET</TerminalButton>
                <TerminalButton variant="danger" size="small" :disabled="!writeEnabled || !supports('zone_presets.delete')" @click="send({ op: 'zone_presets.delete', args: { index: entryIndex(entry, index) } })">DELETE</TerminalButton>
              </template>
            </div>
          </article>
        </div><div v-else class="empty-note">{{ catalog?.entries.length ? 'No catalog entries match this search and ownership filter.' : 'Load the selected Host catalog to show its entries.' }}</div>
      </TerminalPanel>
      <TerminalPanel v-if="catalogKind === 'presets'" class="section-gap" kicker="CURRENT HOST CONFIG" title="Save user waveform preset">
        <div class="field-pair"><label>Channel<select v-model="savePresetChannel"><option>A</option><option>B</option></select></label><label>Name<input v-model="presetName" maxlength="256" placeholder="Preset name" /></label><label>Author<input v-model="presetAuthor" maxlength="256" placeholder="Author label" /></label></div>
        <TerminalButton class="section-gap-small" variant="primary" :disabled="!writeEnabled || !supports('presets.save_user') || !presetName.trim() || !presetAuthor.trim()" @click="saveUserPreset">SAVE CURRENT PRESET</TerminalButton>
      </TerminalPanel>
      <TerminalPanel v-else class="section-gap" kicker="CURRENT ZONE MAPPINGS" title="Save zone set">
        <div class="field-pair"><label>Name<input v-model="zonePresetName" maxlength="256" placeholder="Zone set name" /></label><TerminalButton variant="primary" :disabled="!writeEnabled || !supports('zone_presets.save') || !zonePresetName.trim()" @click="saveZonePreset">SAVE CURRENT ZONES</TerminalButton></div>
      </TerminalPanel>
    </template>

    <template v-else-if="section === 'Alarm'">
      <div class="two-column">
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">HOST CONFIGURATION</span><h3>Alarm schedule</h3></div></div>
          <div v-if="Object.keys(alarmConfig).length" class="form-grid alarm-grid">
            <label class="check-row"><input type="checkbox" :checked="alarmDraft.enabled" :disabled="!writeEnabled" @change="alarmDraft.enabled = boolFrom($event)" /> Enabled</label>
            <label>Hour<input type="number" min="0" max="23" :value="alarmDraft.hour" :disabled="!writeEnabled" @change="alarmNumber('hour',$event)" /></label>
            <label>Minute<input type="number" min="0" max="59" :value="alarmDraft.minute" :disabled="!writeEnabled" @change="alarmNumber('minute',$event)" /></label>
            <label>Channels<select v-model="alarmDraft.channels" :disabled="!writeEnabled"><option>A</option><option>B</option><option>Both</option></select></label>
            <label>Start strength<input type="number" min="0" max="200" :value="alarmDraft.start_strength" :disabled="!writeEnabled" @change="alarmNumber('start_strength',$event)" /></label>
            <label>Peak strength<input type="number" min="0" max="200" :value="alarmDraft.peak_strength" :disabled="!writeEnabled" @change="alarmNumber('peak_strength',$event)" /></label>
            <label>Ramp seconds<input type="number" min="0" max="3600" :value="alarmDraft.ramp_secs" :disabled="!writeEnabled" @change="alarmNumber('ramp_secs',$event)" /></label>
            <label>Max duration seconds<input type="number" min="30" max="3600" :value="alarmDraft.max_duration_secs" :disabled="!writeEnabled" @change="alarmNumber('max_duration_secs',$event)" /></label>
            <label>Repeats<input type="number" min="1" max="20" :value="alarmDraft.repeats" :disabled="!writeEnabled" @change="alarmNumber('repeats',$event)" /></label>
            <label>Pulse on ms<input type="number" min="0" max="65535" :value="alarmDraft.pulse_on_ms" :disabled="!writeEnabled" @change="alarmNumber('pulse_on_ms',$event)" /></label>
            <label>Pulse off ms<input type="number" min="0" max="65535" :value="alarmDraft.pulse_off_ms" :disabled="!writeEnabled" @change="alarmNumber('pulse_off_ms',$event)" /></label>
            <label>Snooze minutes<input type="number" min="1" max="60" :value="alarmDraft.snooze_mins" :disabled="!writeEnabled" @change="alarmNumber('snooze_mins',$event)" /></label>
          </div><div v-else class="empty-note">Alarm configuration was not included in this Host snapshot.</div>
          <div class="button-row section-gap-small"><TerminalButton variant="primary" :disabled="!writeEnabled || !supports('alarm.set_config') || !Object.keys(alarmConfig).length" @click="send({ op: 'alarm.set_config', args: { config: { ...alarmDraft } } })">SAVE ALARM CONFIG</TerminalButton><TerminalButton :disabled="!writeEnabled || !supports('alarm.set_channels')" @click="send({ op: 'alarm.set_channels', args: { channels: alarmDraft.channels } })">SET CHANNELS</TerminalButton></div>
        </section>
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">CURRENT HOST STATE</span><h3>Alarm status</h3></div></div><KeyValueTable :value="alarmStatus" empty-label="Alarm status was not included in this snapshot." />
          <div class="button-row section-gap"><TerminalButton variant="primary" :disabled="!writeEnabled || !supports('alarm.test')" @click="send({ op: 'alarm.test', args: {} })">TEST ALARM</TerminalButton><TerminalButton variant="danger" :disabled="!writeEnabled || !supports('alarm.stop')" @click="send({ op: 'alarm.stop', args: {} })">STOP</TerminalButton><TerminalButton :disabled="!writeEnabled || !supports('alarm.snooze')" @click="send({ op: 'alarm.snooze', args: {} })">SNOOZE</TerminalButton></div>
        </section>
      </div>
    </template>

    <template v-else-if="section === 'Setup'">
      <div class="two-column">
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">HOST PREFERENCES</span><h3>egui setup state</h3></div></div>
          <template v-if="snapshot.prefs"><div class="form-grid"><label class="check-row"><input type="checkbox" :checked="snapshot.prefs.auto_save" :disabled="!writeEnabled || !supports('prefs.set_auto_save')" @change="setPreference('prefs.set_auto_save',$event)" /> Auto save</label><label class="check-row"><input type="checkbox" :checked="snapshot.prefs.alarm_tab_visible" :disabled="!writeEnabled || !supports('prefs.set_alarm_tab_visible')" @change="setPreference('prefs.set_alarm_tab_visible',$event)" /> Show Alarm tab</label></div></template>
          <div v-else class="empty-note">Host did not advertise snapshot.prefs. Current preferences are unavailable.</div>
          <div class="button-row section-gap"><TerminalButton :disabled="!writeEnabled || !supports('config.save')" @click="send({ op: 'config.save', args: {} })">SAVE HOST CONFIG</TerminalButton><TerminalButton variant="subtle" :disabled="!writeEnabled || !supports('config.load')" @click="send({ op: 'config.load', args: {} })">LOAD HOST CONFIG</TerminalButton></div>
        </section>
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">HOST-ONLY POLICY</span><h3>Remote Maximum Strength</h3></div><span class="tag">READ ONLY</span></div>
          <div class="policy-state"><strong>{{ snapshot.remote_policy.remote_strength.enabled ? 'Enabled' : 'Disabled' }}</strong><small>Power Limits min/max writes only</small></div>
          <div class="limit-strip"><span>Channel A maximum</span><strong>{{ snapshot.remote_policy.remote_strength.max_by_channel.A ?? '—' }}</strong></div>
          <div class="limit-strip"><span>Channel B maximum</span><strong>{{ snapshot.remote_policy.remote_strength.max_by_channel.B ?? '—' }}</strong></div>
          <p class="microcopy">The Terminal displays the Host's actual values and sends the requested Power Limits value. The Host enforces this remote policy and returns any rejection.</p>
          <p class="microcopy">Change Remote Access and per-channel limits in the Host's local Setup window.</p>
        </section>
      </div>
    </template>

  </template>
</template>

<script setup lang="ts">
import { computed, reactive, ref, watch } from "vue";
import KeyValueTable from "./KeyValueTable.vue";
import ModulationEditor from "./ModulationEditor.vue";
import { useTerminalStore } from "../stores/terminal";
import type { ChannelConfig, JsonValue } from "../protocol/types";
import type { AlarmConfigInput, AllowedOperation, ChannelId, ContactMode, ModulationConfigInput, ModulationKind, ZoneType } from "../protocol/operations";

const props = defineProps<{ section: string }>();
const store = useTerminalStore();
const catalogKind = ref<"presets" | "zone_presets">("presets");
const savePresetChannel = ref<ChannelId>("A");
const presetName = ref("");
const presetAuthor = ref("");
const zonePresetName = ref("");
const lastQueued = ref("");
const ukfFields = ["q", "r", "alpha", "beta", "kappa"] as const;
const normsFields = ["speed", "acc", "recoil"] as const;
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
const logs = computed(() => store.logs);
const writeEnabled = computed(() => store.connected && !!snapshot.value);

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
const ukfDraft = reactive({ q: 0, r: 0, alpha: 0, beta: 0, kappa: 0 });
const normsDraft = reactive({ speed: 0, acc: 0, recoil: 0 });

watch(() => JSON.stringify(alarmConfig.value), () => {
  const value = alarmConfig.value;
  Object.assign(alarmDraft, value);
}, { immediate: true });
watch(() => JSON.stringify(snapshot.value?.config.ukf), () => Object.assign(ukfDraft, asRecord(snapshot.value?.config.ukf)), { immediate: true });
watch(() => JSON.stringify(snapshot.value?.config.norms), () => Object.assign(normsDraft, asRecord(snapshot.value?.config.norms)), { immediate: true });

function asRecord(value: JsonValue | undefined | null): Record<string, JsonValue> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, JsonValue> : {};
}

function channelStatus(key: "channel_a" | "channel_b") {
  return asRecord(statusObject.value[key]);
}

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

function setZoneMode(channel: ChannelId, index: number, event: Event) {
  void send({ op: "zones.set_mode", args: { channel, index, mode: (event.target as HTMLSelectElement).value as ContactMode } });
}

function setZoneValue(op: "zones.set_scale" | "zones.set_threshold_min" | "zones.set_threshold_max", channel: ChannelId, index: number, event: Event) {
  void send({ op, args: { channel, index, value: numberFrom(event) } });
}

function setChannelNumber(op: "channels.set_frequency" | "channels.set_intensity", channel: ChannelId, segment: number, event: Event) {
  void send({ op, args: { channel, segment, value: numberFrom(event) } });
}

function setPowerLimit(op: "channels.set_limit_min" | "channels.set_limit_max", channel: ChannelId, event: Event) {
  void send({ op, args: { channel, value: numberFrom(event) } });
}

function submitModulation(channel: ChannelId, kind: ModulationKind, segment: number, config: ModulationConfigInput) {
  void send({ op: "modulation.apply", args: { channel, kind, segment, config } });
}

async function loadCatalog() {
  try { await store.fetchCatalog(catalogKind.value); } catch (error) { store.error = String(error); }
}

async function refreshCatalog() {
  if (catalogKind.value === "presets") await send({ op: "presets.refresh", args: {} });
  else await send({ op: "zone_presets.reload", args: {} });
}

function entryIndex(entry: Record<string, JsonValue>, fallback: number): number {
  return typeof entry.index === "number" ? entry.index : fallback;
}

function presetEntryName(entry: Record<string, JsonValue>): string {
  const nested = asRecord(entry.preset);
  return typeof entry.name === "string" ? entry.name : typeof nested.name === "string" ? nested.name : "Host entry";
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

function editUkf(key: keyof typeof ukfDraft, event: Event) { ukfDraft[key] = numberFrom(event); }
function editNorm(key: keyof typeof normsDraft, event: Event) { normsDraft[key] = numberFrom(event); }

async function loadLogs(cursor?: string | null) {
  try { await store.fetchLogs(cursor); } catch (error) { store.error = String(error); }
}

async function refreshState() {
  try { await store.fetchState(); } catch (error) { store.error = String(error); }
}
</script>

<template>
  <div v-if="!snapshot" class="panel empty-state">
    <span class="empty-glyph">◈</span>
    <h2>{{ store.connection.phase === 'reconnecting' ? 'Resuming Host session' : 'Host state is not connected' }}</h2>
    <p>Open Connect, verify the saved SPKI fingerprint, and sign in to read this section from the Host.</p>
  </div>

  <template v-else>
    <div v-if="!writeEnabled && store.connection.phase !== 'reconnecting'" class="session-banner"><span>↻</span><div><strong>Showing the last Host snapshot</strong><small>Controls are disabled until the Terminal reconnects and receives a fresh snapshot.</small></div></div>
    <div v-if="lastQueued" class="queue-banner"><span>✓</span><small>{{ lastQueued }}</small></div>

    <template v-if="section === 'Status'">
      <div class="metric-grid">
        <article class="metric-card"><span class="eyebrow">REMOTE DEVICES</span><strong>{{ snapshot.devices.length }}</strong><small>{{ snapshot.devices.filter(device => device.connected).length }} connected</small></article>
        <article class="metric-card"><span class="eyebrow">ACTIVE DEVICE</span><strong class="metric-text">{{ snapshot.active_device_mac || 'No device selected' }}</strong><small>Host index {{ snapshot.active_device }}</small></article>
        <article class="metric-card"><span class="eyebrow">VRCHAT / OSC</span><strong class="metric-text">{{ snapshot.vrchat.connected ? 'CONNECTED' : 'OFFLINE' }}</strong><small>OSC {{ snapshot.vrchat.osc_port }} · OSCQuery {{ snapshot.vrchat.oscquery_port ?? '—' }}</small></article>
        <article class="metric-card"><span class="eyebrow">BRIDGE REVISION</span><strong>{{ snapshot.bridge_revision }}</strong><small>Host published state</small></article>
      </div>
      <div class="two-column section-gap">
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">DEVICE INVENTORY</span><h3>Host devices</h3></div><button class="button subtle" :disabled="!writeEnabled" @click="refreshState">REFRESH</button></div>
          <div v-if="snapshot.devices.length" class="device-list">
            <article v-for="(device, index) in snapshot.devices" :key="device.mac" class="device-row">
              <span class="device-state" :class="{ online: device.connected }"></span>
              <div class="grow"><strong>{{ device.name || 'Unnamed device' }}</strong><small>{{ device.mac }} · {{ device.reconnecting ? 'reconnecting' : device.connected ? 'connected' : 'disconnected' }}</small></div>
              <span class="battery">{{ device.battery == null ? '—' : `${device.battery}%` }}</span>
              <span v-if="index === snapshot.active_device" class="tag">ACTIVE</span>
              <button class="button subtle" :disabled="!writeEnabled || !supports('devices.switch')" @click="send({ op: 'devices.switch', args: { index } })">SELECT</button>
            </article>
          </div><div v-else class="empty-note">The Host currently reports no devices.</div>
        </section>
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">LIVE OUTPUT STATE</span><h3>Channels A / B</h3></div><span class="tag">HOST VALUES</span></div>
          <div class="channel-status-grid">
            <div v-for="channel in ['A', 'B'] as const" :key="channel" class="channel-status" :class="`channel-${channel.toLowerCase()}`">
              <div class="channel-title">CHANNEL {{ channel }}</div><strong>{{ channelStatus(channel === 'A' ? 'channel_a' : 'channel_b').strength ?? '—' }}</strong><small>reported strength</small>
              <div class="kv-row compact"><span>raw level</span><b>{{ channelStatus(channel === 'A' ? 'channel_a' : 'channel_b').raw_level ?? '—' }}</b></div>
              <div class="kv-row compact"><span>active zones</span><b>{{ Array.isArray(channelStatus(channel === 'A' ? 'channel_a' : 'channel_b').active_zones) ? (channelStatus(channel === 'A' ? 'channel_a' : 'channel_b').active_zones as JsonValue[]).length : 0 }}</b></div>
            </div>
          </div>
          <div class="state-footer">Device link: <b>{{ statusObject.device_connected ? 'connected' : 'not connected' }}</b> · Values are read from Host snapshots.</div>
        </section>
      </div>
    </template>

    <template v-else-if="section === 'Zones'">
      <div class="two-column">
        <section v-for="item in channelItems" :key="item.id" class="panel">
          <div class="panel-heading"><div><span class="eyebrow">CHANNEL {{ item.id }}</span><h3>Configured zones</h3></div><span class="tag">{{ item.channel.zones.length }} ZONES</span></div>
          <button class="button subtle section-gap-small" :disabled="!writeEnabled || !supports('zones.add_all_from_avatar')" @click="send({ op: 'zones.add_all_from_avatar', args: { channel: item.id } })">ADD ALL CURRENT AVATAR ZONES</button>
          <div v-if="item.channel.zones.length" class="zone-list">
            <article v-for="(zone, index) in item.channel.zones" :key="`${zone.zone_type}-${zone.name}`" class="zone-row zone-control-row">
              <div class="grow"><strong>{{ zone.name }}</strong><small>{{ zone.zone_type }} · list index {{ index }}</small></div>
              <label>Mode<select :value="zone.mode" :disabled="!writeEnabled || !supports('zones.set_mode')" @change="setZoneMode(item.id, index, $event)"><option v-for="mode in ['Depth','Speed','Acc','Recoil']" :key="mode">{{ mode }}</option></select></label>
              <label>Scale<input type="number" min="0" max="100" :value="zone.scale" :disabled="!writeEnabled || !supports('zones.set_scale')" @change="setZoneValue('zones.set_scale', item.id, index, $event)" /></label>
              <label>Min<input type="number" min="1" max="100" :value="zone.min_threshold" :disabled="!writeEnabled || !supports('zones.set_threshold_min')" @change="setZoneValue('zones.set_threshold_min', item.id, index, $event)" /></label>
              <label>Max<input type="number" min="1" max="100" :value="zone.max_threshold" :disabled="!writeEnabled || !supports('zones.set_threshold_max')" @change="setZoneValue('zones.set_threshold_max', item.id, index, $event)" /></label>
              <button class="button danger small-button" :disabled="!writeEnabled || !supports('zones.remove')" @click="send({ op: 'zones.remove', args: { channel: item.id, index } })">REMOVE</button>
            </article>
          </div><div v-else class="empty-note">No configured zones in this Host snapshot.</div>
        </section>
      </div>
      <section class="panel section-gap"><div class="panel-heading"><div><span class="eyebrow">VRCHAT DISCOVERY</span><h3>Avatar zones</h3></div><span class="tag">{{ snapshot.avatar_zones.length }}</span></div>
        <div v-if="snapshot.avatar_zones.length" class="zone-list">
          <article v-for="zone in snapshot.avatar_zones" :key="`${zone.zone_type}-${zone.id}`" class="zone-row">
            <div class="grow"><strong>{{ zone.id }}</strong><small>{{ zone.zone_type }}{{ zone.is_tps ? ' · TPS' : '' }}</small></div>
            <span class="telemetry-inline">L {{ zone.level }} · V {{ zone.velocity }} · A {{ zone.acceleration }} · R {{ zone.recoil }}</span>
            <button class="button subtle" :disabled="!writeEnabled || !supports('zones.add')" @click="addAvatarZone('A', zone.zone_type, zone.id)">ADD TO A</button>
            <button class="button subtle" :disabled="!writeEnabled || !supports('zones.add')" @click="addAvatarZone('B', zone.zone_type, zone.id)">ADD TO B</button>
          </article>
        </div><div v-else class="empty-note">The Host has not reported discovered avatar zones.</div>
      </section>
    </template>

    <template v-else-if="section === 'Channels'">
      <div class="two-column">
        <section v-for="item in channelItems" :key="item.id" class="panel">
          <div class="panel-heading"><div><span class="eyebrow">CHANNEL {{ item.id }}</span><h3>Waveform configuration</h3></div><span class="tag">{{ item.channel.aggregation }}</span></div>
          <div class="limit-strip"><span>Power Limits · actual Host state</span><strong>{{ item.channel.limits.min }} — {{ item.channel.limits.max }}</strong></div>
          <div class="policy-state"><span>Remote Maximum Strength</span><strong>{{ snapshot.remote_policy.remote_strength.enabled ? `A ${snapshot.remote_policy.remote_strength.max_by_channel.A ?? '—'} · B ${snapshot.remote_policy.remote_strength.max_by_channel.B ?? '—'}` : 'disabled' }}</strong></div>
          <div class="limit-editor">
            <label>Set minimum<input type="number" min="0" max="200" :value="item.channel.limits.min" :disabled="!writeEnabled || !supports('channels.set_limit_min')" @change="setPowerLimit('channels.set_limit_min', item.id, $event)" /></label>
            <label>Set maximum<input type="number" min="0" max="200" :value="item.channel.limits.max" :disabled="!writeEnabled || !supports('channels.set_limit_max')" @change="setPowerLimit('channels.set_limit_max', item.id, $event)" /></label>
            <label>Aggregation<select :value="item.channel.aggregation" :disabled="!writeEnabled || !supports('channels.set_aggregation')" @change="send({ op: 'channels.set_aggregation', args: { channel: item.id, mode: ($event.target as HTMLSelectElement).value as 'Max' | 'Sum' | 'Average' } })"><option>Max</option><option>Sum</option><option>Average</option></select></label>
          </div>
          <div class="segment-table"><div class="segment-head"><span>SEGMENT</span><span>FREQUENCY · RAW</span><span>INTENSITY</span></div>
            <div v-for="(frequency, index) in item.channel.frequency" :key="index" class="segment-row"><span>{{ index + 1 }}</span>
              <input type="number" min="10" max="255" :value="frequency" :disabled="!writeEnabled || !supports('channels.set_frequency')" @change="setChannelNumber('channels.set_frequency', item.id, index, $event)" />
              <input type="number" min="0" max="100" :value="item.channel.intensity[index]" :disabled="!writeEnabled || !supports('channels.set_intensity')" @change="setChannelNumber('channels.set_intensity', item.id, index, $event)" />
            </div>
          </div>
          <p class="microcopy">Remote caps apply only to remote Power Limits min/max writes. Values above remain visible as reported by the Host; any rejection appears as the Host error.</p>
        </section>
      </div>
    </template>

    <template v-else-if="section === 'Tuning'">
      <div class="two-column">
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">HOST FILTER CONFIG</span><h3>UKF parameters</h3></div></div>
          <div class="form-grid"><label v-for="key in ukfFields" :key="key">{{ key }}<input type="number" step="any" :value="ukfDraft[key]" :disabled="!writeEnabled || !supports('tuning.set_ukf')" @input="editUkf(key, $event)" /></label>
            <button class="button" :disabled="!writeEnabled || !supports('tuning.set_ukf')" @click="send({ op: 'tuning.set_ukf', args: { config: { ...ukfDraft } } })">SET UKF VALUES</button>
            <button class="button subtle" :disabled="!writeEnabled || !supports('tuning.reset_ukf')" @click="send({ op: 'tuning.reset_ukf', args: {} })">RESET UKF</button>
          </div>
        </section>
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">MOTION SCALE</span><h3>Norms</h3></div></div>
          <div class="form-grid"><label v-for="key in normsFields" :key="key">{{ key }}<input type="number" step="any" :value="normsDraft[key]" :disabled="!writeEnabled || !supports('tuning.set_norms')" @input="editNorm(key, $event)" /></label>
            <button class="button" :disabled="!writeEnabled || !supports('tuning.set_norms')" @click="send({ op: 'tuning.set_norms', args: { config: { ...normsDraft } } })">SET NORMS</button>
            <button class="button subtle" :disabled="!writeEnabled || !supports('tuning.reset_norms')" @click="send({ op: 'tuning.reset_norms', args: {} })">RESET NORMS</button>
          </div>
        </section>
      </div>
    </template>

    <template v-else-if="section === 'Modulation'">
      <section v-for="item in channelItems" :key="item.id" class="panel section-gap">
        <div class="panel-heading"><div><span class="eyebrow">CHANNEL {{ item.id }}</span><h3>Segment modulation</h3></div><button class="button subtle" :disabled="!writeEnabled || !supports('modulation.clear_channel')" @click="send({ op: 'modulation.clear_channel', args: { channel: item.id } })">CLEAR CHANNEL MODULATION</button></div>
        <div class="segment-table modulation-table"><div class="segment-head"><span>SEGMENT</span><span>FREQUENCY</span><span>INTENSITY</span></div>
          <div v-for="index in [0,1,2,3]" :key="index" class="segment-row modulation-row"><strong>{{ index + 1 }}</strong>
            <div><code>{{ item.channel.freq_modulation[index] ? JSON.stringify(item.channel.freq_modulation[index]) : 'none' }}</code><ModulationEditor :channel="item.id" kind="freq" :segment="index" :current="item.channel.freq_modulation[index]" :disabled="!writeEnabled || !supports('modulation.apply')" :can-clear="writeEnabled && supports('modulation.clear_segment')" @apply="submitModulation(item.id, 'freq', index, $event)" @clear="send({ op: 'modulation.clear_segment', args: { channel: item.id, kind: 'freq', segment: index } })" /></div>
            <div><code>{{ item.channel.intensity_modulation[index] ? JSON.stringify(item.channel.intensity_modulation[index]) : 'none' }}</code><ModulationEditor :channel="item.id" kind="intensity" :segment="index" :current="item.channel.intensity_modulation[index]" :disabled="!writeEnabled || !supports('modulation.apply')" :can-clear="writeEnabled && supports('modulation.clear_segment')" @apply="submitModulation(item.id, 'intensity', index, $event)" @clear="send({ op: 'modulation.clear_segment', args: { channel: item.id, kind: 'intensity', segment: index } })" /></div>
          </div>
        </div>
      </section>
    </template>

    <template v-else-if="section === 'Presets'">
      <section class="panel"><div class="panel-heading"><div><span class="eyebrow">HOST CATALOGS</span><h3>{{ catalogKind === 'presets' ? 'Waveform presets' : 'Zone presets' }}</h3></div>
        <div class="button-row compact-row"><select v-model="catalogKind" class="select-compact"><option value="presets">Waveform presets</option><option value="zone_presets">Zone presets</option></select><button class="button subtle" :disabled="!writeEnabled" @click="loadCatalog">LOAD FROM HOST</button><button class="button subtle" :disabled="!writeEnabled || !supports(catalogKind === 'presets' ? 'presets.refresh' : 'zone_presets.reload')" @click="refreshCatalog">REFRESH HOST CATALOG</button></div>
      </div>
        <div v-if="catalog?.entries.length" class="catalog-list">
          <article v-for="(entry, index) in catalog.entries" :key="String(entry.id ?? index)" class="catalog-row"><span class="catalog-index">{{ entryIndex(entry, index) }}</span><div class="grow"><strong>{{ presetEntryName(entry) }}</strong><small>{{ entry.description ?? asRecord(entry.preset).description ?? '' }}</small></div><span v-if="entry.user" class="tag">USER</span>
            <template v-if="catalogKind === 'presets'"><button class="button subtle" :disabled="!writeEnabled || !supports('presets.apply')" @click="send({ op: 'presets.apply', args: { channel: 'A', index: entryIndex(entry,index) } })">APPLY A</button><button class="button subtle" :disabled="!writeEnabled || !supports('presets.apply')" @click="send({ op: 'presets.apply', args: { channel: 'B', index: entryIndex(entry,index) } })">APPLY B</button><button v-if="entry.user" class="button danger small-button" :disabled="!writeEnabled || !supports('presets.delete_user')" @click="send({ op: 'presets.delete_user', args: { index: entryIndex(entry,index) } })">DELETE</button></template>
            <template v-else><button class="button subtle" :disabled="!writeEnabled || !supports('zone_presets.apply')" @click="send({ op: 'zone_presets.apply', args: { index: entryIndex(entry,index) } })">APPLY</button><button class="button danger small-button" :disabled="!writeEnabled || !supports('zone_presets.delete')" @click="send({ op: 'zone_presets.delete', args: { index: entryIndex(entry,index) } })">DELETE</button></template>
          </article>
        </div><div v-else class="empty-note">Load the selected catalog to read the current Host ordering and entries.</div>
      </section>
      <section class="panel section-gap"><div class="panel-heading"><div><span class="eyebrow">SAVE FROM CURRENT HOST CONFIG</span><h3>Save user preset</h3></div></div>
        <div class="field-pair"><label>Channel<select v-model="savePresetChannel"><option>A</option><option>B</option></select></label><label>Name<input v-model="presetName" maxlength="256" /></label><label>Author<input v-model="presetAuthor" maxlength="256" /></label></div>
        <button class="button primary section-gap-small" :disabled="!writeEnabled || !supports('presets.save_user') || !presetName || !presetAuthor" @click="saveUserPreset">SAVE USER PRESET</button>
      </section>
      <section v-if="catalogKind === 'zone_presets'" class="panel section-gap"><div class="panel-heading"><div><span class="eyebrow">ZONE PRESET CATALOG</span><h3>Save current zones</h3></div></div><div class="field-pair"><label>Name<input v-model="zonePresetName" maxlength="256" /></label><button class="button primary" :disabled="!writeEnabled || !supports('zone_presets.save') || !zonePresetName" @click="saveZonePreset">SAVE ZONE PRESET</button></div></section>
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
          <div class="button-row section-gap-small"><button class="button primary" :disabled="!writeEnabled || !supports('alarm.set_config') || !Object.keys(alarmConfig).length" @click="send({ op: 'alarm.set_config', args: { config: { ...alarmDraft } } })">SAVE ALARM CONFIG</button><button class="button" :disabled="!writeEnabled || !supports('alarm.set_channels')" @click="send({ op: 'alarm.set_channels', args: { channels: alarmDraft.channels } })">SET CHANNELS</button></div>
        </section>
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">CURRENT HOST STATE</span><h3>Alarm status</h3></div></div><KeyValueTable :value="alarmStatus" empty-label="Alarm status was not included in this snapshot." />
          <div class="button-row section-gap"><button class="button primary" :disabled="!writeEnabled || !supports('alarm.test')" @click="send({ op: 'alarm.test', args: {} })">TEST ALARM</button><button class="button danger" :disabled="!writeEnabled || !supports('alarm.stop')" @click="send({ op: 'alarm.stop', args: {} })">STOP</button><button class="button" :disabled="!writeEnabled || !supports('alarm.snooze')" @click="send({ op: 'alarm.snooze', args: {} })">SNOOZE</button></div>
        </section>
      </div>
    </template>

    <template v-else-if="section === 'Setup'">
      <div class="two-column">
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">HOST PREFERENCES</span><h3>egui setup state</h3></div></div>
          <template v-if="snapshot.prefs"><div class="form-grid"><label class="check-row"><input type="checkbox" :checked="snapshot.prefs.auto_save" :disabled="!writeEnabled || !supports('prefs.set_auto_save')" @change="setPreference('prefs.set_auto_save',$event)" /> Auto save</label><label class="check-row"><input type="checkbox" :checked="snapshot.prefs.alarm_tab_visible" :disabled="!writeEnabled || !supports('prefs.set_alarm_tab_visible')" @change="setPreference('prefs.set_alarm_tab_visible',$event)" /> Show Alarm tab</label></div></template>
          <div v-else class="empty-note">Host did not advertise snapshot.prefs. Current preferences are unavailable.</div>
          <div class="button-row section-gap"><button class="button" :disabled="!writeEnabled || !supports('config.save')" @click="send({ op: 'config.save', args: {} })">SAVE HOST CONFIG</button><button class="button subtle" :disabled="!writeEnabled || !supports('config.load')" @click="send({ op: 'config.load', args: {} })">LOAD HOST CONFIG</button></div>
        </section>
        <section class="panel"><div class="panel-heading"><div><span class="eyebrow">HOST-ONLY POLICY</span><h3>Remote Maximum Strength</h3></div><span class="tag">READ ONLY</span></div>
          <div class="policy-state"><span class="device-state" :class="{ online: snapshot.remote_policy.remote_strength.enabled }"></span><strong>{{ snapshot.remote_policy.remote_strength.enabled ? 'Enabled' : 'Disabled' }}</strong><small>Power Limits min/max writes only</small></div>
          <div class="limit-strip"><span>Channel A maximum</span><strong>{{ snapshot.remote_policy.remote_strength.max_by_channel.A ?? '—' }}</strong></div>
          <div class="limit-strip"><span>Channel B maximum</span><strong>{{ snapshot.remote_policy.remote_strength.max_by_channel.B ?? '—' }}</strong></div>
          <p class="microcopy">The Terminal displays the Host's actual values and sends the requested Power Limits value. The Host enforces this remote policy and returns any rejection.</p>
          <p class="microcopy">Change Remote Access and per-channel limits in the Host's local Setup window.</p>
        </section>
      </div>
    </template>

    <template v-else-if="section === 'Logs'">
      <section class="panel"><div class="panel-heading"><div><span class="eyebrow">HOST LOG BUFFER</span><h3>Sanitized log lines</h3></div><div class="button-row compact-row"><button class="button subtle" :disabled="!writeEnabled" @click="loadLogs()">LOAD LATEST</button><button class="button subtle" :disabled="!writeEnabled || !logs?.next_cursor" @click="loadLogs(logs?.next_cursor)">NEXT PAGE</button></div></div>
        <div v-if="logs?.lines.length" class="logs-list"><article v-for="line in logs.lines" :key="line.seq" class="log-line"><span>{{ line.seq }}</span><code>{{ line.text }}</code></article></div>
        <div v-else class="empty-note">Load a page to read sanitized Host logs.</div>
        <div v-if="logs" class="state-footer">Cursor {{ logs.cursor }} · {{ logs.lines.length }} lines</div>
      </section>
    </template>
  </template>
</template>

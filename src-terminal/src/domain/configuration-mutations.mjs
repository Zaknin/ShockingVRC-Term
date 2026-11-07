export const CONFIGURATION_MUTATION_TIMEOUT_MS = 10_000;
const MAX_TRACKED_MUTATIONS = 64;

export function createConfigurationSyncState() {
  return { cursor: null, latestSnapshot: null, mutations: {} };
}

export function sameSnapshotContext(left, right) {
  return Boolean(left && right)
    && left.session_generation === right.session_generation
    && left.server_epoch === right.server_epoch
    && left.transport_generation === right.transport_generation;
}

export function shouldAcceptConfigurationSnapshot(currentCursor, incoming, activeContext) {
  if (!incoming || !Number.isSafeInteger(incoming.seq) || incoming.seq < 0) return false;
  if (!sameSnapshotContext(incoming, activeContext)) return false;
  if (!currentCursor || !sameSnapshotContext(currentCursor, incoming)) return true;
  return incoming.seq > currentCursor.seq;
}

export function configurationMutationDescriptors(operation, snapshot) {
  if (!operation || !snapshot) return [];
  const args = operation.args ?? {};
  const channel = args.channel === "A" ? "A" : args.channel === "B" ? "B" : null;
  const descriptors = [];
  const add = (key, target) => {
    // null is an intentional configuration value for clearing modulation slots.
    if (target !== undefined) descriptors.push({ key, operation: operation.op, target });
  };

  if (channel && ["channels.set_limit_min", "channels.set_limit_max"].includes(operation.op)) {
    const field = operation.op.endsWith("_min") ? "min" : "max";
    add(`channels.${channel}.limits.${field}`, args.value);
  } else if (channel && ["channels.set_frequency", "channels.set_intensity"].includes(operation.op)) {
    const field = operation.op === "channels.set_frequency" ? "frequency" : "intensity";
    add(`channels.${channel}.${field}.${args.segment}`, args.value);
  } else if (channel && operation.op === "modulation.apply") {
    const field = args.kind === "freq" ? "freq_modulation" : "intensity_modulation";
    const key = `channels.${channel}.${field}.${args.segment}`;
    if (!configurationValuesEqual(readConfigurationValue(snapshot, key), args.config)) add(key, args.config);
  } else if (channel && operation.op === "modulation.clear_segment") {
    const field = args.kind === "freq" ? "freq_modulation" : "intensity_modulation";
    add(`channels.${channel}.${field}.${args.segment}`, null);
  } else if (channel && operation.op === "modulation.clear_channel") {
    for (const field of ["freq_modulation", "intensity_modulation"]) {
      for (let segment = 0; segment < 4; segment += 1) add(`channels.${channel}.${field}.${segment}`, null);
    }
  } else if (channel && operation.op === "zones.set_mode") {
    add(`zones.${channel}.${args.index}.mode`, args.mode);
  } else if (channel && operation.op === "channels.set_aggregation") {
    add(`channels.${channel}.aggregation`, args.mode);
  } else if (channel && ["zones.set_scale", "zones.set_threshold_min", "zones.set_threshold_max"].includes(operation.op)) {
    const field = operation.op === "zones.set_scale" ? "scale"
      : operation.op === "zones.set_threshold_min" ? "min_threshold" : "max_threshold";
    add(`zones.${channel}.${args.index}.${field}`, args.value);
  } else if (operation.op === "tuning.set_ukf" || operation.op === "tuning.set_norms") {
    const group = operation.op === "tuning.set_ukf" ? "ukf" : "norms";
    for (const [field, target] of Object.entries(args.config ?? {})) {
      if (!configurationValuesEqual(readConfigurationValue(snapshot, `tuning.${group}.${field}`), target)) {
        add(`tuning.${group}.${field}`, target);
      }
    }
  } else if (operation.op === "alarm.set_config") {
    for (const [field, target] of Object.entries(args.config ?? {})) {
      if (!configurationValuesEqual(readConfigurationValue(snapshot, `alarm.${field}`), target)) add(`alarm.${field}`, target);
    }
  } else if (operation.op === "alarm.set_channels") {
    add("alarm.channels", args.channels);
  } else if (operation.op === "prefs.set_auto_save") {
    add("prefs.auto_save", args.enabled);
  } else if (operation.op === "prefs.set_alarm_tab_visible") {
    add("prefs.alarm_tab_visible", args.enabled);
  }
  return descriptors;
}

export function beginConfigurationMutation(state, { key, operation, target, operationId, context, now = Date.now() }) {
  const existing = state.mutations[key];
  const currentValue = state.latestSnapshot ? readConfigurationValue(state.latestSnapshot, key) : undefined;
  if (configurationValuesEqual(currentValue, target) && existing?.status !== "pending") return state;

  const mutations = { ...state.mutations };
  mutations[key] = {
    key,
    operation,
    operationId,
    requestId: null,
    target,
    observedHostValue: currentValue ?? null,
    status: "pending",
    queued: false,
    context: context ? { ...context } : null,
    ackContext: null,
    startedAt: now,
    deadline: now + CONFIGURATION_MUTATION_TIMEOUT_MS,
    elapsedMs: null,
  };
  return { ...state, mutations: limitMutations(mutations) };
}

export function markConfigurationCommandQueued(state, { operationId, requestId, context }) {
  const mutations = { ...state.mutations };
  let changed = false;
  for (const [key, mutation] of Object.entries(mutations)) {
    if (mutation.operationId !== operationId || mutation.status !== "pending") continue;
    if (!sameSnapshotContext(mutation.context, context)) continue;
    if (Number.isSafeInteger(mutation.context?.seq) && context?.seq < mutation.context.seq) continue;
    mutations[key] = { ...mutation, requestId, queued: true, ackContext: context ? { ...context } : null };
    changed = true;
  }
  return changed ? { ...state, mutations } : state;
}

export function markConfigurationCommandRejected(state, { operationId, now = Date.now() }) {
  const mutations = { ...state.mutations };
  let changed = false;
  for (const [key, mutation] of Object.entries(mutations)) {
    if (mutation.operationId !== operationId || mutation.status !== "pending") continue;
    mutations[key] = settled(mutation, "rejected", readConfigurationValue(state.latestSnapshot, key), now);
    changed = true;
  }
  return changed ? { ...state, mutations } : state;
}

export function markConfigurationOutcomeUnknown(state, { operationId, now = Date.now() }) {
  const mutations = { ...state.mutations };
  let changed = false;
  for (const [key, mutation] of Object.entries(mutations)) {
    if (mutation.operationId !== operationId || mutation.status !== "pending") continue;
    mutations[key] = settled(mutation, "unknown", readConfigurationValue(state.latestSnapshot, key), now);
    changed = true;
  }
  return changed ? { ...state, mutations } : state;
}

export function expireConfigurationMutations(state, now = Date.now()) {
  const mutations = { ...state.mutations };
  let changed = false;
  for (const [key, mutation] of Object.entries(mutations)) {
    if (mutation.status !== "pending" || now < mutation.deadline) continue;
    mutations[key] = settled(mutation, "unknown", readConfigurationValue(state.latestSnapshot, key), now);
    changed = true;
  }
  return changed ? { ...state, mutations } : state;
}

export function reconcileConfigurationSession(state, nextContext, now = Date.now()) {
  const mutations = { ...state.mutations };
  let changed = false;
  for (const [key, mutation] of Object.entries(mutations)) {
    if (mutation.status !== "pending" || sameSnapshotContext(mutation.context, nextContext)) continue;
    mutations[key] = settled(mutation, "unknown", readConfigurationValue(state.latestSnapshot, key), now);
    changed = true;
  }
  const cursor = state.cursor && sameSnapshotContext(state.cursor, nextContext) ? state.cursor : null;
  const latestSnapshot = cursor ? state.latestSnapshot : null;
  return changed || cursor !== state.cursor || latestSnapshot !== state.latestSnapshot
    ? { ...state, cursor, latestSnapshot, mutations }
    : state;
}

export function acceptConfigurationSnapshot(state, incoming, activeContext, now = Date.now()) {
  if (!shouldAcceptConfigurationSnapshot(state.cursor, incoming, activeContext)) {
    return { state, accepted: false };
  }

  const sameContext = sameSnapshotContext(state.cursor, incoming);
  const mutations = { ...state.mutations };
  for (const [key, mutation] of Object.entries(mutations)) {
    const actual = readConfigurationValue(incoming, key);
    if (mutation.status === "pending" && !sameSnapshotContext(mutation.context, incoming)) {
      mutations[key] = settled(mutation, "unknown", actual, now);
      continue;
    }
    if (mutation.status === "pending" && mutation.queued
        && sameSnapshotContext(mutation.ackContext, incoming)
        && incoming.seq > mutation.ackContext.seq
        && configurationValuesEqual(actual, mutation.target)) {
      mutations[key] = settled(mutation, "confirmed", actual, now);
      continue;
    }
    if (actual !== undefined && actual !== mutation.observedHostValue) {
      mutations[key] = { ...mutation, observedHostValue: actual };
    }
  }

  return {
    state: {
      ...state,
      cursor: {
        session_generation: incoming.session_generation,
        server_epoch: incoming.server_epoch,
        transport_generation: incoming.transport_generation,
        seq: incoming.seq,
      },
      latestSnapshot: incoming,
      mutations,
    },
    accepted: true,
    contextChanged: !sameContext,
  };
}

export function readConfigurationValue(snapshot, key) {
  if (!snapshot || typeof key !== "string") return undefined;
  const hostSnapshot = snapshot.state ?? snapshot;
  const parts = key.split(".");
  let current = hostSnapshot.config;
  if (parts[0] === "channels") {
    current = current?.[parts[1] === "A" ? "channel_a" : "channel_b"];
    parts.splice(0, 2);
  } else if (parts[0] === "zones") {
    current = current?.[parts[1] === "A" ? "channel_a" : "channel_b"]?.zones;
    const index = Number(parts[2]);
    current = Number.isSafeInteger(index) ? current?.[index] : undefined;
    parts.splice(0, 3);
  } else if (parts[0] === "tuning") {
    current = current?.[parts[1]];
    parts.splice(0, 2);
  } else if (parts[0] === "alarm") {
    current = hostSnapshot.alarm?.config;
    parts.shift();
  } else if (parts[0] === "prefs") {
    current = hostSnapshot.prefs;
    parts.shift();
  } else {
    return undefined;
  }
  for (const part of parts) {
    if (current == null) return undefined;
    current = current[part];
  }
  return current;
}

export function configurationValuesEqual(left, right) {
  if (Object.is(left, right)) return true;
  if (left == null || right == null || typeof left !== "object" || typeof right !== "object") return false;
  return stableJson(left) === stableJson(right);
}

function settled(mutation, status, observedHostValue, now) {
  return { ...mutation, status, queued: mutation.queued, observedHostValue: observedHostValue ?? null, elapsedMs: Math.max(0, now - mutation.startedAt) };
}

function limitMutations(mutations) {
  const entries = Object.entries(mutations);
  if (entries.length <= MAX_TRACKED_MUTATIONS) return mutations;
  entries.sort((left, right) => left[1].startedAt - right[1].startedAt);
  for (const [key, mutation] of entries) {
    if (entries.length <= MAX_TRACKED_MUTATIONS) break;
    if (mutation.status !== "pending") delete mutations[key];
  }
  return mutations;
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

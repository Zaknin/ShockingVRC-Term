export const ENGINE_MODE_CHANGE_TIMEOUT_MS = 10_000;

const activeModeStatuses = new Set(["pending", "queued", "reconnecting"]);
const terminalSessionPhases = new Set([
  "disconnected",
  "user_disconnected",
  "host_kicked",
  "session_revoked",
  "host_unavailable",
  "transport_lost",
]);

export function createGraphSessionState() {
  return {
    modeChange: {
      requestedMode: null,
      status: "idle",
      message: "",
      requestId: null,
      baselineContext: null,
      acknowledgementContext: null,
      startedAt: null,
      deadline: null,
    },
    latestSnapshot: null,
  };
}

export function hasGraphFeature(capabilities, feature) {
  return Array.isArray(capabilities?.features) && capabilities.features.includes(feature);
}

export function canRequestEngineMode(state, supported) {
  return Boolean(supported) && !activeModeStatuses.has(state?.modeChange?.status);
}

export function beginEngineModeChange(state, { mode, context, supported, now = Date.now() }) {
  if (!supported) {
    return replaceMode(state, {
      requestedMode: mode,
      status: "unsupported",
      message: "This Host does not support remote engine mode changes.",
      requestId: null,
      baselineContext: cloneContext(context),
      acknowledgementContext: null,
      startedAt: now,
      deadline: null,
    });
  }
  if (mode !== "classic" && mode !== "graph") {
    return replaceMode(state, modeResult(mode, "rejected", "Choose Classic or Graph mode."));
  }
  if (!isValidContext(context)) {
    return replaceMode(state, modeResult(mode, "stale", "Wait for a fresh authenticated Host snapshot before changing mode."));
  }
  if (state.latestSnapshot && sameSession(state.latestSnapshot.context, context)
      && state.latestSnapshot.engineMode === mode) {
    return replaceMode(state, {
      ...modeResult(mode, "confirmed", `Host is already in ${modeLabel(mode)} mode.`),
      baselineContext: cloneContext(context),
      startedAt: now,
    });
  }

  return replaceMode(state, {
    requestedMode: mode,
    status: "pending",
    message: `Waiting for Host to queue ${modeLabel(mode)} mode.`,
    requestId: null,
    baselineContext: cloneContext(context),
    acknowledgementContext: null,
    startedAt: now,
    deadline: now + ENGINE_MODE_CHANGE_TIMEOUT_MS,
  });
}

export function acknowledgeEngineModeChange(state, acknowledgement, now = Date.now()) {
  const change = state.modeChange;
  if (change.status !== "pending") return state;
  if (!acknowledgement?.queued) {
    return replaceMode(state, {
      ...change,
      status: "rejected",
      message: "Host rejected the engine mode change.",
      requestId: acknowledgement?.request_id ?? null,
      deadline: null,
    });
  }

  const ackContext = {
    session_generation: acknowledgement.session_generation,
    server_epoch: acknowledgement.server_epoch,
    transport_generation: acknowledgement.transport_generation,
    seq: acknowledgement.snapshot_seq,
  };
  if (!isValidContext(ackContext) || !sameSession(change.baselineContext, ackContext)
      || ackContext.seq < change.baselineContext.seq) {
    return replaceMode(state, {
      ...change,
      status: "stale",
      requestId: acknowledgement.request_id ?? null,
      acknowledgementContext: isValidContext(ackContext) ? cloneContext(ackContext) : null,
      message: "Host session changed while the mode request was being queued. Refresh Host state before trying again.",
      deadline: null,
    });
  }

  const next = replaceMode(state, {
    ...change,
    status: "queued",
    requestId: acknowledgement.request_id ?? null,
    acknowledgementContext: cloneContext(ackContext),
    message: `Host queued ${modeLabel(change.requestedMode)} mode. Waiting for a newer Host snapshot.`,
  });
  return reconcileLatestModeSnapshot(next, now);
}

export function acceptEngineModeSnapshot(state, { context, engineMode, phase = "online", now = Date.now() }) {
  if (!isValidContext(context)) return state;
  const latest = state.latestSnapshot;
  if (latest && sameSession(latest.context, context) && context.seq <= latest.context.seq) return state;

  const next = {
    ...state,
    latestSnapshot: {
      context: cloneContext(context),
      engineMode: typeof engineMode === "string" ? engineMode : null,
    },
  };
  return resolveModeSnapshot(next, next.latestSnapshot, phase, now);
}

function resolveModeSnapshot(state, snapshot, phase = "online", now = Date.now()) {
  const change = state.modeChange;
  if (!activeModeStatuses.has(change.status)) return state;
  if (phase === "reconnecting") {
    return replaceMode(state, { ...change, status: "reconnecting", message: "Host is reconnecting. The mode request has not been retried." });
  }
  if (phase !== "online") return state;
  if (!sameSession(change.baselineContext, snapshot.context)) {
    return replaceMode(state, {
      ...change,
      status: "stale",
      message: "A different Host session reported state before the mode change was confirmed.",
      deadline: null,
    });
  }

  const acknowledgementContext = change.acknowledgementContext;
  if (!acknowledgementContext) return state;
  if (snapshot.context.seq <= acknowledgementContext.seq) return state;
  if (snapshot.engineMode === change.requestedMode) {
    return replaceMode(state, {
      ...change,
      status: "confirmed",
      message: `Host confirmed ${modeLabel(change.requestedMode)} mode in snapshot ${snapshot.context.seq}.`,
      deadline: null,
    });
  }
  if (snapshot.engineMode === "classic" || snapshot.engineMode === "graph") {
    return replaceMode(state, {
      ...change,
      status: "stale",
      message: `A newer Host snapshot still reports ${modeLabel(snapshot.engineMode)} mode. The requested change was not confirmed.`,
      deadline: null,
    });
  }
  return replaceMode(state, {
    ...change,
    message: "Waiting for a newer Host snapshot that includes the authoritative engine mode.",
  });
}

export function updateEngineModeConnection(state, { phase, context }) {
  const change = state.modeChange;
  if (!activeModeStatuses.has(change.status)) return state;
  if (phase === "reconnecting") {
    return replaceMode(state, {
      ...change,
      status: "reconnecting",
      message: "Host is reconnecting. The mode request has not been retried.",
    });
  }
  if (phase === "online") {
    if (!isValidContext(context) || !sameSession(change.baselineContext, context)) {
      return replaceMode(state, {
        ...change,
        status: "stale",
        message: "The Host session changed before the requested mode was confirmed.",
        deadline: null,
      });
    }
    return replaceMode(state, {
      ...change,
      status: change.acknowledgementContext ? "queued" : "pending",
      message: change.acknowledgementContext
        ? `Host queued ${modeLabel(change.requestedMode)} mode. Waiting for a newer Host snapshot.`
        : `Waiting for Host to queue ${modeLabel(change.requestedMode)} mode.`,
    });
  }
  if (terminalSessionPhases.has(phase)) {
    return replaceMode(state, {
      ...change,
      status: "stale",
      message: "The Host session ended before the requested mode was confirmed. The request was not retried.",
      deadline: null,
    });
  }
  return state;
}

export function rejectEngineModeChange(state, error, now = Date.now()) {
  const change = state.modeChange;
  if (!activeModeStatuses.has(change.status)) return state;
  const detail = String(error ?? "Mode change failed.");
  const ambiguous = /outcome is unknown|timed?\s*out|session ended|interrupted|not replayed/i.test(detail);
  return replaceMode(state, {
    ...change,
    status: ambiguous ? "unknown" : "rejected",
    message: ambiguous
      ? `${detail} No automatic retry was performed.`
      : detail,
    deadline: null,
    finishedAt: now,
  });
}

export function expireEngineModeChange(state, now = Date.now()) {
  const change = state.modeChange;
  if (!activeModeStatuses.has(change.status) || change.deadline == null || now < change.deadline) return state;
  return replaceMode(state, {
    ...change,
    status: "unknown",
    message: "Timed out waiting for Host confirmation. Outcome is unknown; the request was not retried automatically.",
    deadline: null,
    finishedAt: now,
  });
}

function reconcileLatestModeSnapshot(state, now) {
  const latest = state.latestSnapshot;
  const acknowledgementContext = state.modeChange.acknowledgementContext;
  if (!latest || !acknowledgementContext || !sameSession(latest.context, acknowledgementContext)
      || latest.context.seq <= acknowledgementContext.seq) return state;
  return resolveModeSnapshot(state, latest, "online", now);
}

function replaceMode(state, modeChange) {
  return { ...state, modeChange };
}

function modeResult(requestedMode, status, message) {
  return {
    requestedMode,
    status,
    message,
    requestId: null,
    baselineContext: null,
    acknowledgementContext: null,
    startedAt: null,
    deadline: null,
  };
}

function cloneContext(context) {
  if (!context) return null;
  return {
    session_generation: context.session_generation,
    server_epoch: context.server_epoch,
    transport_generation: context.transport_generation,
    seq: context.seq,
  };
}

function isValidContext(context) {
  return Number.isSafeInteger(context?.session_generation)
    && context.session_generation >= 0
    && typeof context?.server_epoch === "string"
    && context.server_epoch.length > 0
    && Number.isSafeInteger(context?.transport_generation)
    && context.transport_generation >= 0
    && Number.isSafeInteger(context?.seq)
    && context.seq >= 0;
}

function sameSession(left, right) {
  return isValidContext(left) && isValidContext(right)
    && left.session_generation === right.session_generation
    && left.server_epoch === right.server_epoch;
}

function modeLabel(mode) {
  return mode === "graph" ? "Graph" : "Classic";
}

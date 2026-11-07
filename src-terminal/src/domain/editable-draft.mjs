export function beginEdit(state) {
  return { ...state, editing: true, pending: false, rejected: false, unknown: false };
}

export function updateDraft(state, value) {
  return { ...state, draft: value, rejected: false, unknown: false };
}

export function commitDraft(state, hostValue, revision) {
  const target = state.draft;
  const hasPendingRequest = state.pending || state.phase === "pending";
  const pending = !Object.is(target, hostValue) || hasPendingRequest;
  return {
    ...state,
    editing: false,
    pending,
    phase: pending ? "pending" : "idle",
    target,
    revision,
    rejected: false,
    unknown: false,
  };
}

export function syncDraftOnSnapshot(state, hostValue, revision, mutationStatus = null, mutationTarget = undefined) {
  if (state.editing) return state;
  if (mutationStatus === "pending") {
    const target = mutationTarget ?? state.target;
    return { ...state, draft: target, target, revision, phase: "pending", pending: true, rejected: false, unknown: false };
  }
  if (mutationStatus === "rejected") {
    return { ...state, draft: hostValue, target: hostValue, revision, phase: "rejected", pending: false, rejected: true, unknown: false };
  }
  if (mutationStatus === "unknown") {
    return { ...state, draft: hostValue, target: hostValue, revision, phase: "unknown", pending: false, rejected: false, unknown: true };
  }
  if (state.pending && mutationStatus !== "confirmed" && !Object.is(hostValue, state.target)) {
    return { ...state, revision, phase: "pending", pending: true, rejected: false, unknown: false };
  }
  return {
    ...state,
    draft: hostValue,
    target: hostValue,
    editing: false,
    phase: state.pending ? "confirmed" : "idle",
    pending: false,
    rejected: false,
    unknown: false,
  };
}

export function initialDraft(hostValue, revision = 0) {
  return { draft: hostValue, editing: false, pending: false, target: hostValue, revision, phase: "idle", rejected: false, unknown: false };
}

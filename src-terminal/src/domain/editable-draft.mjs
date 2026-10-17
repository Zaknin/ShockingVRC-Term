export function beginEdit(state) {
  return { ...state, editing: true, pending: false, rejected: false };
}

export function updateDraft(state, value) {
  return { ...state, draft: value, rejected: false };
}

export function commitDraft(state, hostValue, revision) {
  return {
    ...state,
    editing: false,
    pending: !Object.is(state.draft, hostValue),
    target: state.draft,
    revision,
    rejected: false,
  };
}

export function syncDraftOnSnapshot(state, hostValue, revision) {
  if (state.editing) return state;
  if (state.pending && revision <= state.revision) return state;
  const rejected = state.pending && !Object.is(hostValue, state.target);
  return {
    ...state,
    draft: hostValue,
    editing: false,
    pending: false,
    rejected,
  };
}

export function initialDraft(hostValue, revision = 0) {
  return { draft: hostValue, editing: false, pending: false, target: hostValue, revision, rejected: false };
}

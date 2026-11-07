export interface EditableDraftState {
  draft: number;
  editing: boolean;
  pending: boolean;
  target: number;
  revision: number;
  rejected: boolean;
}

export function beginEdit(state: EditableDraftState): EditableDraftState;
export function updateDraft(state: EditableDraftState, value: number): EditableDraftState;
export function commitDraft(state: EditableDraftState, hostValue: number, revision: number): EditableDraftState;
export function syncDraftOnSnapshot(state: EditableDraftState, hostValue: number, revision: number): EditableDraftState;
export function initialDraft(hostValue: number, revision?: number): EditableDraftState;

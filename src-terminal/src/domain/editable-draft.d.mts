export interface EditableDraftState {
  draft: number;
  editing: boolean;
  pending: boolean;
  phase: "idle" | "pending" | "rejected" | "confirmed" | "unknown";
  target: number;
  revision: number;
  rejected: boolean;
  unknown: boolean;
}

export function beginEdit(state: EditableDraftState): EditableDraftState;
export function updateDraft(state: EditableDraftState, value: number): EditableDraftState;
export function commitDraft(state: EditableDraftState, hostValue: number, revision: number): EditableDraftState;
export function syncDraftOnSnapshot(state: EditableDraftState, hostValue: number, revision: number, mutationStatus?: "pending" | "rejected" | "confirmed" | "unknown" | null, mutationTarget?: number): EditableDraftState;
export function initialDraft(hostValue: number, revision?: number): EditableDraftState;

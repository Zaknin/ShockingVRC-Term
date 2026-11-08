import type { GraphApplyResult, GraphModel } from "../protocol/types";

export interface GraphDraftState {
  graph: GraphModel;
  baseRevision: number | null;
  stale: boolean;
  dirty: boolean;
  saved: boolean;
}

export function createGraphDraftState(graph: GraphModel, baseRevision: number | null, hostGraph: GraphModel, saved?: boolean): GraphDraftState;
export function reconcileGraphDraftWithHost(draft: GraphDraftState | null, hostRevision: number, hostGraph: GraphModel): GraphDraftState;
export function updateGraphDraftState(draft: GraphDraftState, graph: GraphModel, hostGraph: GraphModel | null): GraphDraftState;
export function confirmGraphDraftApply(draft: GraphDraftState, result: Partial<GraphApplyResult> & Pick<GraphApplyResult, "state">): GraphDraftState;

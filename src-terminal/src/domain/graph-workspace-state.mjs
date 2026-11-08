import { cloneGraph } from "./graph-editor.mjs";

function sameGraph(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

export function createGraphDraftState(graph, baseRevision, hostGraph, saved = false) {
  return {
    graph: cloneGraph(graph),
    baseRevision,
    stale: baseRevision === null,
    dirty: !sameGraph(graph, hostGraph),
    saved,
  };
}

export function reconcileGraphDraftWithHost(draft, hostRevision, hostGraph) {
  if (!draft) return createGraphDraftState(hostGraph, hostRevision, hostGraph, false);
  return {
    ...draft,
    stale: draft.baseRevision === null || draft.baseRevision !== hostRevision,
    dirty: !sameGraph(draft.graph, hostGraph),
  };
}

export function updateGraphDraftState(draft, graph, hostGraph) {
  return {
    ...draft,
    graph: cloneGraph(graph),
    dirty: hostGraph ? !sameGraph(graph, hostGraph) : true,
    saved: false,
  };
}

export function confirmGraphDraftApply(draft, result) {
  if (result.state === "applied" && Number.isSafeInteger(result.graph_revision)) {
    return {
      ...draft,
      baseRevision: result.graph_revision,
      stale: false,
      dirty: false,
      saved: false,
    };
  }
  if (result.state === "conflict") return { ...draft, stale: true, dirty: true };
  return draft;
}

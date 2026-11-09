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
    pendingApplyId: null,
    applyOutcomeUnknown: false,
    confirmedApply: null,
  };
}

export function reconcileGraphDraftWithHost(draft, hostRevision, hostGraph) {
  if (!draft) return createGraphDraftState(hostGraph, hostRevision, hostGraph, false);
  const confirmedApply = draft.confirmedApply;
  if (confirmedApply && hostRevision >= confirmedApply.revision) {
    const safeToAdoptReceipt = hostRevision === confirmedApply.revision
      && sameGraph(draft.graph, confirmedApply.submittedGraph);
    const graph = safeToAdoptReceipt ? cloneGraph(hostGraph) : draft.graph;
    return {
      ...draft,
      graph,
      baseRevision: safeToAdoptReceipt ? confirmedApply.revision : draft.baseRevision,
      stale: draft.applyOutcomeUnknown === true || !safeToAdoptReceipt,
      dirty: !sameGraph(graph, hostGraph),
      confirmedApply: null,
    };
  }
  return {
    ...draft,
    stale: draft.applyOutcomeUnknown === true || draft.baseRevision === null || draft.baseRevision !== hostRevision,
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

export function confirmGraphDraftApply(draft, result, submittedGraph = draft.graph) {
  if (result.state === "applied" && Number.isSafeInteger(result.graph_revision)) {
    const draftMatchesApply = sameGraph(draft.graph, submittedGraph);
    return {
      ...draft,
      baseRevision: draftMatchesApply ? result.graph_revision : draft.baseRevision,
      stale: !draftMatchesApply,
      dirty: !draftMatchesApply,
      saved: false,
      pendingApplyId: null,
      applyOutcomeUnknown: false,
      confirmedApply: {
        revision: result.graph_revision,
        submittedGraph: cloneGraph(submittedGraph),
      },
    };
  }
  if (result.state === "host_changed" && Number.isSafeInteger(result.graph_revision)) {
    return {
      ...draft,
      baseRevision: result.graph_revision,
      stale: true,
      dirty: true,
      saved: false,
      pendingApplyId: null,
      applyOutcomeUnknown: false,
    };
  }
  if (result.state === "applied_unreconciled" && Number.isSafeInteger(result.graph_revision)) {
    const draftMatchesApply = sameGraph(draft.graph, submittedGraph);
    return {
      ...draft,
      baseRevision: draftMatchesApply ? result.graph_revision : draft.baseRevision,
      stale: true,
      dirty: !draftMatchesApply,
      saved: false,
      pendingApplyId: null,
      applyOutcomeUnknown: false,
      confirmedApply: {
        revision: result.graph_revision,
        submittedGraph: cloneGraph(submittedGraph),
      },
    };
  }
  if (result.state === "conflict") {
    return {
      ...draft,
      stale: true,
      dirty: true,
      pendingApplyId: null,
      applyOutcomeUnknown: false,
      confirmedApply: null,
    };
  }
  if (result.state === "unknown") {
    return {
      ...draft,
      stale: true,
      dirty: true,
      pendingApplyId: result.operation_id ?? null,
      applyOutcomeUnknown: true,
    };
  }
  if (result.state === "rejected") {
    return { ...draft, pendingApplyId: null, applyOutcomeUnknown: false, confirmedApply: null };
  }
  return draft;
}

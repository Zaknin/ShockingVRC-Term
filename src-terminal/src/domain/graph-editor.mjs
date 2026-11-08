const MAX_NODE_ID = Number.MAX_SAFE_INTEGER;
const MAX_GRAPH_NODES = 4096;
const MAX_GRAPH_EDGES = 16384;
const MAX_HISTORY = 80;

export function cloneGraph(graph) {
  return JSON.parse(JSON.stringify(graph));
}

function nodeType(node) {
  return node?.kind && typeof node.kind === "object" && typeof node.kind.type === "string"
    ? node.kind.type
    : null;
}

export function graphNodeTemplate(catalog, type) {
  return catalog?.entries?.find((entry) => entry.kind?.type === type) ?? null;
}

function graphNode(graph, id) {
  return graph.nodes.find((node) => node.id === id) ?? null;
}

function pinSpec(node, catalog, direction, key) {
  return graphNodePins(node, catalog, direction).find((pin) => pin.key === key) ?? null;
}

export function graphNodePins(node, catalog, direction) {
  const template = graphNodeTemplate(catalog, nodeType(node));
  if (nodeType(node) !== "Group") return template?.[direction] ?? [];

  const inner = node.kind.graph;
  if (!inner || !Array.isArray(inner.nodes) || !Array.isArray(inner.edges)) return [];
  const boundaryType = direction === "inputs" ? "GroupInput" : "GroupOutput";
  const boundary = inner.nodes.find((child) => nodeType(child) === boundaryType);
  const ports = boundary?.kind?.ports;
  if (!Array.isArray(ports)) return [];
  return ports.map((port) => {
    const edge = direction === "inputs"
      ? inner.edges.find((item) => item.from.node === boundary.id && item.from.pin === port.key)
      : inner.edges.find((item) => item.to.node === boundary.id && item.to.pin === port.key);
    const endpointId = direction === "inputs" ? edge?.to.node : edge?.from.node;
    const endpointPin = direction === "inputs" ? edge?.to.pin : edge?.from.pin;
    const endpoint = inner.nodes.find((child) => child.id === endpointId);
    const endpointTemplate = graphNodeTemplate(catalog, nodeType(endpoint));
    const endpointPins = endpointTemplate?.[direction === "inputs" ? "inputs" : "outputs"] ?? [];
    const inferred = endpointPins.find((pin) => pin.key === endpointPin);
    return {
      key: port.key,
      label: port.label || port.key,
      value_type: inferred?.value_type ?? "Float",
      default: inferred?.default ?? 0,
      min: inferred?.min ?? 0,
      max: inferred?.max ?? 1,
    };
  });
}

function allocateNodeIds(graph, count) {
  if (!Number.isInteger(count) || count < 0 || graph.nodes.length + count > MAX_GRAPH_NODES) {
    throw new Error("Graph node limit reached");
  }
  const used = new Set(graph.nodes.map((node) => node.id));
  let candidate = Math.max(0, ...used) + 1;
  const ids = [];
  while (ids.length < count) {
    if (candidate > MAX_NODE_ID) {
      candidate = 1;
      while (used.has(candidate)) candidate += 1;
      if (candidate > MAX_NODE_ID) throw new Error("Graph node ID space is exhausted");
    }
    while (used.has(candidate)) candidate += 1;
    used.add(candidate);
    ids.push(candidate);
    candidate += 1;
  }
  return ids;
}

function validPosition(position) {
  return Array.isArray(position)
    && position.length === 2
    && position.every((value) => Number.isFinite(value) && Math.abs(value) <= 1_000_000);
}

export function addGraphNode(graph, template, position) {
  if (!template?.kind || typeof template.kind.type !== "string") throw new Error("Unsupported Graph node template");
  if (!validPosition(position)) throw new Error("Graph node position must be finite and bounded");
  const next = cloneGraph(graph);
  const [id] = allocateNodeIds(next, 1);
  next.nodes.push({ id, kind: cloneGraph(template.kind), pos: [...position] });
  return next;
}

export function moveGraphNode(graph, id, position) {
  if (!validPosition(position)) throw new Error("Graph node position must be finite and bounded");
  const next = cloneGraph(graph);
  const node = graphNode(next, id);
  if (!node) throw new Error("Graph node no longer exists");
  node.pos = [...position];
  return next;
}

export function moveGraphNodes(graph, positions) {
  const entries = positions instanceof Map ? [...positions.entries()] : Object.entries(positions ?? {}).map(([id, value]) => [Number(id), value]);
  if (entries.some(([id, position]) => !Number.isSafeInteger(id) || !validPosition(position))) {
    throw new Error("Graph node positions must be finite and bounded");
  }
  const next = cloneGraph(graph);
  for (const [id, position] of entries) {
    const node = graphNode(next, id);
    if (!node) throw new Error("Graph node no longer exists");
    node.pos = [...position];
  }
  return next;
}

export function removeGraphNodes(graph, ids) {
  const removed = new Set(ids);
  const next = cloneGraph(graph);
  next.nodes = next.nodes.filter((node) => !removed.has(node.id));
  next.edges = next.edges.filter((edge) => !removed.has(edge.from.node) && !removed.has(edge.to.node));
  return next;
}

export function setGraphInputDefault(graph, catalog, nodeId, pinKey, value) {
  if (!Number.isFinite(value)) throw new Error("Graph input default must be finite");
  const next = cloneGraph(graph);
  const node = graphNode(next, nodeId);
  if (!node) throw new Error("Graph node no longer exists");
  const spec = pinSpec(node, catalog, "inputs", pinKey);
  if (!spec) throw new Error("Graph defaults can only be set on supported input pins");
  const min = Math.min(spec.min, spec.max);
  const max = Math.max(spec.min, spec.max);
  const normalized = Math.min(max, Math.max(min, value));
  node.defaults ??= {};
  node.defaults[pinKey] = spec.value_type === "Bool" ? (normalized >= 0.5 ? 1 : 0) : normalized;
  return next;
}

export function editGraphNodeParameter(graph, nodeId, key, value) {
  if (key === "type" || ["__proto__", "prototype", "constructor"].includes(key)) {
    throw new Error("Graph parameter is not supported for editing");
  }
  const next = cloneGraph(graph);
  const node = graphNode(next, nodeId);
  if (!node || !node.kind || typeof node.kind !== "object" || !Object.hasOwn(node.kind, key)) {
    throw new Error("Graph parameter is not supported for editing");
  }
  const current = node.kind[key];
  if (typeof current === "number") {
    if (typeof value !== "number" || !Number.isFinite(value)) throw new Error("Graph numeric parameter must be finite");
  } else if (typeof current === "boolean") {
    if (typeof value !== "boolean") throw new Error("Graph parameter type cannot be changed");
  } else if (typeof current === "string") {
    if (typeof value !== "string" || value.length > 4096) throw new Error("Graph text parameter is invalid");
  } else {
    throw new Error("Graph parameter is not supported for editing");
  }
  node.kind[key] = value;
  return next;
}

export function connectGraphPins(graph, catalog, from, to) {
  const source = graphNode(graph, from.node);
  const target = graphNode(graph, to.node);
  if (!source || !target) throw new Error("Graph connection endpoint no longer exists");
  const sourcePin = pinSpec(source, catalog, "outputs", from.pin);
  const targetPin = pinSpec(target, catalog, "inputs", to.pin);
  if (!sourcePin || !targetPin) throw new Error("Graph connection must use an output and an input pin");
  if (sourcePin.value_type !== targetPin.value_type) throw new Error("Graph connection pin types do not match");
  const next = cloneGraph(graph);
  next.edges = next.edges.filter((edge) => !(edge.to.node === to.node && edge.to.pin === to.pin));
  next.edges.push({
    from: { node: from.node, pin: from.pin },
    to: { node: to.node, pin: to.pin },
  });
  if (next.edges.length > MAX_GRAPH_EDGES) throw new Error("Graph connection limit reached");
  return next;
}

export function disconnectGraphEdge(graph, edgeIndex) {
  const next = cloneGraph(graph);
  if (Number.isInteger(edgeIndex) && edgeIndex >= 0 && edgeIndex < next.edges.length) next.edges.splice(edgeIndex, 1);
  return next;
}

export function createGraphHistory(graph) {
  return { graph: cloneGraph(graph), undo: [], redo: [] };
}

export function recordGraphHistory(state, graph) {
  return {
    graph: cloneGraph(graph),
    undo: [...state.undo, cloneGraph(state.graph)].slice(-MAX_HISTORY),
    redo: [],
  };
}

export function undoGraphHistory(state) {
  if (state.undo.length === 0) return state;
  const undo = [...state.undo];
  const graph = undo.pop();
  return { graph, undo, redo: [...state.redo, cloneGraph(state.graph)].slice(-MAX_HISTORY) };
}

export function redoGraphHistory(state) {
  if (state.redo.length === 0) return state;
  const redo = [...state.redo];
  const graph = redo.pop();
  return { graph, undo: [...state.undo, cloneGraph(state.graph)].slice(-MAX_HISTORY), redo };
}

import type { GraphModel, GraphNode, GraphNodeCatalog, GraphNodeTemplate } from "../protocol/types";

export interface GraphHistory {
  graph: GraphModel;
  undo: GraphModel[];
  redo: GraphModel[];
}

export function cloneGraph(graph: GraphModel): GraphModel;
export function graphNodeTemplate(catalog: GraphNodeCatalog | null, type: string): GraphNodeTemplate | null;
export function graphNodePins(node: GraphNode, catalog: GraphNodeCatalog | null, direction: "inputs" | "outputs"): GraphNodeTemplate["inputs"];
export function addGraphNode(graph: GraphModel, template: GraphNodeTemplate, position: [number, number]): GraphModel;
export function moveGraphNode(graph: GraphModel, id: number, position: [number, number]): GraphModel;
export function moveGraphNodes(graph: GraphModel, positions: Map<number, [number, number]> | Record<number, [number, number]>): GraphModel;
export function removeGraphNodes(graph: GraphModel, ids: number[]): GraphModel;
export function setGraphInputDefault(graph: GraphModel, catalog: GraphNodeCatalog | null, nodeId: number, pinKey: string, value: number): GraphModel;
export function editGraphNodeParameter(graph: GraphModel, nodeId: number, key: string, value: string | number | boolean): GraphModel;
export function connectGraphPins(graph: GraphModel, catalog: GraphNodeCatalog | null, from: { node: number; pin: string }, to: { node: number; pin: string }): GraphModel;
export function disconnectGraphEdge(graph: GraphModel, edgeIndex: number): GraphModel;
export function createGraphHistory(graph: GraphModel): GraphHistory;
export function recordGraphHistory(state: GraphHistory, graph: GraphModel): GraphHistory;
export function undoGraphHistory(state: GraphHistory): GraphHistory;
export function redoGraphHistory(state: GraphHistory): GraphHistory;

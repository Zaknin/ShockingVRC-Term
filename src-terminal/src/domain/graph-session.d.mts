export type EngineMode = "classic" | "graph";
export type EngineModeChangeStatus = "idle" | "pending" | "queued" | "confirmed" | "rejected" | "unknown" | "stale" | "reconnecting" | "unsupported";

export interface GraphSessionContext {
  session_generation: number;
  server_epoch: string;
  transport_generation: number;
  seq: number;
}

export interface EngineModeChange {
  requestedMode: EngineMode | null;
  status: EngineModeChangeStatus;
  message: string;
  requestId: string | null;
  baselineContext: GraphSessionContext | null;
  acknowledgementContext: GraphSessionContext | null;
  startedAt: number | null;
  deadline: number | null;
  finishedAt?: number;
}

export interface GraphSessionState {
  modeChange: EngineModeChange;
  latestSnapshot: { context: GraphSessionContext; engineMode: string | null } | null;
}

export interface EngineModeAcknowledgement {
  request_id?: string;
  queued: boolean;
  session_generation: number;
  server_epoch: string;
  transport_generation: number;
  snapshot_seq: number;
}

export const ENGINE_MODE_CHANGE_TIMEOUT_MS: 10000;
export function createGraphSessionState(): GraphSessionState;
export function hasGraphFeature(capabilities: { features?: string[] } | null | undefined, feature: string): boolean;
export function canRequestEngineMode(state: GraphSessionState, supported: boolean): boolean;
export function beginEngineModeChange(state: GraphSessionState, input: { mode: EngineMode; context: GraphSessionContext | null; supported: boolean; now?: number }): GraphSessionState;
export function acknowledgeEngineModeChange(state: GraphSessionState, acknowledgement: EngineModeAcknowledgement, now?: number): GraphSessionState;
export function acceptEngineModeSnapshot(state: GraphSessionState, input: { context: GraphSessionContext; engineMode?: string | null; phase?: string; now?: number }): GraphSessionState;
export function updateEngineModeConnection(state: GraphSessionState, input: { phase: string; context: GraphSessionContext | null }): GraphSessionState;
export function rejectEngineModeChange(state: GraphSessionState, error: unknown, now?: number): GraphSessionState;
export function expireEngineModeChange(state: GraphSessionState, now?: number): GraphSessionState;

export type SnapshotContext = {
  session_generation: number;
  server_epoch: string;
  transport_generation: number;
};
export type SnapshotCursor = SnapshotContext & { seq: number };
export type MutationStatus = "pending" | "confirmed" | "rejected" | "unknown";
export interface ConfigurationMutation {
  key: string;
  operation: string;
  operationId: number;
  requestId: string | null;
  target: unknown;
  observedHostValue: unknown;
  status: MutationStatus;
  queued: boolean;
  context: SnapshotContext | null;
  ackContext: SnapshotCursor | null;
  startedAt: number;
  deadline: number;
  elapsedMs: number | null;
}
export interface ConfigurationSyncState {
  cursor: SnapshotCursor | null;
  latestSnapshot: Record<string, unknown> | null;
  mutations: Record<string, ConfigurationMutation>;
}
export const CONFIGURATION_MUTATION_TIMEOUT_MS: number;
export function createConfigurationSyncState(): ConfigurationSyncState;
export function sameSnapshotContext(left: SnapshotContext | null, right: SnapshotContext | null): boolean;
export function shouldAcceptConfigurationSnapshot(current: SnapshotCursor | null, incoming: SnapshotCursor, active: SnapshotContext | null): boolean;
export function configurationMutationDescriptors(operation: any, snapshot: Record<string, unknown> | null): Array<{ key: string; operation: string; target: unknown }>;
export function beginConfigurationMutation(state: ConfigurationSyncState, input: { key: string; operation: string; target: unknown; operationId: number; context: SnapshotContext | null; now?: number }): ConfigurationSyncState;
export function markConfigurationCommandQueued(state: ConfigurationSyncState, input: { operationId: number; requestId: string; context: SnapshotCursor | null }): ConfigurationSyncState;
export function markConfigurationCommandRejected(state: ConfigurationSyncState, input: { operationId: number; now?: number }): ConfigurationSyncState;
export function markConfigurationOutcomeUnknown(state: ConfigurationSyncState, input: { operationId: number; now?: number }): ConfigurationSyncState;
export function expireConfigurationMutations(state: ConfigurationSyncState, now?: number): ConfigurationSyncState;
export function reconcileConfigurationSession(state: ConfigurationSyncState, nextContext: SnapshotContext | null, now?: number): ConfigurationSyncState;
export function acceptConfigurationSnapshot(state: ConfigurationSyncState, incoming: SnapshotCursor & Record<string, unknown>, active: SnapshotContext, now?: number): { state: ConfigurationSyncState; accepted: boolean; contextChanged?: boolean };
export function readConfigurationValue(snapshot: Record<string, unknown> | null, key: string): unknown;
export function configurationValuesEqual(left: unknown, right: unknown): boolean;

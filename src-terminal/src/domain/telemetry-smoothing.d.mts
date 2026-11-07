import type { ConnectionState, HostSnapshot } from "../protocol/types";

export function telemetryBarPresentation(
  previous: number | null,
  value: number | null,
  smooth?: boolean,
): { value: number | null; transitionMs: number };

export function telemetryResetIdentity(
  connection: ConnectionState | null | undefined,
  snapshot: HostSnapshot | null | undefined,
): string;

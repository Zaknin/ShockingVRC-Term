import type { ModulationFunction } from "../protocol/operations";

export function cloneModulationPayload<T>(value: T): T;
export function parseModulationFunctionJson(source: string): ModulationFunction;

import type { ModulationConfigInput, ModulationFunction } from "../protocol/operations";

export function rawToHz(raw: number): number;
export function hzToRaw(hz: number): number;
export function modulationChartDomain(points: readonly number[]): { min: number; max: number };
export function modulationChartLabelPrecision(domain: { min: number; max: number }): number;
export function formatNumber(value: number, maxFraction?: number): string;
export function defaultModulationConfig(kind: "freq" | "intensity"): ModulationConfigInput;
export function evaluateFunction(func: ModulationFunction, x: number, depth?: number): number;
export function composeDepth(func: ModulationFunction): number;
export function sampleModulation(
  config: ModulationConfigInput,
  baseValue: number,
  sourceValue: number,
  sampleCount?: number,
  durationSeconds?: number,
): number[];

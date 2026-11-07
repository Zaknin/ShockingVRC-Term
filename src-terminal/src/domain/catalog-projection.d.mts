import type { JsonValue } from "../protocol/types";

export function entryIndex(entry: Record<string, JsonValue>, fallback: number): number;
export function presetEntryName(entry: Record<string, JsonValue>): string;
export function presetAuthorLabel(entry: Record<string, JsonValue>): string;
export function presetNumberArray(entry: Record<string, JsonValue>, key: "frequency" | "intensity"): number[] | null;
export function presetModulationCount(entry: Record<string, JsonValue>): number;

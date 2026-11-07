import type { JsonValue } from "../protocol/types";

export function entryIndex(entry: Record<string, JsonValue>, fallback: number): number;
export interface ProjectedCatalogEntry {
  entry: Record<string, JsonValue>;
  hostIndex: number;
  identity: string;
}
export function projectCatalogEntries(entries: readonly Record<string, JsonValue>[] | null | undefined): ProjectedCatalogEntry[];
export function filterCatalogEntries(entries: readonly ProjectedCatalogEntry[], query?: string, scope?: "ALL" | "MINE"): ProjectedCatalogEntry[];
export function reconcileCatalogSelection(entries: readonly ProjectedCatalogEntry[], selectedIdentity: string | null | undefined): string | null;
export function presetEntryName(entry: Record<string, JsonValue>): string;
export function presetAuthorLabel(entry: Record<string, JsonValue>): string;
export function presetNumberArray(entry: Record<string, JsonValue>, key: "frequency" | "intensity"): number[] | null;
export function presetModulationCount(entry: Record<string, JsonValue>): number;

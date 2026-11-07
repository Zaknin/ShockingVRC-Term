export interface HostTheme {
  id: string;
  name: string;
  base: [number, number, number];
  contrast: [number, number, number];
  accent: [number, number, number];
}
export interface HostPalette {
  id: string;
  name: string;
  light: boolean;
  base: string;
  text: string;
  accent: string;
  tokens: Record<string, string>;
}
export const HOST_THEMES: readonly HostTheme[];
export function lerp(a: number, b: number, t: number): number;
export function lerpHue(a: number, b: number, t: number): number;
export function mixOklch(a: [number, number, number], b: [number, number, number], aShare: number): [number, number, number];
export function oklchToRgb(value: [number, number, number]): number[];
export function hostPalette(themeId: string): HostPalette;
export function applyHostPalette(themeId: string, root?: HTMLElement): HostPalette;
export function readSavedTheme(storage?: Storage): string;
export function saveTheme(id: string, storage?: Storage): string;

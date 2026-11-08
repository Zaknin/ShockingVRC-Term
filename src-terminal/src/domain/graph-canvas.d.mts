export interface GraphCanvasRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

export function clientToViewBoxPosition(
  clientX: number,
  clientY: number,
  rect: GraphCanvasRect,
  viewWidth?: number,
  viewHeight?: number,
): [number, number];

export function clientToGraphPosition(
  clientX: number,
  clientY: number,
  rect: GraphCanvasRect,
  pan?: [number, number],
  zoom?: number,
  viewWidth?: number,
  viewHeight?: number,
): [number, number];

export function clampContextMenuPosition(
  clientX: number,
  clientY: number,
  menuWidth: number,
  menuHeight: number,
  viewportWidth: number,
  viewportHeight: number,
  padding?: number,
): [number, number];

import test from "node:test";
import assert from "node:assert/strict";
import {
  clampContextMenuPosition,
  clientToGraphPosition,
  clientToViewBoxPosition,
} from "../src/domain/graph-canvas.mjs";

test("canvas pointer mapping accounts for centered SVG letterboxing", () => {
  const tallViewport = { left: 80, top: 40, width: 480, height: 620 };
  assert.deepEqual(clientToViewBoxPosition(320, 350, tallViewport), [480, 310]);

  const wideViewport = { left: 0, top: 0, width: 1200, height: 620 };
  assert.deepEqual(clientToViewBoxPosition(600, 310, wideViewport), [480, 310]);
});

test("graph placement applies canvas pan, zoom, and CSS-scaled viewport coordinates", () => {
  const scaledViewport = { left: 100, top: 50, width: 1440, height: 930 };
  assert.deepEqual(clientToGraphPosition(700, 500, scaledViewport, [40, 20], 2), [180, 140]);
});

test("menu placement stays within the app viewport at all four edges", () => {
  assert.deepEqual(clampContextMenuPosition(790, 590, 240, 180, 800, 600), [552, 412]);
  assert.deepEqual(clampContextMenuPosition(2, 4, 240, 180, 800, 600), [8, 8]);
});


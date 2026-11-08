export function clientToViewBoxPosition(clientX, clientY, rect, viewWidth = 960, viewHeight = 620) {
  const scale = Math.min(rect.width / viewWidth, rect.height / viewHeight);
  if (!Number.isFinite(scale) || scale <= 0) return [0, 0];
  const renderedWidth = viewWidth * scale;
  const renderedHeight = viewHeight * scale;
  const offsetX = (rect.width - renderedWidth) / 2;
  const offsetY = (rect.height - renderedHeight) / 2;
  return [
    (clientX - rect.left - offsetX) / scale,
    (clientY - rect.top - offsetY) / scale,
  ];
}

export function clientToGraphPosition(clientX, clientY, rect, pan = [0, 0], zoom = 1, viewWidth = 960, viewHeight = 620) {
  const [x, y] = clientToViewBoxPosition(clientX, clientY, rect, viewWidth, viewHeight);
  const safeZoom = Number.isFinite(zoom) && zoom > 0 ? zoom : 1;
  return [(x - pan[0]) / safeZoom, (y - pan[1]) / safeZoom];
}

export function clampContextMenuPosition(clientX, clientY, menuWidth, menuHeight, viewportWidth, viewportHeight, padding = 8) {
  const maxX = Math.max(padding, viewportWidth - menuWidth - padding);
  const maxY = Math.max(padding, viewportHeight - menuHeight - padding);
  return [
    Math.max(padding, Math.min(clientX, maxX)),
    Math.max(padding, Math.min(clientY, maxY)),
  ];
}

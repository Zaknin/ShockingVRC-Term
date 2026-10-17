export function zoneThresholdWireToUi(value) {
  return value / 100;
}

export function zoneThresholdUiToWire(value) {
  return Math.round(value * 100);
}

export function resetCalibrationOperations(channel, index) {
  return [
    { op: "zones.set_scale", args: { channel, index, value: 100 } },
    { op: "zones.set_threshold_min", args: { channel, index, value: 1 } },
    { op: "zones.set_threshold_max", args: { channel, index, value: 100 } },
  ];
}

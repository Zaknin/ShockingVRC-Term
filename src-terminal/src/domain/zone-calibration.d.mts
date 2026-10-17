import type { AllowedOperation, ChannelId } from "../protocol/operations";

export function zoneThresholdWireToUi(value: number): number;
export function zoneThresholdUiToWire(value: number): number;
export function resetCalibrationOperations(channel: ChannelId, index: number): AllowedOperation[];

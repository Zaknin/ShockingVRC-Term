import type { ModulationFunction } from "../protocol/operations";

export declare const HOST_FUNCTIONS: readonly ModulationFunction[];
export declare function displayFunction(func: ModulationFunction): string;
export declare function functionGroup(func: ModulationFunction): string;
export declare function functionListFor(current: ModulationFunction): ModulationFunction[];
export declare function groupFunctions(functions: readonly ModulationFunction[], query?: string): Array<{ label: string; functions: ModulationFunction[] }>;
export declare function functionSparkline(func: ModulationFunction): readonly number[];
export declare function modulationParameterLabel(key: string): string;
export declare function formatModulationParameter(key: string, value: number): string;

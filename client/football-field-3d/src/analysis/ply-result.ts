export const MAX_PLY_BYTES = 64 * 1024 * 1024;
export const MAX_PLY_POINTS = 2_000_000;

export interface PlyCloud {
  fileName: string;
  byteLength: number;
  positions: Float32Array<ArrayBuffer>;
  colors?: Float32Array<ArrayBuffer>;
  minimum: [number, number, number];
  maximum: [number, number, number];
  declaredPitchCoordinates: boolean;
}

export interface LocalAnalysis {
  cloud: PlyCloud;
  aligned: boolean;
  pointSize: number;
  importedAt: string;
}

export type PlyWorkerMessage = { ok: true; cloud: PlyCloud } | { ok: false; error: string };

export interface ResultFrame {
  ordinal: number;
  presentIndex: number;
  sourcePts: number;
  time: number;
  Ti: number[] | null;
  postwarp?: number[];
}

export interface DiagnosticProjection {
  kind: 'auxiliary-radial-postwarp';
  variant: string;
  matrix: number[];
  radial: {
    centre: [number, number];
    scale: number;
    k1: number;
    support: [number, number, number, number];
  };
}

export interface ResultShot {
  id: string;
  video: File;
  width: number;
  height: number;
  centre: [number, number, number];
  frames: ResultFrame[];
  warnings: string[];
  projection?: DiagnosticProjection;
}

export interface ResultBundle {
  name: string;
  pitch: [number, number];
  shots: ResultShot[];
}

export interface BundleFile { path: string; file: File }
export type BundleMessage = { ok: true; bundle: ResultBundle } | { ok: false; error: string };

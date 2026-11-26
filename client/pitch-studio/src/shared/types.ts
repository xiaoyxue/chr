export interface FieldDimensions { length: number; width: number }
export interface Point { x: number; y: number }
export interface GroundRectangle extends Point { sizeX: number; sizeY: number; rz?: number }
export interface Pose extends Point { z: number; rx: number; ry: number; rz: number }
export type AssetRole = 'car' | 'bank-left' | 'bank-right';
export interface FusionAsset { id: string; name: string; contentType: string; url: string }
/** One end of a Fusion model motion: field-space offset of its bottom centre in metres (z down) and a scale multiplier. */
export interface FusionMotionKey { x: number; y: number; z: number; scale: number }
export interface FusionModelMotion { enabled: boolean; start: FusionMotionKey; end: FusionMotionKey; duration: number; playback: 'once' | 'loop' | 'ping-pong' }
/**
 * Fusion's model pose: normalized offsets in the region, size factor and heading in degrees,
 * then optional whole-model animation (#157): rotation about a fixed field axis (`swing`
 * by ±amplitude degrees, or `continuous`, every `swingPeriod` seconds) and a timed motion.
 */
export interface FusionModelPose {
  x: number; y: number; scale: number; yaw: number;
  swingAxis?: 'x' | 'y' | 'z' | null;
  swingAmplitude?: number;
  swingPeriod?: number;
  rotationMode?: 'swing' | 'continuous';
  motion?: FusionModelMotion;
}
interface SlotBase extends Pose { id: string; name: string; adId?: string; adName?: string; asset?: FusionAsset; assetRotation?: number }
export interface GroundPlacement extends SlotBase {
  type: '2D'; sizeX: number; sizeY: number; assetRole?: never;
  /** Fusion's `assetOpacity` (#188): the image's alpha is multiplied by this, 0 to 1. */
  assetOpacity: number;
}
export interface ModelPlacement extends SlotBase {
  type: '3D';
  assetRole?: AssetRole;
  region?: { width: number; height: number };
  modelPose?: FusionModelPose;
  scaleX: number;
  scaleY: number;
  scaleZ: number;
}
export type Placement = GroundPlacement | ModelPlacement;
export type ArtworkRotation = 0 | 90 | 180 | 270;
/** How a flag appears: opening from its vertical centre line over `duration` seconds, from the scene load or an operator cue. */
export interface FlagReveal { direction: 'center_horizontal'; duration: number; start: 'load' | 'cue' }
/**
 * A Fusion v2 Tifo placement (#210): an image on a rectangle of the package's point
 * cloud, whose coordinates are the pitch frame (metres, z down). `corners` are top-left,
 * top-right, bottom-right, bottom-left as the artwork reads. The studio seeds an
 * editable point-cloud flag from it (placements/tifo.ts).
 */
export interface FusionTifo {
  id: string;
  name: string;
  adId: string;
  adName: string;
  asset: FusionAsset;
  corners: [number, number, number][];
  width: number;
  height: number;
  assetRotation: ArtworkRotation;
  reveal: Omit<FlagReveal, 'start'>;
}
export interface Match {
  id: string;
  name: string;
  date: string;
  venue: string;
  venueId: string;
  mode: 'api' | 'offline';
  offlinePackageId?: string;
  field: FieldDimensions;
  placements: Placement[];
  /** A v2 package's point cloud of the field: the PLY's SHA-256 (its cloud id) and Fusion's file name. */
  pointCloud?: { sha256: string; fileName: string };
  /** A v2 package's Tifo placements; they need `pointCloud`. */
  tifos?: FusionTifo[];
}
export interface MatchRecord {
  match: Match;
  draft: Placement[];
  hiddenPlacementIds?: string[];
  updatedAt?: string;
  /** An offline match's material id → the stored file the studio reads it from; its name is the SHA-256 the package declared. */
  assetFiles?: Record<string, string>;
  /** An offline GLB material id → its alpha-blended material names, found when the package was imported. */
  assetBlended?: Record<string, string[]>;
  /** The stored file of an offline match's point cloud (`match.pointCloud`), named by its SHA-256. */
  pointCloudFile?: string;
}
export interface StorageAdapter { getItem(key: string): string | null; setItem(key: string, value: string): void }
export interface FusionAdapter {
  lookupMatch(id: string): Promise<unknown>;
  submitScan(request: { matchId: string; venueId: string; file: File }): Promise<{ taskId: string }>;
  getAnalysis(taskId: string): Promise<unknown>;
  savePlacements(request: { matchId: string; placements: Placement[] }): Promise<unknown>;
}
export interface PreviewLens { pan: number; tilt: number; zoom: number }

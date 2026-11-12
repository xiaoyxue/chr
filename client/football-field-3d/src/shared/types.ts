export interface FieldDimensions { length: number; width: number }
export interface Point { x: number; y: number }
export interface GroundRectangle extends Point { sizeX: number; sizeY: number; rz?: number }
export interface Pose extends Point { z: number; rx: number; ry: number; rz: number }
export type AssetRole = 'car' | 'bank-left' | 'bank-right';
export interface FusionAsset { id: string; name: string; contentType: string; url: string }
export interface FusionModelPose { x: number; y: number; scale: number; yaw: number }
interface SlotBase extends Pose { id: string; name: string; adId?: string; adName?: string; asset?: FusionAsset; assetRotation?: number }
export interface GroundPlacement extends SlotBase { type: '2D'; sizeX: number; sizeY: number; assetRole?: never }
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
export interface Match {
  id: string;
  name: string;
  date: string;
  venue: string;
  venueId: string;
  mode: 'api';
  field: FieldDimensions;
  placements: Placement[];
}
export interface MatchRecord { match: Match; draft: Placement[]; hiddenPlacementIds?: string[]; updatedAt?: string }
export interface StorageAdapter { getItem(key: string): string | null; setItem(key: string, value: string): void }
export interface FusionAdapter {
  lookupMatch(id: string): Promise<unknown>;
  submitScan(request: { matchId: string; venueId: string; file: File }): Promise<{ taskId: string }>;
  getAnalysis(taskId: string): Promise<unknown>;
  savePlacements(request: { matchId: string; placements: Placement[] }): Promise<unknown>;
}
export interface PreviewLens { pan: number; tilt: number; zoom: number }

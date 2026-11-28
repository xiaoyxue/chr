// The size limits of an offline Fusion package, one definition for the page (offline-reader.ts,
// uploads) and its onsite server (scripts/studio-store.ts), so a package one accepts the other does too.
import { MAX_PLY_BYTES } from '../analysis/ply-result.ts';

const MiB = 1024 * 1024, GiB = 1024 * MiB;

/**
 * All materials of a package together. One constant, not a setting: the page is built ahead of
 * time (the Nix package), so a value read at run time could differ between the page and its server.
 */
export const MAX_MATERIAL_BYTES = 5 * GiB;
/**
 * Each material on its own: chr-server imports an asset of at most 256 MiB (MAX_ASSET_BYTES in
 * server/runtime/src/program/assets.rs), so a larger one could be imported but never rendered.
 */
export const MAX_ASSET_BYTES = 256 * MiB;
export const MAX_MANIFEST_BYTES = MiB;
/** A whole package ZIP: its materials, point cloud and manifest, with room for the archive's own records. */
export const MAX_PACKAGE_BYTES = MAX_MATERIAL_BYTES + MAX_PLY_BYTES + 4 * MAX_MANIFEST_BYTES;

/** A byte count for messages: 258.5 MiB, 5 GiB. */
export function formatBytes(bytes: number): string {
  const [unit, size] = bytes >= GiB ? ['GiB', GiB] : bytes >= MiB ? ['MiB', MiB] : ['KiB', 1024];
  return `${Number((bytes / size).toFixed(1))} ${unit}`;
}

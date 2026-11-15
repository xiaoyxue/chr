// Where the studio places a local ad, shared by its preview (local-ad-scene.ts)
// and its render description, so the program output matches the preview.
// Three.js world = chr world: metres, ground Z = 0, up is negative Z.
import * as THREE from 'three';
import type { LocalAd } from './local-ad.ts';

/** Keeps a flat ad just off the surface it was drawn on, towards the side it was picked from. */
const SURFACE_LIFT = 0.015;

export const adNormal = (ad: LocalAd) => new THREE.Vector3().crossVectors(new THREE.Vector3(...ad.plane.u), new THREE.Vector3(...ad.plane.v)).normalize();

/**
 * The ad's root frame: its rectangle's centre, moved `offset` (plus the surface
 * lift) along the normal u × v, with local axes u, v and the normal.
 */
export function adRoot(ad: LocalAd) {
  const { plane } = ad, u = new THREE.Vector3(...plane.u), v = new THREE.Vector3(...plane.v), normal = adNormal(ad);
  const position = new THREE.Vector3(...plane.origin).addScaledVector(u, plane.width / 2).addScaledVector(v, plane.height / 2)
    .addScaledVector(normal, ad.offset + SURFACE_LIFT * ad.normalSign);
  return new THREE.Matrix4().makeBasis(u, v, normal).setPosition(position);
}

/**
 * How a 3D ad's model sits in its root frame: stood up out of the plane towards
 * the picking side (glTF +Y along normalSign × normal), turned by `yaw` about its
 * own up axis and scaled to fit its glTF footprint (`size` X × Z) into the rectangle.
 */
export function modelFit(ad: LocalAd, footprint: readonly [number, number]) {
  return {
    rotation: new THREE.Euler(ad.normalSign * Math.PI / 2, ad.yaw * Math.PI / 180, 0),
    scale: Math.min(ad.plane.width / footprint[0], ad.plane.height / footprint[1]) * ad.scale,
  };
}

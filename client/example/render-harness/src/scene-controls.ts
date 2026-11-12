import {
  parseSceneFrameParams, parseSceneFrameResult,
  type RenderPinhole, type RenderSceneOperation, type RenderSceneSnapshot, type RenderTarget,
  type RenderVec3,
} from "./scene-frame.ts";

type PoseCamera = Extract<RenderPinhole, { kind: "intrinsics_pose" }>;
export type CameraEdit = "zoom-in" | "zoom-out" | "dolly-in" | "dolly-out" |
  "orbit-left" | "orbit-right" | "orbit-up" | "orbit-down";

/** Forward row-major pose; local +Z looks forward, world up is -Z. */
export function editCamera(camera: RenderPinhole, edit: CameraEdit): PoseCamera {
  if (camera.kind !== "intrinsics_pose") throw new Error("Controls require an intrinsics_pose camera");
  const next = structuredClone(camera);
  if (edit.startsWith("zoom")) {
    const factor = edit === "zoom-in" ? 1.1 : 1 / 1.1;
    next.intrinsics.fx *= factor;
    next.intrinsics.fy *= factor;
    next.intrinsics.skew *= factor;
    if (next.intrinsics.fx < 1 || next.intrinsics.fx > 100_000) throw new Error("Zoom limit");
  } else if (edit.startsWith("dolly")) {
    const distance = edit === "dolly-in" ? 0.4 : -0.4;
    for (let row = 0; row < 3; row++) next.camera_to_world[row]![3] += camera.camera_to_world[row]![2] * distance;
  } else {
    // Orbit the synthetic scene's origin, rotating centre and basis together.
    // This also works at the initial top-down view without a look-at pole singularity.
    const horizontal = edit === "orbit-left" || edit === "orbit-right";
    const axis: RenderVec3 = horizontal ? [0, 0, -1] :
      [camera.camera_to_world[0][0], camera.camera_to_world[1][0], camera.camera_to_world[2][0]];
    const angle = (edit === "orbit-left" || edit === "orbit-up" ? 1 : -1) * Math.PI / 36;
    const [x, y, z] = axis;
    const c = Math.cos(angle), s = Math.sin(angle), t = 1 - c;
    const rotation = [
      [t*x*x+c, t*x*y-s*z, t*x*z+s*y],
      [t*x*y+s*z, t*y*y+c, t*y*z-s*x],
      [t*x*z-s*y, t*y*z+s*x, t*z*z+c],
    ];
    for (let row = 0; row < 3; row++) for (let col = 0; col < 4; col++) {
      next.camera_to_world[row]![col] = rotation[row]!.reduce(
        (sum, coefficient, index) => sum + coefficient * camera.camera_to_world[index]![col]!, 0);
    }
  }
  return next;
}

export interface SceneRpc { call(method: string, params?: unknown): Promise<unknown> }

/** Bounded serialized edits. No revision guessing or retry after uncertain replies. */
export class SceneControls {
  readonly rpc: SceneRpc;
  readonly target: RenderTarget;
  snapshot: RenderSceneSnapshot | null = null;
  initial: RenderSceneSnapshot | null = null;
  uncertain = false;
  #tail: Promise<unknown> = Promise.resolve();
  #queued = 0;

  constructor(rpc: SceneRpc, target: RenderTarget) {
    this.rpc = rpc;
    this.target = parseSceneFrameParams("scene.get", target);
  }

  async #query(): Promise<RenderSceneSnapshot> {
    const snapshot = parseSceneFrameResult("scene.get", await this.rpc.call("scene.get", this.target));
    if (snapshot.runtime_id !== this.target.runtime_id || snapshot.scene_id !== this.target.scene_id) {
      throw new Error("Scene reply belongs to another target");
    }
    this.snapshot = snapshot;
    return snapshot;
  }

  async load(): Promise<RenderSceneSnapshot> {
    if (this.#queued !== 0) throw new Error("Wait for queued edits before reloading");
    const snapshot = await this.#query();
    this.initial ??= structuredClone(snapshot);
    this.uncertain = false;
    return snapshot;
  }

  mutate(operations: (scene: RenderSceneSnapshot) => RenderSceneOperation[]): Promise<RenderSceneSnapshot> {
    if (this.#queued >= 8) return Promise.reject(new Error("Scene edit queue is full"));
    this.#queued++;
    const job = this.#tail.then(async () => {
      if (this.uncertain) throw new Error("Reload scene after uncertain mutation; no automatic retry");
      const snapshot = await this.#query();
      const params = parseSceneFrameParams("scene.mutate", {
        ...this.target, expected_revision: snapshot.scene_revision, operations: operations(snapshot),
      });
      try {
        const applied = parseSceneFrameResult("scene.mutate", await this.rpc.call("scene.mutate", params));
        if (applied.runtime_id !== this.target.runtime_id || applied.scene_id !== this.target.scene_id) {
          throw new Error("Mutation reply belongs to another target");
        }
        return await this.#query();
      } catch (error) {
        this.uncertain = true;
        try { await this.#query(); } catch { /* Keep the last known snapshot, not an invented revision. */ }
        throw error;
      }
    }).finally(() => { this.#queued--; });
    this.#tail = job.catch(() => undefined);
    return job;
  }
}

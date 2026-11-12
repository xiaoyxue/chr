import { SceneFrameSchemas as S, type RenderPinhole, type RenderSceneOperation,
  type RenderSceneSnapshot, type RenderVec3 } from "./scene-frame.ts";
import { editCamera, type CameraEdit } from "./scene-controls.ts";

type Instance = ReturnType<typeof S.instance>;
type Pose = Extract<RenderPinhole, { kind: "intrinsics_pose" }>;
interface Choice { instance: Instance; bounds: { min: RenderVec3; max: RenderVec3 } }
export interface PitchMetadata {
  profile: "pitch-demo-v1";
  target: { runtime_id: string; scene_id: string };
  configuration_id: string;
  source_frame: string;
  center_semantics: "synthetic" | "conditional_fixed_ti";
  ground_status: string;
  pitch: [number, number];
  original_camera: Extract<RenderPinhole, { kind: "ti_c" }>;
  preview_camera: Pose;
  models: Choice[];
}

function record(value: unknown): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid pitch metadata");
  return value as Record<string, unknown>;
}
function text(value: unknown): string {
  if (typeof value !== "string" || !value.trim()) throw new Error("Missing pitch provenance");
  return value;
}
function vector(value: unknown, length: number): number[] {
  if (!Array.isArray(value) || value.length !== length ||
    !value.every(n => typeof n === "number" && Number.isFinite(n))) throw new Error("Invalid pitch vector");
  return value as number[];
}

/** Explicit native-written local metadata, not a discovery protocol or a GLB loader. */
export function parsePitchMetadata(value: unknown): PitchMetadata {
  const v = record(value), target = record(v.target);
  const original = S.pinhole(v.original_camera), preview = S.pinhole(v.preview_camera);
  if (v.profile !== "pitch-demo-v1" || original.kind !== "ti_c" || preview.kind !== "intrinsics_pose" ||
    !["synthetic", "conditional_fixed_ti"].includes(String(v.center_semantics)) ||
    JSON.stringify(original.extent) !== JSON.stringify(preview.extent) ||
    !Array.isArray(v.models) || v.models.length < 1 || v.models.length > 8) throw new Error("Invalid pitch profile");
  const pitch = vector(v.pitch, 2) as [number, number];
  if (pitch.some(n => n <= 0 || n > 1000)) throw new Error("Invalid pitch dimensions");
  const models = v.models.map(item => {
    const c = record(item), b = record(c.bounds);
    const min = vector(b.min, 3) as RenderVec3, max = vector(b.max, 3) as RenderVec3;
    if (min.some((n, i) => n > max[i]!)) throw new Error("Invalid model bounds");
    const instance = S.instance(c.instance);
    if (instance.instance_id !== "model") throw new Error("Invalid model selection slot");
    return { instance, bounds: { min, max } };
  });
  return {
    profile: "pitch-demo-v1", target: { runtime_id: text(target.runtime_id), scene_id: text(target.scene_id) },
    configuration_id: text(v.configuration_id), source_frame: text(v.source_frame),
    center_semantics: v.center_semantics as PitchMetadata["center_semantics"], ground_status: text(v.ground_status),
    pitch, original_camera: original, preview_camera: preview, models,
  };
}

export interface RigEdit {
  rig: "none" | "points" | "spots";
  sun: boolean;
  height: number;
  targets: [number, number][];
  intensity: number;
  radius: number;
  inner: number;
  outer: number;
  sunEmission: number;
  sunDirection: RenderVec3;
  sunRadius: number;
}

/** Keep the imported source immutable even if a caller mutates a returned copy. */
export class PitchControls {
  readonly #metadata: PitchMetadata;
  constructor(value: unknown) { this.#metadata = structuredClone(parsePitchMetadata(value)); }
  get metadata(): PitchMetadata { return structuredClone(this.#metadata); }

  check(scene: RenderSceneSnapshot): void {
    const m = this.#metadata;
    if (scene.runtime_id !== m.target.runtime_id || scene.scene_id !== m.target.scene_id ||
      scene.state.configuration_id !== m.configuration_id) throw new Error("Pitch metadata belongs to another installation");
  }

  camera(scene: RenderSceneSnapshot, action: CameraEdit | "reset"): RenderSceneOperation[] {
    this.check(scene);
    if (action === "reset") return [{ kind: "camera_set", camera: structuredClone(this.#metadata.original_camera) }];
    if (scene.state.camera.mode !== "manual") throw new Error("Manual preview camera required");
    let camera = scene.state.camera.camera;
    if (camera.kind === "ti_c") {
      if (JSON.stringify(camera) !== JSON.stringify(this.#metadata.original_camera)) {
        throw new Error("Unknown imported camera; reset before editing");
      }
      camera = this.#metadata.preview_camera;
    }
    return [{ kind: "camera_set", camera: editCamera(camera, action) }];
  }

  model(scene: RenderSceneSnapshot, index: number, scale: number, yaw: number, mirrored: boolean): RenderSceneOperation[] {
    this.check(scene);
    const choice = this.#metadata.models[index];
    if (!choice || !Number.isFinite(scale) || scale < 0.05 || scale > 10 || !Number.isFinite(yaw)) throw new Error("Invalid model edit");
    const instance = structuredClone(choice.instance), source = choice.instance.model;
    const c = Math.cos(yaw), s = Math.sin(yaw);
    for (let col = 0; col < 3; col++) {
      const x = source[0][col]! * (mirrored ? -1 : 1), y = source[1][col]!;
      instance.model[0][col] = scale * (c*x-s*y);
      instance.model[1][col] = scale * (s*x+c*y);
      instance.model[2][col] = scale * source[2][col]!;
    }
    // Conservative all-parts world AABB: all eight corners, max Z is the ground-facing side.
    let maxZ = -Infinity;
    for (let corner = 0; corner < 8; corner++) {
      let z = 0;
      for (let axis = 0; axis < 3; axis++) {
        z += instance.model[2][axis]! * (corner & (1 << axis) ? choice.bounds.max[axis]! : choice.bounds.min[axis]!);
      }
      maxZ = Math.max(maxZ, z);
    }
    instance.model[2][3] = -maxZ;
    return [{ kind: "instance_put", instance }];
  }

  lighting(scene: RenderSceneSnapshot, edit: RigEdit): RenderSceneOperation[] {
    this.check(scene);
    const { height, intensity, radius, inner, outer, sunEmission, sunRadius } = edit;
    if (typeof edit.sun !== "boolean" || ![height,intensity,radius,inner,outer,sunEmission,sunRadius,...edit.sunDirection].every(Number.isFinite) ||
      height < 30 || height > 40 || intensity < 0 || intensity > 100000 || radius < 0 || radius > 5 ||
      inner < 0 || inner >= outer || outer >= Math.PI/2 || sunEmission < 0 || sunEmission > 20 ||
      sunRadius < 0 || sunRadius > 0.1 || edit.sunDirection.every(n => n === 0) ||
      edit.targets.length !== 4 || !["none","points","spots"].includes(edit.rig)) throw new Error("Invalid rig controls");
    const [length, width] = this.#metadata.pitch;
    const corners = [[-length/2,-width/2],[-length/2,width/2],[length/2,-width/2],[length/2,width/2]];
    const lights = [];
    const operations: RenderSceneOperation[] = [];
    for (const [i, [x, y]] of corners.entries()) {
      const target = vector(edit.targets[i], 2);
      if (Math.abs(target[0]!) > length/2 || Math.abs(target[1]!) > width/2) throw new Error("Spot target must be on the pitch");
      const position: RenderVec3 = [x!, y!, -height];
      if (edit.rig !== "none") {
        const common = { light_id: `tower-light-${i}`, position, intensity: [intensity,intensity,intensity],
          source_radius: radius, shadow_mode: null };
        lights.push(edit.rig === "points" ? { ...common, kind: "point" } :
          { ...common, kind: "spot", direction: [target[0]!-x!,target[1]!-y!,height], inner_angle: inner, outer_angle: outer });
      }
      operations.push({ kind: "instance_put", instance: S.instance({
        instance_id: `tower-${i}`, asset_ref: "pitch-tower",
        model: [[0.2,0,0,x],[0,0.2,0,y],[0,0,(height-1)/2,0],[0,0,0,1]], visible: true, material_overrides: [],
      }) });
    }
    if (edit.sun) lights.push({ light_id: "sun", kind: "directional", direction: edit.sunDirection,
      irradiance: [sunEmission,sunEmission,sunEmission], angular_radius: sunRadius, shadow_mode: null });
    operations.push({ kind: "lighting_set", lighting: S.lighting({ lights, environment: scene.state.lighting.environment }) });
    return operations;
  }
}

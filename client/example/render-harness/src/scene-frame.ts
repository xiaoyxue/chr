// Mirror of common/src/scene_frame. Limited synthetic profile, not runtime support.
// Exact required keys include nullable fields. Geometry math stays in the renderer.

type Schema<T> = (value: unknown) => T;
type Infer<S> = S extends Schema<infer T> ? T : never;

function invalid(): never { throw new Error("Invalid scene_frame_v1 payload"); }
function refine<T>(schema: Schema<T>, test: (value: T) => boolean): Schema<T> {
  return value => { const parsed = schema(value); return test(parsed) ? parsed : invalid(); };
}
const text: Schema<string> = value => typeof value === "string" ? value : invalid();
const finite: Schema<number> = value =>
  typeof value === "number" && Number.isFinite(value) ? value : invalid();
const bool: Schema<boolean> = value => typeof value === "boolean" ? value : invalid();
const uint = refine(finite, value => Number.isInteger(value) && value >= 0 && value <= 4294967295);
const positive = refine(uint, value => value > 0);
const id = refine(text, value => value.length > 0);
const u64 = refine(text, value => /^(0|[1-9][0-9]*)$/.test(value)
  && (value.length < 20 || (value.length === 20 && value <= "18446744073709551615")));
const budget = refine(u64, value => value !== "0");
function literal<const T extends string | number>(...values: T[]): Schema<T> {
  return value => values.some(item => item === value) ? value as T : invalid();
}
function nullable<T>(schema: Schema<T>): Schema<T | null> {
  return value => value === null ? null : schema(value);
}
function array<T>(schema: Schema<T>): Schema<T[]> {
  return value => Array.isArray(value) ? value.map(schema) : invalid();
}
function tuple<const S extends Schema<unknown>[]>(...schemas: S): Schema<{ -readonly [K in keyof S]: Infer<S[K]> }> {
  return value => {
    if (!Array.isArray(value) || value.length !== schemas.length) return invalid();
    return schemas.map((schema, i) => schema(value[i])) as { -readonly [K in keyof S]: Infer<S[K]> };
  };
}
function object<S extends Record<string, Schema<unknown>>>(fields: S): Schema<{ [K in keyof S]: Infer<S[K]> }> {
  return value => {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return invalid();
    const input = value as Record<string, unknown>;
    const keys = Object.keys(fields);
    if (Object.keys(input).length !== keys.length || !keys.every(key => Object.hasOwn(input, key))) return invalid();
    return Object.fromEntries(keys.map(key => [key, fields[key]!(input[key])])) as { [K in keyof S]: Infer<S[K]> };
  };
}
function union<const S extends Schema<unknown>[]>(...schemas: S): Schema<Infer<S[number]>> {
  return value => {
    for (const schema of schemas) {
      try { return schema(value) as Infer<S[number]>; } catch { /* Try the next exact variant. */ }
    }
    return invalid();
  };
}
function unique<T>(values: T[], key: (value: T) => string | number): boolean {
  return new Set(values.map(key)).size === values.length;
}
function gcd(a: number, b: number): number {
  while (b !== 0) [a, b] = [b, a % b];
  return a;
}
const vec3 = tuple(finite, finite, finite);
const vec4 = tuple(finite, finite, finite, finite);
const matrix3 = tuple(finite, finite, finite, finite, finite, finite, finite, finite, finite);
const matrix4 = tuple(vec4, vec4, vec4, vec4);
const version = object({ id, revision: u64 });
const frameKey = object({ stream_id: id, sequence: u64 });
const timeTerm = refine(positive, value => value <= 2147483647);
const timeBase = refine(object({ numerator: timeTerm, denominator: timeTerm }),
  value => gcd(value.numerator, value.denominator) === 1);
const timeline = object({ timeline_id: id, time_base: timeBase });
const position = object({ timeline_id: id, ticks: u64 });
const effect = object({ runtime_id: id, sequence: u64 });
const extent = object({ width: positive, height: positive });
const runtimeFields = { runtime_id: id };
const targetFields = { ...runtimeFields, scene_id: id };
const submissionTargetFields = { ...runtimeFields, submission_id: id };
const runtimeTarget = object(runtimeFields);
const target = object(targetFields);
const imageTarget = object({ ...runtimeFields, image_ref: id });
const submissionTarget = object(submissionTargetFields);
const shadowMode = literal("off", "hard", "pcf", "pcss");
const configuration = object({
  ...runtimeFields, configuration_id: id, profile: literal("scene_frame_v1"),
  native_extent: extent, output_extent: extent, output_rate: timeBase,
  fit: literal("contain", "cover", "stretch"),
  input_stage: literal("scene_background", "display_background"),
  output_transfer: literal("srgb", "bt709"), exposure: finite,
  tone_map: literal("aces", "reinhard"), sample_count: literal(1, 4),
  shadows: object({
    resolution: positive, filter_radius: finite,
    pcf_samples: literal(1, 4, 9, 16, 25, 36, 49, 64),
    depth_bias: finite, normal_bias: finite, max_bytes: budget, max_samples_per_fragment: budget,
    pcss: object({
      blocker_samples: refine(positive, n => n <= 64),
      filter_samples: refine(positive, n => n <= 64),
      max_search_radius: refine(finite, n => n > 0),
      max_filter_radius: refine(finite, n => n > 0),
    }),
    cascades: object({
      count: literal(1, 2, 3, 4), max_distance: refine(finite, n => n > 0),
      split_lambda: refine(finite, n => n >= 0 && n <= 1),
      blend_fraction: refine(finite, n => n >= 0 && n <= 0.5),
    }),
  }),
  environment: object({
    resolution: refine(positive, n => n >= 64 && n <= 1024 && (n & (n - 1)) === 0),
    samples: refine(positive, n => n <= 1024), max_bytes: budget,
  }),
  max_intermediate_bytes: budget,
  limits: object({
    scenes: positive, instances_per_scene: positive, lights_per_scene: positive,
    operations_per_mutation: positive, published_frames: positive, frames_per_submission: positive,
    inflight_frames: positive, retained_submissions: positive, render_timeout_ms: positive,
  }),
});
const pbrFactors = object({
  base_color: vec4, metallic: finite, roughness: finite, emissive: vec3,
  alpha_cutoff: nullable(finite), double_sided: bool, normal_scale: finite,
  occlusion_strength: finite, unlit: bool,
});
const instance = object({
  instance_id: id, asset_ref: id, model: matrix4, visible: bool,
  material_overrides: refine(array(object({ part: uint, factors: pbrFactors })),
    values => unique(values, value => value.part)),
});
const lightFields = { light_id: id, shadow_mode: nullable(shadowMode) };
const light = union(
  object({ ...lightFields, kind: literal("directional"), direction: vec3, irradiance: vec3, angular_radius: finite }),
  object({ ...lightFields, kind: literal("point"), position: vec3, intensity: vec3, source_radius: finite }),
  object({
    ...lightFields, kind: literal("spot"), position: vec3, direction: vec3,
    intensity: vec3, source_radius: finite, inner_angle: finite, outer_angle: finite,
  }),
);
const lighting = object({
  lights: refine(array(light), values => unique(values, value => value.light_id)),
  environment: nullable(object({ asset_ref: id, gain: finite, rotation: finite, visible: bool })),
});
const clipFields = { near_m: finite, far_m: finite };
const clipping = (value: { near_m: number; far_m: number }) => value.near_m > 0 && value.near_m < value.far_m;
const pinholeFields = { extent, ...clipFields };
const pinhole = refine(union(
  object({
    ...pinholeFields, kind: literal("intrinsics_pose"),
    intrinsics: object({ fx: finite, fy: finite, skew: finite, cx: finite, cy: finite }),
    camera_to_world: matrix4,
  }),
  object({ ...pinholeFields, kind: literal("ti_c"), ti: matrix3, camera_center: vec3 }),
), clipping);
const cameraMode = union(
  object({ mode: literal("manual"), camera: pinhole }),
  refine(object({ mode: literal("tracked"), ...clipFields }), clipping),
);
const sceneState = object({
  world: version, configuration_id: id, camera: cameraMode,
  instances: refine(array(instance), values => unique(values, value => value.instance_id)),
  lighting, shadow_mode: shadowMode,
});
const sceneCreate = object({ ...targetFields, state: sceneState });
const appliedFields = { ...targetFields, scene_revision: u64, effect };
const sameRuntime = (value: { runtime_id: string; effect: { runtime_id: string } }) =>
  value.runtime_id === value.effect.runtime_id;
const sceneApplied = refine(object(appliedFields), sameRuntime);
const sceneSnapshot = refine(object({ ...appliedFields, state: sceneState }), sameRuntime);
const operation = union(
  object({ kind: literal("instance_put"), instance }),
  object({ kind: literal("instance_remove"), instance_id: id }),
  object({ kind: literal("lighting_set"), lighting }),
  object({ kind: literal("shadow_mode_set"), shadow_mode: shadowMode }),
  object({ kind: literal("camera_set"), camera: pinhole }),
);
const sceneMutate = object({
  ...targetFields, expected_revision: u64,
  operations: refine(array(operation), values => values.length > 0),
});
const imageDescription = refine(object({
  extent, format: literal("rgba8_unorm", "bgra8_unorm", "rgba16_float"),
  transfer: literal("linear", "srgb", "bt709"), alpha: literal("straight", "premultiplied"),
}), value => value.format !== "rgba16_float" || value.transfer === "linear");
const framePublish = refine(object({
  ...runtimeFields, producer_ticket: id, frame: frameKey, timeline, position, image: imageDescription,
}), value => value.timeline.timeline_id === value.position.timeline_id);
const publishedFrame = object({ ...runtimeFields, frame: frameKey, image_ref: id, position });
const geometry = union(
  object({ kind: literal("scene_camera") }),
  object({ kind: literal("ti_c"), world: version, context_id: nullable(id), extent, ti: matrix3, camera_center: vec3 }),
);
const frameInput = object({ frame: frameKey, image_ref: id, position, geometry });
const frameIdentity = (value: { frame: RenderFrameKey }) => JSON.stringify([value.frame.stream_id, value.frame.sequence]);
const submit = object({
  ...targetFields, submission_id: id, expected_scene_revision: nullable(u64),
  frames: refine(array(frameInput), values => values.length > 0 && unique(values, frameIdentity)),
});
const receiptFields = { ...submissionTargetFields, scene_id: id, scene_revision: u64, effect };
const acceptedFrame = object({ frame: frameKey, position });
const submissionAccepted = refine(object({
  ...receiptFields, frames: refine(array(acceptedFrame), values => values.length > 0 && unique(values, frameIdentity)),
}), sameRuntime);
const failureKind = literal("write_failed", "deadline_exceeded", "cancelled", "render_failed", "runtime_failed");
const delivery = union(
  object({ state: literal("queued") }),
  object({ state: literal("transmitted"), output_stream_id: id, first_slot: u64 }),
  object({ state: literal("dropped"), reason: literal("queue_full", "output_failed") }),
);
const frameResultFields = { frame: frameKey, position };
const frameResult = union(
  object({ ...frameResultFields, state: literal("pending") }),
  object({ ...frameResultFields, state: literal("failed"), reason: failureKind }),
  object({ ...frameResultFields, state: literal("rendered"), output_frame_id: id, delivery }),
);
const submissionSnapshot = refine(object({
  ...receiptFields, frames: refine(array(frameResult), values => values.length > 0 && unique(values, frameIdentity)),
}), sameRuntime);
const cancelResult = refine(object({
  ...submissionTargetFields, cancelled: array(frameKey), too_late: array(frameKey),
}), value => unique([...value.cancelled, ...value.too_late], frame => JSON.stringify([frame.stream_id, frame.sequence])));
const releaseResult = object({ released: bool });
const rejectionKinds = [
  "runtime_changed", "not_found", "conflict", "incompatible_input", "capacity", "invalid_state", "unsupported_profile",
] as const;
const rejection = refine(object({
  code: literal(2001, 2002, 2003, 2004, 2005, 2006, 2007), message: text,
  data: object({ kind: literal(...rejectionKinds), frame: nullable(frameKey) }),
}), value => value.code === 2001 + rejectionKinds.indexOf(value.data.kind));

export const SceneFrameSchemas = {
  id, u64, timeBase, frameKey, timeline, position, effect, version, extent,
  configuration, pbrFactors, instance, light, lighting, pinhole, cameraMode,
  sceneState, sceneCreate, sceneApplied, sceneSnapshot, sceneMutate,
  imageDescription, framePublish, publishedFrame, imageTarget, geometry, frameInput,
  submit, submissionTarget, submissionAccepted, submissionSnapshot, cancelResult, releaseResult, rejection,
};

export const SceneFrameMethods = {
  "render.get_configuration": { params: runtimeTarget, result: configuration },
  "scene.create": { params: sceneCreate, result: sceneApplied },
  "scene.get": { params: target, result: sceneSnapshot },
  "scene.mutate": { params: sceneMutate, result: sceneApplied },
  "scene.release": { params: target, result: releaseResult },
  "frame.publish": { params: framePublish, result: publishedFrame },
  "frame.release": { params: imageTarget, result: releaseResult },
  "render.submit": { params: submit, result: submissionAccepted },
  "render.get": { params: submissionTarget, result: submissionSnapshot },
  "render.cancel": { params: submissionTarget, result: cancelResult },
  "render.release": { params: submissionTarget, result: releaseResult },
} as const;
export type SceneFrameMethod = keyof typeof SceneFrameMethods;
export type SceneFrameParams<M extends SceneFrameMethod> = Infer<typeof SceneFrameMethods[M]["params"]>;
export type SceneFrameResult<M extends SceneFrameMethod> = Infer<typeof SceneFrameMethods[M]["result"]>;

export function parseSceneFrameParams<M extends SceneFrameMethod>(method: M, value: unknown): SceneFrameParams<M> {
  return SceneFrameMethods[method].params(value) as SceneFrameParams<M>;
}
export function parseSceneFrameResult<M extends SceneFrameMethod>(method: M, value: unknown): SceneFrameResult<M> {
  return SceneFrameMethods[method].result(value) as SceneFrameResult<M>;
}

export type RenderId = Infer<typeof id>;
export type RenderU64 = Infer<typeof u64>;
export type RenderVec3 = Infer<typeof vec3>;
export type RenderVec4 = Infer<typeof vec4>;
export type RenderMatrix3 = Infer<typeof matrix3>;
export type RenderMatrix4 = Infer<typeof matrix4>;
export type RenderFrameKey = Infer<typeof frameKey>;
export type RenderTimeBase = Infer<typeof timeBase>;
export type RenderTimeline = Infer<typeof timeline>;
export type RenderPosition = Infer<typeof position>;
export type RenderEffect = Infer<typeof effect>;
export type RenderVersion = Infer<typeof version>;
export type RenderExtent = Infer<typeof extent>;
export type RenderTarget = Infer<typeof target>;
export type RenderShadowMode = Infer<typeof shadowMode>;
export type RenderConfiguration = Infer<typeof configuration>;
export type RenderPbrFactors = Infer<typeof pbrFactors>;
export type RenderInstance = Infer<typeof instance>;
export type RenderLight = Infer<typeof light>;
export type RenderLighting = Infer<typeof lighting>;
export type RenderPinhole = Infer<typeof pinhole>;
export type RenderCameraMode = Infer<typeof cameraMode>;
export type RenderSceneState = Infer<typeof sceneState>;
export type RenderSceneCreate = Infer<typeof sceneCreate>;
export type RenderSceneApplied = Infer<typeof sceneApplied>;
export type RenderSceneSnapshot = Infer<typeof sceneSnapshot>;
export type RenderSceneMutate = Infer<typeof sceneMutate>;
export type RenderSceneOperation = Infer<typeof operation>;
export type RenderImageDescription = Infer<typeof imageDescription>;
export type RenderFramePublish = Infer<typeof framePublish>;
export type RenderPublishedFrame = Infer<typeof publishedFrame>;
export type RenderImageTarget = Infer<typeof imageTarget>;
export type RenderFrameGeometry = Infer<typeof geometry>;
export type RenderFrameInput = Infer<typeof frameInput>;
export type RenderSubmit = Infer<typeof submit>;
export type RenderSubmissionTarget = Infer<typeof submissionTarget>;
export type RenderSubmissionAccepted = Infer<typeof submissionAccepted>;
export type RenderSubmissionSnapshot = Infer<typeof submissionSnapshot>;
export type RenderFrameResult = Infer<typeof frameResult>;
export type RenderFailureKind = Infer<typeof failureKind>;
export type RenderCancelResult = Infer<typeof cancelResult>;
export type RenderReleaseResult = Infer<typeof releaseResult>;
export type RenderRejection = Infer<typeof rejection>;

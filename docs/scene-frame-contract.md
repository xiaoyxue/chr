# Initial scene and frame control contract

**Status:** `approved-for-synthetic-implementation` - limited owner approval for
the initial #12 profile, recorded on 2026-11-12 in #12. This is not blanket
production-policy approval, approval of #63, or a claim that methods are implemented.

Approved scope: the method/type/error surface below, revision and bounded retry/
release semantics, whole-batch admission with independent post-admission results,
and #20/#21 implementation using explicit configuration, registered synthetic
assets/producers and a trusted local controller. Production authorization,
hardware acquisition, tracking-loss and hot-reconfiguration policy remain gated.

**Scope:** Linux, retained static models, pinhole cameras, direct/IBL lighting,
frame publication, render submission and program-output association.

**Depends on:** [Protocol conventions](protocol-conventions.md),
[Execution model](execution-model.md),
[Frame ingestion](render-frame-input.md),
[Coordinate contract](../server/docs/coordinates.md).

## Boundary and implementation mapping

`common::scene_frame` now supplies the initial DTOs, `Validate`, `parse_payload`,
`parse_params`, method constants and typed rejection conversion to the existing
JSON-RPC error object. The TypeScript mirror is
`client/example/render-harness/src/scene-frame.ts`, reexported as `SceneFrame`
by `protocol.ts`. This implements the wire boundary only, not runtime methods.
Call the shared parser before native math validation and resource resolution;
plain serde decoding alone does not check cross-field metadata or installed
capacity. IDs/time/finite numbers are validated scalar types. Required nullable
fields must be present even when null; unknown fields are errors.
DTO objects reject positional arrays. Numeric integer fields accept equivalent
JSON spellings (`1`, `1.0`, `1e0`), matching JavaScript's number domain; U64 text
never uses that normalization and remains canonical decimal only.

Shared synthetic fixtures live in `common/tests/fixtures/scene_frame.json`.
`cargo test --locked -p chr-common` checks round trips, negative payloads and
the same mock handler over both IPC transports. Run the harness's
`npm run typecheck` and `npm test` against those same fixtures. These CPU/control
checks establish neither GPU behavior nor production authorization or policy.
The current DTO representation uses unsigned 32-bit dimensions/counts and signed
31-bit rational terms; actual supported dimensions and capacities still require
explicit native/configuration validation.

JSON-RPC 2.0, NativeSocket framing and WebSocket text messages remain unchanged.
Every public method below uses the same handler and semantics over both control
transports. Image/model bytes and GPU dependencies are not embedded in JSON.
Existing triangle, SDI-status and WebRTC-signaling methods remain compatible.

| Owner | Existing implementation / future responsibility |
| --- | --- |
| Renderer | `scene::{Scene, SceneView, ModelMatrix, Lighting}`, `camera::Camera`, `PbrSettings`, `RenderSettings`, `composition::FrameCompositor`. No networking or frame scheduler. |
| #20 | One Rust DTO/validation definition and its TypeScript mirror. No renderer resource lookup or camera math in `common/`. |
| #21 | Registered producers, published resources, readiness and physical release. Opaque refs never expose a slot, pointer or texture-array layer. |
| #38 | [Opt-in retained control adapter](../server/docs/retained-control.md): asset/camera mapping, atomic scenes and immutable binding through an explicit admission port. Default daemon output is unchanged. |
| #22 | Join input/geometry, serialize scene binding, schedule bounded work and preserve submission provenance. |
| #31 | [Explicit synthetic program output](../server/docs/program-output.md) connects completed renderer results to the existing simulated SDI port and monitor; no render-direct second stream or production-policy approval. |

The initial profile excludes orthographic/lens wire extensions, area/atmosphere
lights, animation, live hardware acquisition and recording/calibration services.
Native renderer support does not automatically make a feature part of this
wire profile. A synthetic producer and registered synthetic assets are enough
to exercise the whole candidate without any of those implementations.

## Identity and time notation

These prefixed aliases make this document independently typecheckable. They
describe the same wire shapes proposed in #63, not a competing common library:
#20 must share one definition for each underlying identity/time type.
General clock/effect rules remain owned by the execution model.

```typescript
type RenderId = string;
type RenderU64 = string;
type RenderVec3 = [number, number, number];
type RenderVec4 = [number, number, number, number];
type RenderMatrix3 = [
  number, number, number,
  number, number, number,
  number, number, number,
];
type RenderMatrix4 = [
  RenderVec4, RenderVec4, RenderVec4, RenderVec4,
];
type RenderVersion = { id: RenderId; revision: RenderU64 };
type RenderFrameKey = { stream_id: RenderId; sequence: RenderU64 };
type RenderTimeBase = { numerator: number; denominator: number };
type RenderTimeline = {
  timeline_id: RenderId;
  time_base: RenderTimeBase;
};
type RenderPosition = { timeline_id: RenderId; ticks: RenderU64 };
type RenderEffect = { runtime_id: RenderId; sequence: RenderU64 };
type RenderTarget = { runtime_id: RenderId; scene_id: RenderId };
```

IDs are nonempty, opaque and never intentionally reused for a different object.
`RenderU64` is canonical unsigned decimal text, at most `18446744073709551615`;
it is not a JSON number. Incrementing beyond that bound fails rather than wraps.
Matrices and vectors contain finite values; matrix direction/layout is stated
where used. Undeclared fields are rejected at the wire boundary.

A stream restart creates a new stream ID. A seek/reset creates a new timeline;
positions on different timelines are not directly comparable. Time bases are
reduced positive rational seconds per tick with integer numerator/denominator
no larger than `2147483647`. Sequence numbers, media ticks and runtime effect
order have distinct meanings even if their example numbers happen to match.

Every process restart creates a new runtime ID and invalidates its resource,
scene and submission handles. This profile does not promise restart-persistent
runtime state or exactly-once effects.

## Configuration and resource entry

Before enabling this profile, the runtime installs a fully validated immutable
configuration and declares its ID. `render.get_configuration` retrieves it.
Hot output-format changes are deliberately excluded: stop/reconfigure/restart
is a separate protocol gate. Scene creation names the installed configuration;
clients cannot silently replace output color/timing by choosing another scene.

```typescript
type RenderExtent = { width: number; height: number };
type RenderShadowMode = "off" | "hard" | "pcf" | "pcss";
type RenderConfiguration = {
  runtime_id: RenderId;
  configuration_id: RenderId;
  profile: "scene_frame_v1";
  native_extent: RenderExtent;
  output_extent: RenderExtent;
  output_rate: RenderTimeBase;
  fit: "contain" | "cover" | "stretch";
  input_stage: "scene_background" | "display_background";
  output_transfer: "srgb" | "bt709";
  exposure: number;
  tone_map: "aces" | "reinhard";
  sample_count: 1 | 4;
  shadows: {
    resolution: number;
    filter_radius: number;
    pcf_samples: 1 | 4 | 9 | 16 | 25 | 36 | 49 | 64;
    depth_bias: number;
    normal_bias: number;
    max_bytes: RenderU64;
    max_samples_per_fragment: RenderU64;
    pcss: {
      blocker_samples: number;
      filter_samples: number;
      max_search_radius: number;
      max_filter_radius: number;
    };
    cascades: {
      count: 1 | 2 | 3 | 4;
      max_distance: number;
      split_lambda: number;
      blend_fraction: number;
    };
  };
  environment: {
    resolution: number;
    samples: number;
    max_bytes: RenderU64;
  };
  max_intermediate_bytes: RenderU64;
  limits: {
    scenes: number;
    instances_per_scene: number;
    lights_per_scene: number;
    operations_per_mutation: number;
    published_frames: number;
    frames_per_submission: number;
    inflight_frames: number;
    retained_submissions: number;
    render_timeout_ms: number;
  };
};
```

All dimensions, capacities and timeout values are positive supported integers;
memory/sample budgets are positive. `output_rate` is seconds per output slot,
not FPS: `1/50` denotes 50 slots per second. No example value becomes an implicit
production default. Limits account for private draining work as well as public
handles; an acknowledged cancellation does not create free physical capacity.

Native renderer validation is authoritative for camera/material/light math and
quality constraints. In particular, PCSS stages use 1..64 samples and positive
caps; cascades have `0 <= split_lambda <= 1`, `0 <= blend_fraction <= 0.5`;
environment resolution is power-of-two 64..1024 with 1..1024 samples.
Unsupported settings/budgets fail without silent quality downgrade.

Output is opaque with linear-sRGB/Rec.709 D65 primaries and black clear.
Source images declare their transfer and alpha below. Scene-linear backgrounds
and CG share one exposure/tone-map; display backgrounds bypass that transform.
Keep linear-premultiplied filtering/composition in the renderer. The configured
output conversion then feeds the SDI port. A transfer tag does not convert
unknown primaries, HDR PQ/HLG, gamut or video range.

Assets and producer write tickets enter through a separately owned native
registration boundary. It supplies immutable content, validated descriptors,
finite pixel data, authorized access and retained leases. A configured local
file adapter may verify/copy a content-addressed file; this profile does not
add an arbitrary-path RPC or assume clients share the server filesystem.
Unknown asset references or write tickets are rejected, not lazily guessed.

The approved first implementation uses an in-process synthetic registration
adapter and a trusted local controller. This is an explicit test/integration
scope, not a production registration or authorization protocol. All limits and
output settings must be supplied and validated; example values are not defaults.

## Scene, camera and lighting schemas

```typescript
type RenderPbrFactors = {
  base_color: RenderVec4;
  metallic: number;
  roughness: number;
  emissive: RenderVec3;
  alpha_cutoff: number | null;
  double_sided: boolean;
  normal_scale: number;
  occlusion_strength: number;
  unlit: boolean;
};
type RenderInstance = {
  instance_id: RenderId;
  asset_ref: RenderId;
  model: RenderMatrix4;
  visible: boolean;
  material_overrides: {
    part: number;
    factors: RenderPbrFactors;
  }[];
};
type RenderLight = {
  light_id: RenderId;
  shadow_mode: RenderShadowMode | null;
} & (
  | {
      kind: "directional";
      direction: RenderVec3;
      irradiance: RenderVec3;
      angular_radius: number;
    }
  | {
      kind: "point";
      position: RenderVec3;
      intensity: RenderVec3;
      source_radius: number;
    }
  | {
      kind: "spot";
      position: RenderVec3;
      direction: RenderVec3;
      intensity: RenderVec3;
      source_radius: number;
      inner_angle: number;
      outer_angle: number;
    }
);
type RenderLighting = {
  lights: RenderLight[];
  environment: {
    asset_ref: RenderId;
    gain: number;
    rotation: number;
    visible: boolean;
  } | null;
};
type RenderPinhole = {
  extent: RenderExtent;
  near_m: number;
  far_m: number;
} & (
  | {
      kind: "intrinsics_pose";
      intrinsics: {
        fx: number; fy: number; skew: number; cx: number; cy: number;
      };
      camera_to_world: RenderMatrix4;
    }
  | {
      kind: "ti_c";
      ti: RenderMatrix3;
      camera_center: RenderVec3;
    }
);
type RenderCameraMode =
  | { mode: "manual"; camera: RenderPinhole }
  | { mode: "tracked"; near_m: number; far_m: number };
type RenderSceneState = {
  world: RenderVersion;
  configuration_id: RenderId;
  camera: RenderCameraMode;
  instances: RenderInstance[];
  lighting: RenderLighting;
  shadow_mode: RenderShadowMode;
};
type RenderSceneSnapshot = RenderTarget & {
  scene_revision: RenderU64;
  effect: RenderEffect;
  state: RenderSceneState;
};
type RenderSceneCreate = RenderTarget & { state: RenderSceneState };
type RenderSceneOperation =
  | { kind: "instance_put"; instance: RenderInstance }
  | { kind: "instance_remove"; instance_id: RenderId }
  | { kind: "lighting_set"; lighting: RenderLighting }
  | { kind: "shadow_mode_set"; shadow_mode: RenderShadowMode }
  | { kind: "camera_set"; camera: RenderPinhole };
type RenderSceneMutate = RenderTarget & {
  expected_revision: RenderU64;
  operations: RenderSceneOperation[];
};
type RenderSceneApplied = RenderTarget & {
  scene_revision: RenderU64;
  effect: RenderEffect;
};
```

Models are forward row-major local-to-world affine transforms acting on column
vectors; last row is `[0,0,0,1]`, the linear part is nonsingular. Negative-Z-up
and metres are preserved. Use visibility, not zero scale, to hide an instance.
GLB basis conversion is performed once by the existing importer, not again in
the runtime adapter.

Imported GLB material maps are retained. Per-part overrides replace factors
only for that instance; they do not mutate the shared asset or other instances.
Part indices are unique in one override list and must exist in the registered
asset. Alpha masking is supported; general transparent-mesh alpha blending is
not implied by a base-color alpha value.

Material scalar constraints match `PbrMaterial`: base color, metallic,
roughness, occlusion strength and nonnull cutoff are in `[0,1]`; emissive values
are nonnegative; normal scale is finite. PBR is the main path and `unlit` is an
explicit diagnostic/material choice.

Light IDs are unique. Directions are nonzero light-travel vectors. Direct-light
RGB values are nonnegative, in the native API's W/m^2 irradiance for directional
lights and W/sr intensity for local lights. Positions/source radii are metres;
angles are radians. `0 <= inner < outer < pi/2`; directional angular radius is
in `[0,pi/2)`, local source radius is nonnegative. Source size controls PCSS
softness, not a different finite-area BRDF. Null per-light shadow mode inherits
the scene mode; explicit `off` disables shadowing, not illumination.

Environment references resolve to supported OpenEXR radiance. Gain is
nonnegative; rotation is yaw about world up (-Z). Hiding its background leaves
IBL enabled. This is unoccluded IBL, not atmosphere or scene sky visibility.

Pinhole K is in native pixels; pose is a proper rigid camera-to-world transform.
Ti is the original forward ground-XY-to-SNC matrix; C is a compatible world
centre. Require `0 < near_m < far_m`, valid geometry and the configured native
extent. Never invert/transpose/renormalize imported Ti or add half-pixel offsets.
Manual camera updates replace the whole validated camera. Zoom/orbit/reset are
client-side edits of a separate manual camera, not mutations of calibration.
Tracked scenes reject `camera_set`; switching mode requires a new scene.
Their `near_m`/`far_m` are positive optical distances in metres satisfying
`near_m < far_m`. Each tracked frame constructs its native Ti+C camera using
these values from the bound scene snapshot, not values inferred from Ti/C or
undocumented defaults. Changing tracked clipping requires another scene in
this initial profile.

## Scene operation effects

`scene.create` validates and prepares the full state before publishing revision
`"0"` with an effect receipt. Reusing a live scene ID rejects with conflict;
there is no persistent create-deduplication promise for these runtime objects.

`scene.mutate` prepares a bounded, nonempty list against one expected revision.
Apply list order to a temporary state, then recheck runtime, scene existence
and expected revision under the same serialization boundary as other mutations.
On success publish one complete snapshot, increment the revision once and assign
one effect sequence. On failure publish nothing and release only new temporary
resources. JSON-RPC batch is not this atomic operation.

Instance put creates/replaces one instance; removing an absent instance rejects
the entire mutation. Duplicate IDs in a complete state are invalid. Multiple
operations on the same instance in one mutation follow list order.
Changing world or fixed configuration is not a mutation; create another scene.
An accepted old frame is never silently rebound to the replacement scene.

`scene.get` returns current full state, revision and effect for reconnect.
After an uncertain mutation reply, query and reconcile; do not automatically
retry using a newly fetched expected revision. Runtime effect order is not a
source frame or wall-clock timestamp.

`scene.release` removes the public scene immediately and disallows new work.
Already accepted submissions retain their immutable snapshots and assets.
It does not imply that those physical resources have finished or can be reused.

## Frame publication and submission schemas

```typescript
type RenderImageDescription = {
  extent: RenderExtent;
  format: "rgba8_unorm" | "bgra8_unorm" | "rgba16_float";
  transfer: "linear" | "srgb" | "bt709";
  alpha: "straight" | "premultiplied";
};
type RenderFramePublish = {
  runtime_id: RenderId;
  producer_ticket: RenderId;
  frame: RenderFrameKey;
  timeline: RenderTimeline;
  position: RenderPosition;
  image: RenderImageDescription;
};
type RenderPublishedFrame = {
  runtime_id: RenderId;
  frame: RenderFrameKey;
  image_ref: RenderId;
  position: RenderPosition;
};
type RenderImageTarget = { runtime_id: RenderId; image_ref: RenderId };
type RenderFrameGeometry =
  | { kind: "scene_camera" }
  | {
      kind: "ti_c";
      world: RenderVersion;
      context_id: RenderId | null;
      extent: RenderExtent;
      ti: RenderMatrix3;
      camera_center: RenderVec3;
    };
type RenderFrameInput = {
  frame: RenderFrameKey;
  image_ref: RenderId;
  position: RenderPosition;
  geometry: RenderFrameGeometry;
};
type RenderSubmit = RenderTarget & {
  submission_id: RenderId;
  expected_scene_revision: RenderU64 | null;
  frames: RenderFrameInput[];
};
type RenderSubmissionTarget = {
  runtime_id: RenderId;
  submission_id: RenderId;
};
type RenderSubmissionAccepted = RenderSubmissionTarget & {
  scene_id: RenderId;
  scene_revision: RenderU64;
  effect: RenderEffect;
  frames: {
    frame: RenderFrameKey;
    position: RenderPosition;
  }[];
};
type RenderFailureKind =
  | "write_failed"
  | "deadline_exceeded"
  | "cancelled"
  | "render_failed"
  | "runtime_failed";
type RenderFrameResult = {
  frame: RenderFrameKey;
  position: RenderPosition;
} & (
  | { state: "pending" }
  | { state: "failed"; reason: RenderFailureKind }
  | {
      state: "rendered";
      output_frame_id: RenderId;
      delivery:
        | { state: "queued" }
        | {
            state: "transmitted";
            output_stream_id: RenderId;
            first_slot: RenderU64;
          }
        | { state: "dropped"; reason: "queue_full" | "output_failed" };
    }
);
type RenderSubmissionSnapshot = Omit<RenderSubmissionAccepted, "frames"> & {
  frames: RenderFrameResult[];
};
type RenderCancelResult = RenderSubmissionTarget & {
  cancelled: RenderFrameKey[];
  too_late: RenderFrameKey[];
};
type RenderReleaseResult = { released: boolean };
```

### Publication

A producer ticket refers to one registered source frame and its native
lease/readiness dependency. Validate all supplied metadata against that
registration; possession of a string alone is not authorization to another
producer's resource. Publication consumes the write ticket once and creates
one immutable public image reference. Repeated publication of that ticket
rejects; an unsuccessful publication does not consume it.

Publication is allowed before the registered producer's physical write completes.
Readiness is a dependency, not a client-supplied boolean. No consumer reads
before it is satisfied; write failure fails dependent submissions. Publishing
does not mean rendered, submitted or transmitted.

Require matching stream/frame, timeline ID, position and native extent. Float
images use linear transfer; unorm images use the declared transfer. Premultiplied
RGB is multiplied in linear light before encoding the transfer function.
Unsupported image domains/color inputs reject at registration, not by relabeling
an arbitrary captured frame as corrected pinhole data.

`frame.release` revokes future use of that public reference and releases the
publisher's lease. Existing accepted submissions keep their own references.
The underlying slot can be reused only after producer and all consumer/GPU
accesses finish. Slot generation/readiness are owner details, not wire fields.

### Admission, snapshot and failures

Submit a nonempty bounded batch whose frame keys are unique within that batch.
Validate the whole batch, reserve all required admission/result capacity and
bind one current scene snapshot under the mutation serialization boundary.
`expected_scene_revision: null` explicitly selects current; a concrete value
is a compare guard, not a request for arbitrary historical snapshots.

Bad refs, mismatched identities/worlds, missing geometry or capacity rejection
reject the **whole request** with no accepted frames. After admission, per-frame
physical failures are independent and do not roll back frames already rendered.
The accepted receipt lists frame identities, actual scene revision and effect.
It cannot claim output merely because the batch was accepted.

Tracked scenes require `ti_c`; manual scenes require `scene_camera`. Geometry
must match the configured native extent and scene world. A nonnull context ID
must resolve to the immutable camera/world/C binding used by that frame;
incompatible supplied C rejects. Null explicitly means caller-supplied synthetic
Ti/C without a calibration service, not "use the current calibration".

Images and geometry may arrive independently at the runtime's join adapter.
The adapter joins them by FrameKey **before** calling this complete-input
submission contract. Its bounded waiting/late-input policy is a separate #22
adapter decision; the public submit request cannot omit geometry and silently
wait for an uncorrelated later message. No stale Ti or invented camera repairs
a missing input.

Each frame's injected monotonic deadline starts at accepted admission and
includes readiness/queue/render waiting. The immutable configuration provides
the timeout. Media pauses and wall-clock changes do not disable it. This is not
a full capture-to-output latency budget or a production SDI clock-selection rule.

One batch has one scene revision. If a resolved effect boundary requires
different revisions, split the batch at that boundary. Animation is excluded;
media positions still remain associated with their source frames. Replay binds
recorded snapshots/effect boundaries, not whichever state preparation happens
to finish first on another run.

Submission IDs are caller-unique within one runtime. While a submission is
retained, an identical normalized request returns its original accepted receipt;
different content under the same ID rejects. After explicit release there is
no lifetime history/deduplication guarantee; callers must not reuse the ID.
This is bounded retry recovery, not a general exactly-once service.

## Cancellation, output and physical release

Before SDI handoff, cancellation makes a nonterminal frame logically failed and
suppresses future output handoff. A concurrent handoff/cancel is serialized.
Once handed to the port, or already terminal, it is `too_late`; cancellation
cannot retract a possibly transmitted frame. Previously rendered pixels and
failed frames are not rewritten into another successful result.

Logical timeout/cancellation does not complete producer reads, GPU writes or
consumer work. Keep their resources quarantined and count them against capacity
until physical completion. Reject new admissions instead of blocking the
render/control loop, silently overwriting a held slot or growing another pool.

Rendered completion produces a native output resource; the result metadata
exposes only its opaque identity. SDI handoff uses a bounded nonblocking queue.
If full, that rendered frame becomes `delivery: dropped`. The port's actual
transmission records its output stream and first slot. Repeating the same frame
does not append an unbounded list to the receipt; detailed port counters/logs
are separate. The monitor consumes what the port transmitted; monitor skips
never manufacture a render-direct output or change a transmitted result.

The output port retains its current/repeated frame until replacement or
shutdown. GPU completion alone cannot free it. Input and output allocations
remain separate; a transmitted output cannot overwrite an input still used by
another tracker/recorder/renderer consumer.

`render.get` reads the retained snapshot. A rendered frame with queued delivery
is not yet a terminal delivery result. `render.release` accepts only when every
frame is failed or rendered with transmitted/dropped delivery; otherwise reject
as invalid state. Release removes the public receipt, not outstanding physical
leases. Releasing an unknown receipt returns `released: false`; there is no
unbounded tombstone store. Current retained receipts and private draining work
both count towards the declared limits.

No implicit result eviction, restart recovery or disconnect-as-cancel is
promised. A client that abandons retained work can cause bounded admission
rejections until it is released by an authorized controller; this never blocks
the SDI clock. Connection loss does not silently retract program output.

The initial synthetic path preserves the existing port's repeat-on-underrun
behavior. This is not approval of the product's tracking-loss/failover policy;
that remains with #1/#57/#60 and must not be inferred from a simulation.

## Public method table

All params and results below are exact schemas; no method responds with an
unconstrained success/message wrapper.

| Method | Params | Result | Success confirms |
| --- | --- | --- | --- |
| `render.get_configuration` | `{ runtime_id: RenderId }` | `RenderConfiguration` | Installed immutable configuration read. |
| `scene.create` | `RenderSceneCreate` | `RenderSceneApplied` | Initial retained state applied. |
| `scene.get` | `RenderTarget` | `RenderSceneSnapshot` | Current complete snapshot read. |
| `scene.mutate` | `RenderSceneMutate` | `RenderSceneApplied` | One atomic mutation actually applied. |
| `scene.release` | `RenderTarget` | `RenderReleaseResult` | Public scene removed, or already absent. |
| `frame.publish` | `RenderFramePublish` | `RenderPublishedFrame` | Immutable frame reference published. |
| `frame.release` | `RenderImageTarget` | `RenderReleaseResult` | Public image reference revoked, or already absent. |
| `render.submit` | `RenderSubmit` | `RenderSubmissionAccepted` | Full batch admitted with a bound scene. |
| `render.get` | `RenderSubmissionTarget` | `RenderSubmissionSnapshot` | Retained per-frame completion/delivery state read. |
| `render.cancel` | `RenderSubmissionTarget` | `RenderCancelResult` | Per-frame cancellation outcome resolved. |
| `render.release` | `RenderSubmissionTarget` | `RenderReleaseResult` | Terminal public receipt released, or already absent. |

Unknown scene/submission queries fail; release of an already absent handle is
an idempotent no-op. An old runtime ID still rejects even if the numeric/string
handle happens to match a new process's object.

## Rejection schema

Use existing standard JSON-RPC errors for malformed envelopes/method params.
The following positive codes are the approved initial-profile allocation, distinct
from #63's `1000`/recording/calibration/tracking proposal. Existing
`-32000`/`-32001` server errors remain compatible.

```typescript
type RenderRejection = {
  code: 2001 | 2002 | 2003 | 2004 | 2005 | 2006 | 2007;
  message: string;
  data: {
    kind:
      | "runtime_changed"
      | "not_found"
      | "conflict"
      | "incompatible_input"
      | "capacity"
      | "invalid_state"
      | "unsupported_profile";
    frame: RenderFrameKey | null;
  };
};
```

Codes follow `kind` order in the union above: `2001` runtime changed through
`2007` unsupported profile. Messages are English; clients branch on code/kind.
`frame` identifies the first failing frame when rejection is frame-specific,
otherwise null. A single rejection does not claim partial acceptance.
Post-admission failures use `RenderFrameResult`, never a second RPC response.

## Typed payload examples

These are synthetic business payloads. The protocol-conventions document owns
the full JSON-RPC envelope; the same payloads apply to either transport.

```typescript
const renderSceneCreateExample: RenderSceneCreate = {
  runtime_id: "runtime-01",
  scene_id: "scene-demo",
  state: {
    world: { id: "synthetic-world", revision: "1" },
    configuration_id: "configuration-01",
    camera: {
      mode: "manual",
      camera: {
        kind: "intrinsics_pose",
        extent: { width: 1280, height: 720 },
        near_m: 0.1,
        far_m: 1000,
        intrinsics: { fx: 1000, fy: 1000, skew: 0, cx: 640, cy: 360 },
        camera_to_world: [
          [1, 0, 0, 0], [0, 1, 0, 0],
          [0, 0, 1, -10], [0, 0, 0, 1],
        ],
      },
    },
    instances: [],
    lighting: {
      lights: [{
        light_id: "sun",
        kind: "directional",
        direction: [0, 0, 1],
        irradiance: [1, 1, 1],
        angular_radius: 0.00465,
        shadow_mode: null,
      }],
      environment: null,
    },
    shadow_mode: "pcf",
  },
};

const renderMutationExample: RenderSceneMutate = {
  runtime_id: "runtime-01",
  scene_id: "scene-demo",
  expected_revision: "0",
  operations: [{ kind: "shadow_mode_set", shadow_mode: "pcss" }],
};

const renderTrackedSceneExample: RenderSceneCreate = {
  ...renderSceneCreateExample,
  scene_id: "scene-tracked",
  state: {
    ...renderSceneCreateExample.state,
    camera: { mode: "tracked", near_m: 0.1, far_m: 1000 },
  },
};

const renderMutationAppliedExample: RenderSceneApplied = {
  runtime_id: "runtime-01",
  scene_id: "scene-demo",
  scene_revision: "1",
  effect: { runtime_id: "runtime-01", sequence: "7" },
};

const renderSceneSnapshotExample: RenderSceneSnapshot = {
  ...renderMutationAppliedExample,
  state: { ...renderSceneCreateExample.state, shadow_mode: "pcss" },
};

const renderPublishExample: RenderFramePublish = {
  runtime_id: "runtime-01",
  producer_ticket: "write-ticket-17",
  frame: { stream_id: "stream-01", sequence: "17" },
  timeline: {
    timeline_id: "capture-01",
    time_base: { numerator: 1, denominator: 50 },
  },
  position: { timeline_id: "capture-01", ticks: "17" },
  image: {
    extent: { width: 1280, height: 720 },
    format: "rgba8_unorm",
    transfer: "bt709",
    alpha: "straight",
  },
};

const renderSubmitExample: RenderSubmit = {
  runtime_id: "runtime-01",
  scene_id: "scene-demo",
  submission_id: "submission-17",
  expected_scene_revision: "4",
  frames: [{
    frame: { stream_id: "stream-01", sequence: "17" },
    image_ref: "image-17",
    position: { timeline_id: "capture-01", ticks: "17" },
    geometry: { kind: "scene_camera" },
  }],
};

const renderAcceptedExample: RenderSubmissionAccepted = {
  runtime_id: "runtime-01",
  submission_id: "submission-17",
  scene_id: "scene-demo",
  scene_revision: "4",
  effect: { runtime_id: "runtime-01", sequence: "23" },
  frames: [{
    frame: { stream_id: "stream-01", sequence: "17" },
    position: { timeline_id: "capture-01", ticks: "17" },
  }],
};

const renderTransmittedExample: RenderSubmissionSnapshot = {
  ...renderAcceptedExample,
  frames: [{
    frame: { stream_id: "stream-01", sequence: "17" },
    position: { timeline_id: "capture-01", ticks: "17" },
    state: "rendered",
    output_frame_id: "output-17",
    delivery: {
      state: "transmitted",
      output_stream_id: "program-01",
      first_slot: "42",
    },
  }],
};

const renderRejectedExample: RenderRejection = {
  code: 2004,
  message: "Frame identity does not match the published image",
  data: {
    kind: "incompatible_input",
    frame: { stream_id: "stream-01", sequence: "18" },
  },
};

const renderPublishedExample: RenderPublishedFrame = {
  runtime_id: "runtime-01",
  frame: { stream_id: "stream-01", sequence: "17" },
  image_ref: "image-17",
  position: { timeline_id: "capture-01", ticks: "17" },
};

const renderQueryExample: RenderSubmissionTarget = {
  runtime_id: "runtime-01",
  submission_id: "submission-17",
};

const renderCancelledExample: RenderCancelResult = {
  ...renderQueryExample,
  cancelled: [{ stream_id: "stream-01", sequence: "17" }],
  too_late: [],
};

const renderReleasedExample: RenderReleaseResult = { released: true };
```

The cancellation and transmission examples are alternative outcomes, not two
successive states of the same run. Configuration discovery takes only the
observed runtime ID; scene/image/receipt release uses the target schemas in the
method table. These actions do not carry hidden positional arguments.

## Conformance scenarios

| Scenario | Required observable outcome |
| --- | --- |
| Two mutations prepare from revision 4 | Exactly one publishes revision 5; the loser conflicts without partial instance/light changes. |
| Mutate during accepted submission | Receipt and all frames retain their bound old scene/effect and resources. |
| Stale image ref after slot reuse | Reject before admission; never read a newer picture under the old FrameKey. |
| Image published before write readiness | Submit may accept; execution waits for the real dependency or fails its monotonic deadline. |
| Out-of-order image/geometry arrival | Join by FrameKey before submit; no pairing by position in arrays or arrival order. |
| Old calibration result after a switch | Use its compatible old C/world/scene or reject; never substitute the current C. |
| One invalid frame in a batch | No frame admitted; publisher refs remain owned by the caller. |
| Write/render failure after admission | Only affected frames fail; successful/transmitted siblings are not rolled back. |
| Timeout/cancel while GPU work runs | Logical failure can be visible, but held slots remain unavailable until physical completion. |
| SDI handoff races cancellation | Exactly one wins; already handed-off work is too late to retract. |
| A frame is rendered but queue full | Report rendered/dropped, not transmitted; existing output cadence continues. |
| Client releases a transmitted receipt | Output port can still repeat its retained frame; receipt release does not free that resource early. |
| Client disconnect/reconnect | Query current scene/retained submissions; no assumed durable notification replay or automatic repeat mutation. |
| Many completed/released submissions | Bookkeeping stays bounded; no forever-growing opaque-ID history is required. |
| Pause/step/accelerate or change batch size | Recorded source association and resolved scene/effect boundaries remain the same; protection clocks are independently testable. |

## Approval gates and handoff

The owner approved the following for the limited initial synthetic scope:

- `scene_frame_v1`, the method/type/static-capability surface, decimal U64,
  FrameKey/logical time and error codes 2001-2007.
- Scene-revision binding, whole-request admission/rejection, independent
  post-admission results, retained-request retry and explicit terminal release.
- Cancellation/timeout does not release physical leases early; capacity
  exhaustion rejects rather than overwrites or creates unbounded storage.
- #20 shared DTOs/conformance and #21 bounded resources may now be implemented
  with registered synthetic resources, explicit settings and a trusted local controller.

Still unapproved: production producer/asset registration and authorization,
production capacity/latency values, administrative abandoned-result reclamation,
the #22 join/late-input policy, tracking-loss/failover and hot output
reconfiguration. Broader #63 business semantics are not approved by this decision.

#63 is not a build/merge prerequisite for this document. Its shared wire shapes
are compatible inputs; approval of this renderer profile does not approve #63.
Shared identity/time/effect DTOs must have one common definition. The paired
renderer roadmaps are unchanged because this slice does not expand native
renderer features or their Linux delivery scope.

Under the approval above, #20 can add DTOs/conformance fixtures and #21 can
implement the synthetic/native resource owner independently. #12 remains open
for the remaining production decisions. Later #38/#22/#31 integration must join
existing renderer APIs without inventing another compositor or treating
synthetic acceptance as approval of production registration/output policy.

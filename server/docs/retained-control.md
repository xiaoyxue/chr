# Retained scene control adapter

`chr_runtime::retained` implements the runtime-only bridge for the
[approved initial synthetic contract](../../docs/scene-frame-contract.md).
This is an **explicit native embedding opt-in**, not a new daemon default or a
claim of production scene rendering. The shipped binary defaults to the triangle;
[`--synthetic-program`](program-output.md) installs this adapter and connects
synthetic composition to the existing SDI port, whose program tap feeds the
`chr-monitor` encoder/WebRTC chain.

## Installation and ownership

1. Supply every field of `chr_common::scene_frame::Configuration`. There are no
   production defaults. Native quality/range and canvas-budget checks run before
   installation; the admission port must additionally approve device/output
   support (or explicitly declared synthetic capabilities).
2. Populate `Registrations` with immutable world versions, models returned by
   `chr_renderer::glb::import_glb` (or trusted synthetic native -Z-up mesh assets),
   decoded environment maps from
   `chr_renderer::exr::import_exr`, and optional immutable `CameraContext` values.
   These are trusted local native registrations, not a production authorization
   service. No RPC accepts file paths, URLs or asset bytes. Registrations freeze
   when moved into the adapter.
3. Install the resource-backed [synthetic `FrameSubmissions`](frame-submissions.md)
   port (or implement `RenderAdmission`), then construct
   `SceneAdapter::new(configuration, registrations, port)`.
4. Call `SceneControl::spawn(adapter, explicit_queue_capacity)` inside Tokio and
   install the endpoint with `ControlHandler::with_scenes`. Give this **same**
   handler to `chr_common::server::IpcServer` for both listeners.
5. Retain/await the worker handle during shutdown. `SceneControl::shutdown`
   closes native admission and drains accepted CPU commands even while transports
   retain endpoint clones. Dropping all endpoints also drains admitted commands;
   disconnected replies do not undo effects. Neither operation is a GPU fence.

Both transports and `SceneControl::call_typed` use the shared strict parser and
one bounded queue. A full queue rejects immediately with `capacity`; a stopped
owner returns `invalid_state`. The CPU owner never waits for producer readiness,
GPU work, SDI, or encoding. Scene queries and edits remain independent of that
work. The existing triangle queue and signaling/status paths are unchanged.

## Atomic scene mapping

Creation prepares the complete state before publishing revision zero. Mutation
checks the expected revision and applies the bounded operation list in order to
temporary state. **Every intermediate operation is mapped**, even if a later
operation overwrites it. An absent remove, unresolved reference, unsupported
camera, native range failure or exceeded budget publishes nothing. A successful
batch publishes one revision and effect. Counters use checked arithmetic.

Forward row-major model/pose matrices and Ti coefficients are passed to the
existing renderer APIs without transposition, normalization, a second GLB basis
conversion, or half-pixel offsets. Existing `ModelMatrix`, `Camera::from_pose`,
`Camera::from_ti` and `Lighting` math remains authoritative; this adapter does not
port another reconstruction/rendering algorithm from private reference code.
Additional float-range checks prevent invalid GPU casts; all calculations and
fixtures are public/synthetic.

Instance overrides replace factors only; mesh and texture payloads remain shared.
This first implementation **rebuilds a native Scene after each temporary
operation**. Native resource IDs therefore change and a future renderer consumer
may reupload meshes/materials and rebuild shadow caches. This is deliberately a
simple transactional implementation, not an optimal incremental GPU cache.

Manual camera edits replace the complete pinhole. Tracked scenes cannot change
camera/clipping mode. Each frame uses clipping from the bound revision, validates
world and extent, and validates nonnull context IDs against the registered
world/extent/C. Null context explicitly permits caller-supplied synthetic Ti/C;
it never means “current calibration.” No lens/distortion placeholder is exposed.

Directional, point and spot lights map to existing renderer types, including
per-light shadow overrides; scene shadow mode and fixed PCF/PCSS/cascade,
environment, exposure, tone-map, sample and composition settings are retained.
CPU shadow/environment/canvas memory checks reject rather than downgrade.
Device-specific allocations/format support remain the real renderer port's gate.

## Admission seam for #22

`RenderAdmission` is the adapter's explicit boundary. The opt-in
`frame_submissions::FrameSubmissions` implements its synthetic scheduler:

- `configure`: validate fixed configuration against installed port capabilities.
- `existing`: inspect the normalized common `Submit` **before** live-scene lookup.
  Identical retained retries return the original receipt; different content under
  the same ID conflicts. A retry after scene release does not rebind.
- `admit`: receives `BoundSubmission` containing the common request, candidate
  receipt, immutable `Arc<BoundScene>` and one validated native camera per frame.
  Atomically resolve and validate **all** images and metadata, reserve all
  admission/result capacity, and retain the scene and resource leases. `Err`
  must expose no accepted work. Only `Ok` commits the adapter's effect and returns
  the accepted receipt. The port must not perform GPU work or wait here.
- `get`, `cancel`, `release`: the port owns retained results, deadlines,
  cancellation/handoff serialization and logical versus physical completion.
  Cancellation/release are not evidence that a physical lease can be reused.

Do not create a second authoritative scene map or command queue in #22. Route
admission and mutations through this owner. `BoundScene` exposes read-only
snapshot, native view, manual camera and native settings accessors. Retained views
survive scene mutation/release and same-ID recreation. Released scene generations
still retained by admissions count against scene capacity until their last
snapshot is dropped.

The resource owner is `chr_runtime::frame_resources` (#21). The synthetic admission
adapter uses its bounded `acquire_id` lookup, verifies registered
FrameKey/position/extent, retains consumer leases, and checks readiness without
blocking this control owner. It keeps physical work guards until physical completion.
There is no additional public-image registry here. `frame.publish/release` use
the same scene queue and narrow admission-port methods. Their default returns
`unsupported_profile`; `FrameSubmissions` enables them only with an explicitly
installed producer capability. Possession of a ticket string alone does not
authorize publication. The native join, deadlines and submission lifecycle are
documented in [frame submissions](frame-submissions.md); the explicit
[program-output opt-in](program-output.md) supplies the GPU/output driver without
changing the default daemon.

## Acceptance evidence and limits

`cargo test --locked -p chr-runtime -p chr-common` exercises:

- Actual NativeSocket and WebSocket requests through `ControlHandler`,
  `SceneControl` and the same authoritative scene, including competing edits,
  shared reads, render retries and effect boundaries.
- Synthetic GLB basis preservation, shared texture retention, per-instance
  overrides, native cameras/lighting/shadows, rollback and release/recreation.
- Invalid references/context/world/extent/Ti, all-or-none batch rejection,
  draining scene capacity and bounded acquisition queue behavior.

The original test admission port declares two retained frames, validates a fixed synthetic
resource set and exposes pending/cancelled results only. It produces no GPU/SDI
output and cannot establish #21/#22 physical integration or #31 live acceptance.
Additional resource-backed tests exercise the real synthetic port over both
IPC transports, including installed limits, physical draining, producer readiness,
terminal release and synthetic execution/output events.
GPU rendering, SDI hardware, capture, codecs, production authentication/calibration,
tracking-loss policy and Windows/UI acceptance are outside this slice.

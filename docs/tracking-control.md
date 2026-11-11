# Tracking service contract

**Status:** `proposed` - the #60 native-adapter candidate; no tracking algorithm,
production IPC transport or product tracking-loss policy is approved here.

**Scope:** Deliver frame images and immutable reference context to a tracker,
then return frame-associated Ti or an explicit unavailable result.

**Depends on:** [Protocol conventions](protocol-conventions.md),
[Execution model](execution-model.md), [Calibration](calibration-control.md),
[Frame ingestion](render-frame-input.md),
[Business contexts](business-context.md).

## Confirmed boundary

chr organizes video delivery and geometry-result ingestion even when the
tracking algorithm lives elsewhere. Initial geometry is pinhole-only.
The renderer takes a matching image reference and resolved Ti/C; it does not
load the atlas or perform reference-image matching.

## Candidate adapter and deployment

Start with a bounded, asynchronous **native adapter port** that can be exercised
by a synthetic tracker. This is an internal contract, not a set of public
JSON-RPC methods or a claim that production tracking runs in-process.

A producer obtains read access through #21's resource owner. `image_ref` is an
opaque reference in that owner's namespace, never a GPU pointer, slot or
cross-process shared handle. The adapter retains the resource lease while its
CPU/GPU reader works. The metadata schemas below deliberately do not serialize
the actual lease or pixel bytes.

For a future external process, a transport adapter must define image export,
readiness, reference/resource lifetime, process death and bounded backpressure.
It may preserve existing NativeSocket/WebSocket control framing, but cannot
claim another process can dereference a local `image_ref`. Choose the image
transport only after the actual tracker boundary and required formats are known.
Production IPC/GPU interop remains an explicit #60 implementation gate.

## Operations and effects

| Native operation | Input | Result | Successful effect |
| --- | --- | --- | --- |
| Prepare reference | Complete calibration, target camera/stream and source image description. | `TrackingContext`. | Reference resources ready; old active context unchanged. |
| Submit frame | `TrackingFrame` and a resource-owner lease. | `TrackingAccepted` or `TrackingRejection`. | Frame/context association accepted and lease retained, not a geometry result. |
| Complete frame | Adapter-owned asynchronous completion. | `TrackingResult`. | Geometry or explicit unavailability recorded for exactly one accepted request. |
| Retire context | `TrackingRetireParams`. | `TrackingRetired`. | No future admission; report `released` only after all readers/resources finish. |

Calibration load publishes the prepared context atomically with its complete
binding. Preparation is internal and does not add a public activate workflow.
Reference preparation failures leave the previous binding intact.

## Metadata schemas

Shared `Matrix3`, reference, frame and time types come from the owning documents.

```typescript
type TrackingImageDescription = {
  width: number;
  height: number;
  pixel_format: "rgba8_unorm";
  transfer: "srgb";
  image_domain: "pinhole_snc";
};

type TrackingContext = {
  runtime_id: BusinessId;
  context_id: BusinessId;
  calibration_id: BusinessId;
  camera_id: BusinessId;
  stream_id: BusinessId;
  world: VersionRef;
  input: TrackingImageDescription;
};

type TrackingFrame = {
  request_id: BusinessId;
  context_id: BusinessId;
  frame: FrameKey;
  image_ref: BusinessId;
  image: TrackingImageDescription;
  position: MediaPosition;
  timeout_ms: number;
};

type TrackingAccepted = {
  request_id: BusinessId;
  context_id: BusinessId;
  frame: FrameKey;
};

type TrackingRejection = {
  code: 1201 | 1202 | 1203 | 1204;
  message: string;
  data: { request_id: BusinessId; context_id: BusinessId };
};

type TrackingResult = TrackingAccepted & (
  | {
      state: "available";
      ti: Matrix3;
      image: TrackingImageDescription;
    }
  | {
      state: "unavailable";
      reason:
        | "not_tracked"
        | "deadline_exceeded"
        | "cancelled"
        | "worker_failed"
        | "invalid_geometry";
    }
);

type TrackingRetireParams = {
  context_id: BusinessId;
  mode: "drain" | "cancel";
};

type TrackingRetired = {
  context_id: BusinessId;
  state: "draining" | "released";
};
```

The synthetic profile reads already-pinhole RGBA8/sRGB images. It is not a
production SDI pixel/color profile and does not prescribe the future tracker's
algorithm or precision. Any source conversion occurs in the named ingestion
adapter and preserves FrameKey and native geometry extent. If that conversion
is unavailable or would require unapproved distortion correction, reject the
profile rather than relabeling bytes.

Dimensions are positive integers within configured capacity. The image
description equals the prepared context's supported input. Frame stream,
runtime ownership, image resource identity and context all agree. Each position
names a registered timeline; timeout is a positive, supported integer in
monotonic milliseconds, unrelated to how quickly media time advances.

`request_id` is a native work identity scoped to one runtime, **not JSON-RPC id**.
The caller must not reuse it or submit the same `(context_id, FrameKey)` twice.
The adapter rejects duplicates among retained work without taking another
lease. After work is fully retired it keeps no unbounded history: historical
nonreuse is a caller obligation, not a promised lifetime duplicate detector.
This port provides no exactly-once retry/replay cache. Replay uses a fresh
runtime/context and its recorded frame associations.

C is resolved from the immutable calibration bound to `context_id`; it is not
looked up from whichever camera is active when a result arrives. A valid result
must match the accepted request/frame/context and native image description.
Validate finite, nonsingular Ti and the declared pinhole domain before exposing
it to the renderer; failure produces `invalid_geometry`, never a default camera.

## Bounded scheduling and lifetime

Configure explicit limits for contexts, accepted frames and completion storage
before starting the adapter. Reject further admissions when any required
capacity cannot be reserved. A rejected submission does not transfer lease
ownership. An accepted submission reserves room for its terminal result so a
slow result consumer cannot require unbounded storage.

Image and geometry may complete out of order; pair by request/frame/context,
not by arrival, slot or array index. Accepted work produces exactly one terminal
result while the adapter instance remains alive. Unexpected process termination
is reported by the runtime's supervision/recovery boundary, not by promising
notifications from a dead process.

Deadlines start at admission and include readiness waiting. At timeout or
explicit cancellation, emit one `unavailable` result and stop using late
geometry, but **do not release a lease until all physical readers finish**.
A running GPU operation or external reader cannot be made safe by changing a
logical task state. Quarantined leases still consume capacity. If a reader will
not terminate, exhaust/reject within the configured bound and report failure;
do not allocate an unbounded replacement pool or overwrite held frames.

Late or duplicate worker callbacks cannot produce a second terminal result.
Their resource-release signals still need processing. Retain identity and
accepted-work bookkeeping until both the terminal result is consumed and
physical work finishes; only then recycle its bounded slot. This distinguishes
result completion from reader completion without retaining every historical ID.

Retiring a context closes admissions and cancels or drains its accepted work
according to the caller's explicit mode. Existing render submissions retain
their own immutable binding references. Polling retirement can return `draining`;
`released` means no tracker reader/reference resources remain. Context IDs are
never reused, so a late old-context result cannot match a new calibration.

## Calibration switches and output policy

The runtime assigns the active calibration binding when admitting a frame to
tracking. Loading another calibration affects later admissions only. Old-context
results may still complete for their original frames; #22 either renders them
with compatible old C/world/scene bindings or rejects them explicitly.

An `unavailable` result is **not** an instruction to repeat stale geometry,
freeze program output, pass video through, hide graphics or select external
bypass. Those are distinct runtime/product output policies. The existing
simulated SDI repeat-on-underrun behavior is not automatically the approved
live-tracking failure policy.

## Rejections

These are candidate native error values, reserved consistently with the shared
business error registry; a later wire adapter must map them explicitly.

| Code | Meaning |
| --- | --- |
| `1201` | Unknown, retired or incompatible tracking context. |
| `1202` | Stale/not-owned image reference, incompatible frame/image identity or unsupported profile. |
| `1203` | Bounded context/frame/completion capacity unavailable. |
| `1204` | Work identity or frame/context submission duplicates retained work. |

`message` is English, and `data` always echoes the attempted request/context.
Rejected work does not acquire resources or emit an asynchronous result.
Accepted work failures use `TrackingResult`, not a second submit response.

## Payload example

```typescript
const trackingResultExample: TrackingResult = {
  request_id: "track-request-17",
  context_id: "tracking-context-01",
  frame: { stream_id: "stream-01", sequence: "17" },
  state: "unavailable",
  reason: "deadline_exceeded",
};
```

## Simulation acceptance

- Submit frames A/B and complete B/A: the exact request, image and geometry
  association is preserved.
- Reject a reused slot reference or another stream's result without modifying
  accepted work; numeric sequence equality does not establish identity.
- Switch calibration while A is pending: A retains old compatible C/world;
  later admitted B uses the new context.
- Timeout while a simulated GPU reader is active: report unavailability, retain
  the lease, reject capacity overflow and release only at reader completion.
- Deliver late and duplicate callbacks: one terminal result, no duplicate
  consumer release, and no new context confused with the retired one.
- Run more frames than the concurrent capacity with fresh caller IDs: consumed,
  physically finished work releases bookkeeping; memory does not grow with the
  runtime's historical frame count. Duplicates of still-retained work reject.
- Pause media time while advancing protection time: timeout remains exercisable;
  changing wall clock does not advance the recorded content.

## Open decisions

- Approve the native-port/synthetic profile as the first contract-validation
  slice and reconcile its fields with #12/#21/#22/#36.
- Choose the real tracker boundary, production image transport/format,
  cancellation/death proof and reference preparation budgets.
- Approve actual tracking-loss/output behavior and measurable tracking quality
  criteria; do not close #60 solely because a synthetic adapter passes.
- Scope any later multi-camera, distorted-domain, depth/segmentation or streamed
  remote deployment support explicitly. No such placeholder fields are added.

# Calibration records

**Status:** `draft` - the complete-record boundary and load-as-enable direction
are confirmed; the #59 schemas and lifecycle below are `proposed`.

**Scope:** Generate, query and enable camera calibration records without defining
the modelling/tracking algorithm or selecting its technology stack.

**Depends on:** [Protocol conventions](protocol-conventions.md),
[Execution model](execution-model.md), [Recording](recording-control.md),
[Business contexts](business-context.md),
[Coordinate contract](../server/docs/coordinates.md).

## Confirmed decisions

- A complete record contains atlas reference images paired with their registered
  ground-XY-to-image-SNC Ti, plus world camera centre C in the same pitch frame.
- Camera identity, image extent/domain and world context must be resolvable.
- Feature descriptors, indexes and matching caches are algorithm internals,
  not a renderer input or a mandatory public field.
- `calibration.load` reads and enables a complete record. There is no separate
  public activate step or collection of loaded-but-inactive calibrations.
- Initial geometry is pinhole-only; no distortion or ground-only success variant.

## Methods

| Method | Params | Result | Successful effect |
| --- | --- | --- | --- |
| `calibration.create` | `CalibrationCreateParams` | `CalibrationReference` | Generation identity and parameters durably accepted, not a completed model or activation. |
| `calibration.get` | `CalibrationTargetParams` | `CalibrationSnapshot` | Coherent task/record snapshot plus its current active binding, if any. |
| `calibration.load` | `CalibrationLoadParams` | `CalibrationBinding` | Complete record and tracking resources prepared; the new binding has actually taken effect. |

Create uses the shared caller-selected-ID deduplication rule. Identical create
returns the existing reference even if the task failed; retry generation with a
new ID. Create never replaces the active calibration. Get of a failed task is
a successful query, unlike get of an unknown ID.

Load validates the expected binding revision both before preparation and at the
atomic switch. A racing load that wins first makes the other stale. If the same
record is already bound to the same target and the expected revision matches,
return the existing binding without another effect. A stale revision is still
a conflict; do not reinterpret it as a successful historical retry.

## Payload schemas

```typescript
type Vec3 = [number, number, number];
type Matrix3 = [
  number, number, number,
  number, number, number,
  number, number, number,
];

type WorldContext = {
  reference: VersionRef;
  pitch_id: BusinessId;
  length_m: number;
  width_m: number;
  convention: "chr_pitch_negative_z_up_v1";
};

type CalibrationCreateParams = {
  calibration_id: BusinessId;
  session_id: BusinessId;
  camera_id: BusinessId;
  world: WorldContext;
  input: RecordingArtifacts;
};

type CalibrationTargetParams = { calibration_id: BusinessId };
type CalibrationReference = { calibration_id: BusinessId };

type AtlasImage = {
  image_id: BusinessId;
  image: LocalFileRef;
  width: number;
  height: number;
  image_domain: "pinhole_snc";
  ti: Matrix3;
};

type CalibrationRecord = CalibrationReference & {
  schema_version: 1;
  camera_id: BusinessId;
  world: WorldContext;
  input: RecordingArtifacts;
  camera_center: Vec3;
  atlas: AtlasImage[];
};

type CalibrationLoadParams = {
  session_id: BusinessId;
  runtime_id: BusinessId;
  camera_id: BusinessId;
  stream_id: BusinessId;
  calibration_id: BusinessId;
  expected_binding_revision: UInt64Text;
};

type CalibrationBinding = {
  session_id: BusinessId;
  runtime_id: BusinessId;
  camera_id: BusinessId;
  stream_id: BusinessId;
  calibration_id: BusinessId;
  world: VersionRef;
  binding_revision: UInt64Text;
  tracking_context_id: BusinessId;
  effect: AppliedEffect;
};

type CalibrationFailure = {
  kind:
    | "input_unreadable"
    | "input_incompatible"
    | "model_incomplete"
    | "storage_failed"
    | "interrupted"
    | "internal_error";
  message: string;
};

type CalibrationSnapshot = CalibrationReference & {
  active_binding: CalibrationBinding | null;
} & (
  | { state: "queued" }
  | { state: "building"; progress: number | null }
  | {
      state: "completed";
      record: CalibrationRecord;
      record_file: LocalFileRef;
    }
  | { state: "failed"; failure: CalibrationFailure }
);
```

All coordinates and matrix entries are finite. World dimensions are positive
metres; C is above ground (`Cz < 0`) in this candidate camera profile. Image
dimensions are positive integers. Atlas is nonempty with unique `image_id`s,
verified image references and nonsingular, defined Ti mappings. Validation
preserves the imported forward row-major coefficients; it does not transpose,
invert, renormalize or derive frame identity from atlas order.

`world.reference` must resolve to exactly the declared pitch dimensions and
coordinate convention. Camera compatibility is not established merely by
matching the pitch ID. SNC uses the native image extent and `sqrt(width*height)`;
do not silently fit a recorded image and reinterpret Ti in the fitted extent.

Create verifies both recording artifacts, their hashes and metadata association
before acceptance; it retains verified immutable inputs for the task lifetime.
This first candidate requires matching metadata. Importing raw scans without
metadata is a separate approval gate, not a reason to fabricate provenance.
Unknown image encodings or unsupported resource sizes are explicit rejections.

`progress` is either a producer-reported fraction in `[0,1]` or `null` when no
meaningful fraction is available. A value of `1` alone does not mean completion.
Only a full, validated, durably published record is `completed`. Structural
completeness does not certify physical calibration accuracy; rehearsal approval
and quality thresholds are separate product decisions.

`active_binding` reports the current runtime's binding only when this record is
active. It is not persisted as part of the immutable artifact. It is `null` for
noncompleted records, inactive records and after restart until another load.
The initial single-camera profile has at most one binding; multi-camera queries
will require an explicitly extended profile, not an overloaded null value.

## Storage and recovery

The runtime service owns a configured local record store. The public record is
a UTF-8 JSON `CalibrationRecord` with `schema_version: 1`, stored under a
service-managed calibration directory; image files and the original recording
references are explicit `LocalFileRef`s. Get returns the record and its verified
`record_file` reference. Clients cannot choose arbitrary internal task paths.

Persist the create identity/parameters before acceptance. Queue/build failures
are retained under the same ID. Publish the complete record only after images,
hashes and the record file are valid and durable; incomplete output never
replaces an existing record. Completed records are immutable: another modelling
attempt or a coordinate correction receives a new calibration ID.

On restart, queued/building tasks become `failed: interrupted`; terminal records
remain queryable. No task or live calibration automatically resumes. Existing
completed records can be loaded again only with the new runtime ID.

A portable export must copy referenced bytes and rewrite host-local paths while
preserving their digests and identities. A JSON file containing old absolute
paths is not a portable package. An externally produced record may be registered
only through the same validation boundary; discovery/import/export commands and
retention are explicitly outside this first method set.

There is no cancellation RPC in this candidate. Clients cannot assume that
disconnecting cancels generation. A later cancellation method must specify
terminal state and partial-file ownership rather than reusing load or stop.

## Atomic load and in-flight frames

1. Resolve the complete immutable record and target session/camera/stream. Reject
   world or image-domain incompatibility and live-stage changes prohibited by
   the control candidate.
2. Prepare the tracker reference resources without replacing the old binding.
   The adapter must explicitly support the source/reference image domains and
   extents; IDs alone do not prove compatibility.
3. Under the same effect-serialization boundary used by session transitions,
   recheck runtime, source, expected binding revision and the complete target
   compatibility, including the session's current stage and world revision.
   If entry into live won during preparation, reject the load as `invalid_state`
   and release its temporary resources. Otherwise publish the fresh tracking
   context and record/C/world as one binding with a new `AppliedEffect`. Clear
   old rehearsal approval and increment the session-runtime revision together
   with the binding revision.
4. Return the applied binding. Retain the old immutable context/resources until
   its in-flight tracker and render consumers finish.

The initial unbound revision is `"0"`; each actual replacement increments the
revision without wrapping. State is authoritative in the runtime, not in an
atlas directory or the client's selected row.

At frame admission to tracking, the runtime snapshots one complete binding.
That frame retains its original context and compatible C through rendering,
even if another load happens meanwhile. A not-yet-admitted frame uses the new
binding. Calibration switches do not automatically create a new capture stream;
they change context/binding provenance. #22 must reject incompatible
scene/world submissions, not repair them with current C or a stale Ti.

A preparation, validation or compare-and-swap failure leaves the old binding
valid and releases only the new temporary resources. An uncertain reply is
resolved by reading current session state and `calibration.get`; clients do not
blindly repeat load with the latest revision.

## Errors

Use standard errors and the shared `1000` state/revision/runtime rejections, plus:

| Code | Message | Required data | Methods |
| --- | --- | --- | --- |
| `1101` | `Calibration not found` | `{ calibration_id: BusinessId }` | get/load |
| `1102` | `Calibration incomplete` | `{ calibration_id: BusinessId }` | load |
| `1103` | `Calibration context incompatible` | `{ camera_id: BusinessId, kind: "world" \| "camera" \| "image_domain" }` | create/load |
| `1104` | `Calibration resources unavailable` | `{ calibration_id: BusinessId }` | create/load |

Unreadable/hash-invalid create input is `Invalid params` before acceptance;
later producer failures belong to `CalibrationFailure`. Failure to prepare
tracking resources rejects load without publishing a partial binding.

## Payload examples

```typescript
const calibrationLoadExample: CalibrationLoadParams = {
  session_id: "session-01",
  runtime_id: "runtime-01",
  camera_id: "camera-main",
  stream_id: "stream-01",
  calibration_id: "calibration-01",
  expected_binding_revision: "0",
};

const calibrationBoundExample: CalibrationBinding = {
  session_id: "session-01",
  runtime_id: "runtime-01",
  camera_id: "camera-main",
  stream_id: "stream-01",
  calibration_id: "calibration-01",
  world: { id: "pitch-world", revision: "1" },
  binding_revision: "1",
  tracking_context_id: "tracking-context-01",
  effect: { runtime_id: "runtime-01", sequence: "12" },
};
```

## Simulation acceptance

- Synthetic generation returns a stable task ID without replacing the active
  record; duplicates query the original task and conflicts create nothing.
- Reject swapped image/Ti pairs, mismatched declared image extents, modified
  file content and incompatible world/camera references at clear boundaries.
  Structural checks do not claim to detect every physically wrong calibration.
- Fail model generation after acceptance: get returns a failed task; load rejects
  it and preserves the old active record.
- Race two loads from revision `3`: only one becomes revision `4`; the other
  rejects without invalidating either the winner or older in-flight frames.
- Start preparing a load during approved rehearsal, then enter live: the pending
  load must fail its commit-time stage check. In the opposite order, loading
  clears approval and advances session revision, so the stale live-entry request
  cannot succeed.
- Deliver an old-context result after a successful load: resolve C from that
  context, not from the newly active record, or reject incompatible submission.
- Restart with incomplete work: retained failure is queryable and live binding
  is absent. No atlas is required for direct renderer tests with valid Ti/C.

## Open decisions

- Product approval of the local record profile, input-metadata requirement and
  caller-selected IDs; shared schema alignment with #12/#36.
- Concrete modelling adapter, measurable progress/quality criteria, resource
  limits and operator correction workflow, without choosing an algorithm stack.
- Portable package/import/discovery, retention, cancellation and later
  multi-camera behavior.
- Production reference-resource preparation and failure protection policy for
  the tracking adapter. This draft cannot claim full #59 acceptance without it.

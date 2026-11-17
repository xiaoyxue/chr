# Cam1 input recording

**Status:** `draft` - Cam1 and the combined MP4/metadata deliverable are
`confirmed`; the concrete #58 profile below is `proposed`, not implemented.

**Scope:** Record main-camera input for offline modelling/calibration, not the
delayed rendered program output.

**Depends on:** [Protocol conventions](protocol-conventions.md),
[Execution model](execution-model.md), [Business contexts](business-context.md).

## Confirmed decisions

- The logical main-camera SDI source is called `Cam1`.
- Deliver **MP4 plus associated per-frame metadata as one unit**.
- Missing source evidence must not be fabricated. A file's existence alone is
  not evidence of a complete recording.

## Proposed first profile

Use a configured progressive source and an explicitly selected, immutable
recording profile. No implicit overwrite, pause/resume, preroll, interpolation,
frame-rate conversion or deinterlacing is included. A source restart, format
change or timeline reset ends this task as failed; reconnection does not splice
a different stream into it.

Only one nonterminal recording per actual stream is admitted. This is a
candidate resource policy, not a claim that concurrent recording is impossible.
The operator supplies positive input/finalization protection timeouts; no
wall-clock or FPS-derived default is hidden in the protocol.

## Methods

| Method | Params | Result | Successful effect |
| --- | --- | --- | --- |
| `recording.start` | `RecordingStartParams` | `RecordingReference` | Identity, normalized parameters and exclusive output reservation are durably accepted. No first frame is promised. |
| `recording.stop` | `RecordingTargetParams` | `RecordingReference` | A sticky stop intent is accepted for this task, not a claim of finalization. |
| `recording.get` | `RecordingTargetParams` | `RecordingSnapshot` | Read a coherent task snapshot, including a failed task. |

Creation uses a caller-selected `recording_id` and the shared create-deduplication
rule. Repeating the same start returns the existing ID; different parameters
with the same ID are `identity_conflict`. Repeating stop, including after a
terminal state, returns the reference without changing the first stop reason.
Start after failure with the same ID does not retry encoding.

## Effects and state

| Transition | Condition |
| --- | --- |
| accepted -> `preparing` | Parameters, source binding, profile and output reservation validated. |
| `preparing` -> `recording` | Resources ready and first eligible source frame accepted. |
| `preparing` -> `failed` | Preparation fails, input times out or stop is accepted before a frame is admitted. |
| `recording` -> `finalizing` | Manual stop or maximum-duration boundary stops new admissions. Already accepted frames drain. |
| any nonterminal -> `failed` | Source change/loss, encoder/storage/metadata failure, protection timeout or process interruption. |
| `finalizing` -> `completed` | Both artifacts and their consistent association are finished, verified and published. |

Stop and frame admission are serialized by the recording worker. A stop
accepted before first admission yields `stopped_before_first_frame`, not an
empty successful MP4. During finalization, stop is idempotent. Source loss
after the stop boundary does not invalidate already admitted frames; encoder
and storage failures can still fail finalization.

After a service restart, previously committed terminal records remain queryable.
Nonterminal tasks become `failed` with `interrupted`; they are not silently
resumed or upgraded to completed because files happen to exist.

## Payload schemas

Shared scalar/reference/time types are defined by the protocol and execution
documents. All IDs and paths are nonempty.

```typescript
type RecordingStartParams = {
  recording_id: BusinessId;
  session_id: BusinessId;
  runtime_id: BusinessId;
  source: "Cam1";
  stream_id: BusinessId;
  timeline_id: BusinessId;
  output_directory: string;
  profile: VersionRef;
  max_duration: MediaDuration | null;
  input_timeout_ms: number;
  finalize_timeout_ms: number;
};

type RecordingTargetParams = { recording_id: BusinessId };
type RecordingReference = { recording_id: BusinessId };

type RecordingRange = {
  timeline: Timeline;
  first_frame: FrameKey;
  last_frame: FrameKey;
  first_position: MediaPosition;
  end_position_exclusive: MediaPosition;
  frame_count: UInt64Text;
};

type RecordingArtifacts = {
  video: LocalFileRef;
  frame_metadata: LocalFileRef;
};

type RecordingFailure = {
  kind:
    | "stopped_before_first_frame"
    | "source_lost"
    | "source_changed"
    | "encoding_failed"
    | "metadata_failed"
    | "storage_failed"
    | "timeout"
    | "interrupted"
    | "internal_error";
  message: string;
};

type RecordingSnapshot = RecordingReference & {
  session_id: BusinessId;
  runtime_id: BusinessId;
  stream_id: BusinessId;
} & (
  | { state: "preparing" }
  | { state: "recording"; range: RecordingRange }
  | {
      state: "finalizing";
      range: RecordingRange;
      stop_reason: "manual" | "max_duration";
    }
  | {
      state: "completed";
      range: RecordingRange;
      stop_reason: "manual" | "max_duration";
      artifacts: RecordingArtifacts;
    }
  | {
      state: "failed";
      failure: RecordingFailure;
      partial_paths: string[];
    }
);
```

`runtime_id` identifies the process incarnation that accepted the recording,
including when a later process reads its retained terminal record.
`output_directory` is an absolute, previously nonexistent directory on the
runtime host. Atomically reserve it and reject an existing target. The task
owns its temporary files and must not delete or overwrite unrelated files.
The output names are `video.mp4` and `frames.ndjson` within that directory.

Timeouts are positive integer milliseconds within the implementation's declared
supported range; unsupported values are rejected. `max_duration: null` means
no content-duration cap, not unlimited disk or disabled protection deadlines.
An explicit duration has positive ticks and must be exactly representable in
the bound source timeline. Profile and timeline references must resolve before
acceptance; accepted parameters never silently follow a newer profile.

`frame_count` is positive. Range positions and frame keys all belong to the
bound source timeline/stream. `range` while recording is the admitted range,
not a promise those frames have already been written. Failed snapshots do not
carry complete `artifacts`; `partial_paths` may be empty and is never a list of
guaranteed usable deliverables.

## Recording time and metadata

Begin at the next eligible frame after preparation. The limit is measured from
that frame's logical presentation start: admit frames whose start is before
`first_position + max_duration`. Keep whole frames; the final frame's duration
can extend beyond that threshold. `end_position_exclusive` reports the end of
the last admitted source frame, not the request arrival time. Missing source
frames create gaps rather than invented pictures or clock evidence.

Metadata is UTF-8 NDJSON: one `RecordingMetadataHeader`, followed by one
`RecordedFrame` per **presentation** frame, in presentation order. Container
packet/decode order cannot stand in for that order.

```typescript
type ClockSample = {
  clock_id: BusinessId;
  ticks: string;
  time_base: TimeBase;
};

type RecordingMetadataHeader = {
  schema_version: 1;
  recording_id: BusinessId;
  session_id: BusinessId;
  stream_id: BusinessId;
  source_timeline: Timeline;
  file_timeline: Timeline;
  profile: VersionRef;
  video_sha256: string;
  width: number;
  height: number;
  frame_count: UInt64Text;
};

type RecordedFrame = {
  presentation_index: UInt64Text;
  source: FrameKey;
  source_position: MediaPosition;
  source_duration: MediaDuration;
  file_position: MediaPosition;
  file_duration: MediaDuration;
  capture_time: ClockSample | null;
  hardware_time: ClockSample | null;
  timecode: {
    text: string;
    nominal_fps: number;
    drop_frame: boolean;
  } | null;
  continuity: "continuous" | "discontinuity" | "unknown";
};
```

`ClockSample.ticks` is canonical signed decimal within signed 64-bit bounds;
it records an external clock sample, not an unsigned content position. Keep
the clock identity/time base with it. `null` means unavailable, not zero.
Timecode text uses `HH:MM:SS:FF`; nominal rate and drop-frame flag determine its
interpretation. Preserve wraps, repeats and jumps; do not turn timecode into an
identity or UTC timestamp. The first row's continuity is `unknown`.

The first candidate has exactly one source frame per output presentation frame,
with no synthetic repeats or transforms. `presentation_index` starts at zero
and is contiguous; source sequence may have gaps. Source positions increase
strictly within the bound timeline; an unrepresentable discontinuity fails the
task rather than being silently repaired. File PTS has its own origin and time
base, associated explicitly through each row. B-frame packet reordering is
allowed because it does not change presentation association.

Finish video and metadata writes, verify counts, hashes and associations, then
durably publish the completed task record. Temporary/incomplete artifacts are
not discoverable as completed. A failure between artifact publication and the
terminal record commit remains an interrupted task with known residual paths.
Only a committed successful record establishes normal deliverability.

The resolved profile must state codec/parameters, dimensions, pixel format,
bit depth, chroma sampling, rate and color interpretation. Recording quality
and missing-timecode acceptability still require product approval. This
candidate permits missing clock/timecode evidence but never missing source
identity or presentation association. It records video only; audio and raw ANC
are explicitly excluded pending a separate approved profile.

## Errors

In addition to standard errors and the shared `1000` rejections:

| Code | Message | Required data | Methods |
| --- | --- | --- | --- |
| `1001` | `Recording not found` | `{ recording_id: BusinessId }` | stop/get |
| `1002` | `Source unavailable` | `{ source: string, stream_id: BusinessId }` | start |
| `1003` | `Output unavailable` | `{ path: string }` | start |
| `1004` | `Recording resources unavailable` | `{ stream_id: BusinessId }` | start |

Errors before acceptance reject the RPC; later failures appear in the task.
An input/profile mismatch cannot partially reserve output and leave a hidden
running task. Every failure must release unused reservations and frame leases.
The recorder may retain a bounded number of #21 input references; encoder lag
or capacity exhaustion fails this recording instead of blocking live rendering
or releasing a frame while its encoder still reads it.

## Payload examples

```typescript
const recordingStartExample: RecordingStartParams = {
  recording_id: "rec-scan-01",
  session_id: "session-01",
  runtime_id: "runtime-01",
  source: "Cam1",
  stream_id: "stream-01",
  timeline_id: "capture-01",
  output_directory: "/recordings/scan-01",
  profile: { id: "scan-profile", revision: "1" },
  max_duration: {
    ticks: "300",
    time_base: { numerator: 1, denominator: 1 },
  },
  input_timeout_ms: 5000,
  finalize_timeout_ms: 30000,
};

const recordingFailureExample: RecordingSnapshot = {
  recording_id: "rec-scan-01",
  session_id: "session-01",
  runtime_id: "runtime-01",
  stream_id: "stream-01",
  state: "failed",
  failure: {
    kind: "metadata_failed",
    message: "Frame metadata could not be finalized",
  },
  partial_paths: ["/recordings/scan-01/video.mp4"],
};
```

Timeouts and paths above are synthetic examples, not approved production defaults.

## Simulation acceptance

- Repeat start with reordered JSON fields: one task; change the stream/profile
  under the same ID: reject without creating another task.
- Stop before first admission: failed empty task; stop after admission:
  finalizing then completed only after both artifacts finish.
- Change wall clock or replay speed: the resolved input range and source/PTS
  association are unchanged. Advance protection time independently to fail I/O.
- Introduce source gaps and reordered encoder packets: retain true source
  identities and presentation order without inventing timecode.
- Fail either artifact write or fill the bounded encoder queue: no completed
  artifact pair, no early frame reuse and no blocked live render loop.
- Restart during preparation/finalization: query the same retained task as
  interrupted; an existing MP4 alone never turns it into success.

## Open decisions

- Approve caller-selected IDs, directory reservation and the first loss/retry
  policy; align shared time/identity schemas with #12.
- Select the recording quality profile, resource budgets and protection limits.
- Decide whether missing optional timing evidence is acceptable for modelling.
- Approve retention/deletion, audio/ANC, interlaced sources, concurrent recording
  and any later program-output recording as separately scoped extensions.

## References

- [Capture stream and hardware time](https://sdk-doc.blackmagicdesign.com/decklink-sdk/DeckLinkAPI/interfaces/idecklinkvideoinputframe.html)
- [Timecode and flags](https://sdk-doc.blackmagicdesign.com/decklink-sdk/DeckLinkAPI/interfaces/idecklinktimecode.html)
- [Frame ANC packets](https://sdk-doc.blackmagicdesign.com/decklink-sdk/DeckLinkAPI/interfaces/idecklinkvideoframeancillarypackets.html)
- [FFmpeg MOV/MP4 muxer](https://ffmpeg.org/ffmpeg-formats.html#MOV_002fMPEG_002d4_002fISOMBFF-muxers)

These are interface references, not a hardware/SDK selection.

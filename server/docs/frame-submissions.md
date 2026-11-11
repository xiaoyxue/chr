# Synthetic frame admission and scheduling

`chr_server::frame_submissions` implements the approved native mechanics of #22.
It is an explicit embedding opt-in, **not** the shipped daemon's renderer, GPU
integration or hardware SDI acceptance. The triangle/signaling/status path and
render -> SDI -> monitor topology are unchanged. The authoritative wire types
remain `chr_common::scene_frame`; no registration RPC or second schema is added.

## Installation and progress

1. Create `FrameResources` with explicit `ResourceConfig` and producer allowlist.
   Obtain the trusted local producer capability before moving this owner.
2. Construct `FrameSubmissions::new(configuration, resources, producer, clock)`.
   The supplied configuration declares exactly the supported synthetic output/
   quality/limits; `configure` rejects any different configuration. Resource
   runtime ID, native extent and frame capacity must agree. Native quality and
   memory validation still runs in `SceneAdapter::new`.
3. Pass a clone as the `RenderAdmission` port to the existing `SceneAdapter`.
   Install its single `SceneControl` in the same `ControlHandler` for both IPCs.
   Keep the other clone as the native injector/driver. It references the **same**
   bounded owner, not a second scene registry or mutation queue. Installing that
   owner into another adapter is rejected.
4. The trusted injector uses `register` to obtain the existing write ticket and
   producer guard. `frame.publish` validates runtime before lookup and all
   metadata against registration, using the installed producer capability, not
   a producer selected from a caller's stream/ticket string. Publication may
   precede readiness. Producer readiness and physical completion stay separate.
   `cancel_write` revokes an abandoned unpublished registration, but does not
   complete its producer guard.
5. Drive `tick`, `next_ready`, and `handoff` regularly. `get`, cancellation and
   release also poll. `MonotonicClock::now_ms` supplies independent monotonic
   milliseconds; the driver must continue ticking while media is paused.
   No hidden worker waits for a GPU, and no wall clock is read. Backward clock
   movement rejects; deadline arithmetic is checked. Polling delay can delay
   observation, so embedders own progress frequency.

The shared mutex serializes short bounded CPU state transitions, including
cancel versus output handoff. External output ports must be nonblocking and
must not reenter the owner. No physical completion is awaited while holding it.
Registration copies synthetic pixels through #21; it is not a capture adapter.

## Admission and identity

The retained adapter validates complete geometry/cameras and binds its immutable
scene before `admit`. Admission resolves every image through #21, verifies
FrameKey/position/native extent, retains every input lease and reserves the whole
batch before returning success. Invalid last input or capacity exhaustion leaves
no partial submission and does not commit the candidate effect.

`FrameJoin` is bounded native staging for independently arriving published images
and keyed geometry. `finish(runtime, explicit_frame_order)` produces a complete
batch or rejects missing/duplicate/incompatible input. It does not wait, select a
late-input timeout, synthesize a camera, or reuse old Ti. The caller explicitly
chooses order and divides batches at resolved scene/effect boundaries.

The request, ordered logical positions, bound scene and cameras stay retained.
Identical normalized retries return the original receipt before live scene
lookup, even after mutation/release. Different retained same-ID content conflicts.
No receipt is implicitly evicted. Release requires terminal results; unknown
release returns false, while unknown get/cancel returns NotFound. Caller ID reuse
after release is forbidden by contract. Private per-execution/per-delivery cells
still prevent stale native events from updating any subsequently reused slot/ID.
Output IDs have their own checked owner/issuance namespace, unrelated to input
refs, FrameKeys, RPC IDs or SDI slots.

## Execution and output seams for #31

| Native API | Obligation |
| --- | --- |
| `next_ready() -> Option<Execution>` | Returns one ready job with provenance, bound scene, native camera and input lease. Establishes #21 `WorkGuard` **before** external asynchronous submission. Pending input is never read. |
| `Execution::rendered(payload)` / `failed()` | Physical completion observer calls only after all associated input accesses stop. Payload owns a separate output allocation, not overwritten input storage. |
| `handoff(&mut impl OutputPort)` | Polls completion and nonblockingly transfers rendered output. Full returns the frame and records `dropped/queue_full`. Before this boundary cancel suppresses output; after it cancel is too late. |
| `OutputFrame` | Carries original provenance, opaque output resource and delivery observer. Port retains it for queued/current/repeated output until replacement/shutdown, independently of public result release. |
| `DeliveryObserver::transmitted(stream, slot)` | Only an actual port transmission event records stream and first slot. Repeats do not append history or replace that first association. |
| `DeliveryObserver::failed()` | Terminal output failure if not transmitted. Dropping an untransmitted `OutputFrame` also reports output failure. |

`rendered/queued` is observable but not terminal delivery. No rendering or
handoff operation fabricates transmission. The payload is an opaque native owner
(`Any + Send + Sync`); #31 must supply correctly sized/synchronized output storage,
a genuinely bounded port, actual output events, and retain current output
resources through replacement/shutdown. It must connect monitoring to actual
port output only. Synthetic tests use tiny separately allocated values, not
daemon pixels or claims of hardware transmission.

Each frame's deadline begins at admission and includes readiness, queue and
render waiting. Completion is observed on polling: an unobserved pending frame
at its deadline fails, even if a late physical result is then available.
Rendered frames are not timed out while waiting for actual delivery. After
admission, write/render/deadline failures are independent; rendered siblings
are not rolled back. Cancellation and timeout suppress late completed output,
but do not complete physical work.

## Separate bounded lifetimes

- `retained_submissions` bounds the fixed record array: live receipts **and**
  released-but-physically-draining records occupy those same cells.
- `inflight_frames` includes pending, executing, awaiting-delivery and logically
  failed but physically running frames. No secondary draining registry exists.
- `frames_per_submission` bounds per-record requests, cameras and results.
  Terminal retained results remain bounded by the record and batch limits.
- Cancelling an undispatched frame can drop its consumer lease immediately.
  Pending/abandoned producer storage is **still** charged independently by #21's
  frame-cell/byte budgets. Scheduler capacity is not a promise of input capacity.
- Running work keeps its private completion cell and immutable bound scene even
  after public release. Only actual completion permits reclaiming the record.
  Dropped execution observers produce `runtime_failed` (unless already failed)
  and quarantine that bounded capacity; dropped #21 work guards do not certify
  physical completion. There is no unsafe force-reclaim or overflow pool.
- Transmitted/current output belongs to the port, not the input pool or public
  receipt. The installed #31 output owner must bound its queue/current resources.

Shutdown must stop admission and synchronize actual physical work before
destroying resource owners; logical release or dropping futures is not a fence.

## Evidence and policy boundary

`cargo test --locked -p chr-server -p chr-common` includes real resource-backed
publication/admission over NativeSocket and WebSocket through one handler/scene
queue, strict metadata errors, retry/query/cancel/release, reordered keyed joins,
whole-batch rollback, frozen snapshots, independent deadlines/readiness failures,
draining capacity, stale completions and first-slot delivery.

CPU-only synthetic execution/output tests establish lifecycle mechanics, not
GPU pixels, capture, SDI hardware, Windows/UI, or codec interoperability. No
atlas/calibration service, animation, atmosphere, public distortion, production
authorization, waiting/late-input policy, tracking-loss fallback, or hot
reconfiguration policy is implemented or implied. Those remain separate gates.

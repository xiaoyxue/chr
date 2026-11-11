# Business contexts and control ownership

**Status:** `proposed` - a reviewable candidate for #57, not approved product
policy or implemented RPC methods.

**Scope:** Identity, persistence, state authority and the four-stage workflow.

**Depends on:** [System architecture](system-architecture.md),
[Business workflow](business-workflow.md),
[Protocol conventions](protocol-conventions.md),
[Execution model](execution-model.md).

## Candidate release boundary

Start with one active session and one configured main camera, `Cam1`, per runtime.
Keep camera identity separate from the replaceable capture-stream instance.
The initial layout uses static images and static GLB instances with manual
visibility control. Multi-camera cuts, video/animation playback, automatic
scheduling and unattended failover are excluded from this candidate, not removed
from the product roadmap.

These are explicit review choices for B3-B5, not conclusions inferred from the
current renderer. Renderer scene serialization remains owned by #12.

## Identity and ownership

| Object | Lifetime and owner |
| --- | --- |
| Pitch/world revision | Immutable dimensions and coordinate definition in the Local Server's catalog. Calibration and layouts bind an exact revision. |
| Camera | Persistent physical/rig identity, not a stream or a calibrated pose. Repositioning requires another applicable calibration. |
| Layout revision | Immutable asset references and placement document in the catalog. Same asset may have several independent scene instances. |
| Session | A retained preparation/rehearsal/live run for one pitch revision and selected camera. It is not the match, pitch, recording or process identity. |
| Recording/calibration | Retained task and artifact records owned by the runtime service's data store; the catalog may index them but does not invent task status. |
| Runtime incarnation | New `runtime_id` after every process restart. Applied state and in-flight resource ownership are authoritative only within it. |
| Capture stream | New `stream_id` after each capture/decode restart; may be replaced during a retained session only through explicit rebinding. |

Use the shared `VersionRef`, `FrameKey` and `LocalFileRef` definitions. An ID
cannot silently select a newer version. A calibration may be reused in another
session only after validating camera, world revision and image domain.

## Configuration, runtime and observation

The Local Server owns durable editable configuration and immutable published
layout/world versions. The runtime owns currently bound configuration, applied
scene/calibration revisions, task state and observed input/output health.
WebUI and CLI cache these views; neither is a second state authority.

For this candidate, business editing goes through Local Server. Authorized
clients may use the existing NativeSocket or WebSocket directly for runtime
queries and approved runtime controls; both still enter the same handler.
There is no generic Local Server proxy required for every frame or media packet.
Credentials, remote deployment and additional role models are not chosen here.

An edit, a published layout and a runtime application are separate effects.
Saving a layout never changes the active scene. Publishing it validates asset
references and creates an immutable revision. Applying it must validate and
prepare the complete snapshot before atomically replacing the active binding.
The renderer's actual SDI output, observed through its WebRTC monitor, is distinct
from both configuration acceptance and frame-render completion.

```typescript
type SessionStage =
  | "preparation"
  | "scanning"
  | "rehearsal"
  | "live"
  | "closed";

type SessionRecord = {
  schema_version: 1;
  session_id: BusinessId;
  revision: UInt64Text;
  world: VersionRef;
  camera_id: BusinessId;
  desired_layout: VersionRef | null;
};

type LiveSelection = {
  layout: VersionRef;
  calibration_id: BusinessId;
  calibration_binding_revision: UInt64Text;
  render_configuration: VersionRef;
  stream_id: BusinessId;
};

type SessionRuntimeState = {
  session_id: BusinessId;
  runtime_id: BusinessId;
  revision: UInt64Text;
  stage: SessionStage;
  applied: LiveSelection | null;
  approval: {
    selection: LiveSelection;
    effect: AppliedEffect;
  } | null;
};
```

`SessionRecord.revision` and `SessionRuntimeState.revision` are independent:
one protects saved configuration, the other runtime transitions. `null` means
there is no selected/applied/approved value, not "use the latest".

All mutations supply the relevant expected revision; runtime mutations also
name the observed `runtime_id`. Reject stale versions without partial changes.
The runtime serializes accepted effects as defined by the execution model.
No last-writer-wins overwrite or notification-only state recovery is assumed.
Reconnect uses complete configuration/runtime snapshots; an uncertain mutation
reply requires reconciliation, not automatic resubmission with a fresh revision.

## Proposed workflow transitions

These operation names describe domain actions, not new frozen RPC names.

| Action | Preconditions | Successful effect |
| --- | --- | --- |
| Create/select session | Known world revision and camera; no conflicting active session. | Retained identity and a `preparation` runtime state, no live approval. |
| Enter scanning | `preparation` or `rehearsal`; approved source binding; not live. | Stage becomes `scanning`; recording is still a separate explicitly started task. |
| Finish scanning | No active scan recording; any claimed usable recording is complete. | Stage returns to `preparation`; no implicit calibration generation/load. |
| Enter rehearsal | Compatible layout, calibration, input stream and render configuration are prepared. | Their exact selection is applied, stage becomes `rehearsal`; approval is cleared. |
| Approve rehearsal | Operator inspected the actual monitor for the currently applied selection. | Record that exact selection and effect; do not approve a newer unobserved revision. |
| Enter live | Current selection still equals the approval; required input/tracking health and an explicitly configured failure policy are present. | Stage becomes `live`; no new media output or implicit scene replacement. |
| Leave live | Explicit operator action. | Return to `rehearsal`, clear approval; this alone does not choose a video fallback. |
| Close session | Not live; no active recording/generation work or in-flight context replacement. | Retained session is closed, bindings are released after consumers finish; artifacts remain. |

Rehearsal and live share the **same** SDI program/monitor chain. A software stage
does not isolate an SDI cable from broadcast. External routing or operator
procedures must establish a rehearsal-only destination before on-site use.
Separate rehearsal hardware and the product failure policy remain approval gates.

While live, this candidate permits only manual visibility operations within the
approved layout. Asset, placement, calibration, world correction, render-format
or stream changes require leaving live and repeating rehearsal. Saving a new
desired layout is allowed because it does not apply it. Preview-camera edits
never mutate the tracked program camera or approve a live configuration.

A runtime restart clears approval and live bindings and returns the retained
session to preparation; it does not silently resume live. Source/context loss
invalidates approval and requires explicit recovery. The actual program-frame
response to loss is a separately selected policy, not an invented stale camera,
automatic blank frame or claim that the existing simulated repeat policy meets
broadcast requirements.

World-coordinate corrections create a new world/calibration-compatible revision;
instance placement corrections create a new layout revision. Neither rewrites
the original Ti coefficients or disguises operator changes as tracking output.
The correction operator and recalculation workflow remain a separate design gate.

## Review scenarios

- Two clients save against revision `7`: exactly one publishes revision `8`;
  the other receives a conflict and reloads rather than silently overwriting.
- A runtime reply is lost: the client queries the current runtime revision and
  selection. Matching current state is not proof of an unobserved historical
  output; no automatic second effect is triggered.
- A calibration from another camera or world revision is rejected before
  changing the active selection.
- A new layout saved during live leaves the applied layout unchanged. Applying
  it requires leaving live, preparing it, rehearsing and approving again.
- Input restarts with a new stream identity: an old frame/result/approval cannot
  be reused merely because the logical source is still named `Cam1`.

## Approval and implementation gates

#57 remains open until the release boundary, ownership/topology, transition
policy and external rehearsal isolation are approved. This document does not
define a complete Local Server CRUD API or select its storage technology.
Exact layout/scene wire schema belongs to #12/#36; runtime adaptation to #38.

Authorization roles, deletion/retention, coordinate-correction operators and
production tracking-loss/output policies require follow-up decisions. The
recording, calibration and tracking candidates can be reviewed independently
using the shared identities; they do not require a finished WebUI.

# Native synthetic frame resources

`chr_server::frame_resources` implements the bounded native ownership slice of
[#21](https://github.com/xiaoyxue/chr/issues/21), under the
[limited synthetic contract](../../docs/scene-frame-contract.md).
It is not connected to JSON-RPC, scene binding, the render scheduler, SDI, or
the monitor. Existing runtime behavior is unchanged.

## Registration and identity

Install one `FrameResources` with explicit `ResourceConfig` and an immutable
allowlist of synthetic producer stream IDs. There are no implicit capacity or
extent defaults. This resource-specific configuration does not install or
validate the full render output/quality configuration.

`producer` returns an owner-scoped capability for a registered stream.
`register` checks stream, native extent, timeline/position agreement, image
description, exact tightly packed byte length, and finite binary16 values.
It copies caller-owned synthetic pixels and returns a `WriteTicket` and a
`ProducerGuard`. RGBA8/BGRA8 use byte components; RGBA16 uses little-endian IEEE
binary16 components. Float images must declare linear transfer. Color tags
describe caller-supplied content; they do not perform color conversion or
certify real captured/calibrated image domains. Premultiplied RGB must already
have been multiplied in linear light before transfer encoding.

`publish` validates all metadata against registration and consumes the ticket
once. Invalid metadata does not consume it. A successful call returns an opaque
`ImageRef` even while readiness is pending. `FrameMetadata` uses
`chr_common::scene_frame` identities, timeline, position and image descriptors;
there is no second wire scalar or `FrameKey` definition. Frame sequence is never
derived from arrival, allocation, RPC, submission, or output order.

Tickets, image refs and synthetic `AssetRef`s are typed native capabilities;
their `id()` accessors return opaque `chr_common::scene_frame::Id` values for
control DTOs. Internal slot/generation fields are private, with no address,
slot, or layer exposed to callers. Owner identity rejects foreign capabilities,
including those from an owner recreated with the same configuration. Reuse
increments a nonwrapping generation; an exhausted cell cannot be reused.

The owner allocates separate ticket/image IDs from its runtime ID, a process-local
owner namespace, and a nonwrapping issuance counter. These are not slot numbers,
slot generations, FrameKeys, effect sequences or output slots. Their encoding
is private: callers only store and compare IDs. Process restart must install a
new runtime ID, as required by the common contract. Namespace/counter exhaustion
fails rather than reusing an ID. IDs are not cryptographic authorization tokens;
publication by ID also requires the configured producer capability.

ID lookup scans the same fixed-capacity cells that own pixels/readiness, in
bounded O(slot count) time. Revocation immediately removes an ID from lookup,
while its physically occupied cell remains charged until safe reclamation.
There is no second map, ownership registry, tombstone list or draining overflow.
There is no registration RPC, arbitrary-path loader or production auth scheme.

`register_asset` separately copies immutable, immediately ready synthetic image
assets. Asset slot/byte budgets and handle types are separate from source frames.
This is not model loading or a #38 scene/asset resolver.

## Native API and control adapter seam

| API | Responsibility |
| --- | --- |
| `FrameResources::new(ResourceConfig, Vec<Id>)` | Install explicit limits, native extent, runtime ID and producer allowlist. |
| `producer(&Id)` | Trusted native controller obtains a configured producer capability. |
| `register(&Producer, FrameMetadata, &[u8])` | Copy validated synthetic pixels; return `(WriteTicket, ProducerGuard<FrameMetadata>)`. |
| `publish(&WriteTicket, &FrameMetadata)` | Consume a native ticket and return `ImageRef`. |
| `publish_id(&Producer, &Id, &FrameMetadata)` | Resolve the common ticket ID and validate its registered producer and full metadata, using the same ownership. |
| `acquire(&ImageRef)` / `acquire_id(&Id)` | Retain one `Lease<FrameMetadata>`, including pending readiness. |
| `release(&ImageRef)` / `release_id(&Id)` | Revoke future lookup/acquisition; retain physical work. |
| `cancel_write(&WriteTicket)` | Revoke an unpublished registration without completing its producer. |
| `register_asset(ImageDescription, &[u8])` | Return an immutable ready synthetic `AssetRef`, charged to the separate asset budget. |
| `acquire_asset(&AssetRef)` / `acquire_asset_id(&Id)` | Retain the same asset ownership through native or common IDs. |
| `release_asset(&AssetRef)` / `release_asset_id(&Id)` | Revoke the asset without invalidating retained leases. |
| `reclaim`, `occupied_frames`, `occupied_assets` | Reclaim safe cells or inspect physical occupancy, including draining work. |

An RPC adapter validates the common request and runtime ID before calling this
owner. For `frame.publish`, it supplies its authorized `Producer`, passes the
request's `producer_ticket` to `publish_id`, and builds `PublishedFrame` using
`ImageRef::id()` and the registered frame/position. The publisher hold remains
in the owner even if the adapter discards the returned native handle.
`frame.release` calls `release_id`; absent/revoked IDs return false. The adapter
must reject a mismatched request runtime ID before lookup, rather than treating
it as an absent resource.

#22 obtains input through `acquire_id`, checks the retained immutable metadata
against submission frame/position, and keeps that lease through its own bounded
admission/scheduling lifetime. #38 can resolve synthetic image assets through
`acquire_asset_id`. These callers do not need a string-to-resource registry:
this module owns that lookup and all resource lifetimes. Transport handlers,
scene binding, batch admission/result retention and error-to-wire mapping
remain adapter work; adding them must not duplicate the resource owner.

## Caller lifetime and synchronization duties

- A single trusted controller serializes mutation via `&mut FrameResources`.
  Lease acquisition can retain pending input; `pixels` and `begin_work` reject
  pending/failed writes. Independent leases can be moved to concurrent workers.
- Synthetic bytes are already immutable at registration. `ProducerGuard::ready`
  models satisfaction of a native read dependency; it is not a client boolean.
  `fail` resolves readiness failure independently of physical completion.
  Only one readiness resolution succeeds.
- Move the producer guard to the actual completion observer. Call `complete`
  only when all producer access has stopped. Completion with unresolved readiness
  fails that write. Dropping the guard does **not** certify completion: it fails
  pending readiness and leaves the cell charged.
- Hold a `Lease` for every synchronous consumer. For any use outliving the
  caller (including future GPU submission), call `begin_work` before submitting
  it and move the returned `WorkGuard` to its physical completion observer.
  Call `WorkGuard::complete` only after those accesses finish. Dropping an
  unfinished work guard leaves the cell charged, rather than guessing that
  cancellation completed the work.
- Native fence observation, device visibility and submission synchronization
  are the adapter's responsibility; these synthetic guards do not poll a GPU,
  cancel hardware, or establish a zero-copy path. Keep the owner alive during
  operation. Before destroying/replacing it, the controller must stop admission
  and synchronize external work; shutdown is not proof of device completion.

`release` revokes future acquisition and the publisher's hold immediately.
Previously acquired leases remain usable. `cancel_write` similarly revokes an
unpublished ticket without claiming producer completion. Neither operation
completes physical work. Submission/cancellation/result handling is not part of
this module; a future scheduler must retain its own leases and completion guards
even after a logical timeout, cancellation or public result release.

## Capacity and separation

Fixed cell arrays and explicit total pixel-byte budgets bound frames and assets.
Admission sweeps releasable cells, then rejects with `Capacity` rather than
waiting for workers or growing a private draining pool. Pixel copies occur only
after admission checks. Reclamation requires public revocation, producer
physical completion, zero unfinished work guards, and no consumer/producer
Arc references. A controller may also call `reclaim` explicitly.

Abandoned completion observers quarantine capacity until synchronized shutdown;
there is deliberately no unsafe force-reclaim method. `occupied_frames` and
`occupied_assets` include draining/quarantined cells. Revoked refs need no
tombstones, and repeated release returns false. Metadata retained by the owner
is bounded by configured producer/slot counts, not lifetime history.

This owner has no output-writing API: its published pixels are immutable.
Future output allocations/identities and the SDI repeated-current-frame hold
must remain separate; an output cannot be allocated by overwriting an input.
No #22 join timeout/tracking-loss policy, #31 output scheduling, production
policy, hardware capture, physical SDI acceptance, or real assets are supplied.

## Tests

From the repository root:

```sh
nix develop --command cargo test --locked -p chr-server --lib frame_resources
```

Deterministic synthetic tests cover one-shot publication and metadata rollback,
out-of-order identity, stale/foreign/unknown refs, generation exhaustion,
pending/failing writes, concurrent consumers, public release/cancel during
physical work, fail-closed abandoned guards, separate asset budgets, malformed
pixels, byte/slot exhaustion, and repeated bounded reclamation. Thread tests use
channels, not sleeps. Common-ID cases cover producer authorization, lookup before
readiness, release during work, stale IDs after slot reuse, owner/asset namespace
separation, and nonwrapping issuance exhaustion without another registry.
No reference algorithms or private assets were copied;
the implementation uses ordinary ownership and atomic dependency state.

# Render description

**Status:** `implemented` opt-in profile `chr-render-description-v1`, used with the
[tracked camera simulation](tracking-simulation.md). It describes what to draw and
with which lens, independent of any client's own scene model. The schema lives in
`chr_common::description`; the football client produces it from a Fusion match.

## Shape

```json
{
  "profile": "chr-render-description-v1",
  "camera": {
    "width": 1920, "height": 1080, "fps": 30,
    "radial": {"centre_px": [960, 540], "scale_px": 960, "coefficients": [-0.054, 0, 0],
               "support": [[-160, 680], [2080, 1090]]},
    "reference": {"ti": [[...], [...], [...]], "center": [x, y, z], "frame_warps": [...]}
  },
  "scene": {
    "ground": {"pitch_m": [105, 68]},
    "lighting": null,
    "shadow_mode": null,
    "assets": [
      {"id": "ASSET-car", "kind": "glb", "sha256": "…", "blend_as": "mask",
       "source": {"package": "/srv/match.fusion.zip", "entry": "assets/ASSET-car.glb"}},
      {"id": "ASSET-ad", "kind": "image_quad", "sha256": "…",
       "source": {"path": "/srv/ads/ad.png"}}
    ],
    "instances": [
      {"id": "PLC-1", "asset": "ASSET-car", "model": [[...], [...], [...], [0, 0, 0, 1]]},
      {"id": "PLC-2", "asset": "ASSET-ad", "model": [[...], [...], [...], [0, 0, 0, 1]], "visible": true}
    ]
  }
}
```

- `camera` fixes the native extent, rate and `radial-poly-native-px-v1` lens for
  the server's lifetime. Each frame's `Ti`, `C` and warp arrive with the frame
  (`tracking.frame`). The optional `reference` camera serves still renders
  (`chr-renderer-cli render --description <json> --frame <n>`); the live server
  ignores it.
- `scene` replaces the whole scene. `ground.pitch_m` sizes an invisible shadow
  catcher 1.5 times larger than the pitch. `lighting: null` selects four 35 m
  corner spots aimed at the centre plus a sun; otherwise it is a `scene_frame`
  `Lighting` (at most 8 lights). `shadow_mode: null` selects PCSS.
- Coordinates follow [coordinates](coordinates.md): pitch-centred metres, X
  along the length, Y along the width, ground Z = 0, up is negative Z. `model`
  is forward row-major and maps asset coordinates to the world.

## Assets

An asset is imported once and may back many instances; its mesh and textures
are shared.

| `kind` | Asset coordinates | Material |
| --- | --- | --- |
| `glb` | The importer's basis: glTF +Y-up turned to -Z-up, `(x, y, z) -> (x, z, -y)`, node transforms baked | As authored. `alphaMode: BLEND` is rejected unless `blend_as: "mask"`, which imports it as an alpha mask (cutoff 0.5) and logs a warning. The renderer has no translucent pass. |
| `image_quad` (PNG, JPEG) | Unit quad `[-0.5, 0.5]^2` at Z = 0, facing up. The image's top edge is at -Y and its left edge at -X, so it reads upright in a top view with +Y down | Unlit ground decal: blended by the image's alpha over the opaque CG (`alpha * colour + (1 - alpha) * behind`), in instance order, after opaque geometry and before the ground catcher, so CG shadows darken it; it casts no shadow and writes no depth |

`source` is either `{"path": …}` or `{"package": <zip>, "entry": <name>}`, such
as a Fusion offline package. `sha256` (lowercase hex of the source bytes) is
verified before import. Assets are at most 256 MiB. In a startup file, relative
paths resolve against the file's directory. Asset ID `chr-ground` is reserved.

## Loading

| When | How | Paths |
| --- | --- | --- |
| Startup | `chr-server --description <json>` (with the tracked input of `--tracked-scene`) | Absolute, or relative to the file |
| Runtime | `description.load {"scene": …}` over the control WebSocket or NativeSocket | Absolute, inside a `--asset-root <dir>` folder (repeatable); without one the method is rejected with `unsupported_profile` |

`description.load` answers `{scene_id, scene_revision, assets_imported,
assets_reused}`. An asset whose ID was already registered with the same SHA-256
and options is reused; the same ID with other content is a `conflict`.
Registrations are never removed: a server holds at most 64 assets and 1 GiB of
runtime asset bytes, and a restart clears them. A description places at most 63
instances.

A load is all or none for the scene. Instances that name unknown assets are
rejected first; any import failure leaves the scene unchanged. If creating the
new scene fails, the previous scene is recreated. Assets imported before such a
failure remain registered.

`chr-server` reads and verifies each runtime asset once and keeps the verified
bytes as a copy in a private directory beside its worker rings (mode 0700,
removed on shutdown). Render workers receive these copies in their `hello` and
import them again, so later changes to the caller's files cannot break a worker
restart. When the asset list grows, `chr-server` replaces its worker at once,
without backoff. Until the new worker is ready (about one second for a 27 MB
GLB), frames go out as clean feed or repeats, as during any worker restart.

## From the football client

`client/football-field-3d` converts the current match's placement draft:

- **3D placements:** `fitFusionModel` on the loaded GLB's three.js bounds. In
  chr's basis, the preview's centring becomes the translation `(-cx, -cz,
  min_y)`, followed by the placement's translation (Z −0.015), yaw and uniform
  scale.
- **2D placements:** the preview's contain fit of the image in its area,
  rotated by `rz + assetRotation`, at Z −0.02.
- **Hidden placements:** become `visible: false`.
- **Blended GLBs:** a GLB with BLEND materials gets `blend_as: "mask"`, and the
  page lists those materials after sending.
- **Asset sources:** assets are entries of the offline ZIP, read by the server
  from the ZIP's onsite path. The path is remembered when the ZIP was imported
  by path; otherwise type it. Matches from the Fusion API are not supported yet.

**发送到渲染器** calls `description.load` through the page's `/chr-render`
WebSocket proxy (`CHR_RENDER_URL`, default `ws://127.0.0.1:8787`), so one tunnel
to the page is enough. **导出渲染描述** downloads `{"scene": …}`; add a `camera`
block for a startup file, for example
`jq -s '.[0] + {scene: .[1].scene}' camera.json scene.json`.

## Run

```sh
server/scripts/run-tracked.sh --scene clip-manifest.json --video clip.mp4 \
  --description start.json --asset-root /srv/matches --srt 127.0.0.1:9001
```

The simulator replays the clip manifest's cameras; the server draws the
description. Start with `"assets": [], "instances": []` and send the match from
the client.

## Acceptance

```sh
cargo test --locked -p chr-runtime --test description
cargo test --locked -p chr-runtime --test description -- --ignored   # hardware GPU
cargo test --locked -p chr-renderer --test glb
npm --prefix client/football-field-3d run test:offline
```

The tests cover:

- shared assets and reserved IDs;
- SHA-256, root and relative-path checks, and ZIP entries;
- reuse, conflict, unknown assets and restoring the previous scene;
- the `blend_as` opt-in;
- an unlit quad drawn through the tracked camera on the GPU;
- the client's quad, GLB and asset conversion.

Recorded on the Linux RTX 3090 host: the Fusion package from the client
conversion loaded at runtime with 3 assets (two PNG ads and a 27 MB GLB whose
badge was masked). The worker was replaced in about one second, and the SRT
output showed the car and both ads in their placements.

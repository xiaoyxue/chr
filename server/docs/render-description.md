# Render description

**Status:** `implemented` opt-in profile `chr-render-description-v1`, used with the
[tracked camera simulation](sdi-simulation.md). It describes what to draw and
with which lens, independent of any client's own scene model. The schema lives in
`chr_common::description`; the pitch-studio client produces it from a Fusion match.

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
       "source": {"path": "/srv/ads/ad.png"}},
      {"id": "ASSET-flag", "kind": "flag", "sha256": "…",
       "source": {"path": "/srv/flags/club.png"}}
    ],
    "instances": [
      {"id": "PLC-1", "asset": "ASSET-car", "model": [[...], [...], [...], [0, 0, 0, 1]]},
      {"id": "PLC-2", "asset": "ASSET-ad", "model": [[...], [...], [...], [0, 0, 0, 1]], "visible": true,
       "opacity": 0.35},
      {"id": "FLAG-1", "asset": "ASSET-flag", "model": [[...], [...], [...], [0, 0, 0, 1]],
       "flag": {"width_m": 3.2, "height_m": 2.0, "amplitude_m": 0.32, "frequency": 1.35,
                "speed": 1.0, "material": "simple"}}
    ]
  }
}
```

- `camera` fixes the native extent, rate (`fps`: an integer, or a string such as
  `"30000/1001"`; it must be `chr-server`'s `--fps`) and `radial-poly-native-px-v1` lens for
  the server's lifetime. Each frame's `Ti`, `C` and warp arrive with the frame
  (`tracking.frame`). The optional `reference` camera serves still renders
  (`chr-renderer-cli render --description <json> --frame <n>`); the live server
  ignores it.
- `scene` replaces the whole scene. `ground.pitch_m` sizes an invisible shadow
  catcher 1.5 times larger than the pitch. Beyond it, an invisible holdout ground
  2 km across, which receives no shadows, reaches cameras up to about 1 km from the
  pitch centre. Together they are the real ground: whatever lies below them is not
  drawn, so the camera's picture shows there (a model rising from the ground
  appears only above it), even from a camera standing outside the catcher.
  `lighting` is described under [Lighting](#lighting). `shadow_mode: null` selects
  PCSS.
- Coordinates follow [coordinates](coordinates.md): pitch-centred metres, X
  along the length, Y along the width, ground Z = 0, up is negative Z. `model`
  is forward row-major and maps asset coordinates to the world.

## Motion

Any instance may carry `motion`, a rigid animation of the whole instance about its
`model` (#157). Rotation and scale act about `pivot` (for a Fusion model, its
neutral bottom centre); the path moves the pivot in fixed world axes, unaffected by
the model's heading or rotation:

```json
"motion": {
  "pivot": [5, 6, -0.015],
  "rotation": {"axis": [0, 0, 1], "mode": "continuous", "amplitude": 0, "period": 20},
  "path": {"start": {"offset": [-8, 2, 0], "scale": 0.5},
           "end": {"offset": [16, -6, -4], "scale": 1.5},
           "duration": 4, "playback": "ping_pong"}
}
```

At `t` seconds the model is `T(pivot + offset(t)) * scale(t) * R(axis, angle(t)) *
T(-pivot) * model`:

| Field | Meaning |
| --- | --- |
| `rotation.axis` | World axis, any length; positive angles turn right-handed about it, so about +Z from +X towards +Y like a heading |
| `rotation.mode` | `"swing"`: `amplitude * sin(2 pi t / period)`, `amplitude` in (0, pi] radians on each side; `"continuous"`: a full turn every `period` |
| `rotation.period` | Seconds, 0.1 to 60 |
| `path.start`, `path.end` | `offset`: world metres of the pivot (negative Z raises it); `scale`: positive multiplier of the neutral scale |
| `path.duration` | Seconds from start to end, positive; offsets and scale interpolate linearly |
| `path.playback` | `"once"` holds the end, `"loop"` restarts at the start, `"ping_pong"` returns over another `duration` |

Rotation and path run together: a finished `once` path keeps rotating. Nothing is
clamped or refitted, so a model may leave its region or go below the ground, where
the ground hides it. Time
zero is the first frame a render worker draws with the loaded scene, so each
`description.load` plays from the start; a restarted worker starts again. A still
render (`--frame <n>`) uses `t = n / fps`. The render worker evaluates the motion on
the CPU for each frame; the main pass, shadows and bounds use the moved model, and
the shaders are unchanged. Invalid settings reject the load with the instance and
field named.

## Assets

An asset is imported once and may back many instances; its mesh and textures
are shared.

| `kind` | Asset coordinates | Material |
| --- | --- | --- |
| `glb` | The importer's basis: glTF +Y-up turned to -Z-up, `(x, y, z) -> (x, z, -y)`, node transforms baked | As authored, including the material extensions in [glTF support](gltf-support.md); an unimplemented extension is ignored with a warning unless the file requires it. `alphaMode: BLEND` is rejected unless `blend_as: "mask"`, which imports it as an alpha mask (cutoff 0.5) and logs a warning. The renderer has no translucent pass. |
| `flag` (PNG, JPEG) | Flag-local metres: X along the width from the left edge, Y down from the top edge, Z = X × Y. The image's top-left texel is at the origin and reads upright from the -Z side, mirrored from the other. Size comes from the instance's `flag` | Double-sided artwork; texels with alpha below 0.5 are cut out. `material: "simple"` lights it like SimpleWave under a fixed key light above the viewer; `"pbr"` uses the scene's lights, environment and shadows. It casts shadows that follow the wave |
| `environment` (OpenEXR) | A latitude-longitude panorama, 2:1: +X at the image centre, +Y a quarter further right, up (-Z) along the top row | Not drawn: only `lighting.environment.asset_ref` may name it, never an instance. Linear sRGB/D65 RGB(A), HALF or FLOAT, NONE/RLE/ZIP/ZIPS/PIZ; finite negative texels become zero (see `chr_renderer::exr::import_exr`) |
| `image_quad` (PNG, JPEG) | Unit quad `[-0.5, 0.5]^2` at Z = 0, facing up. The image's top edge is at -Y and its left edge at -X, so it reads upright in a top view with +Y down | Unlit ground decal: blended over the opaque CG by the image's alpha times the instance's [`opacity`](#image-opacity) (`alpha = opacity * image alpha`, `alpha * colour + (1 - alpha) * behind`), in instance order, after opaque geometry and before the ground catcher, so CG shadows darken it; it casts no shadow and writes no depth |

### Image opacity

An `image_quad` instance may carry `opacity`, a number from 0 to 1 inclusive: its
alpha is the image's own alpha times this, exactly once, so an opaque texel at 0.35
covers 35 % of what lies behind and a texel of alpha 0.5 covers 17.5 %. Transparent
texels stay transparent and 0 hides the quad. Without `opacity` an image quad is
drawn at 0.5 (#163). Instances of one image keep their own opacity; the asset and
its image are unchanged. Other values, or `opacity` on another asset kind, reject
the load with the instance named. chr-server applies it as the override of the
quad's material (`material_overrides`, part 0, `base_color` alpha).

## Lighting

`lighting: null` selects the default rig below. Otherwise `lighting` is a
`scene_frame` `Lighting` and replaces it completely: at most 8 lights of the kinds
`directional`, `point` and `spot`, an optional environment and an optional
constant fill. Units are linear RGB: a directional light's `irradiance` in W/m²
perpendicular to it, a point or spot light's `intensity` in W/sr (a spot's before
its cone falloff), radiance in W/(m² sr). Directions are where the light travels,
in world coordinates (+Z down); angles are radians.

| Default light | Configuration |
| --- | --- |
| `tower-light-0` … `-3` (`spot`) | At the four pitch corners 35 m up, `(±length/2, ±width/2, -35)`, aimed at the centre spot; `intensity` `[16000, 16800, 20000]`, `inner_angle` 0.4, `outer_angle` 0.9, `source_radius` 0.5 m |
| `sun` (`directional`) | `direction` `[0.5, 0.6, 1.0]` (from the -X -Y side, about 52° high), `irradiance` `[2.5, 2.35, 2.1]`, `angular_radius` 0.00465 (the sun's) |
| Environment, constant | None |

All lights inherit the scene's `shadow_mode` unless they set their own.

```json
"lighting": {
  "lights": [
    {"light_id": "sun", "kind": "directional", "direction": [0.5, 0.6, 1.0],
     "irradiance": [2.5, 2.35, 2.1], "angular_radius": 0.00465, "shadow_mode": null},
    {"light_id": "tower-0", "kind": "spot", "position": [-52.5, -34, -35],
     "direction": [52.5, 34, 35], "intensity": [16000, 16800, 20000],
     "source_radius": 0.5, "inner_angle": 0.4, "outer_angle": 0.9, "shadow_mode": null},
    {"light_id": "lamp", "kind": "point", "position": [0, 30, -12],
     "intensity": [800, 800, 800], "source_radius": 0.3, "shadow_mode": "off"}
  ],
  "environment": {"asset_ref": "IBL-orlando", "gain": 0.5, "rotation": 0.0, "visible": false,
                  "max_radiance": 50},
  "constant": [0.1, 0.1, 0.1]
}
```

- **Environment (IBL):** `asset_ref` names an `environment` asset of the same
  description. `gain` scales its radiance, `rotation` turns it about the up axis.
  Keep `visible: false`: the program composites CG over the camera's picture, so a
  drawn sky would cover it. Image-based light is unoccluded (no shadows); keep the
  sun a `directional` light, which casts them. `max_radiance` (optional, before
  `gain`) clamps brighter panorama texels: set it when the panorama holds a sun
  that a directional light already carries, which would otherwise light shadows a
  second time and sparkle on glossy, finely bumped surfaces (#177). A server
  prefilters each panorama once per cap on
  the GPU (256 x 128 x 6 roughness layers, 64 filtered samples from the source's mip
  chain); a source with its mips plus its layers may take up to 256 MiB of GPU
  memory, enough for 4096 x 2048.
- **Constant fill:** `constant` is a uniform radiance from every direction, added
  to the environment's and shaded the same way (diffuse and specular, also
  unoccluded). It lifts shadows evenly; it does not appear as a background.

### Flags

A `flag` asset is only the artwork; each instance carries its own `flag` object,
which is required for flag assets and rejected for every other asset. Two instances
can share one artwork with different sizes and motions, and editing a flag never
re-registers its asset.

A flag moves in one of two ways, chosen by `type`: `"default"` (or absent) is the
closed-form SimpleWave below; `"native"` is a physically based cloth simulated by
the renderer (#219, [Native flags](#native-flags)). Only the fields of the chosen
type are accepted: wave fields on a native flag, or wind fields on a default one,
are refused.

| Field | Meaning |
| --- | --- |
| `width_m`, `height_m` | Flag size in metres, positive |
| `type` | Optional, `"default"`: `"default"` or `"native"`. Written only for native flags |
| `amplitude_m` | Default flags. Optional, 1.1: wave coefficient in metres; the surface leaves its plane by at most 1.53 times this. Any value works, but the free edges fold over once 0.24 times it exceeds `height_m` |
| `frequency` | Default flags. Optional, 1.35: primary wave cycles from the pinned centre line to either free edge |
| `speed` | Default flags. Optional, 5.5: time scale; 0 freezes the wave |
| `wind_mps` | Native flags. Optional, 7: wind speed in m/s, 0 to 18 |
| `wind_direction_deg` | Native flags. Optional, 25: base direction the wind blows towards, in degrees about the flag-local Y axis: 0 along +X (towards the right edge as the artwork reads), 90 along -Z (towards the side that reads it), -90 away from it |
| `auto_direction` | Native flags. Optional, `true`: turn the wind smoothly to a new target within 45° of the base every 0.5 s, the same sequence every run |
| `tension` | Native flags. Optional, 0.65: bending stiffness from 0 (soft and flowing) to 1 (crisp and firm) |
| `raise` | Native flags. Optional: `{"duration_s": 6, "start": "load"}` raises the flag from folded over `duration_s` seconds (finite, above 0), starting with the scene (`load`) or with a cue (`cue`, `description.cue_reveal`); absent, it hangs raised from the start |
| `slack` | Native flags. Optional, 0.1: the part of the cloth's length the pinned edges do not span, 0 (taut) to 0.2. The reference's 0.1 makes the cloth 11 % longer than `height_m` and bows it out by about a fifth of its height; a large flag lying along a stand sags by that much and looks short, so tifo-sized flags want 0.01 to 0.03 |
| `material` | Optional, `"cloth"`: `"simple"` (a fixed two-sided key light, no scene lights or shadows), `"pbr"` (the scene's lights, environment and shadows) or `"cloth"` (PBR fabric: matte, a weak highlight and a soft sheen at grazing angles) |

The default flag's defaults are the wave and material pitch-studio gives a tifo,
within its limits (amplitude 0 to 5 m, frequency 0.3 to 3, speed 0 to 10), so
`{"width_m": 36, "height_m": 13.5}` is a complete tifo flag. A native flag's are
the reference simulation's controls.
| `artwork_rotation` | Optional: 0, 90, 180 or 270 degrees, turning the artwork clockwise as it reads, like a 2D Fusion placement's `assetRotation`; the artwork stretches to the flag's size |
| `reveal` | Optional: how the flag appears (#210, #215), `{"direction": "left_to_right", "duration_s": 2, "start": "load"}`; absent, the flag is shown from the start |

As in [Simulation-SimpleWave](https://github.com/xiaoyxue/Simulation-SimpleWave),
there is no pole: the centre line stays pinned and both halves wave outwards. Make
`model` rigid, with columns `[u, v, u × v, origin]`: unit width axis, unit axis down
the flag, their cross product, and the top-left corner. Size then stays in `flag`,
not in the matrix. The wave's time is each frame's media time, so a still render of
`--frame <n>` is reproducible.

#### Native flags

A native flag is the cloth of the AER WebGPU flag simulation
([xiaoyxue/Simulation](https://github.com/xiaoyxue/Simulation) at `ff9c934`, its
raised state), ported to the renderer: 65 × 41 particles whose top row is pinned
along the flag's top edge (Y = 0) and bottom row along its bottom edge
(Y = `height_m`), with free sides. The cloth is longer than that span by its
`slack` (11 % at the default), so it billows; it starts as a slack arc bowed towards
the side that reads the artwork, or flat without slack. Changing `slack` restarts
the cloth.
Each fixed 1/120 s step integrates the reference's visually tuned gravity
(3.8 m/s², world down turned into the flag's axes through `model`), damping and an
aerodynamic load from the gusting wind, then relaxes structural, shear and bending
distances in 36 Jacobi iterations. There is no pole, ground contact, self-collision
or air simulation, and the wind is relative to the flag, not to the world. The
reference is tuned at 3.2 × 4.8 m; much larger flags move like a smaller flag in
slow motion.

The cloth's clock is the scene's time, as for reveals: it starts at rest when the
scene starts playing, and each frame the renderer steps it up to the frame (at
most 8 steps a frame, so it keeps real time down to 15 fps; a longer stall is
skipped, not made up). A cloth first drawn late, such as a still render of
`--frame <n>`, takes up to its last 3 s at once, so stills are reproducible and
show a cloth in the wind. Hiding a native flag (`visible: false`) forgets its
state; it restarts at rest when shown again. Reveals and artwork rotation work as
for default flags; a native flag's shadows follow it every frame.

A **raise** is the reference's lift. The cloth starts folded in accordion folds just
above its pinned bottom edge (over 5.2 % of `height_m`, the reference's 0.25 m of
4.8 m); its top edge then rises to the flag's top on a smoothstep over
`duration_s` and stays, while the wind keeps blowing. The bottom edge never moves.
With `"start": "load"` the raise starts with the scene; with `"cue"` the flag waits
folded until `description.cue_reveal` names its instance, and every further cue folds
it and raises it again. A raise and a reveal may share an instance's cue.

A **reveal** opens the flag once over `duration_s` seconds (any finite number
above 0; there is no upper limit, as in Fusion) and then keeps it whole. At
progress `p = t / duration_s` (clamped to 1) it shows the part of the flag's
rectangle that `direction` has opened, in flag-local `x / width_m` and
`y / height_m`, measured from the top-left corner (the `model` origin) with `x`
along the top edge and `y` down the left edge:

| `direction` | Fusion `tifoReveal.direction` | Shown at progress `p` |
| --- | --- | --- |
| `left_to_right` | `left-to-right` | `x / width_m <= p` |
| `right_to_left` | `right-to-left` | `x / width_m >= 1 - p` |
| `top_to_bottom` | `top-to-bottom` | `y / height_m <= p` |
| `bottom_to_top` | `bottom-to-top` | `y / height_m >= 1 - p` |
| `center_horizontal` | `center-horizontal` | `|2 x / width_m - 1| <= p`: from the vertical centre line out to the left and right |
| `center_vertical` | `center-vertical` | `|2 y / height_m - 1| <= p`: from the horizontal centre line out to the top and bottom |

The direction belongs to the rectangle as its corners lay it out, as in Fusion's
preview, not to the artwork: `artwork_rotation` turns the image inside the
rectangle and leaves the opening edge where it is. The grid points not yet shown
collapse onto the edge of the shown part, so the edge is exact, the artwork there
is cut at its own texels (never squeezed or stretched), and shadows follow it.
`"load"` starts with the scene, at its motion's time zero. `"cue"` hides the
flag until a cue starts it:

| Method | Params | Result |
| --- | --- | --- |
| `description.cue_reveal` | `{"instance": <flag instance ID>}` | `{"cue": n}`, the instance's cues since its scene was loaded |

A cue reaches the render worker with the next frame, which starts the reveal at
its own scene time; a repeated cue restarts that flag's reveal only, leaving other
flags, their reveals and every motion as they are. A cue of an instance that is not
a flag with `"start": "cue"` in the current scene is rejected with `not_found`.
`description.load` clears the cues, so a new scene's cued flags wait again; a
restarted render worker replays the latest cue's reveal from its start, as it
replays motion.

`source` is either `{"path": …}` or `{"package": <zip>, "entry": <name>}`, such
as a Fusion offline package. `sha256` (lowercase hex of the source bytes) is
verified before import. Assets are at most 256 MiB. In a startup file, relative
paths resolve against the file's directory. Asset IDs `chr-ground` and
`chr-ground-holdout` are reserved.

## Loading

| When | How | Paths |
| --- | --- | --- |
| Startup | `chr-server --description <json>` (with the tracked input of `--tracked-scene`) | Absolute, or relative to the file |
| Runtime | `description.load {"scene": …}` over the control WebSocket or NativeSocket | Absolute, inside a `--asset-root <dir>` folder (repeatable); without one the method is rejected with `unsupported_profile` |

`description.load` answers `{scene_id, scene_revision, assets_imported,
assets_reused}`. An asset whose ID was already registered with the same kind,
SHA-256 and options is reused; the same ID with other content is a `conflict`.
Registrations are never removed: a server holds at most 64 assets and 1 GiB of
runtime asset bytes, and a restart clears them. A description places at most 63
instances.

A load is all or none for the scene. Instances that name unknown assets are
rejected first; any import failure leaves the scene unchanged. If creating the
new scene fails, the previous scene is recreated. Assets imported before such a
failure remain registered.

`chr-server` reads and verifies each runtime asset once and keeps the verified
bytes as a copy in a private directory beside its worker sockets (mode 0700,
removed on shutdown). Render workers receive these copies in their `hello` and
import them again, so later changes to the caller's files cannot break a worker
restart. When the asset list grows, `chr-server` replaces its worker at once,
without backoff. Until the new worker is ready (about one second for a 27 MB
GLB), frames go out as clean feed or repeats, as during any worker restart.

## From the pitch-studio client

`client/pitch-studio` converts the current match's Fusion placement draft and
its local placements, both kept in the studio store on the onsite server
(`STUDIO_DATA_DIR`, see [development](../../docs/development.md)); placements
made onsite are stored per match, apart from the Fusion package, so importing a
newer package keeps them:

- **Fusion 3D placements:** `fitFusionModel` on the loaded GLB's three.js bounds.
  Fusion's heading (`modelPose.yaw`) and size factor fit the footprint into the
  region; the studio's scale (0.1–5) and rotation (`rz`, about the model's own up
  axis, glTF +Y) then act about that neutral bottom centre without refitting, so
  the bottom stays on the ground, rotating keeps the size and the model may leave
  its region. In chr's basis, the preview's centring becomes the translation
  `(-cx, -cz, min_y)`, followed by the placement's translation (Z −0.015), yaw and
  uniform scale.
- **Fusion model animation:** `modelPose`'s rotation (`swingAxis`, `swingAmplitude`
  degrees, `swingPeriod`, `rotationMode`, swing when omitted) and `motion` (field
  metres, z down, `once` / `loop` / `ping-pong`) become the instance's
  [`motion`](#motion) about its neutral bottom centre: Fusion's field axes are chr's
  world axes. Disabled or absent settings send no motion but are kept through import
  and the store; invalid ones fail the import naming the field. The preview plays
  the same motion from when the models load.
- **Fusion 2D placements:** the preview's contain fit of the image in its area,
  rotated by `rz + assetRotation`, at Z −0.02. The studio scales the area (relative
  to Fusion's size) and rotates it about its centre; the rotated area must stay on
  the pitch. Fusion's per-placement `assetOpacity` (#188; 0 to 1, 1 when an older
  manifest lacks it; anything else fails the import naming the placement) is kept
  in the draft, editable as 不透明度 in the inspector once unlocked, drawn by the
  preview and sent as the instance's [`opacity`](#image-opacity), never multiplied
  by another 0.5.
- **Fusion tifos** (package v2, #210): a v2 offline ZIP (`packageVersion` 2,
  `schemaVersion` 2, whose `coordinates` add `z: down` and the motion spaces) may
  carry the field's point cloud (`match.field.pointCloud`, a `point-cloud/*.ply`
  entry checked by size and SHA-256 and stored in the studio store) and `Tifo`
  Ads. A Tifo placement lies on the point cloud, whose coordinates are the pitch's:
  four `corners` (top-left, top-right, bottom-right, bottom-left) that must form
  its `width` x `height` rectangle, one PNG or JPEG, `assetRotation`
  (0/90/180/270), `assetOpacity` 1 (chr flags are opaque; other values fail the
  import) and an optional `tifoReveal` (#215): `direction`, one of the six in the
  table under [Flags](#flags) (`left-to-right`, `right-to-left`, `top-to-bottom`,
  `bottom-to-top`, `center-horizontal`, `center-vertical`), and `duration` in
  seconds, any finite number above 0, 2 when absent or null. An absent or null
  `tifoReveal` gives a static tifo, shown whole from the start. An unknown
  direction or a duration that is not a positive finite number fails the import
  with a message naming the placement and the accepted values; nothing is clamped
  or replaced. Importing
  the ZIP seeds each tifo the match lacks as a point-cloud **flag** `tifo-<id>`
  of the package's cloud, which counts as confirmed: `u` along the top edge, `v`
  down the left edge, a tifo wave (amplitude 1.1 m, frequency 1.35, speed 5.5,
  `cloth`) offset by its peak, -1.53 x 1.1 = -1.683 m, the image from the store
  as its material, its rotation and its reveal (same direction and duration,
  starting on load), or none. From then on it is an
  ordinary flag the operator edits, and importing again neither duplicates it nor
  undoes edits. `server/scripts/fusion_description.py` maps a package the same way,
  with or without its point cloud: the corners are pitch coordinates, so it needs
  none and only checks one the package carries (#217).
  For example, `"tifoReveal": {"direction": "bottom-to-top", "duration": 3.5}`
  becomes the flag's `"reveal": {"direction": "bottom_to_top", "duration_s": 3.5,
  "start": "load"}`.
- **Local placements:** drawn in the studio on the pitch (2D, 3D) or on a
  point-cloud plane (2D, 3D, flags). Each lies on a rectangle with unit axes `u`
  (image right) and `v` (image down) and normal `u × v`; the preview and the
  description share one transform (`local-ad-transform.ts`), lifted 15 mm off
  the surface towards the side it was drawn from, plus the normal offset.
  - **2D** → `image_quad` with `model` `[u · width, v · height, u × v, centre]`,
    so the quad's top-left texel lands on the rectangle's top-left corner, and
    the placement's 不透明度 (0 to 1, 0.5 until set) as its
    [`opacity`](#image-opacity).
  - **3D** → `glb`: the glTF scene centred on its footprint, stood up out of the
    plane, turned by its yaw and fitted to the rectangle, then expressed in chr's
    GLB basis.
  - **Flags** (point cloud only): **选面放旗帜** picks three points; the plane is
    turned so that, seen from the picking side, `u` points right and `v` down
    (world +Z) and `u × v` points away from the viewer; the default normal offset
    is -0.5 m, towards the viewer, so the wave (up to 1.53 × amplitude) clears the
    surface. The instance's rigid `model` is `[u, v, u × v, top-left + offset ·
    (u × v)]` with its own `flag` size and wave. Pitch flags of earlier versions
    are removed when a match loads, and the page says how many. The inspector
    also sets the flag's 图片旋转 (`artwork_rotation`), 锁定宽高比 (the rectangle
    follows the turned image's aspect), and its 展开 (`reveal`: none or one of the
    six directions, 时长 in seconds above 0, 开始 加载时 / 手动触发); 预览展开 plays
    it in the page with the renderer's geometry, and 触发展开 calls
    `description.cue_reveal` for a sent flag that starts on a cue.

  chr-server reads local materials from their server paths (inside its
  `--asset-root`); a material chosen on the operator's computer is uploaded to
  the studio store and read from there. Each distinct material is one asset
  (`LOCAL-` or, for flags, `FLAG-` plus the first 16 hex of its SHA-256). A send
  downloads no material: a stored file is named by its SHA-256, and any other
  server file is hashed by the page server (`/local-files/sha256`, cached until
  the file's size, modification time or inode changes); a GLB's bounds and BLEND
  materials are noted in the placement when the material is chosen and reused
  while its hash matches. A point-cloud
  placement is sent once its cloud (by the PLY's SHA-256) has been confirmed to
  share pitch coordinates, also without loading that cloud again; editing it needs
  the cloud. Placements that cannot be sent are left out and the page says why.
- **Disabled placements:** become `visible: false`.
- **Lighting:** the match's preset from **场景灯光** (in 广告位预览,
  between the pitch editor and the send bar) becomes `scene.lighting`, always
  explicit: its enabled point, spot and directional lights (at most 8; colours times
  intensities), its panorama as an `environment` asset `IBL-` plus the first 16 hex of
  its SHA-256 (hashed by the page server, not downloaded; `visible: false`), and its
  constant fill. Presets are one global library in the store's `lighting.json`, with
  the preset each match uses. A preset names its panorama by its file name in the IBL
  folder (`STUDIO_IBL_DIR`), and a send resolves it there, so a library moves between
  servers; a library that named it by absolute path is read by its file name. Two built-ins are offered: chr-server's default rig lit by
  `fly-studio-03_4K.exr` (gain 0.5; the panorama IBL picks first when switched on), and
  `orlando_stadium_4k.exr` (gain 0.5, `max_radiance` 50, which caps off its sun) with
  that rig and a constant fill of 0.1. The IBL card's 亮度上限 sets `max_radiance`
  (starting from 50 when switched on; off sends none). Every
  value of the selected preset is editable at once; editing a built-in stores the
  operator's version under its ID, which 恢复内置默认 drops. Each light, the IBL and
  the constant fill have their own 启用 switch; switched off, they keep their values
  but are not sent.
  A directional light is set by the azimuth (from +X towards +Y) and elevation of
  the side it comes from, a spot by its position, height and aim point on the ground.
- **Blended GLBs:** a GLB with BLEND materials gets `blend_as: "mask"`, and the
  page lists those materials after sending.
- **Fusion asset sources:** assets are entries of the offline ZIP, read by the
  server from the ZIP's onsite path: the path it was imported from, or its copy
  in the studio store for a ZIP chosen on the operator's computer. Their SHA-256
  is the one the package declared (verified at import), and their BLEND
  materials are noted at import, so a send reads none of them. Matches from
  the Fusion API are not supported yet.
- More than 63 instances are refused before sending.

**场次配置广告位** lists every placement: Fusion, 球场本地 (2D, 3D) and 点云本地
(2D, 3D, flags), each with its size, material and, when it cannot be sent, the
reason; a row selects the placement in its editor, and its switch enables it. The
pitch and point-cloud editors share one inspector: position, size and material
are locked until **🔒 已锁定** is unlocked and save as a field changes; a flag's
motion stays editable while locked and is saved with **保存旗帜属性**. Its type
switches between **默认（正弦飘动）**, whose wave (three sliders) is previewed live
as an animated wave, and **物理模拟（布料）**, a native flag with the reference
simulation's control panel: wind speed, a wind dial (drag, or arrow keys) with
its base angle and slider, automatic direction (the dial's solid arrow shows it
turning, the dashed one the base), fabric tension, **布料余量** (slack, 0–20 %,
default 10 %; its note gives the resulting bulge) and **升旗** (raise from folded: its
duration and whether it starts on load or on **触发升旗**). The page previews a native
flag only at rest; its motion shows in the renderer's output. Switching type keeps
the other type's settings. Both editors have undo and redo
and **保存并发送到渲染器**; sending or exporting with unsaved edits asks to save
them first.

A page reload keeps the session: the store remembers, per match, the point
cloud, result folder and reference video imported by server path (and the page
re-reads them on reload, saying which could not be read), each point cloud's
same-coordinates answer and the selected placements; files chosen locally are
named for selecting again. Another page or browser sees the same data; if it
saved first, a write is refused and the page reloads and says so. The page asks
before leaving only while an edit, a drawing or a send is unfinished. Data of
earlier versions, kept in the browser, is moved to the store once and left in
the browser as a backup.

**导出现场广告位** writes a match's onsite placements (pitch and point-cloud,
with their properties, material server paths, each cloud's PLY SHA-256, its
same-coordinates answer and, if known, its PLY path; no Fusion package data) as
`chr-studio-local-placements-v1` JSON. **导入现场广告位** reads such a file, or a store's
`local-ads.json`, chosen locally or by server path, validates it and asks whether
to merge (same placement id replaced, others kept) or replace all local
placements; it is saved like any edit. Point-cloud placements of another PLY
are imported too and show and send once that PLY is loaded or confirmed.

Both the **广告位预览** and **导入结果与微调** tabs end with the same two rows. The
renderer bar holds only the offline ZIP path, the readiness status and the
prominent **发送到渲染器**; a separate 导出 / 导入 row holds **导出渲染描述**,
**导出位置 JSON**, **导出现场广告位** and **导入现场广告位** (from a file or a
server path), so the live send is never next to a file action. Both tabs share
one path and state. The three exports are written on the onsite server, not
downloaded: into the folder typed in **导出到服务器路径** (by default the match's
`matches/<matchId>/exports/` in the studio store) under `render-scene-<match>.json`,
`placements-<match>.json` or `local-placements-<match>-<time>.json`, or to the
`.json` file typed there; the page asks before replacing a file, shows the
written path and remembers the path per match. **同时下载到本机** (off by default)
also downloads the file. Fusion placements are sent only for an offline-ZIP
match whose preview has loaded its materials and that has a server path; without
the ZIP or its path, sendable local placements are sent alone and the status
says which placements were left out and that the send replaces the whole scene.
The buttons stay disabled while an action runs, so repeated clicks do not queue
repeated loads.
**发送到渲染器** calls `description.load` through the page's `/chr-render`
WebSocket proxy (`CHR_RENDER_URL`, default `ws://127.0.0.1:8787`), so one tunnel
to the page is enough. Before loading, it checks on the same connection that
chr-server answers and that `render.get_worker_status` is `ready`. If not, it
names the cause instead of sending: the proxy target is unreachable (the page
server reports it at `/chr-render-info`), there is no render worker, the worker
is starting or backing off (with its last error), or `description.load` is not
enabled (no `--description` / `--asset-root`). **导出渲染描述** writes `{"scene": …}`; add a `camera`
block for a startup file, for example
`jq -s '.[0] + {scene: .[1].scene}' camera.json scene.json`.

Without the page, `server/scripts/fusion_description.py` converts a Fusion offline
ZIP the same way (Fusion placements and the flags seeded from its tifos, all
enabled, read from the ZIP rather than the store, no other local placements); its
output equals the page's export for an unedited package. `--camera <json>` takes the
`camera` of another description and writes a complete one, and `--move <placement>
<dx> <dy>` shifts a placement in pitch metres first, for example to render a still:

```sh
uv run server/scripts/fusion_description.py match.zip --camera start.json \
  --move PLC-… 0 10 --output output/description/moved.json
cargo run --locked --release -p chr-runtime --bin chr-renderer-cli -- render \
  --description output/description/moved.json --fps 30 --size 1920x1080 \
  --output output/renderer-cli/moved.png
```

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
cargo test --locked -p chr-runtime --lib motion
cargo test --locked -p chr-runtime --test description -- --ignored   # hardware GPU
cargo test --locked -p chr-renderer --test glb
cargo test --locked -p chr-renderer --test flag_gpu -- --ignored   # hardware GPU
cargo test --locked -p chr-renderer --test exr_import
cargo test --locked -p chr-renderer --test environment_gpu -- --ignored   # hardware GPU
npm --prefix client/pitch-studio run test:offline
```

The tests cover:

- shared assets and reserved IDs;
- SHA-256, root and relative-path checks, and ZIP entries;
- reuse, conflict, unknown assets and restoring the previous scene;
- the `blend_as` opt-in;
- an unlit quad drawn through the tracked camera on the GPU;
- flag parameters required for flag assets and rejected for others;
- tifo flags: artwork rotations and reveal directions and durations checked at
  load, cues counted per instance, rejected for flags without a cued reveal and
  cleared by a load; reveals start with the scene or the latest cue, and both the
  turned artwork and each of the six reveal directions (at progress 0, 0.5 and 1,
  under every artwork rotation) are drawn on the GPU with either material;
- environment assets: OpenEXR import, used only by lighting, and a constant fill
  next to them; a constant fill shades like a uniform environment of the same
  radiance;
- the client's quad, GLB and asset conversion, including local placements and flags;
- motion: kept through a load, invalid settings refused, sampled like Fusion's
  example (ping-pong at 0/2/4/6/8 s) and translated in world axes.

Recorded on the Linux RTX 3090 host: the Fusion package from the client
conversion loaded at runtime with 3 assets (two PNG ads and a 27 MB GLB whose
badge was masked). The worker was replaced in about one second, and the SRT
output showed the car and both ads in their placements.

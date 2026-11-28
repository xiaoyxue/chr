# Test scenes

**Status:** `implemented` — scenes that were run on the onsite stack, recorded so
that each can be set up again after the stack moves on to another one.

**Scope:** each scene's input clip, tracking, Fusion package, studio placements and
the look tuned on it, with what was verified. How to launch the stack and where
files live are in the [operations guide](operations.md); flag fields are in the
[render description](../server/docs/render-description.md#flags). Placeholders such
as `<asset-root>` stand for host-specific paths, which belong only in the local
`output/stack/stack.toml`.

## Jensen native flag (deyang, H1)

The scene native flags (#219, PR #225) were tuned on, from 2026-11-27: one large
upright flag with a portrait artwork above the stand opposite the camera, over the
deyang broadcast clip tracked by the H1 map.

### Input and tracking

| Part | Value |
| --- | --- |
| Code | `feature/native-flag` (PR #225); `main` has no native flags before it merges |
| Clip | `deyang-clip-end-5s-120s.mp4`: 0:05–2:00 of `deyang-clip-end.mp4`, 1080p50, 5750 frames (115 s) played in a loop |
| Startup description | 1920 × 1080 at 50 fps, pinhole (radial coefficients 0), no instances |
| Tracker | vchao `pitch-track-live` built at vchao `57a25ab` (newer than chr's pin `209a97b`), hybrid tracking |
| Map | `deyang-cam1-0007scan-station-n16rot-v6.2-h1`: `h1/map/manifest.json` and `calibration/lens-model.json` |
| Camera | Station centre C = (-0.373, 86.846, -13.606) m; focal length 1796 to 2668 px over the loop, deepest from about 85 s to 87.6 s (frames 4254 to 4378) |

```toml
description = "<startup description.json>"
video = "<clips>/deyang-clip-end-5s-120s.mp4"
tracking = "worker"
tracking_cli = "server/scripts/with-cuda-driver.sh"
tracking_worker_args = [
    "<pitch-track-live built at vchao 57a25ab>",
    "--asset-manifest", "<map>/h1/map/manifest.json",
    "--lens-model", "<map>/calibration/lens-model.json",
    "--batch-frames", "1", "--estimate-every", "1", "--interpolation", "logh",
    "--match-max-distance", "0.9", "--match-ratio", "0.9", "--overload-ms", "500",
    "--smoothing-sigma-frames", "2.0", "--smoothing-strength", "0.0",
    "--track", "hybrid", "--track-coast-frames", "500", "--track-map-every", "1",
    "--ledger", "<asset-root>/runs/h1/{stream_id}.arrow",
]
tracking_threads = 4
# <map>/calibration/station.json C_world_metres
tracking_centre = [-0.37269253965707744, 86.84590008104848, -13.606184502193244]
program_delay_ms = 800
size = "1920x1080"
```

### Package and placement

The Fusion offline package `MATCH-3a71bb7b49744df7af7a9fa2d946f404-jensen-native.fusion.zip`
(SHA-256 `67145c292822c2a473a98e1109708b241904ba15c566145368de27b9182d4a05`) lies
under `<asset-root>`. It holds the match "Jensen native flag" on a 105 × 68 m field
with the vchao static-stadium point cloud and one tifo, `PLC-028fbb93a3e9472da11c7cb2d790412c`
(JENSEN). Its corners stand 40 × 40 m upright at Y = -64 from Z = -40 to the ground.
The artwork is `jensen.jpg`, 1920 × 1920 (SHA-256
`fa2e08f2ce6052cbf86b42e0e44f39de95059efff9741543b4d1a7f3b750d44e`).

The studio seeds the flag at the package's 40 m. It was then made 25 × 25 m so
that the whole flag stays in frame at the deepest zoom. Its bottom edge stays on
the ground and it stays centred:

| Placement field (`Tifo JENSEN`) | Value |
| --- | --- |
| `plane` | origin (-12.5, -64, -25), u (1, 0, 0), v (0, 0, 1), 25 × 25 m |
| `offset` | -1.683 (the tifo clearance, towards the camera): the flag's plane is Y = -62.317 |
| Native flag | wind 16 m/s, direction -13°, automatic direction, tension 24 %, slack 10 %, material cloth, no raise ([tuned settings](../server/docs/render-description.md#native-flags)) |
| Reveal | bottom to top, 2 s, at load |

The studio sends it as `model` `[[1, 0, 0, -12.5], [0, 0, -1, -62.317], [0, 1, 0, -25], [0, 0, 0, 1]]`:
X from -12.5 to 12.5 m, Z from -25 m to the ground.

The tuned placements are exported to
`<asset-root>/studio/matches/MATCH-3a71bb7b49744df7af7a9fa2d946f404/exports/jensen-native-tuned.json`
(`chr-studio-local-placements-v1`, with the point cloud confirmed).

### Lighting and shadows

The tuned look was loaded with `lighting: null`: chr-server's default four 35 m
corner spots aimed at the pitch centre and a sun whose light travels along
(0.5, 0.6, 1). A send from the studio uses the match's lighting preset instead.
No preset is selected for this match, so that is the built-in
默认：四角灯塔 + 太阳 + fly-studio IBL, which adds image-based lighting.

The cloth shadows itself: its folds darken the flag by up to 61/255 against a
render without shadows. Its shadow on the ground does not show. The ground shadow
catcher covers 1.5 times the pitch (|X| ≤ 78.75 m, |Y| ≤ 51 m) and the flag stands
outside it. Only the sun shadow of its top 6 m or so reaches the catcher, at Y from
-51 to -47 m, and none is visible on the program. The near spots light the reading
side and cast its shadow behind the flag.

### Verified

- On the program at the deepest zoom, the whole flag stays in frame, top edge
  included.
- With the tuned settings the cloth billows, folds and swings. Before native
  flags were simulated at the reference's height (`b358132`), it moved in shallow,
  slow folds; with slack 1 % and tension 95 % it hangs like a rigid board.
- A 30 s recording of the program from SRT (1080p50 H.264) is
  `output/recordings/native-flag-20261127-2216.mp4` in the stack host's main
  checkout.

### Set it up again

1. Put the package and `jensen.jpg` under `<asset-root>` and set `stack.toml` as above.
   Start the stack from a checkout with native flags.
2. In the studio, import the package by its server path. Then import the point
   cloud and confirm that it shares the pitch coordinates.
3. Choose 导入现场广告位 with the export above to restore the 25 m flag and its
   settings. Otherwise, enter them by hand.
4. Choose 发送到渲染器.

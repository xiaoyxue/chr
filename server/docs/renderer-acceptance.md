# Renderer roadmap acceptance (Linux)

Status: **Linux renderer acceptance for #34, 2026-11-12**, on `main` after #87.
This report covers the renderer library through its typed API. Protocol transport,
server SDI/monitor policy and Windows are separate. The paired translation is
[renderer-acceptance.zh-CN.md](renderer-acceptance.zh-CN.md); the design and
roadmap are in [renderer-design.md](renderer-design.md).

Hardware: **NVIDIA GeForce RTX 3090, driver 580.159.03, Vulkan, wgpu 30**, over SSH.
Numbers below apply to this host and the stated synthetic scenes only; they are
not frame-rate promises.

## Capability table

| Capability | Status | Notes |
|---|---|---|
| Retained scene, per-instance model matrices, static GLB with material parts | Initial contract | Nonuniform scale, mirroring and node transforms applied once; no animation or skinning |
| Pinhole camera from FOV, K + pose, or Ti + C (`S^-1 Ti A(C)`) | Initial contract | The only camera in the initial wire profile |
| PBR (GGX/Burley), unlit, emissive, alpha mask, double-sided | Initial contract | One material path for all lights |
| glTF material extensions (clearcoat, sheen, specular, IOR, anisotropy, iridescence, texture transform) | Extension (#208) | Khronos definitions over the same path; area lights shade the base layer only. [glTF support](gltf-support.md); golden images in `server/renderer/tests/golden/` |
| Directional, point, spot lights | Initial contract | Radiometric units; no hidden key/fill/rim rig |
| Hard / PCF / PCSS shadows, directional cascades, point cubemaps | Initial contract | Per-light modes; shared sample and memory budgets with explicit errors |
| OpenEXR image-based lighting and visible sky | Initial contract | Unoccluded IBL (no sky visibility) |
| Single exposure/tone map, MSAA 1x/4x, diagnostic views, image composition | Initial contract | Composition keeps footage display-referred |
| Orthographic camera | Extension (renderer API) | Not in the wire profile |
| `radial-poly-native-px-v1` distortion + per-frame frame warp | Extension (renderer API) | `CameraModel::{Pinhole, Distorted}`; wire/calibration storage gated by #12/#36/#59 |
| Rectangle/disk/sphere area lights | Extension, labelled approximation | `ltc-plausible-v1`: per-emitter error <= 0.02 Le + 30%, area-only Lambert x (1 - F0) diffuse |
| Area-light shadows | Extension, labelled approximation | `area-shadow-pcss-v1`: centre cubemap + PCSS; penumbra ignores emitter outline and glossy weighting |
| Shadow catcher | Extension | Luminance ratio of shadowed/unshadowed light over footage; the catcher is the real ground and holds out geometry below it; no wire field |
| Ground decal (#113) | Extension | Unlit, premultiplied-alpha blend over opaque CG in instance order, before shadow catchers so CG shadows darken it; no depth write, no cast shadow; exact in linear light before the scene tone map; no wire field |
| Waving flag (#126) | Extension | SimpleWave closed-form surface on a shared 96x60-cell grid: centre line pinned, no pole or cloth solver; a compute pass evaluates it per frame from media time (phases reduced in f64) for the main and shadow passes; `simple` (fixed key light) or `pbr` look; optional wire `flag` on instances |
| Native flag (#219) | Extension, labelled approximation | Port of the AER WebGPU cloth (xiaoyxue/Simulation `ff9c934`): 65x41 PBD particles, top and bottom rows pinned, 10% slack, gusting wind relative to the flag, fixed 1/120 s steps with 36 Jacobi iterations; state persists per instance and follows the scene's clock (at most 8 steps a frame, 3 s warm-up when first drawn); a surface pass resamples it onto the flag grid for the same draws and shadows; visually tuned, no ground contact or self-collision |
| Point/spot PCSS source radius | Preview approximation | Soft-shadow source size, not finite-emitter shading |
| Physical atmosphere (#28), sky visibility (#29) | Optional (owner decision) | Not required for this acceptance |
| Brown-Conrady tangential distortion | Deferred | Native-pixel radial model selected instead |
| Wire mapping for area lights, distortion, catchers | Deferred | Requires approved protocol/calibration contracts; tracked programs carry an opt-in per-frame `lens` and `frame_warp` in `ti_c` geometry ([sdi simulation](sdi-simulation.md), [tracking worker](tracking-worker.md#lens)) |
| GLB animation/skinning, physical SDI, hardware encoder | Deferred | Outside this renderer roadmap |

## Acceptance evidence

| Area (renderer-design section 6) | Evidence |
|---|---|
| Cross-feature matrix, determinism, state isolation, budgets | `acceptance_gpu`: all five light families with Hard/PCF/PCSS, transformed/mirrored instances, material variants, pinhole/orthographic/distorted cameras; byte-identical repeats, unchanged after another scene, explicit budget and invalid-input errors with recovery |
| Coordinate preservation, camera equivalence | `camera`, `mesh_gpu`, `orthographic_gpu` |
| Model transforms, GLB | `scene`, `glb`, `mesh_gpu` |
| Waving flags | `scene::flag` unit tests (pinned centre, normals, bounds, phase wrap), `flag::tests` compute readback against the CPU reference, `flag_gpu` (media time, shadow redraw, simple and PBR looks), runtime `description` |
| Native flags | `scene::cloth` unit tests (rest arc, wind schedule matching the reference, direction, ranges), `flag_cloth::tests` (pinned rows exact, finite and within bounds under 18 m/s at either tension, wind direction, determinism, clock restart, warm-up and frame cap, surface sampling), `flag_gpu` (clock-driven motion, shadow redraw, pinned edge), runtime `description` |
| Distortion | `distortion_gpu` (identity, sub-0.3 px markers for both models, unwarped footage, non-invertible pixels, guard-band budget, output resizing), unit tests in `distortion`, CPU reference `distortion_reference` |
| Depth and composition | `composition_gpu`, `distortion_gpu`, `shadow_catcher_gpu`, `decal_gpu` |
| Direct lights, PBR | `pbr_gpu`, `texture_filtering_gpu` (anisotropic detail at grazing angles) |
| Area emitters | `area_light_gpu` (pinned candidate values, reference within profile, sidedness, size/power, trajectory continuity); evaluation in `area-light-evaluation.md` |
| Shadows, PCF, PCSS, cascades, cubemaps | `shadow_gpu`, `area_shadow_gpu`, `shadow_catcher_gpu` (spot near-plane regression) |
| Sky (unoccluded IBL) | `environment_gpu`, `exr_import` |
| Output and resources | `diagnostics_gpu`, `composition_gpu`, `sdi_gpu`, `triangle_gpu` |
| Bounded queues, exhaustion, lifetimes (runtime) | `program_output::explicit_profile_keeps_input_and_submission_bounded`, `retained_control::declared_capacity_and_queue_rejections_do_not_apply_effects`, `frame_resources` byte budget/exhaustion tests, `sdi::output_repeats_on_underrun_and_drops_when_full` |

## Measured budgets (1920 x 1080, RTX 3090)

Cross-feature matrix (`acceptance_gpu::cross_feature_matrix_frame_costs`): whole-frame
GPU time, median of seven, 1024 shadow maps, 16 + 16 PCSS samples.

| Configuration | Cold frame (maps rebuilt) | Warm frame |
|---|---:|---:|
| Pinhole | 5.8 ms | 5.7 ms |
| Orthographic | 7.9 ms | 7.8 ms |
| Composited over a plate, pinhole, 4x MSAA | - | 5.8 ms |
| Composited, distorted model, 4x MSAA | - | 6.4 ms |

Shadow passes per cold frame: 20 (directional 1, point 6, spot 1, two area lights
6 each); shadow memory 80 MiB; HDR target 15.8 MiB. The synthetic scene is small,
so shadow-pass cost will grow with real geometry.

Area lights (`area_shadow_gpu::area_light_frame_cost`): 1 rectangle 0.31 ms, or
1.5 ms with PCSS; 8 mixed shapes 13.0 ms, or 23.4 ms with PCSS. Curved emitters cost
about 2.5-3 ms each unshadowed. A local broadcast demo (private footage, not committed)
composited one 1080p frame with distortion, shadow catcher and 4x MSAA in 11.6 ms
including readback.

## trd reuse and corrections

Section 2 and section 7 of the design record the per-slice comparison. In summary:
trd's PBR/Disney shading informed the shared GGX/Burley path, but chr corrects the
missing receiver cosine and the arbitrary `1/(0.01 + d^2)` point falloff, replaces
blob shadows with geometric Hard/PCF/PCSS maps, replaces per-object tone mapping
with one output transform, and keeps chr's negative-Z-up world instead of trd's
+Y-up camera assumptions. Each correction has a synthetic regression in the tests above.

## Checks and unverified items

Passing on this host: `cargo fmt --check`, warning-denying workspace Clippy,
workspace CPU tests, the full renderer hardware suite (`cargo test -p chr-renderer
-- --ignored`) and `nix flake check`.

Unverified: Windows; native interactive UI (resize, minimize/restore, HiDPI,
Escape/close), which needs an interactive device rather than SSH; physical SDI
capture/output and hardware encoding; wire-level control of the renderer
extensions. No private reference assets are used as fixtures.

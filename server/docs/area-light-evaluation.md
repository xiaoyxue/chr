# Finite-emitter illumination method gate

Research date: **2026-11-12**. Scope: the unoccluded rectangle/disk/sphere part of
method selection #35 for #26, after the PBR prerequisite. **Candidate selection,
not owner approval or production implementation.** Area lighting stays low
priority. The renderer API, defaults, material shader and `scene_frame_v1` are
unchanged. Finite-emitter visibility belongs to #27: these results include
emitter self-visibility, but no scene shadows, centre-shadow substitute, or soft
shadow claim. Sky and atmosphere work remains deferred.

## Decision

Select **clipped LTC polygon integration** for the next owner-approved
renderer-native experiment: four vertices for rectangles, **64 vertices** for
disks and the **receiver-dependent tangent silhouette** of outward spheres.
The same GGX fit can integrate different angular domains; the geometry and
radiometry still require separate validation. Do not treat a sphere as a disk
of its physical radius placed at its centre.

Reject 16-sided curved emitters as the quality baseline: even the limited
non-grazing fixtures reach 10.09% disk and 5.73% sphere error. The 64-sided
versions reduce those maxima to 2.28% and 2.37%. Reject the current
Lambert-diffuse shortcut as a general replacement for chr's Burley material.
Grazing GGX/Fresnel fitting errors also remain material. **No production
profile passes the entire tested domain.** This is a concrete, bounded choice
of next experiment, not permission to implement or silently change #26.

## Primary sources and reuse

All links below were inspected on the research date; dates refer to publication,
not a claim that the technique is the newest.

| Source | Evidence / qualification |
|---|---|
| [Heitz, Dupuy, Hill, Neubelt, SIGGRAPH 2016](https://eheitzresearch.wordpress.com/415-2/) | LTC fits a cosine-weighted BRDF and integrates it over transformed polygons. Analytic integration of the fitted lobe is not exact GGX and does not provide visibility. |
| [Official reference, pinned revision `31e5e96b54f98f33098f8503003119ba2231a1c6`](https://github.com/selfshadow/ltc_code/tree/31e5e96b54f98f33098f8503003119ba2231a1c6) | `fit/brdf_ggx.h`, `fit/fitLTC.cpp`, `fit/export.h`, `fit/results/ltc.js`, and `webgl/shaders/ltc/ltc_quad.fs` establish the actual fit, packing and lookup. README explicitly warns that these tables differ from the original paper's parameterization. |
| [Heitz and Hill, Real-Time Line- and Disk-Light Shading, July 30, 2017](https://blog.selfshadow.com/publications/s2017-shading-course/) | Official disk shader transforms an ellipse, solves a cubic and uses a tabulated horizon-clipped sphere approximation. It is not simply rectangle corner evaluation. Not implemented here: the polygon path is smaller, shares clipping, and has directly measured discretization error. Consider the analytic extension only if the polygon cost becomes unacceptable; no accuracy/timing claim for that alternative here. |
| [Karis, Real Shading in Unreal Engine 4, SIGGRAPH 2013](https://blog.selfshadow.com/publications/s2013-shading-course/karis/s2013_pbs_epic_notes_v2.pdf) | Representative-point sphere lighting with lobe-width/energy correction is a real alternative, not an uncorrected point light. The notes acknowledge horizon/energy approximation and target spheres/tubes. Not selected for a common rectangle/disk/sphere integral; no local numeric comparison to this alternative is claimed. No code copied. |
| [Pharr, Jakob, Humphreys, PBRT 4e (2023), §6.2 sphere sampling](https://pbr-book.org/4ed/Shapes/Spheres#Sampling) | Uniform surface PDF, area/solid-angle Jacobian and sampling the sphere's subtended cone support an independent reference check. No PBRT code/assets copied. |

### Exact table permission

The pinned repository's [LICENSE](https://github.com/selfshadow/ltc_code/blob/31e5e96b54f98f33098f8503003119ba2231a1c6/LICENSE)
permits source/binary redistribution and modification, requires the paper
reference, and retention of copyright, conditions and disclaimer. It is a
BSD-style license **with an additional paper-citation condition**, not an
assumption that a downloadable LUT is public domain.

The complete notice is retained in
[`LICENSE.ltc`](../renderer/examples/area_light_evaluation/LICENSE.ltc).
The standalone binary exposes it with `--license`; distribute that notice and
this paper citation alongside any binary containing the data:

> Real-Time Polygonal-Light Shading with Linearly Transformed Cosines.
> Eric Heitz, Jonathan Dupuy, Stephen Hill and David Neubelt.
> ACM Transactions on Graphics (Proceedings of ACM SIGGRAPH 2016) 35(4), 2016.

Only the official 64×64 inverse-matrix and magnitude/Fresnel data are repacked:
six little-endian f32 values per texel, **98,304 bytes**. No new/ad hoc fit,
clipless table, third-party library or private code is vendored.
[`import_ltc.py`](../renderer/examples/area_light_evaluation/import_ltc.py)
pins the revision and verifies upstream SHA-256 before repacking:

```text
upstream ltc.js: 21071160163defd419b8f754ab604a8e60c44fcf539bfff9916e0e8e82316c1d
packed ltc.bin: 145b133d265bca7af234dd246c4612e541404831a265948c243bf5d418e88df4
```

### Existing renderer and local reference audit

chr's `mesh.wgsl::brdf` already returns **BRDF × receiver cosine**; the scalar
oracle follows its isotropic GGX, height-correlated Smith, Schlick F0, and
Burley diffuse equations. Effective roughness is clamped to `[0.045,1]`,
with `alpha = roughness²`. The audited official GGX fitter also uses
**height-correlated**, not separable Smith, and the same squared-roughness
mapping. Its lookup is `(roughness, sqrt(1-NdotV))`, with bilinear interpolation
on the 64×64 grid; the fitter's alpha floor is `1e-5`.

The remaining convention differences matter:

- The LTC shape is fitted to F=1 GGX; the official Schlick amplitude
  `F0*magnitude + (1-F0)*fresnel_term` uses that same shape for two differently
  weighted integrals. It is not pointwise Fresnel integration.
- The prototype diffuse term is `base*(1-metal)*(1-F0)*Lambert_integral`.
  chr instead integrates Burley times the **direction-dependent** `(1-F)`.
  This shortcut is measured, not declared to be the shared production material.
- Both reference and candidate are single-scattering GGX; neither adds
  multiple-scattering compensation. No anisotropy, sheen, clearcoat,
  textured emission or ellipsoids are qualified.
- We clip to the physical receiver hemisphere **before** the LTC transform
  and then to the transformed cosine hemisphere. This avoids below-surface
  contribution, but also truncates the fitted lobe. Horizon cases measure the
  resulting fit/truncation error; no clipless approximation is used.

The owner-authorized local `trd-core/src/shader/pbr.wgsl` audit found reusable
concepts (linear base-colour sampling, normal mapping, Disney/GGX/Schlick,
material factors), but only directional/point slots, not rectangle/disk/sphere
transport or LTC. Its broader Disney model has separable Smith, a different
roughness floor, and optional lobes; it is not the chr fitting target.
Its direct-light accumulation adds a BRDF without the receiver cosine.
A reproducible **source-level** discrepancy is the isolated white Burley
diffuse component at `N=V=(0,0,1)`, `L=(sqrt(3)/2,0,0.5)`, roughness 0.5:
`fd90=1.25`, diffuse BRDF=`(1+0.25/32)/pi`; correct unit-irradiance transport
is **half** that value. Below the horizon the correct contribution is zero,
not a positive BRDF evaluated with a clamped cosine. This is an algebraic
synthetic reproduction, not a trd GPU run. No private source was copied or
sent to public services. The arbitrary point falloff `1/(0.01+d²)` is also not
reused as area radiometry.

The earlier paired design's shadow benchmark measures depth/raster/tonemap and
visibility-only references, not this transport integral. Those historical
timings are not imported as LTC cost evidence; its named benchmark example is
absent from this base checkout.

## Experiment and reference

The standalone [`area_light_evaluation`](../renderer/examples/area_light_evaluation/main.rs)
uses synthetic data only. Coordinates are a **local shading frame** with N=+Z;
this is not a change to world negative-Z-up. Scalar radiance represents one
identical linear-RGB channel in W/(m² sr); positions/dimensions are metres.
All cases use base 0.7, metal 0 or 1, roughness 0.045/0.15/0.4/0.85.

There are **240 retained rows (80 per shape)**. Baseline centre is `(0,0,3)`,
rectangle half-extents `(1,0.6)`, disk/sphere radius 1, radiance 1,
NdotV=0.8. Cases include:

- axial; offset centre `(-2,0,3)`; grazing `(-3,0,3)`, NdotV=0.05;
- horizon crossing `(-3,0,0.2)`, planar Y tilt −0.5 rad, NdotV=0.05;
- near plane distance 0.05 / near sphere centre distance 1.02;
- reversed planar emission, explicitly two-sided planar emission; sphere pose
  is irrelevant and its reversed-pose row intentionally equals axial;
- 0.1× size; 2× size at fixed radiance and at fixed total flux;
- receiver inside sphere at centre distance 0.5 (outward contribution zero).

The midpoint quadrature uses **512² and 1024² uniform-area samples**, not a
production default. Rectangle samples are uniform in Cartesian coordinates,
disk radius is `R*sqrt(u)`, and sphere surface height is uniform in `[-R,R]`.
Each sample contributes
`Le * BRDF * max(N.L,0) * emitter_cos / (distance² * pdf_area * count)`,
with `pdf_area=1/A`. Planar emitter cosine is positive-part, or absolute value
only when explicitly two-sided. Sphere cosine is outward positive-part, which
rejects the far hemisphere rather than emitting through the body. Inside is
zero; an on-surface receiver or a coplanar planar receiver is rejected.
Nonfinite geometry/emission, negative radiance, nonpositive dimensions and a
two-sided sphere are rejected. Zero emission is tested.

`flux = pi*A*Le` for one-sided planes and outward spheres; two-sided planes
have twice that flux. Doubling both dimensions gives four times the flux at
fixed radiance; fixed total flux requires Le/4. It does **not** guarantee the
same near-field receiver brightness. Scaling all geometry/distances together
preserves fixed-radiance illumination. No arbitrary range cutoff is applied.

Independent checks include unit-albedo Lambert: axial disk output
`R²/(d²+R²)` (0.1 here), outward sphere `R²/d²` (1/9), rectangle surface versus
edge integral, and the unit-energy broad-disk limit. Sphere directions subtend
`sin(theta_max)=R/d`; axial disk uses `tan(theta_max)=R/d`.
Independent 512²/1024² solid-angle integration uses
`pdf_omega=1/[2*pi*(1-cos(theta_max))]`, checking the area Jacobian without
reusing the surface sampler. Cone results are unavailable for general tilted
disks/rectangles, explicitly marked as such.

Curved candidates use 16/64/256-sided polygons. A sphere's silhouette has
centre `C*(1-R²/d²)` and radius `R*sqrt(1-R²/d²)`. The oracle separately
integrates the fitted LTC density on the actual shape, allowing separation of
polygon discretization from fit/Fresnel/hemisphere truncation **where the
reference resolves that lobe**. The report retains individual diffuse/specular,
F=1 GGX, signed component differences, absolute error, relative error, and
error/Le. Relative error at exactly zero is `undefined`, never regularized
away with a denominator floor.

## Actual scalar findings

All-row 16-sided candidate/reference maximum absolute difference is **0.894813**
at the smooth horizon sphere. The largest relative difference is **29,611.4**
at the near smooth metal sphere (reference only `2.36418e-5`).
These are **not trustworthy LTC-error estimates**: 32 rows have
512²→1024² total-reference change above 0.001, with maximum **0.851928**.
Uniform surface sampling can miss narrow highlights or the very near visible
cap. Even a small absolute convergence delta is insufficient: the independent
cone check exposes missed energy. No failing/near-zero row is discarded.

For example, near metal disk/roughness 0.15 gives surface 1.226963 versus cone
0.699592 (cone delta `4.03e-8`), and LTC64 0.699753.
Near sphere/roughness 0.4/metal gives surface 0.091296 versus cone 0.662022
(cone delta `5.40e-7`) and LTC64 0.654851. The analytic Lambert near-sphere
value is 0.961169, whereas uniform surface 1024² gives only 0.338228.
These failures qualify the oracle's domain, not permission to hide them or to
call a 1024² result ground truth everywhere.

The declared **non-grazing/non-near subset** uses roughness ≥0.15 and excludes
grazing, horizon, near and inside cases by geometry, not by measured error:

| Shape / rows | Max absolute error, 16 / 64 | Max relative error, 16 / 64 | Max surface convergence delta |
|---|---:|---:|---:|
| Rectangle / 42 | 0.001607 / 0.001607 | 1.96% / 1.96% | 6.19e-6 |
| Disk / 42 | 0.008824 / 0.002822 | 10.09% / 2.28% | 5.33e-7 |
| Sphere / 36 | 0.011046 / 0.004619 | 5.73% / 2.37% | 6.60e-5 |

Absolute and relative maxima can occur on different rows. This subset has
NdotV=0.8 and three tested roughness values; it does not prove a continuous
parameter envelope. For disks, largest resolved 16-side specular shape loss is
0.007039, compared with max fitted-lobe/specular error 0.002449. Sphere values
are 0.006857 and 0.004189. Rectangle has no curved-polygon error; its small
polygon/reference residual is quadrature error.

Resolved **failing** grazing cases are equally important: rectangle roughness
0.15 has 60.86% total dielectric error (mostly diffuse mismatch) and 77.35%
metal error (fitted specular); disk metal reaches 80.76% with 16 sides and
80.55% with 64. Surface deltas there are below `2e-9`. Sphere horizon,
roughness 0.4, metal has LTC64 0.413416 versus surface 0.341435,
**21.08%** error, despite surface delta `2.09e-7` and cone agreement within
`8.60e-5`. More polygon sides do not repair BRDF fitting or material mismatch.

## Actual native GPU measurement

Linux, **RTX 3090 / NVIDIA 580.159.03 / Vulkan**, release build, wgpu 30.
Only the example requests `TIMESTAMP_QUERY`; timestamp period is 1 ns.
No ray-tracing or other optional shading feature is enabled. Two pass timestamps
bracket **one compute dispatch**, excluding uploads, pipeline compilation,
allocation, query resolve, CPU work and readback. One priming iteration precedes
seven retained samples per configuration. No window or service is created.

256×256 invocations, 8×8 workgroups, 1 or 8 fully evaluated lights.
The receiver grid spans approximately ±2 m in XY; view cosine varies
0.3–0.95 and roughness 0.15–0.85. It evaluates diffuse plus Schlick LTC,
not just an empty shader or a cached constant. A 9×9 pixel lattice checks all
four output channels against the f64 candidate every iteration (tolerance
`2e-4`; observed maximum **3.10e-7**). This validates the candidate port, **not**
agreement with exact PBR across that GPU grid.

| Shape / vertices | 1 light median ms | 8 lights median ms |
|---|---:|---:|
| Rectangle / 4 | 0.012704 | 0.061536 |
| Disk / 16 | 0.032832 | 0.216448 |
| Disk / 64 | 0.110432 | 0.834624 |
| Sphere / 16 | 0.032800 | 0.216352 |
| Sphere / 64 | 0.110752 | 0.834560 |

These initial measurements are retained in
`output/area-light-methods/gpu-initial.csv`; the final committed-source rerun
writes `gpu.csv` and is recorded separately in `verification.txt`. Timings
can vary on this shared host; neither run establishes an end-to-end budget.
An initial by-value 68-vertex-array shader measured 3.68–342.42 ms. One bounded
streaming-edge rewrite eliminated those arrays and was checked against the
same scalar candidate. The original source/timings remain in ignored
`candidate-array.wgsl` / `gpu-array.csv`; those slow timings are implementation
evidence, not a claim about LTC's inherent cost. Clipped horizon closure is
essential and is included in the final port.

Cost: one 64×64 **128 KiB storage LUT** (two vec4s per entry, padded from the
96 KiB file), four bilinear entry loads per invocation shared across lights,
no history, no depth maps, zero shadow passes. Polygon work is linear in vertex
count with normalization/atan2 edge integration and trigonometric vertex
generation; this is not the faster rational edge approximation in the reference
demo. A 1 MiB output plus 1 MiB readback buffer and small uniform/query buffers
belong to this harness, not a proposed production render target. Texture/f16
packing and raster/clustered integration costs remain unmeasured.
No general FPS, 1080p, Windows, native-UI, SDI or device acceptance is implied.

## Owner decision and remaining gates

**Recommended decision:** authorize a renderer-native *experimental* unoccluded
LTC path for rectangles and 64-sided disk/sphere silhouettes, retaining the
current shared GGX/Burley material and explicit radiance/shape/sidedness units.
Do not approve the current Lambert shortcut or freeze a production algorithm.
Before production, either fit/qualify the actual Burley×`(1-F)` diffuse lobe
and bound specular Fresnel/truncation error, or explicitly approve a separately
named approximation profile. A new fit would need reproducible licensed fitting
inputs/code and reference validation, not an invented LUT.

Provisional **next-experiment** envelope: uniform isotropic emitters, external
noncoplanar receivers, roughness 0.15–0.85, NdotV ≥0.3, centre distance/radius
≥1.5, and planar perpendicular separation ≥radius. Proposed acceptance:
absolute error ≤0.005×Le **and** relative error ≤3% when reference ≥1e-4×Le;
below that level retain absolute and raw relative errors. Reference deltas and
independent formulations must first establish adequate resolution. Proposed
GPU microbudget: ≤1 ms for this 256²/8-light workload, ≤128 KiB LUT, one shading
dispatch and no shadow views. **These budgets/envelopes need owner approval**;
the sparse scalar grid does not certify them. The 0.045 roughness floor,
grazing/near/on-emitter domains, arbitrary poses and full material maps need
additional qualification. Do not silently clamp or discard unsupported inputs.

Temporal noise is absent (deterministic integration, no history), but moving
light/receiver trajectories, LUT interpolation continuity and moving silhouette
faceting have not been accepted. No finite-area visibility method is selected.

## Reproduction and verification

From the workspace root, inside its Nix development environment:

```sh
cargo test --locked -p chr-renderer --example area_light_evaluation
cargo run --locked --release -p chr-renderer --example area_light_evaluation
NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- \
  cargo run --locked --release -p chr-renderer --example area_light_evaluation -- --gpu
python3 server/renderer/examples/area_light_evaluation/summarize.py
```

Six focused example tests cover analytic Lambert integrals, emitter sidedness
and sphere interior, flux/scale/zero emission, invalid dimensions and horizon
clipping, LTC density versus its polygon integral, and independent area versus
solid-angle integration. They are separate from workspace default tests and
the unrelated sky example. The exact final-source validation record is retained
in ignored `output/area-light-methods/verification.txt`, with scalar/GPU CSVs,
logs and provenance alongside it. No dependencies or lockfiles change.

# Distant-environment visibility: bounded method evaluation

Status: research evidence for the open raster-method gate #35, 2026-11-12.
**This is not owner approval, production method selection, or implementation of
#29.** Atmosphere #28 remains paused; dependencies and production APIs are
unchanged. CSM #23 is already implemented. Area lights/shadows, distortion #33
and production acceptance #12 are outside this slice.

## Recommendation

Do not adopt a fixed low-count set of shadowed environment directions, an
uncorrected shared visibility probe, or scalar AO as the full-sky solution.
Retain the explicitly unoccluded IBL path. Investigate **depth-aware,
position-corrected raster probes** next, subject to owner agreement on the
experimental envelope below. That is a recommendation for another experiment,
not evidence that depth-aware probes already meet it. Neither complete raster
candidate is qualified by this CPU experiment.

The distinction matters: bounded directional maps can query visibility at the
actual receiver, but their shared directions undersample glossy BRDFs and bright
HDR regions. Angular probes offer many directions with fewer scene captures,
but a visibility mask captured elsewhere does not represent the receiver's
visibility. Distance data and position-corrected traversal/interpolation are
necessary research, not a cosmetic improvement to a bent normal.

## Existing implementation and local reference audit

Reviewed `renderer-design.md` sections 4.3/4.5 and their paired translation,
`server/renderer/src/{environment.rs,environment_sampling.wgsl,mesh.wgsl,shadow.rs}`,
and the existing shadow benchmark documentation. Current diffuse IBL uses SH;
specular IBL uses GGX-prefiltered panoramas and the split-sum BRDF LUT. Authored
material AO scales these terms but is not scene environmental visibility.
Background visibility is independent of lighting; rotation/gain are late-bound.
This experiment does not call or modify those paths.

The locally authorized trd audit was restricted to the environment resource,
environment-map precomputation, PBR environment lookup, and grounding-shadow
shader (`crates/trd-core/src/render/{environment.rs,env_map.rs}` and
`crates/trd-core/src/shader/{pbr.wgsl,shadow.wgsl}`). Transferable mechanisms are
resource/state separation, consistent background/reflection orientation,
deterministic prefiltering and split-sum lookup. The inspected environment
lighting does not query scene geometry for sky visibility; the grounding blob
is not a directional occlusion representation. This is a scope observation,
not a claim of a reference defect. No private code, images, measurements or
assets were copied into the harness or sent to external services.

Existing directional/cubemap depth storage could support a future experiment,
but its current light allocation, fitting, cache keys and lighting accumulation
are not a reusable public arbitrary-ray visibility service. Exposing one, or
pretending environment samples are authored direct lights, would expand this
slice into production integration. The earlier shadow benchmark measures
different workloads; its GPU milliseconds cannot be reused as sky timings.

## Primary sources and candidate boundary

Public sources verified on 2026-11-12:

1. [Debevec, *A Median Cut Algorithm for Light Probe Sampling*, SIGGRAPH
   poster, 2005](https://vgl.ict.usc.edu/Research/MedianCut/).
   Reduces a radiance distribution to representative light sources. Useful
   precedent for radiance-aware bounded directional maps, not a guarantee of
   glossy or visibility accuracy at 64 lights. Our sampler is **not** median cut.
2. [McGuire et al., *Real-Time Global Illumination using Precomputed Light
   Field Probes*, I3D, 2017](https://research.nvidia.com/publication/2017-02_real-time-global-illumination-using-precomputed-light-field-probes).
   Adds per-texel visibility information and world-space ray queries to probes.
   The published work targets static environments and includes full radiance;
   here only distant-environment visibility is considered. Its performance and
   GI claims do not transfer to a new wgpu implementation.
3. [Majercik et al., *Dynamic Diffuse Global Illumination with Ray-Traced
   Irradiance Fields*, JCGT, 2019](https://research.nvidia.com/publication/2019-05_dynamic-diffuse-global-illumination-ray-traced-irradiance-fields).
   Visibility-aware moment interpolation helps prevent probe light leaking.
   Irradiance is diffuse; the paper uses a separate glossy ray-tracing term.
   A raster capture variant would be new engineering, not a portable drop-in
   DDGI port, and irradiance alone cannot satisfy the specular requirement.
4. [Pharr and Green, *Ambient Occlusion*, GPU Gems chapter 17,
   2004](https://developer.nvidia.com/gpugems/gpugems/part-iii-materials/chapter-17-ambient-occlusion).
   Explicitly describes a diffuse approximation based on accessibility and
   average unoccluded direction. A scalar and bent normal cannot encode
   disconnected openings or the correlation of a narrow reflection lobe with
   directional blockers. Screen depth further omits hidden/offscreen geometry;
   no screen-space filter can reconstruct absent information reliably.
5. [PBRT 4e, *Improving Efficiency*, 2023](https://pbr-book.org/4ed/Monte_Carlo_Integration/Improving_Efficiency).
   Stratification/importance weighting guide the integration below. Fixed
   deterministic quadrature is reproducible, not an unbiased random estimator
   or a proof of error bounds for arbitrary scenes.
6. [wgpu 30 optional features](https://docs.rs/wgpu/30.0.0/wgpu/struct.Features.html).
   A future raster version needs depth attachments, sampled 2D-array textures,
   uniform/storage buffers and bounded shader loops, not hardware RT. Cube
   faces can use six ordinary passes; multiview, bindless resources, float
   filtering and conservative rasterization must not be assumed. Optional GPU
   timestamps must be checked and requested explicitly.

No source implementation, LUT, supplemental code or licensed image is reused.
This is original synthetic Rust with existing dependencies only. The light-field
paper's publication page carries ACM copyright terms; an accompanying code
download is not a blanket license to port it. Any future code/LUT reuse needs a
separate license review. No NVIDIA/engine SDK or RT extension is introduced.

## Reproducible CPU experiment

From the workspace root, inside the existing Nix development shell:

```sh
mkdir -p output/sky-visibility
cargo test --locked -p chr-renderer --example sky_visibility_evaluation
cargo run --locked --release -p chr-renderer --example sky_visibility_evaluation \
  > output/sky-visibility/results.csv
```

The standalone example imports no renderer API and accepts no owner assets.
It outputs scene-linear scalar radiance, not tone-mapped screenshots. Eight
scenes cover open sky, a closed enclosure, a roof, three blockers, a blocker
outside the central view, a 3 cm strip at 40 cm height, a grazing receiver and a
grazing view. Geometry is two-sided opaque analytic axis-aligned rectangles in
metres, with world up -Z. Alpha masks, self-intersection bias and raster texel
coverage are deliberately absent.

Five deterministic states move the receiver along X from -0.5 to +0.5 m and
change its outgoing view direction. This tests view-dependent glossy response
and probe parallax, not rendered pixel reprojection or temporal denoising.
At the central offscreen state, an eye at `(0,0,-4)` looking along +Z with 40°
horizontal FOV cannot see the blocker at X=2..3, Z=-1: the frustum half-width
there is `3 tan(20°) < 2`. The blocker is still part of the visibility oracle.
The central-state receiver normal bisects the view direction and direction
toward the blocker, so its glossy reflection is affected despite the blocker
being outside the view.
The `missing_blocker` control deliberately omits it; only the central state is
proved offscreen, not the entire motion sequence.

Two analytic skies are used: uniform unit radiance and
`0.05 + 40 exp(80 (dot(w, normalize(0.3,0,-1)) - 1))`. The latter is a smooth
HDR cap, not an EXR loader test, solar model, or proof for arbitrarily tiny
high-energy texels. Lobes are unit Lambertian diffuse and single-scatter GGX
at perceptual roughness 0.5/0.2, alpha=roughness², F0=0.04, Schlick Fresnel and
height-correlated Smith masking. These are isolated kernels, not a pixel match
to the production Burley/SH/split-sum approximation.

The target is exclusively

```text
Lo(p,v) = integral_sphere L(w) f(n,v,w) max(n.w,0) V(p,w) dω
```

Blocked environment contributes zero, not local reflected radiance. There is
no interreflection, emissive geometry, GI, transmission or volumetric transport.
All directions on the sphere are represented; no implicit empty lower hemisphere
or horizon cutoff is allowed.

The reference uses midpoint theta/phi quadrature at 256×512 and 512×1024.
Each ring cell has **exact** solid angle
`(cos(theta_lo)-cos(theta_hi)) * 2π/nphi`; using equal lat-long pixel weights
would be wrong. Analytic ray/rectangle intersection supplies visibility. The
run fails if the maximum coarse/fine difference divided by fine unoccluded
radiance exceeds 1%. This convergence check is empirical, not a guaranteed
upper bound. High-sample integration is an offline oracle only.

| Reported method | What is actually evaluated |
|---|---|
| `uniform32/128` | Shared deterministic full-sphere Fibonacci directions, exact visibility, weight 4π/N; idealizes directional maps without raster error |
| `env32/128` | Shared deterministic marginal/conditional inverse-CDF samples of a 64×128 equal-solid-angle sky grid, using a 50:50 uniform/radiance distribution |
| `local_mask` | 32×64 equal-solid-angle binary visibility mask captured at each receiver; fine integration isolates angular representation error |
| `shared_mask` | Same mask captured only at the origin and queried without position correction; includes parallax bias |
| `scalar_ao` | Exact uniform-sky cosine-weighted accessibility multiplied by each lobe's unoccluded radiance; deliberately tests the inappropriate factorization |
| `missing_blocker` | Unoccluded result when the offscreen scene's only blocker is absent |

For `env` cell probability mass `q_j`, the sample weight is
`Δω_j/(N q_j)`, including both mixture components. The marginal/conditional
selection probability is their product, `q_j`; it is not an area PDF or just
inverse luminance. Midpoint sampling within a cell adds discretization bias;
there is no claim of continuous importance-sampling unbiasedness. The reference
uses a different angular grid, rather than sharing this discretization.

CSV `normalized_error = abs(estimate-reference)/unoccluded` uses the fine
oracle's **unoccluded** value for that state/lobe/sky. It is not relative error
against a near-black occluded value. Both absolute radiances are retained.
Errors can exceed 100% and are never clamped. Each method/lobe has 80 states
(eight scenes × two skies × five frames), except the missing-data control.
This measures actual integral error, not similarity of unrelated images.

Probe fine-integration sample counts are intentionally far above a production
budget. These columns isolate representation bias; they are not an equal-cost
comparison with the directional estimators or implementations of light-field
probes/DDGI. No CPU wall time is presented as GPU time.

### Measured results

The Linux release run produced 1,710 finite CSV rows. Maximum reference
coarse/fine disagreement was **0.4994%** of unoccluded radiance, below the 1%
oracle gate. Maximum normalized errors over all fixture states:

| Method | Diffuse | GGX roughness 0.5 | GGX roughness 0.2 |
|---|---:|---:|---:|
| Uniform 32 | 361.30% | 449.02% | 1696.99% |
| Uniform 128 | 34.57% | 57.55% | 152.47% |
| Environment-weighted 32 | 19.33% | 67.82% | 1760.40% |
| Environment-weighted 128 | 9.24% | 60.96% | 353.10% |
| Local 32×64 mask, fine integration | 15.10% | 13.47% | 36.17% |
| Shared 32×64 mask, fine integration | 59.13% | 48.84% | 93.11% |
| Scalar AO | 67.49% | 65.10% | 83.55% |
| Missing offscreen blocker (only that scene) | 4.75% | 11.68% | 66.16% |

These are observed counterexamples, not lower bounds on the best possible
implementation of each family. Radiance-only sampling helps diffuse on average
(environment-weighted 128 mean error 1.46%) but does not importance-sample each
receiver's narrow BRDF or its visibility boundaries. The large errors are
real overestimates from sparse quadrature, not clipped display values.
BRDF importance sampling/MIS was not evaluated: receiver-dependent directions
fit a queryable depth-probe approach more naturally than one shared fixed
shadow-map constellation, but require their own PDF and quality validation.
No measured method meets the provisional all-state 5% diffuse/10% glossy screen.

The fine-integrated **local** mask's 36.17% sharp-gloss error on the central thin
strip isolates angular discretization without parallax. Shared-mask error rises
to 93.11% on the multiple-blocker scene at the displaced receiver, despite the
same fine integration. Increasing shader samples alone cannot repair either
representation error. The absent offscreen blocker's 66.16% sharp-gloss error
occurs in the proved-offscreen central uniform-sky state; it is not merely an
offscreen diffuse-AO argument. Scalar AO's 67.49% diffuse error under the HDR sky
also shows that factoring an unweighted accessibility out of radiance is wrong.

For motion, the maximum consecutive change in signed normalized residual
`(estimate-reference)/unoccluded` is 4.411 for environment-weighted 128, 0.370
for the local mask and 0.757 for the shared mask. Physical brightness changes
are removed by subtracting each state's reference first. This is a five-state
sampling/parallax stress measure, **not** screen-space temporal flicker in a
rendered sequence or a bound on a denoised method. Determinism is not smoothness.

Six focused tests cover solid-angle/Lambertian normalization (including a tilted
receiver), ray/rectangle sidedness and thin geometry, GGX reciprocity/grazing
finiteness, enclosure and offscreen diffuse/glossy blocking, mask parallax and
AO/specular disagreement, and importance weights with deterministic replay.
The high-sample main run separately enforces oracle convergence.

Linux validation passed `cargo fmt --all -- --check`, workspace/all-target
Clippy with `-D warnings`, the six example tests, `nix flake check` and
`nix build`. The Nix workspace suite passed all **124 CPU tests**, including the
viewer test. Direct `cargo test --locked --workspace` with a checkout-local
`TMPDIR` hit the existing viewer test's outside-checkout temporary-directory
assumption; it was not skipped or changed. The Nix sandbox run covers that test
successfully. The 26 renderer and three runtime hardware tests remain their
existing ignored tests, not hardware acceptance for this slice. Windows,
interactive UI and GPU sky-candidate performance are unverified.

## Provisional resource and quality envelope

These are **evaluation assumptions, not approved defaults or promised results**:

* Quality screen: maximum normalized error ≤5% for diffuse and ≤10% for both
  tested GGX lobes; oracle convergence ≤1%. Check every state, not just mean
  error or the open-sky case. Sharper lobes and harder HDR caps remain a separate
  unqualified domain.
* Directional map cost accounting: N depth views on invalidation, N logical
  comparisons per receiver for Hard visibility, or N×K for K-tap PCF.
  At 512² Depth32Float, 32/128 directions are 32/128 MiB of depth payload and
  32/128 passes; cascades multiply these. Even perfect map lookup does not cure
  directional integration error. Do not silently ship these as realtime defaults.
* Possible next probe experiment: at most eight 64² six-face depth captures,
  0.75 MiB depth payload, ≤6 face updates/frame and ≤16 MiB scratch/derived
  visibility data. Full invalidation needs 48 face passes (eight frames under
  that update cap). Explicitly measure or reject the latency for moving scenes.
* Trial shading cap: 8 diffuse + 8 glossy directions, at most two probe
  candidates and four depth steps per direction = 128 logical depth lookups
  per shading point. This cap is **unvalidated** and may be inadequate for
  thin geometry. Do not use the fine-integration mask numbers to claim otherwise.
* Trial GPU target: incremental sky visibility ≤2 ms at 1920×1080 on the
  designated RTX 3090/Vulkan host, separately reporting update and shading
  costs, cold/warm distributions and adapter/features. **No GPU candidate,
  GPU timestamp measurement, or claim of meeting this target exists here.**
  Texel payloads exclude allocation padding, scene buffers and existing IBL.

Depth-aware probes must distinguish blocked, visible and unknown coverage.
Depth reconstruction must use consistent world distances, not treat nonlinear
hardware depth as a distance or mix cubemap face depth conventions.
Depth discontinuities, missing layers and finite traversal caps can still leak.
Parallax correction cannot recover geometry absent from all probes; adding
probes or layers increases storage, captures and query cost. Thin occluders may
vanish under rasterization; conservative coverage is an optional approximation,
not a universal fix. Bias at grazing receivers trades acne for detached shadows.
Interpolating moments or filtering visibility can smear boundaries; evaluate
those errors separately from BRDF/radiance integration.

Geometry/model/alpha/sidedness revisions invalidate affected visibility data;
probe movement invalidates its capture. A radiance-source change need not
invalidate pure visibility, but invalidates radiance-weighted direction choices
and any preintegrated products. BRDF/view changes invalidate view-dependent
shading/history. Deterministic replay requires fixed sample ordering and explicit
history reset. Update throttling must report stale/unknown data rather than
silently return full visibility. No temporal reuse is implemented in this slice.

Reject SSAO/contact blobs, AO/bent-normal-only, uncorrected visibility masks and
diffuse-only DDGI as the sole answer to the diffuse+specular requirement.
Screen-space information may supplement a world-space method, never replace
its missing-data policy. Do not adopt virtualized shadows, full light-field GI
or an engine framework merely to resolve this limited visibility question.

Before any #29 implementation, the owner must decide whether to authorize the
next depth-aware probe experiment and its scene/memory/update/quality envelope,
including the unknown/stale-data behavior and glossy cutoff. Method selection
still requires actual raster quality and GPU timing evidence. Atmosphere can
remain a future scene-linear directional-radiance adapter, with consistent
rotation and explicit sun double-counting policy; no scattering/LUT work or
resumption of #28 is implied.

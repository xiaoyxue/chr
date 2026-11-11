# Brown-Conrady CPU reference

**Status:** implemented synthetic CPU reference for #33, not an approved public
camera model or a production/GPU warp. The initial wire profile remains pinhole.

The paired translation is [distortion-reference.zh-CN.md](distortion-reference.zh-CN.md).
The implementation is private to
`server/renderer/tests/distortion_reference.rs`; it adds no crate exports,
runtime controls, dependency or calibration-file interpretation.

## Run

From the feature checkout root, using the root development shell:

```sh
nix develop
cargo test --locked -p chr-renderer --test distortion_reference -- --nocapture
```

The tests print the maximum pixel round-trip error and iteration count for 585
synthetic samples. No GPU, window, SDI signal or private reference data is used.
CPU success is not evidence of GPU/compositor or real-calibration acceptance.

## Model and coordinate order

Use exactly three radial coefficients `(k1,k2,k3)` and two tangential
coefficients `(p1,p2)`. This is the five-coefficient polynomial Brown-Conrady
model, not the rational, thin-prism, tilted-sensor, fisheye or orthographic model.
The private Rust arrays separate radial/tangential coefficients; they are not
an OpenCV wire-vector parser. OpenCV's five-value order is
`(k1,k2,p1,p2,k3)`.

For an ideal native pixel `(u,v)`, with the existing chr intrinsic skew:

```text
y = (v - cy) / fy
x = (u - cx - skew*y) / fx
r2 = x*x + y*y
s = 1 + k1*r2 + k2*r2*r2 + k3*r2*r2*r2
xd = x*s + 2*p1*x*y + p2*(r2 + 2*x*x)
yd = y*s + p1*(r2 + 2*y*y) + 2*p2*x*y
ud = fx*xd + skew*yd + cx
vd = fy*yd + cy
```

The lens coordinates `(x,y)` are **K-normalized, not SNC**. The world pipeline
is original forward `Ti * A(C)` -> SNC -> native pixels -> K-normalized lens
mapping -> distorted native pixels. A regression uses an elevated world point,
negative-Z camera height and a hand-calculated pixel result to catch applying
the distortion polynomial directly to SNC.

Keep the calibrated native extent and K together. Output fitting is a separate
later operation; it does not justify silently resizing K or rewriting imported
Ti. Pixel centres have no added half-pixel offset. Applying pixel displacements
preserves zero-distortion pixels exactly, including borders, without a special
identity fallback.

## Conservative calibrated domain

The reference accepts a disk `r <= R` in ideal K-normalized coordinates only
when the following lower bound is greater than `1e-6`:

```text
mu = 1
     - 3*max(-k1,0)*R^2
     - 5*max(-k2,0)*R^4
     - 7*max(-k3,0)*R^6
     - 6*hypot(p1,p2)*R
```

The distortion Jacobian is symmetric. Each positive radial term contributes
a positive-semidefinite matrix; a negative degree-`2n` term loses at most
`(2n+1)*abs(kn)*R^(2n)` in its smallest eigenvalue. Writing the tangential
vector as `t=(p2,p1)`, its Jacobian has eigenvalues
`4*dot(t,q) +/- 2*|t|*|q|`, so its minimum is bounded below by `-6*|t|*R`.
Therefore `J >= mu*I` throughout the convex disk in real arithmetic.
Integrating along the segment between two points establishes strong
monotonicity and uniqueness of any inverse inside that disk.

The `1e-6` margin is a conservative evaluation restriction, not a production
calibration threshold or an interval-arithmetic proof. Nonfinite arithmetic
is rejected. Failure of this sufficient check means **uncertified here**,
not proof that every such calibration is invalid. In particular this is not
a general detector or repair method for folded/non-bijective calibrations.

## Bounded inverse and coverage

The inverse uses an analytic 2x2 Jacobian and Newton steps with residual-decreasing
backtracking. Initial guesses outside the ideal disk are projected into it;
that projection is only a starting guess, never a successful clamped result.
Every accepted iterate stays in the calibrated disk.

Evaluation settings explicitly supply 1..64 Newton steps, 1..32 backtracking
trials per step and a positive finite normalized residual tolerance. Synthetic
checks use 32 steps, 16 trials and `1e-12`; these are not runtime defaults or
a universal pixel-accuracy guarantee.

Success requires residual convergence. Budget exhaustion, a rejected step or
numerically unusable Jacobian produces `NoConvergence`, not identity geometry.
Uniqueness on the disk does not guarantee Newton convergence from every start;
failure also does not prove that a mathematical inverse cannot exist.

After solving, source pixel centres must lie in `[0,width-1] x [0,height-1]`.
An inverse outside that rectangle is `OutsideImage`; an ideal query outside the
calibrated disk is `OutsideDomain`. No source coordinate is stretched/clamped
to a border, and no color/alpha is fabricated for a failed sample. In production,
guard bands, filter footprints, uncovered-pixel policy and output fitting
require explicit design and GPU tests.

## Evidence and remaining gates

The suite covers independently calculated forward coordinates, an analytic
Jacobian checked against central differences, zero identity including odd and
portrait extents, radial/tangential/mixed round trips with skew, a valid inverse
whose distorted coordinate lies outside the ideal disk, rejected domains,
nonconvergence, source-image coverage and Ti/C-to-native-pixel ordering.

Before public implementation, #33 still needs approval of model/domain/K/extent,
ideal-versus-distorted Ti provenance and schema evolution. GPU inverse-map
sampling, consistent premultiplied color/alpha/depth handling, guard-band
coverage and avoiding double-warp of already-distorted footage remain unimplemented.
This reference does not undistort real calibration records, alter the shared
compositor or establish real-time performance.

## Primary reference

The normalized projection equations and model distinctions follow OpenCV's
public calibration documentation, pinned here to the
[4.12.0 calibration header documentation](https://github.com/opencv/opencv/blob/4.12.0/modules/calib3d/include/opencv2/calib3d.hpp).
That documentation also warns that calibration fitting does not enforce the
required monotonic/bijective behavior. This reference's solver, domain bound
and synthetic tests are independently implemented; no OpenCV source, fitting
data or private `trd` assets are copied.

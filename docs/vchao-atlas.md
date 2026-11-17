# vchao atlas, V, Ti and T_ref

**Status:** `draft` — an explanation of vchao's geometry for chr readers. vchao's
own documents are authoritative; this page follows the pinned submodule
`external/vchao` (`f209fc9`).

**Scope:** what "atlas" means in vchao, the three products the word is used for,
and the formulas that connect K, R, C, H, V, q, T_ref and Ti. It does not define
a chr protocol. The paired translation is [vchao-atlas.zh-CN.md](vchao-atlas.zh-CN.md).

Sources in `external/vchao`: `README.md` (`Atlas`), `docs/camera-geometry.md`
(the preamble before the contents, and sections 3, 6, 8 and 9), `projective-atlas/README.md` and
`atlas-vq-estimate/README.md`.

## Three things called "atlas"

| | vchao name | Reference coordinates | Contents | Pixels | State |
|---|---|---|---|---|---|
| ① | **`Atlas`** (canonical) | The station's **physical** directions (calibrated) | Per camera station: square charts, 12 headings x 3 pitches, `K=I`, 90° FOV; chart rotations, static reference imagery, coverage | Yes | Design only, not implemented |
| ② | Uncalibrated projective panorama (output of `projective-atlas`) | **An arbitrary chosen frame**, displayed as if `K=I` | The same 36-chart layout filled with the per-channel temporal median of the registered frames' RGB | Yes | Implemented; for display and evaluation |
| ③ | Anchored **V/Q map** (the manifest read by `atlas-vq-estimate`) | The reference plane chosen when the map was built | Landmarks q with descriptors, each view's V, and T_ref | **No** | Implemented; **this is what live Ti estimation uses** |

- In vchao, `Atlas` strictly means ①. vchao calls the panorama ② distinct from
  the canonical Atlas. The V/Q map ③ is a separate projective, uncalibrated
  product, not the canonical Atlas either. ② and ③ use the same kind of
  reference-plane registration (section 3 below), but not necessarily the same
  V: the V/Q map keeps its own fitted V and q, which must stay together.
- "We need an atlas to estimate Ti live" refers to the V/Q map **③**.
- chr's business documents ([calibration records](calibration-control.md),
  [system architecture](system-architecture.md)) use "atlas" loosely for
  "reference images, each with its pitch Ti, plus C". That is closest to the V/Q
  map's `views` table (frames with V and an optional Ti) and is not vchao's
  canonical `Atlas`.

## Formulas

Matrices act on homogeneous column vectors. Image coordinates are SNC
([coordinate contract](../server/docs/coordinates.md)).

### 1. One frame's camera

K is the intrinsics, R the world-to-camera rotation and C the optical centre in
pitch metres.

```text
D_i  = R_iᵀ K_i⁻¹                         image → world direction
P_i  = K_i R_i [I | −C_i]                  world 3D → image
Ti_i = P_i E_0 = K_i R_i [e1 e2 −C_i]      pitch ground (X, Y) → image
A_i,h = P_i E_h = D_i⁻¹ [e1 e2 (h·e3 − C_i)]   horizontal plane at height h → image
```

`E_h` lifts plane coordinates `(X, Y, 1)` to `(X, Y, h, 1)`. A point above the
ground needs C; Ti alone covers only `h = 0`.

### 2. Fixed optical centre: frames differ by a homography

```text
H[j←i] = D_j⁻¹ D_i = K_j R_j R_iᵀ K_i⁻¹
```

C cancels. Every frame of a camera that only pans, tilts and zooms is related
to every other frame by a pure H.

### 3. The reference plane and V

Choose a reference r. The reference plane (the "panorama plane") is an abstract
projective plane (RP²), not a top-down view and not 3D. When r is a real frame:

```text
V_i = X_i = H[i←r] = K_i R_i R_rᵀ K_r⁻¹    reference plane → frame i
U_i = V_i⁻¹                                  frame i → reference plane
H[j←i] = V_j V_i⁻¹
```

vchao's graph code calls the node `X_i` and the panorama map `U_i`; the V/Q map
calls it V. Without calibration only `V_i ~ D_i⁻¹ A` is known, with A unknown
(the gauge), and right-multiplying every `V_i` by the same matrix M leaves all H
unchanged.

### 4. Landmarks q (part of the V/Q map ③)

```text
x_ik ~ V_i q_k      where landmark k appears in frame i
```

The map stores fitted V and q produced outside vchao; vchao does not document
how they are built. Conceptually, building it means fitting all V_i and q_k so
that every observation satisfies the relation above. q is a homogeneous point on the reference plane, not a metric 3D point; its
third component may be zero.

### 5. Anchoring to the pitch

With one anchor frame a whose Ti is known:

```text
T_ref = V_a⁻¹ Ti_a = K_r R_r [e1 e2 −C]    the reference plane's own Ti, a constant
Ti_i  = V_i T_ref
```

Check: `V_i T_ref = K_i R_i R_rᵀ K_r⁻¹ · K_r R_r [e1 e2 −C] = K_i R_i [e1 e2 −C] = Ti_i`.
In general (`V_i ~ D_i⁻¹ A`), `T_ref ~ A⁻¹ [e1 e2 −C]`, and the same check holds
because A cancels. The anchor fixes T_ref. A then follows as
`A ~ [e1 e2 −C] T_ref⁻¹` once C is known (`C_z ≠ 0`); without C, the anchor
alone does not determine A.

### 6. Live estimation (`atlas-vq-estimate`)

```text
new frame → ALIKED features → match against the V/Q map's landmarks q → (x, q) pairs
          → RANSAC, then damped Gauss-Newton on a Huber loss for V_query only (8 DOF);
            all other V and q stay fixed
          → Ti_query = V_query · T_ref
```

It outputs neither C nor K nor R. 3D objects also need C, which comes from a
separate calibration (for example `C_world_metres` in a calibration package).

### 7. Canonical Atlas ①: replace the unknown A with physical directions

```text
S_k     = station k's reference rotation: z towards the pitch centre, gravity up
Q_a     = rotation of chart a relative to the station
D_a     = S_k Q_a                       (K_a = I)
H[a←i]  = D_a⁻¹ D_i = Q_aᵀ S_kᵀ D_i     frame → chart
P_a E_0 = the pitch's projection on the chart; fixed, independent of the frame's pan, tilt and zoom
```

From the panorama ② or the V/Q map ③ to the Atlas ①: solve for the unknown A
(calibration), then `D_i = canon(A · V_i⁻¹)`. The projective product becomes
the canonical Atlas, with physical angles and the real optical centre.

## Summary

- V, q and T_ref live on one reference plane; this reference-plane registration
  is the common idea behind all three products.
- Keep only q, descriptors, V and T_ref: that is the V/Q map ③, used to solve Ti live.
- Warp each frame's pixels back with V: that is the panorama ②, used to inspect and evaluate
  the registration.
- Calibrate the reference plane to the station's physical directions: that is
  ①, the canonical `Atlas`, not yet implemented.

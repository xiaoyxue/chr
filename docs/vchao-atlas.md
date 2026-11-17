# vchao atlas, V, Ti and T_ref

**Status:** `draft` — an explanation of vchao's geometry for chr readers. vchao's
own documents are authoritative; this page follows the pinned submodule
`external/vchao` (`f209fc9`).

**Scope:** what "atlas" means in vchao, the three products the word is used for,
and a formal statement of the geometry that connects $K$, $R$, $C$, $H$, $V$,
$q$, $T_{\mathrm{ref}}$ and $T_i$ (vchao's `Ti`). It does not define a chr
protocol. The paired translation is [vchao-atlas.zh-CN.md](vchao-atlas.zh-CN.md).

Sources in `external/vchao`: `README.md` (`Atlas`), `docs/camera-geometry.md`
(the preamble before the contents, and sections 3, 6, 8 and 9),
`projective-atlas/README.md` and `atlas-vq-estimate/README.md`.

## Three things called "atlas"

| | vchao name | Reference coordinates | Contents | Pixels | State |
|---|---|---|---|---|---|
| ① | **`Atlas`** (canonical) | The station's **physical** directions (calibrated) | Per camera station: square charts, 12 headings x 3 pitches, K = I, 90° FOV; chart rotations, static reference imagery, coverage | Yes | Design only, not implemented |
| ② | Uncalibrated projective panorama (output of `projective-atlas`) | **An arbitrary chosen frame**, displayed as if K = I | The same 36-chart layout filled with the per-channel temporal median of the registered frames' RGB | Yes | Implemented; for display and evaluation |
| ③ | Anchored **V/Q map** (the manifest read by `atlas-vq-estimate`) | The reference plane chosen when the map was built | Landmarks q with descriptors, each view's V, and T_ref | **No** | Implemented; **this is what live Ti estimation uses** |

- In vchao, `Atlas` strictly means ①. vchao calls the panorama ② distinct from
  the canonical Atlas. The V/Q map ③ is a separate projective, uncalibrated
  product, not the canonical Atlas either. ② and ③ are both registrations in the
  sense of Definition 3 below, but not necessarily the same one: the V/Q map
  keeps its own fitted $V$ and $q$, which must stay together.
- "We need an atlas to estimate Ti live" refers to the V/Q map **③**.
- chr's business documents ([calibration records](calibration-control.md),
  [system architecture](system-architecture.md)) use "atlas" loosely for
  "reference images, each with its pitch Ti, plus C". That is closest to the V/Q
  map's `views` table (frames with $V$ and an optional $T_i$) and is not vchao's
  canonical `Atlas`.

## Formal statement

### 0. Notation

- $\mathbb{P}^2 = (\mathbb{R}^3 \setminus \lbrace 0 \rbrace)/\mathbb{R}^\times$
  is the real projective plane. For $x, y \in \mathbb{R}^3 \setminus \lbrace 0 \rbrace$,
  $x \sim y$ means $x = \lambda y$ for some $\lambda \neq 0$; $[x]$ is the class of $x$.
- $\mathrm{PGL}(3) = \mathrm{GL}(3)/\mathbb{R}^\times$ acts on $\mathbb{P}^2$ by
  $[x] \mapsto [Mx]$ (column vectors). Because $3$ is odd,
  $\mathrm{SL}(3) \to \mathrm{PGL}(3)$ is a bijection, and
  $N(M) = M / \sqrt[3]{\det M}$ (real cube root) is the determinant-one
  representative vchao stores (vchao's `N(H)`; `atlas-vq-estimate` calls it
  `canon`, which is not the $K_{33} = 1$ camera gauge `canon` of
  `camera-geometry.md`).
- The world is $W = \mathbb{R}^3$ in pitch metres, with ground plane
  $\Pi_0 = \lbrace Z = 0 \rbrace$ and standard basis $e_1, e_2, e_3$
  (up is $-e_3$, see the [coordinate contract](../server/docs/coordinates.md)).
- Each frame $i$ has an image plane $\mathcal{I}_i \cong \mathbb{P}^2$ in
  undistorted SNC coordinates. Lens distortion $\psi_i$ acts after projection
  and is omitted below.
- $\varphi(x) = (x_1/x_3,\ x_2/x_3)$ is dehomogenisation.
- A frame of $W_f \times H_f$ native pixels has $s_f = \sqrt{W_f H_f}$, and
  $\Lambda_f$ maps native pixel coordinates $(u, v, 1)^\top$ to SNC:

$$
\Lambda_f = \frac{1}{s_f} \begin{bmatrix} 2 & 0 & -W_f \\ 0 & 2 & -H_f \\ 0 & 0 & s_f \end{bmatrix}.
$$

- For $h \in \mathbb{R}$ and $C \in \mathbb{R}^3$,

$$
G_h(C) = \begin{bmatrix} e_1 & e_2 & h\,e_3 - C \end{bmatrix} \in \mathbb{R}^{3\times 3},
\qquad \det G_h(C) = h - C_z .
$$

### 1. Cameras

**Definition 1 (pinhole camera).** Frame $i$ has intrinsics $K_i$ (upper
triangular, $f_x, f_y > 0$, $(K_i)_{33} = 1$), world-to-camera rotation
$R_i \in \mathrm{SO}(3)$ and optical centre $C_i \in W$. Its projection and
direction matrices are

$$
P_i = K_i R_i \begin{bmatrix} I & -C_i \end{bmatrix} \in \mathbb{R}^{3\times 4},
\qquad
D_i = R_i^\top K_i^{-1} \in \mathrm{GL}(3).
$$

An image point $[p] \in \mathcal{I}_i$ back-projects to the ray
$\lbrace C_i + t\,D_i p : t \in \mathbb{R} \rbrace$.

**Proposition 1 (plane homographies).** Let
$E_h : (X, Y, 1)^\top \mapsto (X, Y, h, 1)^\top$ embed the horizontal plane at
height $h$. Then

$$
A_{i,h} := P_i E_h = D_i^{-1} G_h(C_i),
$$

which lies in $\mathrm{PGL}(3)$ if and only if $h \neq C_{i,z}$. The pitch
homography is

$$
T_i := A_{i,0} = D_i^{-1} G_0(C_i) = K_i R_i \begin{bmatrix} e_1 & e_2 & -C_i \end{bmatrix}.
$$

*Proof.* $\begin{bmatrix} I & -C \end{bmatrix} E_h = G_h(C)$ and $K_i R_i = D_i^{-1}$. $\square$

**Proposition 2 (3D points from $T_i$ and $C$).** If $C_{i,z} \neq 0$, every
$X \in W \setminus \lbrace C_i \rbrace$ projects to

$$
p \sim T_i\, G_0(C_i)^{-1} (X - C_i).
$$

*Proof.* $P_i (X, 1)^\top = D_i^{-1}(X - C_i)$, and $T_i = D_i^{-1} G_0(C_i)$ with
$G_0(C_i)$ invertible gives $D_i^{-1} = T_i\, G_0(C_i)^{-1}$. $\square$

So rendering objects above the ground needs exactly $(T_i, C_i)$.

### 2. Stations

**Definition 2 (station).** A station is a set of frames $F$ with a common
optical centre: $C_i = C$ for all $i \in F$.

**Proposition 3.** For $i, j \in F$, corresponding image points are related by

$$
H_{j \leftarrow i} = \left[ D_j^{-1} D_i \right] \in \mathrm{PGL}(3),
$$

independently of scene depth.

*Proof.* For $X = C + t\,D_i p$ with $t \neq 0$,
$P_j (X, 1)^\top = D_j^{-1}(X - C) = t\, D_j^{-1} D_i\, p$. $\square$

### 3. Registrations and the reference plane

**Definition 3 (registration).** A registration of a station $F$ is a family
$(V_i)_{i \in F} \subset \mathrm{PGL}(3)$ with

$$
H_{j \leftarrow i} = V_j V_i^{-1} \qquad \text{for all } i, j \in F .
$$

The common domain $\Sigma \cong \mathbb{P}^2$ of the maps $V_i : \Sigma \to \mathcal{I}_i$
is the **reference plane** (the panorama plane), and $U_i := V_i^{-1}$.

**Proposition 4 (existence and gauge).**

1. For every $A \in \mathrm{GL}(3)$, $V_i = [D_i^{-1} A]$ is a registration.
2. Two registrations $V, V'$ of the same station satisfy $V'_i = V_i M$ for one
   $M \in \mathrm{PGL}(3)$ and all $i$. The registrations form a right
   $\mathrm{PGL}(3)$-torsor.
3. Every registration has the form $V_i = [D_i^{-1} A]$, with $A = D_r V_r$ for
   any $r \in F$.

*Proof.* (1) $V_j V_i^{-1} = D_j^{-1} A A^{-1} D_i = H_{j \leftarrow i}$.
(2) $V'_j V'^{-1}_i = V_j V_i^{-1}$ gives $V_j^{-1} V'_j = V_i^{-1} V'_i$, a
constant $M$. (3) $D_i^{-1} D_r V_r = H_{i \leftarrow r} V_r = V_i V_r^{-1} V_r = V_i$. $\square$

**Interpretation.** A point $[s] \in \Sigma$ is the ray through $C$ with
direction $[A s]$, because $p = V_i s$ gives $D_i p = A s$. So $\Sigma$ is the
space of rays at the optical centre, coordinatised by $A$; it is neither a
top-down view nor 3D. With the root fixed by $V_r = I$, $A = D_r$ and $\Sigma$
is frame $r$'s image plane.

**Identifiability.** The family $\lbrace H_{j \leftarrow i} \rbrace$ does not
determine $A$. The angle between the rays of $s, s' \in \Sigma$ is

$$
\cos \angle(s, s') = \frac{s^\top \omega_\Sigma\, s'}{\sqrt{s^\top \omega_\Sigma\, s}\ \sqrt{s'^\top \omega_\Sigma\, s'}},
\qquad \omega_\Sigma := A^\top A ,
$$

and in an uncalibrated registration $\omega_\Sigma$ is unknown.

### 4. Landmarks (the V/Q map ③)

**Definition 4 (landmark).** A landmark is a point $q_k \in \Sigma$ with a
descriptor $d_k \in \mathbb{R}^{128}$. Its observation in frame $i$ satisfies

$$
x_{ik} \sim V_i\, q_k \qquad \text{(up to measurement noise).}
$$

$q_k$ is a ray, not a metric 3D point; $q_{k,3} = 0$ is allowed and is a point
at infinity of $\Sigma$'s affine chart.

Building a map is the estimation problem

$$
\min_{\lbrace V_i \rbrace,\ \lbrace q_k \rbrace}\ \sum_{i,k} \rho\left( \left\lVert \varphi(V_i q_k) - x_{ik} \right\rVert \right),
$$

whose solution is unique only up to the gauge
$(V_i, q_k) \mapsto (V_i M, M^{-1} q_k)$. vchao consumes a built map and does
not document how it is produced.

### 5. Anchoring to the pitch

**Proposition 5.** Let $C_z \neq 0$ and let $V$ be a registration. There is a
unique $T_{\mathrm{ref}} \in \mathrm{PGL}(3)$ with

$$
T_i = V_i\, T_{\mathrm{ref}} \qquad \text{for all } i \in F,
$$

namely

$$
T_{\mathrm{ref}} = \left[ A^{-1} G_0(C) \right] = V_a^{-1} T_a \qquad \text{for any anchor } a \in F .
$$

*Proof.* Existence: $V_i A^{-1} G_0(C) = D_i^{-1} G_0(C) = T_i$. Uniqueness:
$V_i$ is invertible. The anchor form follows from $T_a = V_a T_{\mathrm{ref}}$. $\square$

$T_{\mathrm{ref}} : \Pi_0 \to \Sigma$ sends a ground point to the ray through
it. Under the gauge $V_i \mapsto V_i M$, $T_{\mathrm{ref}} \mapsto M^{-1} T_{\mathrm{ref}}$
and $V_i T_{\mathrm{ref}}$ is unchanged.

**Proposition 6 (recovering $A$).** If $C$ is known and $C_z \neq 0$, then

$$
A \sim G_0(C)\, T_{\mathrm{ref}}^{-1}, \qquad D_i^{-1} \sim V_i A^{-1},
$$

and the RQ decomposition of $V_i A^{-1}$, with the sign fixed by $\det R_i = 1$
and the scale by $(K_i)_{33} = 1$, gives $K_i$ and $R_i$ uniquely. Without $C$,
the anchor alone does not determine $A$; constraints on $K$ (zero skew, square
pixels) are needed to solve for $C$ and $A$ together.

### 6. Live estimation (`atlas-vq-estimate`)

Given a V/Q map $\lbrace (q_k, d_k) \rbrace$, $T_{\mathrm{ref}}$ and a new frame $f$:

**Step 1 (matching).** ALIKED features of $f$ are matched to the descriptors
$d_k$ (mutual nearest neighbours with ratio tests), giving correspondences
$\lbrace (x_m, q_{k(m)}) \rbrace$, with $x_m$ in native pixels.

**Step 2 (resection).** RANSAC initialises, then damped Gauss-Newton solves the
homography resection $\Sigma \to \mathcal{I}_f$ (8 degrees of freedom, at least
4 correspondences). Residuals are measured in native pixels and $\rho_3$ is
the Huber loss with a 3-pixel threshold:

$$
\hat V_f = \operatorname*{arg\,min}_{V \in \mathrm{PGL}(3)}\ \sum_m \rho_3\left( \left\lVert \varphi\left(\Lambda_f^{-1} V q_{k(m)}\right) - x_m \right\rVert \right).
$$

**Step 3 (output).**

$$
T_f = N\left( \hat V_f\, T_{\mathrm{ref}} \right).
$$

The map's $V_i$, $q_k$ and $T_{\mathrm{ref}}$ stay fixed. $C$, $K$ and $R$ are
not estimated.

### 7. The canonical Atlas ① and the panorama ②

**Definition 5 (canonical Atlas).** For a station with centre $C$, let
$u = (0, 0, -1)^\top$ be world up and $O$ the pitch centre, and define

$$
b_3 = \frac{O - C}{\lVert O - C \rVert}, \qquad
b_1 = \frac{b_3 \times u}{\lVert b_3 \times u \rVert}, \qquad
b_2 = b_3 \times b_1, \qquad
S = \begin{bmatrix} b_1 & b_2 & b_3 \end{bmatrix} \in \mathrm{SO}(3).
$$

Chart $a$ with rotation $Q_a \in \mathrm{SO}(3)$ relative to the station is the
registration with

$$
A = D_a := S\, Q_a \quad (K_a = I), \qquad
H_{a \leftarrow i} = D_a^{-1} D_i = Q_a^\top S^\top D_i ,
$$

and the pitch projects onto it by $T_{\mathrm{ref}}^{(a)} = D_a^{-1} G_0(C)$,
independently of any frame. Here $\omega_\Sigma = A^\top A = I$ and the
orientation is known, so Euclidean angles on the chart are physical ray angles.

**Definition 6 (projective panorama).** Choose a root $r$ with $V_r = I$ and
**assume** $\omega_\Sigma = I$. A display chart $c$ with rotation $R_c$ sends
chart point $u$ to $V_i R_c u$ in frame $i$, and its colour is the per-channel
median of the frames' colours $\mathrm{RGB}_i$

$$
B_c(u) = \operatorname{median} \left\lbrace \mathrm{RGB}_i\left( V_i R_c u \right) : V_i R_c u \text{ inside frame } i \right\rbrace .
$$

The true value is $\omega_\Sigma = D_r^\top D_r = K_r^{-\top} K_r^{-1} \neq I$
in general, so chart angles are not physical.

**Relation.** ② and ③ are registrations (Definition 3) with unknown $A$; ① is
the registration whose $A$ is a known physical rotation. Upgrading ② or ③ to ①
needs exactly the conditions of Proposition 6.

## Summary

- $V$, $q$ and $T_{\mathrm{ref}}$ live on one reference plane $\Sigma$, the ray
  space at the station's optical centre; this registration is the common idea
  behind all three products.
- Keep only $q$, descriptors, $V$ and $T_{\mathrm{ref}}$: that is the V/Q map ③,
  used to solve $T_f$ live.
- Warp each frame's pixels back with $V$: that is the panorama ②, used to
  inspect and evaluate the registration.
- Fix $A$ to the station's physical directions: that is ①, the canonical
  `Atlas`, not yet implemented.

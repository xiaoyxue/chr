# Coordinate contract

## World and projection

World units are metres, with the origin at ground-level pitch centre.
X follows pitch length, Y pitch width, ground is Z=0, and up is negative Z.
The optical centre C is a world-space point; camera height is `-Cz`.
No pitch size or measured camera is hard-coded into the renderer.

All homogeneous vectors are columns. Ti is a forward, row-major 3x3 matrix:

```text
h = Ti * [X, Y, 1]                       # ground point
q = [Cx/Cz, Cy/Cz, 1/Cz]
h = Ti * ([X, Y, 1] - Z*q)              # elevated point, Cz != 0
snc = [h.x/h.z, h.y/h.z]                 # h.z != 0
```

Ground points need no camera centre. For elevated points, do not divide the
intermediate ground vector before Ti: its last coordinate can be zero while
the final image projection is finite. A camera-centre point or final zero
denominator has no finite projection. The API returns an error, not a placeholder.

Imported Ti coefficients retain their original scale (including a supplied
signed determinant +1 convention). The math accepts equivalent nonzero
homogeneous scales, rather than normalizing by h33 or a matrix norm.
Ti is neither camera intrinsics nor a camera rotation. The first example uses
an explicitly synthetic identity Ti, not calibration or private source data.

## Native pixels and SNC

For positive native raster dimensions W,H and s=sqrt(W*H):

```text
x_snc = (2*u - W)/s       u = (s*x_snc + W)/2
y_snc = (2*v - H)/s       v = (s*y_snc + H)/2
```

Pixel x points right and y points down. Integer sample centres are
`0..W-1` and `0..H-1`; add no half-pixel shift. The chart origin is exactly
`(W/2,H/2)`, not `((W-1)/2,(H-1)/2)`. SNC is square-root-area normalization,
not height normalization. Always use the input's native dimensions for Ti data.

## wgpu rendering boundary

wgpu NDC x points right and y points up:

```text
x_ndc =  s/W * x_snc
y_ndc = -s/H * y_snc
```

The shader applies this scale once. The native demo uses its physical surface
dimensions as the synthetic image size and redraws after resize/HiDPI changes.
The triangle's RGB apex is above its green/blue base and maintains equal
pixel-space width and height in rectangular windows.

wgpu clip depth satisfies `0 <= z <= w`. The demo uses `(z,w)=(0.5,1)`;
this is an explicit 2D layer, **not** world-space Z or recovered camera depth.
General 3D clipping, occlusion, and native-video-to-viewport fitting are future
work and must not be inferred from Ti alone.

## Future data ingestion

Source frame identity, presentation index, and source/clip timestamps remain
distinct. Join through explicit frame mappings; never interpret Ti row index
as clip ordinal, pad missing transforms, or recreate PTS from nominal frame rate.
This bootstrap does not load source bundles or implement a video protocol.

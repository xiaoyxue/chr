// Shared by the flag surface passes (flag.wgsl, flag_cloth.wgsl) and flag draws
// (flag_surface.wgsl). FLAG_COLUMNS and FLAG_ROWS are prepended from chr_renderer::scene.

// One vertex of the shared flag grid in flag-local metres; its artwork UV in uv.xy.
struct FlagVertex { position: vec4<f32>, normal: vec4<f32>, tangent: vec4<f32>, uv: vec4<f32> };

// Grid coordinates moved onto the part of the flag a reveal has opened, as
// Flag::revealed_uv: what is not shown yet collapses onto the edge of what is.
fn revealed_uv(grid: vec2<f32>, p: f32, direction: u32) -> vec2<f32> {
    let centred = vec2(0.5) + clamp(grid - vec2(0.5), vec2(-0.5 * p), vec2(0.5 * p));
    switch direction {
        case 1u: { return vec2(grid.x, centred.y); }
        case 2u: { return vec2(min(grid.x, p), grid.y); }
        case 3u: { return vec2(max(grid.x, 1.0 - p), grid.y); }
        case 4u: { return vec2(grid.x, min(grid.y, p)); }
        case 5u: { return vec2(grid.x, max(grid.y, 1.0 - p)); }
        default: { return vec2(centred.x, grid.y); }
    }
}

// The artwork's texture coordinates at flag coordinates (u, v), as Flag::artwork_uv.
fn artwork_uv(u: f32, v: f32, turns: u32) -> vec2<f32> {
    switch turns {
        case 1u: { return vec2(v, 1.0 - u); }
        case 2u: { return vec2(1.0 - u, 1.0 - v); }
        case 3u: { return vec2(1.0 - v, u); }
        default: { return vec2(u, v); }
    }
}

// Grid coordinates in [0, 1]^2 of flag grid vertex `index`.
fn flag_grid(index: u32) -> vec2<f32> {
    let columns = FLAG_COLUMNS + 1u;
    return vec2(f32(index % columns) / f32(FLAG_COLUMNS), f32(index / columns) / f32(FLAG_ROWS));
}

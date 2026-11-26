// SimpleWave flag surface, evaluated once per frame per flag instance.
// FLAG_COLUMNS and FLAG_ROWS are prepended from chr_renderer::scene.
// Flag-local metres: X along the width from the left edge, Y down from the top edge,
// Z = X x Y. The front normal (-Z at rest) faces the side that reads the artwork
// upright. The centre line u = 0.5 stays pinned; both halves wave outwards. A partly
// revealed flag collapses the columns beyond its revealed width onto its edge, and
// the artwork's UV follows its clockwise quarter turns (scene/flag.rs).

struct FlagParams {
    // Width, height, amplitude (m), frequency (cycles from the centre to an edge).
    size_wave: vec4<f32>,
    // Wave phases already reduced modulo 2 pi on the CPU.
    phases: vec4<f32>,
    // Revealed fraction of the width, artwork quarter turns.
    artwork: vec4<f32>,
};
struct FlagVertex { position: vec4<f32>, normal: vec4<f32>, tangent: vec4<f32>, uv: vec4<f32> };

@group(0) @binding(0) var<uniform> params: FlagParams;
@group(0) @binding(1) var<storage,read_write> vertices: array<FlagVertex>;

@compute @workgroup_size(64)
fn cs_flag(@builtin(global_invocation_id) id: vec3<u32>) {
    let columns = FLAG_COLUMNS + 1u;
    if id.x >= columns * (FLAG_ROWS + 1u) { return; }
    let half = 0.5 * params.artwork.x;
    let u = 0.5 + clamp(f32(id.x % columns) / f32(FLAG_COLUMNS) - 0.5, -half, half);
    let v = f32(id.x / columns) / f32(FLAG_ROWS);
    let width = params.size_wave.x;
    let height = params.size_wave.y;
    let amplitude = params.size_wave.z;
    let r = abs(2.0 * u - 1.0);
    let drdu = 2.0 * sign(2.0 * u - 1.0);
    let k = 6.28318530718 * params.size_wave.w;
    let a = k * r - params.phases.x;
    let b = 1.8 * k * r + 3.0 * v - params.phases.y;
    let c = 0.65 * k * r - 4.0 * v - params.phases.z;
    let wave = sin(a) + 0.35 * sin(b) + 0.18 * cos(c);
    let envelope = amplitude * r * r;
    let envelope_du = 2.0 * amplitude * r * drdu;
    // SimpleWave's upward offset, negated because local Y points down the flag.
    let sag = 0.14 - 0.08 * sin(b);
    let position = vec3(width * u, height * v + envelope * sag, envelope * wave);
    let du = vec3(
        width,
        envelope_du * sag - envelope * 0.144 * k * cos(b) * drdu,
        envelope_du * wave + envelope * k * (cos(a) + 0.63 * cos(b) - 0.117 * sin(c)) * drdu
    );
    let dv = vec3(
        0.0,
        height - envelope * 0.24 * cos(b),
        envelope * (1.05 * cos(b) + 0.72 * sin(c))
    );
    vertices[id.x] = FlagVertex(
        vec4(position, 1.0),
        vec4(normalize(cross(dv, du)), 0.0),
        // Handedness -1: cross(normal, tangent) runs down the flag, along +Y.
        vec4(normalize(du), -1.0),
        vec4(artwork_uv(u, v, u32(params.artwork.y)), 0.0, 0.0)
    );
}

fn artwork_uv(u: f32, v: f32, turns: u32) -> vec2<f32> {
    switch turns {
        case 1u: { return vec2(v, 1.0 - u); }
        case 2u: { return vec2(1.0 - u, 1.0 - v); }
        case 3u: { return vec2(1.0 - v, u); }
        default: { return vec2(u, v); }
    }
}

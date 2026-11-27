// Native flag cloth (scene/cloth.rs), after flag_shared.wgsl; CLOTH_COLUMNS and
// CLOTH_ROWS are prepended too. A port of the AER reference's `predict` and
// `constrain` in flag-local metres (X across, Y down, Z = X x Y): its Y-up, +Z-front
// constants turn to -Y and -Z, and its floor contact is dropped. Particles ping-pong
// between two buffers; the bottom row never moves and the top row follows its anchor,
// which rises while the flag is raised.

struct Particle { position: vec4<f32>, previous: vec4<f32> };
struct Step {
    // Flag-local air velocity (m/s) and the strength of its ripple.
    wind: vec4<f32>,
    // Flag-local gravity (m/s^2) and the step's time on the cloth's clock (s).
    gravity: vec4<f32>,
    // Rest spacing across and down the cloth (m), bending strength and the step (s).
    shape: vec4<f32>,
    // Flag-local Y of the pinned top edge after this step: 0 once raised (scene/cloth.rs).
    anchor: vec4<f32>,
};
// flag.rs params(): a cloth's surface reads only its reveal and artwork turns.
struct FlagParams { size_wave: vec4<f32>, phases: vec4<f32>, artwork: vec4<f32> };

@group(0) @binding(0) var<storage,read> source: array<Particle>;
@group(0) @binding(1) var<storage,read_write> destination: array<Particle>;
@group(0) @binding(2) var<uniform> stepping: Step;
@group(0) @binding(3) var<storage,read_write> vertices: array<FlagVertex>;
@group(0) @binding(4) var<uniform> params: FlagParams;

fn pinned(row: u32) -> bool {
    return row == 0u || row == CLOTH_ROWS - 1u;
}

fn particle(column: u32, row: u32) -> vec3<f32> {
    return source[row * CLOTH_COLUMNS + column].position.xyz;
}

@compute @workgroup_size(64)
fn predict(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= CLOTH_COLUMNS * CLOTH_ROWS { return; }
    let x = i % CLOTH_COLUMNS;
    let y = i / CLOTH_COLUMNS;
    let p = source[i];
    if y == 0u {
        let top = vec4(f32(x) * stepping.shape.x, stepping.anchor.x, 0.0, 1.0);
        destination[i] = Particle(top, top);
        return;
    }
    if pinned(y) {
        destination[i] = Particle(p.position, p.position);
        return;
    }
    let dt = stepping.shape.w;
    let t = stepping.gravity.w;
    let velocity = (p.position.xyz - p.previous.xyz) / dt;
    let horizontal = particle(min(x + 1u, CLOTH_COLUMNS - 1u), y) - particle(max(x, 1u) - 1u, y);
    let vertical = particle(x, min(y + 1u, CLOTH_ROWS - 1u)) - particle(x, max(y, 1u) - 1u);
    let area = cross(horizontal, vertical);
    let normal = area / max(length(area), 0.00001);
    let wave = sin(t * 3.7 - f32(x) * 0.15 + f32(y) * 0.19);
    let gust = 0.84 + 0.16 * sin(t * 1.7) + 0.11 * sin(t * 4.1 + 1.2);
    // The reference's ripple lifts the cloth and pushes it towards its front.
    let air = stepping.wind.xyz * gust + vec3(0.0, -0.12 * wave, -0.5 * wave) * stepping.wind.w;
    let relative = air - velocity;
    let pressure = clamp(dot(relative, normal), -15.0, 15.0);
    let aerodynamic = normal * pressure * abs(pressure) * 0.27 + relative * 0.7;
    let acceleration = stepping.gravity.xyz + aerodynamic;
    let next = p.position.xyz + (p.position.xyz - p.previous.xyz) * 0.995 + acceleration * dt * dt;
    destination[i] = Particle(vec4(next, 1.0), vec4(p.position.xyz, 1.0));
}

fn correction(i: u32, x: i32, y: i32, dx: i32, dy: i32, strength: f32) -> vec4<f32> {
    let nx = x + dx;
    let ny = y + dy;
    if nx < 0 || nx >= i32(CLOTH_COLUMNS) || ny < 0 || ny >= i32(CLOTH_ROWS) {
        return vec4(0.0);
    }
    let delta = particle(u32(nx), u32(ny)) - source[i].position.xyz;
    let distance = length(delta);
    let rest = length(vec2(f32(dx) * stepping.shape.x, f32(dy) * stepping.shape.y));
    let weight = select(0.5, 1.0, pinned(u32(ny))) * strength;
    return vec4(delta * ((distance - rest) / max(distance, 0.000001)) * weight, weight);
}

@compute @workgroup_size(64)
fn constrain(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= CLOTH_COLUMNS * CLOTH_ROWS { return; }
    let x = i32(i % CLOTH_COLUMNS);
    let y = i32(i / CLOTH_COLUMNS);
    let p = source[i];
    if pinned(u32(y)) {
        destination[i] = p;
        return;
    }
    var delta = correction(i, x, y, -1, 0, 1.0) + correction(i, x, y, 1, 0, 1.0)
        + correction(i, x, y, 0, -1, 1.0) + correction(i, x, y, 0, 1, 1.0);
    delta += correction(i, x, y, -1, -1, 0.7) + correction(i, x, y, 1, -1, 0.7)
        + correction(i, x, y, -1, 1, 0.7) + correction(i, x, y, 1, 1, 0.7);
    let bend = stepping.shape.z;
    delta += correction(i, x, y, -2, 0, bend) + correction(i, x, y, 2, 0, bend)
        + correction(i, x, y, 0, -2, bend) + correction(i, x, y, 0, 2, bend);
    // Normalised by the actual neighbour weights, fewer at the free edges.
    let next = p.position.xyz + delta.xyz * (0.9 / max(delta.w, 1.0));
    destination[i] = Particle(vec4(next, 1.0), p.previous);
}

// Surface directions at a particle per cell, central inside and one-sided at edges.
fn across(column: u32, row: u32) -> vec3<f32> {
    let a = max(column, 1u) - 1u;
    let b = min(column + 1u, CLOTH_COLUMNS - 1u);
    return (particle(b, row) - particle(a, row)) / f32(b - a);
}

fn down(column: u32, row: u32) -> vec3<f32> {
    let a = max(row, 1u) - 1u;
    let b = min(row + 1u, CLOTH_ROWS - 1u);
    return (particle(column, b) - particle(column, a)) / f32(b - a);
}

fn bilinear(a: vec3<f32>, b: vec3<f32>, c: vec3<f32>, d: vec3<f32>, f: vec2<f32>) -> vec3<f32> {
    return mix(mix(a, b, vec3(f.x)), mix(c, d, vec3(f.x)), vec3(f.y));
}

// The flag grid's surface sampled from the cloth: positions and surface directions
// blended over the cloth cell under each (revealed) grid point, so the shading
// normal is smooth across cells.
@compute @workgroup_size(64)
fn surface(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= (FLAG_COLUMNS + 1u) * (FLAG_ROWS + 1u) { return; }
    let shown = revealed_uv(flag_grid(id.x), params.artwork.x, u32(params.artwork.z));
    let cell = shown * vec2(f32(CLOTH_COLUMNS - 1u), f32(CLOTH_ROWS - 1u));
    let c = min(u32(cell.x), CLOTH_COLUMNS - 2u);
    let r = min(u32(cell.y), CLOTH_ROWS - 2u);
    let f = cell - vec2(f32(c), f32(r));
    let position = bilinear(particle(c, r), particle(c + 1u, r), particle(c, r + 1u), particle(c + 1u, r + 1u), f);
    let du = bilinear(across(c, r), across(c + 1u, r), across(c, r + 1u), across(c + 1u, r + 1u), f);
    let dv = bilinear(down(c, r), down(c + 1u, r), down(c, r + 1u), down(c + 1u, r + 1u), f);
    let normal = cross(dv, du);
    vertices[id.x] = FlagVertex(
        vec4(position, 1.0),
        vec4(normal / max(length(normal), 1e-12), 0.0),
        // Handedness -1: cross(normal, tangent) runs down the flag, along +Y.
        vec4(normalize(du), -1.0),
        vec4(artwork_uv(shown.x, shown.y, u32(params.artwork.y)), 0.0, 0.0)
    );
}

struct Quad {
    r0: vec4f, r1: vec4f, r2: vec4f,
    metadata: vec4u, bounds: vec4u,
}
struct Parameters {
    width: u32, height: u32, source: u32, sequence: u32,
    opacity: f32, color: u32, timecode_overlay: u32, reserved: u32,
}
@group(0) @binding(0) var source_y: texture_2d<f32>;
@group(0) @binding(1) var source_uv: texture_2d<f32>;
@group(0) @binding(2) var output_rgba: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(3) var masks: texture_2d_array<f32>;
@group(0) @binding(4) var logo0: texture_2d<f32>;
@group(0) @binding(5) var logo1: texture_2d<f32>;
@group(0) @binding(6) var logo2: texture_2d<f32>;
@group(0) @binding(7) var<storage, read> quads: array<Quad>;
@group(0) @binding(8) var<uniform> p: Parameters;

fn dimensions(index: u32, level: u32) -> vec2u {
    if index == 0u { return textureDimensions(logo0, level); }
    if index == 1u { return textureDimensions(logo1, level); }
    return textureDimensions(logo2, level);
}
fn levels(index: u32) -> u32 {
    if index == 0u { return textureNumLevels(logo0); }
    if index == 1u { return textureNumLevels(logo1); }
    return textureNumLevels(logo2);
}
fn texel(index: u32, at: vec2i, level: u32) -> vec4f {
    if any(at < vec2i(0)) || any(at >= vec2i(dimensions(index, level))) { return vec4f(0); }
    if index == 0u { return textureLoad(logo0, at, i32(level)); }
    if index == 1u { return textureLoad(logo1, at, i32(level)); }
    return textureLoad(logo2, at, i32(level));
}
fn bilinear(index: u32, uv: vec2f, level: u32) -> vec4f {
    let coordinate = uv * vec2f(dimensions(index, level)) - 0.5;
    let base = vec2i(floor(coordinate));
    let f = fract(coordinate);
    return mix(
        mix(texel(index, base, level), texel(index, base + vec2i(1, 0), level), f.x),
        mix(texel(index, base + vec2i(0, 1), level), texel(index, base + vec2i(1, 1), level), f.x),
        f.y
    );
}
fn sample_logo(quad: Quad, pixel: vec2f) -> vec4f {
    let point = vec3f(pixel, 1);
    let q = dot(quad.r2.xyz, point);
    if abs(q) < 1e-10 { return vec4f(0); }
    let uv = vec2f(dot(quad.r0.xyz, point), dot(quad.r1.xyz, point)) / q;
    let size = vec2f(dimensions(quad.metadata.x, 0u));
    let margin = 0.5 / size;
    if any(uv < -margin) || any(uv > 1 + margin) { return vec4f(0); }
    let dx = vec2f(quad.r0.x - quad.r2.x * uv.x, quad.r1.x - quad.r2.x * uv.y) * size / (2 * q);
    let dy = vec2f(quad.r0.y - quad.r2.y * uv.x, quad.r1.y - quad.r2.y * uv.y) * size / (2 * q);
    let a = dot(dx, dx);
    let b = dot(dy, dy);
    let c = dot(dx, dy);
    let sigma = sqrt(max(0.0, (a + b + sqrt((a - b) * (a - b) + 4 * c * c)) * 0.5));
    let maximum = levels(quad.metadata.x) - 1u;
    let lod = clamp(log2(max(1.0, sigma)), 0.0, f32(maximum));
    let low = u32(floor(lod));
    return mix(bilinear(quad.metadata.x, uv, low),
               bilinear(quad.metadata.x, uv, min(low + 1u, maximum)), fract(lod));
}
fn chroma(at: vec2i) -> vec2f {
    return textureLoad(source_uv, clamp(at, vec2i(0), vec2i(textureDimensions(source_uv)) - 1), 0).rg;
}
fn background(at: vec2u) -> vec3f {
    let luma = textureLoad(source_y, vec2i(at), 0).r;
    let coordinate = vec2f(at) * 0.5 - vec2f(0, 0.25);
    let base = vec2i(floor(coordinate));
    let f = fract(coordinate);
    var uv = mix(mix(chroma(base), chroma(base + vec2i(1, 0)), f.x),
                 mix(chroma(base + vec2i(0, 1)), chroma(base + vec2i(1, 1)), f.x), f.y);
    var y = luma;
    if (p.color & 2u) == 0u {
        y = (y * 255 - 16) / 219;
        uv = (uv * 255 - 128) / 224;
    } else {
        uv -= vec2f(128.0 / 255.0);
    }
    if (p.color & 1u) != 0u {
        return clamp(vec3f(y + 1.5748 * uv.y, y - 0.187324 * uv.x - 0.468124 * uv.y,
                           y + 1.8556 * uv.x), vec3f(0), vec3f(1));
    }
    return clamp(vec3f(y + 1.402 * uv.y, y - 0.344136 * uv.x - 0.714136 * uv.y,
                       y + 1.772 * uv.x), vec3f(0), vec3f(1));
}
const DIGITS = array<u32, 70>(
    14,17,19,21,25,17,14, 4,12,4,4,4,4,14,
    14,17,1,2,4,8,31, 30,1,1,14,1,1,30,
    2,6,10,18,31,2,2, 31,16,16,30,1,1,30,
    14,16,16,30,17,17,14, 31,1,2,4,8,8,8,
    14,17,17,14,17,17,14, 14,17,17,15,1,1,14
);
fn timecode_ink(at: vec2u) -> bool {
    let cell = (at - vec2u(16)) / 2u;
    let slot = cell.x / 6u;
    let column = cell.x % 6u;
    if cell.y >= 7u || slot > 10u || column >= 5u { return false; }
    if slot % 3u == 2u { return column == 2u && (cell.y == 2u || cell.y == 4u); }
    let value = array<u32, 4>(p.sequence / 108000u % 24u, p.sequence / 1800u % 60u,
                              p.sequence / 30u % 60u, p.sequence % 30u)[slot / 3u];
    let digit = (value / select(10u, 1u, slot % 3u == 1u)) % 10u;
    return ((DIGITS[digit * 7u + cell.y] >> (4u - column)) & 1u) != 0u;
}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if id.x >= p.width || id.y >= p.height { return; }
    let at = id.xy;
    var layer = vec4f(0);
    for (var index = 0u; index < 6u; index++) {
        let quad = quads[index];
        if quad.metadata.y == 0u || any(at < quad.bounds.xy) || any(at >= quad.bounds.zw) { continue; }
        var sampled = vec4f(0);
        for (var sy = 0u; sy < 2u; sy++) {
            for (var sx = 0u; sx < 2u; sx++) {
                sampled += sample_logo(quad, vec2f(at) + vec2f(f32(sx), f32(sy)) * 0.5 - 0.25);
            }
        }
        sampled *= 0.25;
        layer = sampled + layer * (1 - sampled.a);
    }
    let visibility = 1 - textureLoad(masks, vec2i(at), i32(p.source), 0).r;
    let weight = p.opacity * visibility;
    var result = vec4f(weight * layer.rgb + (1 - weight * layer.a) * background(at),
                       weight * layer.a);
    if p.timecode_overlay != 0u && all(at >= vec2u(12)) && all(at < vec2u(152, 34)) {
        result = vec4f(result.rgb * 0.2, 1);
        if all(at >= vec2u(16)) && timecode_ink(at) { result = vec4f(1); }
    }
    textureStore(output_rgba, vec2i(at), result);
}

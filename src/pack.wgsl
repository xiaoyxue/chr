struct Parameters {
    width: u32, height: u32, source: u32, sequence: u32,
    opacity: f32, color: u32, timecode_overlay: u32, reserved: u32,
}
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var original_y: texture_2d<f32>;
@group(0) @binding(2) var original_uv: texture_2d<f32>;
@group(0) @binding(3) var<storage, read_write> output: array<u32>;
@group(0) @binding(4) var<uniform> p: Parameters;

fn yuv(rgb: vec3f) -> vec3f {
    var coefficients = vec3f(0.299, 0.587, 0.114);
    var chroma_scale = vec2f(1.772, 1.402);
    if (p.color & 1u) != 0u {
        coefficients = vec3f(0.2126, 0.7152, 0.0722);
        chroma_scale = vec2f(1.8556, 1.5748);
    }
    let y = dot(rgb, coefficients);
    var result = vec3f(y, (rgb.b - y) / chroma_scale.x, (rgb.r - y) / chroma_scale.y);
    if (p.color & 2u) == 0u {
        result = result * vec3f(219, 224, 224) + vec3f(16, 128, 128);
    } else {
        result = result * 255 + vec3f(0, 128, 128);
    }
    return result;
}
fn code(value: f32) -> u32 { return u32(clamp(floor(value + 0.5), 0.0, 255.0)); }
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    let start = id.xy * vec2u(4, 2);
    if start.x >= p.width || start.y >= p.height { return; }
    var chroma_words = 0u;
    var y_words = array<u32, 2>(0u, 0u);
    for (var block = 0u; block < 2u; block++) {
        var chroma = vec2f(0);
        var changed = false;
        for (var row = 0u; row < 2u; row++) {
            for (var column = 0u; column < 2u; column++) {
                let x = block * 2u + column;
                let at = vec2i(start + vec2u(x, row));
                let pixel = textureLoad(source, at, 0);
                let converted = yuv(pixel.rgb);
                var y = converted.x;
                if pixel.a == 0 { y = textureLoad(original_y, at, 0).r * 255; }
                y_words[row] |= code(y) << (x * 8u);
                chroma += converted.yz * 0.25;
                changed = changed || pixel.a > 0;
            }
        }
        if !changed {
            chroma = textureLoad(original_uv, vec2i(start / 2u + vec2u(block, 0)), 0).rg * 255;
        }
        chroma_words |= code(chroma.x) << (block * 16u);
        chroma_words |= code(chroma.y) << (block * 16u + 8u);
    }
    let stride = p.width / 4u;
    output[start.y * stride + id.x] = y_words[0];
    output[(start.y + 1u) * stride + id.x] = y_words[1];
    output[p.width * p.height / 4u + id.y * stride + id.x] = chroma_words;
}

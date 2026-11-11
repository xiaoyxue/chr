// Packs two sRGB-encoded RGB pixels into one UYVY texel: (Cb, Y0, Cr, Y1).
// BT.709 coefficients, 8-bit limited (studio) range: Y in [16, 235], Cb/Cr in [16, 240].

@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the whole target.
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

// Returns (Y', Pb, Pr) with Y' in [0, 1] and Pb/Pr in [-0.5, 0.5].
fn bt709(rgb: vec3<f32>) -> vec3<f32> {
    let y = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    return vec3<f32>(y, (rgb.b - y) / 1.8556, (rgb.r - y) / 1.5748);
}

fn quantize(value: f32) -> u32 {
    return u32(clamp(round(value), 0.0, 255.0));
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<u32> {
    // Framebuffer coordinates: origin top-left, so output row y is source row y.
    let x = u32(position.x) * 2u;
    let y = u32(position.y);
    let left = bt709(textureLoad(source, vec2<u32>(x, y), 0).rgb);
    let right = bt709(textureLoad(source, vec2<u32>(x + 1u, y), 0).rgb);
    // 4:2:2 chroma: the mean of the horizontal pixel pair.
    let chroma = (left.yz + right.yz) * 0.5;
    return vec4<u32>(
        quantize(128.0 + 224.0 * chroma.x),
        quantize(16.0 + 219.0 * left.x),
        quantize(128.0 + 224.0 * chroma.y),
        quantize(16.0 + 219.0 * right.x),
    );
}

struct Quad {
  r0: vec4f, r1: vec4f, r2: vec4f, bounds: vec4i, info: vec4u,
}
struct Parameters { color: vec4f, control: vec4u, quads: array<Quad, 6>, }
struct Mip { width: u32, height: u32, offset: u32, unused: u32, }
@group(0) @binding(0) var yPlane: texture_2d<f32>;
@group(0) @binding(1) var uvPlane: texture_2d<f32>;
@group(0) @binding(2) var output: texture_storage_2d<r32uint, write>;
@group(0) @binding(3) var<uniform> params: Parameters;
@group(0) @binding(4) var<storage, read> artwork: array<vec4f>;
@group(0) @binding(5) var<storage, read> mips: array<Mip>;
@group(0) @binding(6) var mask: texture_2d<f32>;

fn texel(level: Mip, p: vec2i) -> vec4f {
  if (any(p < vec2i(0)) || p.x >= i32(level.width) || p.y >= i32(level.height)) { return vec4f(0); }
  return artwork[level.offset + u32(p.y) * level.width + u32(p.x)];
}
fn bilinear(level: Mip, uv: vec2f) -> vec4f {
  let pixel = uv * vec2f(f32(level.width), f32(level.height)) - 0.5;
  let low = vec2i(floor(pixel));
  let t = fract(pixel);
  return mix(mix(texel(level, low), texel(level, low + vec2i(1, 0)), t.x),
             mix(texel(level, low + vec2i(0, 1)), texel(level, low + vec2i(1)), t.x), t.y);
}
fn sampleQuad(q: Quad, p: vec2f) -> vec4f {
  let h = vec3f(p, 1);
  let d = dot(q.r2.xyz, h);
  if (abs(d) < 1e-10) { return vec4f(0); }
  let uv = vec2f(dot(q.r0.xyz, h), dot(q.r1.xyz, h)) / d;
  if (any(uv < vec2f(0)) || any(uv > vec2f(1))) { return vec4f(0); }
  let size = vec2f(f32(mips[q.info.y].width), f32(mips[q.info.y].height));
  let dx = vec2f(q.r0.x - uv.x * q.r2.x, q.r1.x - uv.y * q.r2.x) * size / d * 0.5;
  let dy = vec2f(q.r0.y - uv.x * q.r2.y, q.r1.y - uv.y * q.r2.y) * size / d * 0.5;
  let a = dot(dx, dx);
  let b = dot(dx, dy);
  let c = dot(dy, dy);
  let sigma = sqrt(max(0.0, 0.5 * (a + c + sqrt((a-c)*(a-c) + 4.0*b*b))));
  let lod = clamp(log2(max(sigma, 1.0)), 0.0, f32(q.info.z - 1u));
  let low = u32(floor(lod));
  return mix(bilinear(mips[q.info.y + low], uv),
             bilinear(mips[q.info.y + min(low + 1u, q.info.z - 1u)], uv), fract(lod));
}
fn composite(p: vec2u, background: vec3f) -> vec4f {
  if (params.control.y == 0u || params.control.x != 0u) { return vec4f(background, 0); }
  var logo = vec4f(0);
  for (var i = 0u; i < 6u; i++) {
    let q = params.quads[i];
    if (q.info.x == 0u || any(vec2i(p) < q.bounds.xy) || any(vec2i(p) >= q.bounds.zw)) { continue; }
    var resolved = vec4f(0);
    for (var y = 0u; y < 2u; y++) {
      for (var x = 0u; x < 2u; x++) {
        resolved += sampleQuad(q, vec2f(p) + vec2f(f32(x), f32(y)) * 0.5 - 0.25) * 0.25;
      }
    }
    logo = resolved + (1.0 - resolved.a) * logo;
  }
  let visibility = 1.0 - textureLoad(mask, p, 0).r;
  let opacity = params.color.z * visibility;
  return vec4f(opacity * logo.rgb + (1.0 - opacity * logo.a) * background, opacity * logo.a);
}
fn decodeColor(p: vec2u) -> vec3f {
  let yByte = textureLoad(yPlane, p, 0).r * 255.0;
  let uvByte = textureLoad(uvPlane, p / 2u, 0).rg * 255.0;
  var y = (yByte - 16.0) / 219.0;
  var uv = (uvByte - 128.0) / 224.0;
  if (params.color.w > 0.5) { y = yByte / 255.0; uv = (uvByte - 128.0) / 255.0; }
  let kr = params.color.x; let kb = params.color.y; let kg = 1.0 - kr - kb;
  let r = y + 2.0 * (1.0 - kr) * uv.y;
  let b = y + 2.0 * (1.0 - kb) * uv.x;
  let g = (y - kr*r - kb*b) / kg;
  return clamp(vec3f(r, g, b), vec3f(0), vec3f(1));
}
fn encodeColor(rgb: vec3f) -> vec3f {
  let kr = params.color.x; let kb = params.color.y; let kg = 1.0 - kr - kb;
  let y = dot(rgb, vec3f(kr, kg, kb));
  let uv = vec2f((rgb.b-y)/(2.0*(1.0-kb)), (rgb.r-y)/(2.0*(1.0-kr)));
  if (params.color.w > 0.5) { return vec3f(y * 255.0, uv * 255.0 + 128.0); }
  return vec3f(y * 219.0 + 16.0, uv * 224.0 + 128.0);
}
fn byte(v: f32) -> u32 { return u32(clamp(round(v), 0.0, 255.0)); }
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
  let size = textureDimensions(yPlane);
  if (id.x >= size.x / 4u || id.y >= size.y / 2u) { return; }
  var chroma = array<vec2f, 2>(vec2f(0), vec2f(0));
  let bypass = params.control.y == 0u || params.control.x != 0u;
  for (var row = 0u; row < 2u; row++) {
    var packed = 0u;
    for (var x = 0u; x < 4u; x++) {
      let p = vec2u(id.x * 4u + x, id.y * 2u + row);
      var value = textureLoad(yPlane, p, 0).r * 255.0;
      if (!bypass) {
        let composed = composite(p, decodeColor(p));
        var yuv = vec3f(value, textureLoad(uvPlane, p / 2u, 0).rg * 255.0);
        if (composed.a > 0.0) { yuv = encodeColor(composed.rgb); }
        value = yuv.x;
        chroma[x / 2u] += yuv.yz * 0.25;
      }
      if (params.control.x == 2u && p.x < 160u && p.y < 80u) { value = 204.0; }
      packed |= byte(value) << (x * 8u);
    }
    textureStore(output, vec2u(id.x, id.y * 2u + row), vec4u(packed));
  }
  if (bypass) {
    chroma[0] = textureLoad(uvPlane, vec2u(id.x * 2u, id.y), 0).rg * 255.0;
    chroma[1] = textureLoad(uvPlane, vec2u(id.x * 2u + 1u, id.y), 0).rg * 255.0;
  }
  let packed = byte(chroma[0].x) | (byte(chroma[0].y) << 8u) |
               (byte(chroma[1].x) << 16u) | (byte(chroma[1].y) << 24u);
  textureStore(output, vec2u(id.x, size.y + id.y), vec4u(packed));
}

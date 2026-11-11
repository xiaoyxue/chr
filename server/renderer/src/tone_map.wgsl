@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> settings: vec4<f32>;
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0))[index];
    return vec4(p,0.0,1.0);
}
@fragment fn fs_main(@builtin(position) pixel: vec4<f32>) -> @location(0) vec4<f32> {
    let value = min(max(textureLoad(source,vec2<i32>(pixel.xy),0).rgb,vec3(0.0)) * settings.x,vec3(65504.0));
    var mapped = value / (vec3(1.0)+value);
    if settings.y > 0.5 {
        mapped = clamp((value*(2.51*value+vec3(0.03))) /
            (value*(2.43*value+vec3(0.59))+vec3(0.14)),vec3(0.0),vec3(1.0));
    }
    return vec4(mapped,1.0);
}

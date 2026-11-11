struct Output { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> };
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> Output {
    let p = array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0))[index];
    var out: Output;
    out.position = vec4(p,0.0,1.0);
    out.uv = p * vec2(0.5,-0.5) + vec2(0.5);
    return out;
}
@fragment fn fs_main(input: Output) -> @location(0) vec4<f32> {
    return textureSampleLevel(source,linear_sampler,input.uv,0.0);
}

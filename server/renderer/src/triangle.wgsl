struct Viewport {
    // SNC to NDC scale; NDC Y is up while SNC Y is down.
    scale: vec2<f32>,
    // (cos, sin) of the display rotation about the SNC origin.
    rotation: vec2<f32>,
}

@group(0) @binding(0) var<uniform> viewport: Viewport;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
}

@vertex
fn vs_main(
    @location(0) snc: vec2<f32>,
    @location(1) color: vec3<f32>,
) -> VertexOutput {
    var output: VertexOutput;
    let r = viewport.rotation;
    let rotated = vec2<f32>(r.x * snc.x - r.y * snc.y, r.y * snc.x + r.x * snc.y);
    output.position = vec4<f32>(rotated * viewport.scale, 0.5, 1.0);
    output.color = color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(input.color, 1.0);
}

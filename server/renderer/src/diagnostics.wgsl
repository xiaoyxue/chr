// Only the clip matrix prefix is used; camera-specific viewing semantics stay shared.
@group(0) @binding(0) var<uniform> clip_from_world: mat4x4<f32>;
struct Line {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>
};
@vertex
fn vs_main(@location(0) position:vec3<f32>, @location(1) color:vec3<f32>)->Line {
    var out:Line;
    out.clip=clip_from_world*vec4(position,1.0);
    out.color=color;
    return out;
}
@fragment
fn fs_main(line:Line)->@location(0) vec4<f32> {
    return vec4(line.color,1.0);
}

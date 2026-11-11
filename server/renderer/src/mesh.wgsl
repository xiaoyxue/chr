struct Camera { clip_from_world: mat4x4<f32> };
struct Object { model: mat4x4<f32>, color: vec4<f32> };
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> object: Object;

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return camera.clip_from_world * object.model * vec4<f32>(position, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return object.color;
}

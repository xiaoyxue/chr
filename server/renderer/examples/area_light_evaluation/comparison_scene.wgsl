// Orthographic view of a rough metallic ground plane, world up -Z.
// The local shading frame below is reflected to world coordinates by the oracle.
const COMPARE_NV: f32 = 0.3;
const COMPARE_ROUGH: f32 = 0.5;
const COMPARE_SPAN: f32 = 6.8;

fn comparison_surface(pixel: vec2<u32>, size: u32) -> vec3<f32> {
    let uv = (vec2<f32>(pixel)+0.5)/f32(size);
    return vec3((uv.y-0.5)*COMPARE_SPAN/COMPARE_NV,
        -(uv.x-0.5)*COMPARE_SPAN,0.0);
}

// Analytic camera-ray intersection: -1 is ground, 0 a planar emitter's
// non-emitting back, 1 an outward sphere's directly visible emission (Le=1).
fn comparison_emitter(shape: u32, p: vec3<f32>) -> f32 {
    let view = vec3(sqrt(1.0-COMPARE_NV*COMPARE_NV),0.0,COMPARE_NV);
    if shape == 2u {
        let delta = vec3(0.0,0.0,2.0)-p;
        let t = dot(delta,view);
        if t>0.0 && dot(delta,delta)-t*t<=1.0 { return 1.0; }
    } else {
        let hit = p+view*(2.0/COMPARE_NV);
        if shape == 0u && abs(hit.x)<=1.0 && abs(hit.y)<=0.6 { return 0.0; }
        if shape == 1u && dot(hit.xy,hit.xy)<=1.0 { return 0.0; }
    }
    return -1.0;
}

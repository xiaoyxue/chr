// Only the fixed, admitted counterexamples. Geometry is in world metres,
// with world up -Z; brdf() is the unmodified renderer GGX/Burley implementation.
struct ProfileParameters { configuration: vec4<u32>, factors: vec4<f32> };
@group(0) @binding(1) var<uniform> profile: ProfileParameters;
@group(0) @binding(2) var<storage, read_write> profile_samples: array<f32>;

@compute @workgroup_size(8,8)
fn profile_reference(@builtin(global_invocation_id) id: vec3<u32>) {
    let shape = profile.configuration.x;
    let size = profile.configuration.y;
    if id.x >= size || id.y >= size { return; }
    let uv = (vec2<f32>(id.xy)+0.5)/f32(size);
    let phi = 2.0*ENV_PI*uv.y;
    var p = vec3(2.0*uv.x-1.0,(2.0*uv.y-1.0)*0.6,2.0);
    var emitter_normal = vec3(0.0,0.0,-1.0);
    var area = 2.4;
    if shape == 1u {
        let radius = sqrt(uv.x);
        p = vec3(radius*cos(phi),radius*sin(phi),2.0);
        area = ENV_PI;
    } else if shape == 2u {
        let z = 1.0-2.0*uv.x;
        let radius = sqrt(1.0-z*z);
        emitter_normal = vec3(radius*cos(phi),radius*sin(phi),z);
        p = vec3(0.0,0.0,2.0)+emitter_normal;
        area = 4.0*ENV_PI;
    }
    p *= vec3(1.0,1.0,-1.0);
    emitter_normal *= vec3(1.0,1.0,-1.0);
    let n = vec3(0.0,0.0,-1.0);
    let nv = profile.factors.w;
    let v = vec3(sqrt(1.0-nv*nv),0.0,-nv);
    let l = normalize(p);
    let emitter_cosine = max(-dot(emitter_normal,l),0.0);
    let weight = area*emitter_cosine/dot(p,p)/(f32(size)*f32(size));
    profile_samples[id.x+size*id.y] = weight*brdf(
        n,v,l,vec3(profile.factors.x),profile.factors.y,profile.factors.z).x;
}

struct ComparisonConfig { shape: u32, samples: u32, size: u32, sides: u32 };
@group(0) @binding(1) var<uniform> comparison_config: ComparisonConfig;
@group(0) @binding(2) var<storage, read_write> comparison_output: array<vec4<f32>>;
var<workgroup> comparison_sum: array<f32,64>;

@compute @workgroup_size(64)
fn comparison_reference(
    @builtin(workgroup_id) pixel: vec3<u32>,
    @builtin(local_invocation_index) lane: u32
) {
    let size = comparison_config.size;
    let shape = comparison_config.shape;
    let p = comparison_surface(pixel.xy,size);
    let emitter = comparison_emitter(shape,p);
    if emitter >= 0.0 {
        if lane == 0u { comparison_output[pixel.x+size*pixel.y] = vec4(vec3(emitter),0.0); }
        return;
    }
    let center = vec3(0.0,0.0,2.0)-p;
    let distance = length(center);
    let axis = center/distance;
    let tangent = normalize(cross(vec3(0.0,1.0,0.0),axis));
    let bitangent = cross(axis,tangent);
    let cosine_max = sqrt(1.0-1.0/(distance*distance));
    let n = comparison_config.samples;
    let count = n*n;
    let view = vec3(sqrt(1.0-COMPARE_NV*COMPARE_NV),0.0,-COMPARE_NV);
    var sum = 0.0;
    for (var index = lane; index < count; index += 64u) {
        let uv = (vec2<f32>(f32(index/n),f32(index%n))+0.5)/f32(n);
        let phi = 2.0*ENV_PI*uv.y;
        var l = vec3(0.0);
        var weight = 0.0;
        if shape == 2u {
            // Exact outward-sphere cone; no tiny visible-cap surface undersampling.
            let cosine = 1.0-(1.0-cosine_max)*uv.x;
            let sine = sqrt(1.0-cosine*cosine);
            l = axis*cosine+sine*(tangent*cos(phi)+bitangent*sin(phi));
            weight = 2.0*ENV_PI*(1.0-cosine_max);
        } else {
            var sample = center+vec3(2.0*uv.x-1.0,(2.0*uv.y-1.0)*0.6,0.0);
            var area = 2.4;
            if shape == 1u {
                let radius = sqrt(uv.x);
                sample = center+vec3(radius*cos(phi),radius*sin(phi),0.0);
                area = ENV_PI;
            }
            l = normalize(sample);
            weight = area*max(l.z,0.0)/dot(sample,sample);
        }
        sum += weight*brdf(vec3(0.0,0.0,-1.0),view,l*vec3(1.0,1.0,-1.0),
            vec3(0.7),1.0,COMPARE_ROUGH).x;
    }
    comparison_sum[lane] = sum/f32(count);
    workgroupBarrier();
    for (var stride=32u; stride>0u; stride/=2u) {
        if lane<stride { comparison_sum[lane] += comparison_sum[lane+stride]; }
        workgroupBarrier();
    }
    if lane==0u {
        comparison_output[pixel.x+size*pixel.y] = vec4(vec3(comparison_sum[0]),1.0);
    }
}

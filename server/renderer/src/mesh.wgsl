struct Camera { clip_from_world: mat4x4<f32>, eye: vec4<f32>, options: vec4<u32> };
struct Object {
    model: mat4x4<f32>, normal: mat3x3<f32>, color: vec4<f32>,
    emissive_cutoff: vec4<f32>, factors: vec4<f32>, flags: vec4<u32>
};
struct Light {
    position_kind: vec4<f32>, direction_inner: vec4<f32>, emission_outer: vec4<f32>
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> object: Object;
@group(2) @binding(0) var base_map: texture_2d<f32>;
@group(2) @binding(1) var base_sampler: sampler;
@group(2) @binding(2) var mr_map: texture_2d<f32>;
@group(2) @binding(3) var mr_sampler: sampler;
@group(2) @binding(4) var normal_map: texture_2d<f32>;
@group(2) @binding(5) var normal_sampler: sampler;
@group(2) @binding(6) var emissive_map: texture_2d<f32>;
@group(2) @binding(7) var emissive_sampler: sampler;
@group(2) @binding(8) var ao_map: texture_2d<f32>;
@group(2) @binding(9) var ao_sampler: sampler;
@group(3) @binding(0) var<storage,read> lights: array<Light>;

struct Vertex {
    @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>, @location(3) tangent: vec4<f32>, @location(4) color: vec4<f32>
};
struct Surface {
    @builtin(position) clip: vec4<f32>, @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>, @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>, @location(4) color: vec4<f32>
};

@vertex
fn vs_main(vertex: Vertex) -> Surface {
    var out: Surface;
    let world = object.model * vec4(vertex.position,1.0);
    out.clip = camera.clip_from_world * world;
    out.position = world.xyz;
    out.normal = object.normal * vertex.normal;
    out.uv = vertex.uv;
    out.tangent = vec4((object.model * vec4(vertex.tangent.xyz,0.0)).xyz,
        vertex.tangent.w * select(1.0,-1.0,object.flags.w!=0u));
    out.color = vertex.color;
    return out;
}

fn pow5(x: f32) -> f32 { let x2=x*x; return x2*x2*x; }

// GGX with height-correlated Smith visibility and Burley diffuse.
fn brdf(n:vec3<f32>,v:vec3<f32>,l:vec3<f32>,base:vec3<f32>,metal:f32,rough:f32)->vec3<f32> {
    let nl=max(dot(n,l),0.0);
    if nl<=0.0 {return vec3(0.0);}
    let nv=max(dot(n,v),0.0001);
    let h=normalize(v+l);
    let nh=max(dot(n,h),0.0);
    let vh=max(dot(v,h),0.0);
    let alpha=rough*rough;
    let a2=alpha*alpha;
    let d0=nh*nh*(a2-1.0)+1.0;
    let distribution=a2/(3.14159265*d0*d0);
    let visibility=0.5/(nl*sqrt(nv*nv*(1.0-a2)+a2)+nv*sqrt(nl*nl*(1.0-a2)+a2));
    let f0=mix(vec3(0.04),base,metal);
    let fresnel=f0+(vec3(1.0)-f0)*pow5(1.0-vh);
    let fd90=0.5+2.0*rough*vh*vh;
    let diffuse=(1.0+(fd90-1.0)*pow5(1.0-nl))*(1.0+(fd90-1.0)*pow5(1.0-nv))/3.14159265;
    return nl * (base*(1.0-metal)*(vec3(1.0)-fresnel)*diffuse + fresnel*distribution*visibility);
}

@fragment
fn fs_main(surface:Surface,@builtin(front_facing) front_facing:bool) -> @location(0) vec4<f32> {
    if camera.options.x==0u { return vec4(object.color.rgb,1.0); }
    let base = textureSample(base_map,base_sampler,surface.uv)*object.color*surface.color;
    let mr = textureSample(mr_map,mr_sampler,surface.uv);
    let normal_texel = textureSample(normal_map,normal_sampler,surface.uv).xyz*2.0-vec3(1.0);
    let emissive = textureSample(emissive_map,emissive_sampler,surface.uv).rgb*object.emissive_cutoff.rgb;
    // Occlusion maps affect indirect lighting, not punctual direct light.
    let occlusion = textureSample(ao_map,ao_sampler,surface.uv).r;
    let px=dpdx(surface.position); let py=dpdy(surface.position);
    let tx=dpdx(surface.uv); let ty=dpdy(surface.uv);
    let front = select(front_facing,!front_facing,object.flags.w!=0u);
    if !front && object.flags.z==0u {discard;}
    if object.emissive_cutoff.w>=0.0 && base.a<object.emissive_cutoff.w {discard;}
    if object.flags.y!=0u {return vec4(min(base.rgb+emissive,vec3(65504.0)),1.0);}
    let v=normalize(camera.eye.xyz-surface.position);
    var n=surface.normal;
    if dot(n,n)<1e-12 {
        n=normalize(cross(px,py));
        n=select(-n,n,dot(n,v)>=0.0);
    } else {
        n=normalize(n);
        if !front {n=-n;}
    }
    if object.flags.x!=0u {
        var t=surface.tangent.xyz;
        var b=vec3(0.0);
        if dot(t,t)>1e-12 {
            t=normalize(t-n*dot(n,t));
            b=cross(n,t)*surface.tangent.w;
        } else {
            let determinant=tx.x*ty.y-tx.y*ty.x;
            if abs(determinant)>1e-10 {
                t=(px*ty.y-py*tx.y)/determinant;
                b=(py*tx.x-px*ty.x)/determinant;
                let scale=inverseSqrt(max(dot(t,t),dot(b,b)));
                t*=scale; b*=scale;
            }
        }
        if dot(t,t)>1e-12 && dot(b,b)>1e-12 {
            n=normalize(mat3x3(t,b,n)*vec3(normal_texel.xy*object.factors.z,normal_texel.z));
        }
    }
    let metal=clamp(object.factors.x*mr.b,0.0,1.0);
    // Finite raster specular lobe; roughness zero is not a sampled delta mirror.
    let rough=clamp(object.factors.y*mr.g,0.045,1.0);
    var radiance=emissive;
    for (var i=0u;i<camera.options.y;i++) {
        let light=lights[i];
        var l=-light.direction_inner.xyz;
        var attenuation=1.0;
        if light.position_kind.w>0.5 {
            let delta=light.position_kind.xyz-surface.position;
            let distance2=dot(delta,delta);
            // An ideal point source has no defined direction at its exact centre.
            if distance2==0.0 {continue;}
            l=delta*inverseSqrt(distance2);
            attenuation=1.0/distance2;
            if light.position_kind.w>1.5 {
                let cosine=dot(-l,light.direction_inner.xyz);
                if cosine<=light.emission_outer.w {continue;}
                if cosine<light.direction_inner.w {
                    attenuation*=smoothstep(light.emission_outer.w,light.direction_inner.w,cosine);
                }
            }
        }
        radiance+=light.emission_outer.rgb*attenuation*brdf(n,v,l,base.rgb,metal,rough);
    }
    return vec4(clamp(radiance,vec3(0.0),vec3(65504.0)),1.0);
}

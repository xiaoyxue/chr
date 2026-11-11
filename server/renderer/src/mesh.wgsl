struct Camera { clip_from_world: mat4x4<f32>, eye: vec4<f32>, options: vec4<u32> };
struct Object {
    model: mat4x4<f32>, normal: mat3x3<f32>, color: vec4<f32>,
    emissive_cutoff: vec4<f32>, factors: vec4<f32>, flags: vec4<u32>
};
struct Light {
    position_kind: vec4<f32>, direction_inner: vec4<f32>, emission_outer: vec4<f32>,
    shadow_matrix: mat4x4<f32>, shadow_params: vec4<f32>,
    shadow_depth: vec4<f32>, pcss: vec4<f32>, pcss_samples: vec4<f32>
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
@group(3) @binding(1) var shadow_maps: texture_depth_2d_array;
@group(3) @binding(2) var shadow_sampler: sampler_comparison;

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

struct ShadowSurface {
    @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) alpha: f32,
    @location(2) position: vec3<f32>
};

@vertex
fn vs_shadow(vertex:Vertex)->ShadowSurface {
    var out:ShadowSurface;
    let world=object.model*vec4(vertex.position,1.0);
    out.clip=camera.clip_from_world*world;
    out.uv=vertex.uv;
    out.alpha=vertex.color.a;
    out.position=world.xyz;
    return out;
}

fn shadow_alpha_test(surface:ShadowSurface,front_facing:bool) {
    let alpha=textureSample(base_map,base_sampler,surface.uv).a*object.color.a*surface.alpha;
    let front=select(front_facing,!front_facing,object.flags.w!=0u);
    if !front && object.flags.z==0u {discard;}
    if object.emissive_cutoff.w>=0.0 && alpha<object.emissive_cutoff.w {discard;}
}

@fragment
fn fs_shadow(surface:ShadowSurface,@builtin(front_facing) front_facing:bool) {
    shadow_alpha_test(surface,front_facing);
}

@fragment
fn fs_shadow_point(surface:ShadowSurface,@builtin(front_facing) front_facing:bool)->@builtin(frag_depth) f32 {
    shadow_alpha_test(surface,front_facing);
    let radial=distance(surface.position,camera.eye.xyz);
    if radial<bitcast<f32>(camera.options.y) {discard;}
    return radial/camera.eye.w;
}

struct CubeAddress { uv:vec2<f32>, face:u32 };

fn cube_coordinates(d:vec3<f32>)->CubeAddress {
    let a=abs(d);
    var uv=vec2(0.0);
    var face=0u;
    if a.x>=a.y && a.x>=a.z {
        if d.x>=0.0 {uv=d.yz/a.x;} else {uv=vec2(-d.y,d.z)/a.x;face=1u;}
    } else if a.y>=a.z {
        if d.y>=0.0 {uv=vec2(-d.x,d.z)/a.y;face=2u;} else {uv=d.xz/a.y;face=3u;}
    } else {
        if d.z>=0.0 {uv=-d.xy/a.z;face=4u;} else {uv=vec2(d.x,-d.y)/a.z;face=5u;}
    }
    return CubeAddress(uv*0.5+vec2(0.5),face);
}

fn cube_direction(face:u32,uv:vec2<f32>)->vec3<f32> {
    let p=uv*2.0-vec2(1.0);
    switch face {
        case 0u: {return vec3(1.0,p.x,p.y);}
        case 1u: {return vec3(-1.0,-p.x,p.y);}
        case 2u: {return vec3(-p.x,1.0,p.y);}
        case 3u: {return vec3(p.x,-1.0,p.y);}
        case 4u: {return vec3(-p.x,-p.y,1.0);}
        default: {return vec3(p.x,-p.y,-1.0);}
    }
}

// Raw radial depth and receiver-plane comparison depth at the actual fetched texel.
fn point_depths(light:Light,direction:vec3<f32>,receiver:vec3<f32>,plane:vec3<f32>)->vec2<f32> {
    let address=cube_coordinates(direction);
    let size=vec2<f32>(textureDimensions(shadow_maps));
    let pixel=clamp(vec2<i32>(address.uv*size),vec2(0),vec2<i32>(size)-vec2(1));
    let ray=normalize(cube_direction(address.face,(vec2<f32>(pixel)+vec2(0.5))/size));
    let numerator=dot(receiver,plane);
    let denominator=dot(ray,plane);
    if numerator*denominator<=0.0 {return vec2(1.0,0.0);}
    let depth=textureLoad(shadow_maps,pixel,i32(light.shadow_params.x)+i32(address.face),0);
    return vec2(depth,min(1.0,numerator/denominator/light.shadow_depth.y-light.shadow_params.y));
}

fn point_compare(light:Light,direction:vec3<f32>,receiver:vec3<f32>,plane:vec3<f32>)->f32 {
    let address=cube_coordinates(direction);
    let size=vec2<f32>(textureDimensions(shadow_maps));
    let pixel=address.uv*size-vec2(0.5);
    let base=floor(pixel);
    let fraction=fract(pixel);
    var result=0.0;
    for(var y=0;y<2;y++) {
        for(var x=0;x<2;x++) {
            // Remap even bilinear neighbours across faces, instead of clamping UVs.
            let ray=cube_direction(address.face,(base+vec2(f32(x),f32(y))+vec2(0.5))/size);
            let depths=point_depths(light,ray,receiver,plane);
            let weight=select(1.0-fraction.x,fraction.x,x==1)*select(1.0-fraction.y,fraction.y,y==1);
            result+=weight*select(0.0,1.0,depths.y<=depths.x);
        }
    }
    return result;
}

fn point_visibility(light:Light,receiver:vec3<f32>,px:vec3<f32>,py:vec3<f32>)->f32 {
    let radius=length(receiver);
    if radius<light.shadow_depth.x || radius>=light.shadow_depth.y {return 1.0;}
    let direction=receiver/radius;
    let plane=cross(px,py);
    if camera.options.z==1u || (camera.options.z==3u && light.pcss.x==0.0) {
        let depths=point_depths(light,direction,receiver,plane);
        return select(0.0,1.0,depths.y<=depths.x);
    }
    let up=select(vec3(0.0,0.0,1.0),vec3(0.0,1.0,0.0),abs(direction.z)>0.99);
    let tangent=normalize(cross(direction,up));
    let bitangent=cross(direction,tangent);
    let texel=2.0/f32(textureDimensions(shadow_maps).x);
    if camera.options.z==2u {
        var visibility=0.0;
        let step=texel*light.shadow_params.w;
        for(var y=-1;y<=1;y++) {
            for(var x=-1;x<=1;x++) {
                visibility+=point_compare(light,direction+step*(f32(x)*tangent+f32(y)*bitangent),receiver,plane);
            }
        }
        return visibility/9.0;
    }
    let near=light.shadow_depth.z;
    let search=min(light.pcss.x*max(radius-near,0.0)/(near*radius),texel*light.pcss.y);
    let blocker_samples=u32(light.pcss_samples.x);
    var sum=0.0;
    var count=0u;
    for(var i=0u;i<blocker_samples;i++) {
        let p=shadow_disk(i,blocker_samples)*search;
        let depths=point_depths(light,direction+p.x*tangent+p.y*bitangent,receiver,plane);
        if depths.x<1.0 && depths.x<depths.y {
            sum+=depths.x*light.shadow_depth.y;
            count++;
        }
    }
    if count==0u {return 1.0;}
    let blocker=sum/f32(count);
    let penumbra=min(light.pcss.x*max(radius-blocker,0.0)/(blocker*radius),texel*light.pcss.z);
    let filter_samples=u32(light.pcss_samples.y);
    var visibility=0.0;
    for(var i=0u;i<filter_samples;i++) {
        let p=shadow_disk(i,filter_samples)*penumbra;
        visibility+=point_compare(light,direction+p.x*tangent+p.y*bitangent,receiver,plane);
    }
    return visibility/f32(filter_samples);
}

fn linear_shadow_depth(light:Light,depth:f32)->f32 {
    let near=light.shadow_depth.x;
    let far=light.shadow_depth.y;
    if light.position_kind.w>0.5 {
        return near/(1.0-depth+depth*(near/far));
    }
    return depth*far;
}

fn shadow_disk(index:u32,count:u32)->vec2<f32> {
    let angle=f32(index)*2.39996323;
    return sqrt(f32(index)/f32(count))*vec2(cos(angle),sin(angle));
}

fn bounded_shadow_radius(radius:vec2<f32>,limit:f32,size:vec2<f32>)->vec2<f32> {
    let largest=max(radius.x*size.x,radius.y*size.y);
    return radius*min(1.0,limit/max(largest,1e-20));
}

fn pcss_visibility(light:Light,uv:vec2<f32>,depth:f32,gradient:vec2<f32>)->f32 {
    let size=vec2<f32>(textureDimensions(shadow_maps));
    let layer=i32(light.shadow_params.x);
    let receiver=linear_shadow_depth(light,clamp(depth,0.0,1.0));
    let near=light.shadow_depth.x;
    var projection=light.shadow_depth.zw;
    var search_world=light.pcss.x*receiver;
    if light.position_kind.w>0.5 {
        projection/=receiver;
        search_world=light.pcss.x*max(receiver-near,0.0)/near;
    }
    let search=bounded_shadow_radius(search_world*projection,light.pcss.y,size);
    let blocker_samples=u32(light.pcss_samples.x);
    var blocker_sum=0.0;
    var blockers=0u;
    for(var i=0u;i<blocker_samples;i++) {
        let sample_uv=uv+shadow_disk(i,blocker_samples)*search;
        if any(sample_uv<vec2(0.0)) || any(sample_uv>=vec2(1.0)) {continue;}
        let pixel=vec2<i32>(sample_uv*size);
        let offset=(vec2<f32>(pixel)+vec2(0.5))/size-uv;
        let stored=textureLoad(shadow_maps,pixel,layer,0);
        if stored<1.0 && stored<depth+dot(gradient,offset) {
            // Average reconstructed distances, never nonlinear perspective depths.
            blocker_sum+=linear_shadow_depth(light,stored);
            blockers++;
        }
    }
    if blockers==0u {return 1.0;}
    let blocker=blocker_sum/f32(blockers);
    var penumbra=light.pcss.x*max(receiver-blocker,0.0);
    if light.position_kind.w>0.5 {penumbra/=blocker;}
    let radius=bounded_shadow_radius(penumbra*projection,light.pcss.z,size);
    let filter_samples=u32(light.pcss_samples.y);
    let receiver_bias=dot(abs(gradient),1.0/size);
    var visibility=0.0;
    for(var i=0u;i<filter_samples;i++) {
        let offset=shadow_disk(i,filter_samples)*radius;
        let sample_uv=uv+offset;
        if any(sample_uv<vec2(0.0)) || any(sample_uv>=vec2(1.0)) {
            visibility+=1.0;
        } else {
            visibility+=textureSampleCompareLevel(shadow_maps,shadow_sampler,sample_uv,layer,
                depth+dot(gradient,offset)-receiver_bias);
        }
    }
    return visibility/f32(filter_samples);
}

fn shadow_visibility(light:Light,position:vec3<f32>,normal:vec3<f32>,px:vec3<f32>,py:vec3<f32>)->f32 {
    if camera.options.z==0u || light.shadow_params.x<0.0 {return 1.0;}
    if light.position_kind.w==1.0 {
        return point_visibility(light,position+normal*light.shadow_params.z-light.position_kind.xyz,px,py);
    }
    let clip=light.shadow_matrix*vec4(position+normal*light.shadow_params.z,1.0);
    if clip.w<=0.0 {return 1.0;}
    let ndc=clip.xyz/clip.w;
    let uv=ndc.xy*vec2(0.5,-0.5)+vec2(0.5);
    if any(uv<vec2(0.0)) || any(uv>=vec2(1.0)) || ndc.z<0.0 || ndc.z>1.0 {return 1.0;}
    let layer=i32(light.shadow_params.x);
    let depth=ndc.z-light.shadow_params.y;
    let size=textureDimensions(shadow_maps);
    let hx=light.shadow_matrix*vec4(px,0.0);
    let hy=light.shadow_matrix*vec4(py,0.0);
    let dx=(hx.xyz-ndc*hx.w)/clip.w;
    let dy=(hy.xyz-ndc*hy.w)/clip.w;
    let ux=dx.xy*vec2(0.5,-0.5);
    let uy=dy.xy*vec2(0.5,-0.5);
    let determinant=ux.x*uy.y-ux.y*uy.x;
    var gradient=vec2(0.0);
    if abs(determinant)>1e-12 {
        gradient=vec2(dx.z*uy.y-dy.z*ux.y,ux.x*dy.z-uy.x*dx.z)/determinant;
    }
    if camera.options.z==1u || (camera.options.z==3u && light.pcss.x==0.0) {
        let pixel=clamp(vec2<i32>(uv*vec2<f32>(size)),vec2(0),vec2<i32>(size)-vec2(1));
        let offset=(vec2<f32>(pixel)+vec2(0.5))/vec2<f32>(size)-uv;
        return select(0.0,1.0,depth+dot(gradient,offset)<=textureLoad(shadow_maps,pixel,layer,0));
    }
    if camera.options.z==3u {return pcss_visibility(light,uv,depth,gradient);}
    var visibility=0.0;
    let step=light.shadow_params.w/vec2<f32>(size);
    // Correct the receiver plane per tap; cover the bilinear footprint's depth slope.
    let receiver_bias=dot(abs(gradient),1.0/vec2<f32>(size));
    for(var y=-1;y<=1;y++) {
        for(var x=-1;x<=1;x++) {
            let offset=vec2<f32>(f32(x),f32(y))*step;
            visibility+=textureSampleCompareLevel(shadow_maps,shadow_sampler,uv+offset,layer,depth+dot(gradient,offset)-receiver_bias);
        }
    }
    return visibility/9.0;
}

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
    let shadow_normal=n;
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
        if dot(n,l)>0.0 {
            radiance+=light.emission_outer.rgb*attenuation*shadow_visibility(light,surface.position,shadow_normal,px,py)*brdf(n,v,l,base.rgb,metal,rough);
        }
    }
    return vec4(clamp(radiance,vec3(0.0),vec3(65504.0)),1.0);
}

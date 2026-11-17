struct Camera {
    clip_from_world: mat4x4<f32>, eye: vec4<f32>, options: vec4<u32>,
    optical_depth: vec4<f32>
};
struct Object {
    model: mat4x4<f32>, normal: mat3x3<f32>, color: vec4<f32>,
    emissive_cutoff: vec4<f32>, factors: vec4<f32>, flags: vec4<u32>
};
struct Light {
    position_kind: vec4<f32>, direction_inner: vec4<f32>, emission_outer: vec4<f32>,
    shadow_matrix: array<mat4x4<f32>,4>, shadow_params: vec4<f32>,
    shadow_depth: array<vec4<f32>,4>, pcss: vec4<f32>, pcss_samples: vec4<f32>,
    cascade_splits: vec4<f32>, cascade_params: vec4<f32>, area: array<vec4<f32>,2>
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
// Lighting functions take a light index and read fields from `lights` directly. A
// by-value Light copy indexed by a runtime cascade spills the whole struct to local
// memory in every fragment; that made shaded frames about three times slower.
@group(3) @binding(0) var<storage,read> lights: array<Light>;
@group(3) @binding(1) var shadow_maps: texture_depth_2d_array;
@group(3) @binding(2) var shadow_sampler: sampler_comparison;
struct Environment {
    sh:array<vec4<f32>,9>, info:vec4<f32>, flags:vec4<f32>, rays:array<vec4<f32>,3>,
    constant:vec4<f32>
};
@group(3) @binding(3) var environment_source:texture_2d<f32>;
@group(3) @binding(4) var environment_filtered:texture_2d_array<f32>;
@group(3) @binding(5) var environment_brdf:texture_2d<f32>;
@group(3) @binding(6) var<uniform> environment:Environment;
// Pinned official LTC GGX table, Heitz et al. SIGGRAPH 2016; see ltc/LICENSE.ltc.
struct LtcEntry { matrix:vec4<f32>, amplitude:vec4<f32> };
@group(3) @binding(7) var<storage,read> ltc_table:array<LtcEntry>;

fn environment_direction(d:vec3<f32>)->vec3<f32> {
    let c=environment.info.y;
    let s=environment.info.z;
    // Inverse yaw about world up (-Z), shared by background and illumination.
    return vec3(c*d.x-s*d.y,s*d.x+c*d.y,d.z);
}

fn environment_diffuse(n:vec3<f32>)->vec3<f32> {
    let basis=array<f32,9>(0.2820947918,0.4886025119*n.x,0.4886025119*n.y,0.4886025119*n.z,
        1.0925484306*n.x*n.y,1.0925484306*n.y*n.z,1.0925484306*n.x*n.z,
        0.3153915653*(3.0*n.z*n.z-1.0),0.5462742153*(n.x*n.x-n.y*n.y));
    var result=vec3(0.0);
    for(var i=0u;i<9u;i++) {result+=environment.sh[i].rgb*basis[i];}
    return max(result,vec3(0.0));
}

fn environment_lighting(n:vec3<f32>,v:vec3<f32>,base:vec3<f32>,metal:f32,rough:f32,ao:f32)->vec3<f32> {
    let sky=environment.flags.x!=0.0 && environment.info.x!=0.0;
    let constant=environment.constant.rgb;
    if !sky && all(constant==vec3(0.0)) {return vec3(0.0);}
    let nv=max(dot(n,v),0.0001);
    let f0=mix(vec3(0.04),base,metal);
    let fresnel=f0+(max(vec3(1.0-rough),f0)-f0)*pow5(1.0-nv);
    // A uniform radiance L is its own diffuse (SH) and prefiltered specular radiance.
    var irradiance=constant;
    var prefiltered=constant;
    if sky {
        let gain=environment.flags.y*environment.info.x;
        irradiance+=environment_diffuse(environment_direction(n))*gain;
        let uv=panorama_uv(environment_direction(reflect(-v,n)));
        let level=rough*environment.info.w;
        let lower=i32(floor(level));
        let upper=min(lower+1,i32(environment.info.w));
        prefiltered+=mix(sample_float_layer(environment_filtered,uv,lower),
            sample_float_layer(environment_filtered,uv,upper),fract(level))*gain;
    }
    let dfg=sample_float_image(environment_brdf,vec2(nv,rough),false).rg;
    let diffuse=(vec3(1.0)-fresnel)*(1.0-metal)*base*irradiance;
    let specular=prefiltered*(f0*dfg.x+vec3(dfg.y));
    return (diffuse+specular)*ao;
}

@vertex
fn vs_sky(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32> {
    let p=vec2(f32((index<<1u)&2u),f32(index&2u));
    return vec4(p*2.0-vec2(1.0),1.0,1.0);
}

@fragment
fn fs_sky(@builtin(position) pixel:vec4<f32>)->@location(0) vec4<f32> {
    if environment.info.x==0.0 {return vec4(0.0,0.0,0.0,1.0);}
    let ray=normalize(environment.rays[0].xyz+environment.rays[1].xyz*pixel.x+environment.rays[2].xyz*pixel.y);
    let radiance=(sample_float_image(environment_source,panorama_uv(environment_direction(ray)),true)
        *environment.flags.y)*environment.info.x;
    return vec4(clamp(radiance,vec3(0.0),vec3(65504.0)),1.0);
}

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
fn point_depths(li:u32,direction:vec3<f32>,receiver:vec3<f32>,plane:vec3<f32>)->vec2<f32> {
    let address=cube_coordinates(direction);
    let size=vec2<f32>(textureDimensions(shadow_maps));
    let pixel=clamp(vec2<i32>(address.uv*size),vec2(0),vec2<i32>(size)-vec2(1));
    let ray=normalize(cube_direction(address.face,(vec2<f32>(pixel)+vec2(0.5))/size));
    let numerator=dot(receiver,plane);
    let denominator=dot(ray,plane);
    if numerator*denominator<=0.0 {return vec2(1.0,0.0);}
    let depth=textureLoad(shadow_maps,pixel,i32(lights[li].shadow_params.x)+i32(address.face),0);
    return vec2(depth,min(1.0,numerator/denominator/lights[li].shadow_depth[0].y-lights[li].shadow_params.y));
}

fn point_compare(li:u32,direction:vec3<f32>,receiver:vec3<f32>,plane:vec3<f32>)->f32 {
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
            let depths=point_depths(li,ray,receiver,plane);
            let weight=select(1.0-fraction.x,fraction.x,x==1)*select(1.0-fraction.y,fraction.y,y==1);
            result+=weight*select(0.0,1.0,depths.y<=depths.x);
        }
    }
    return result;
}

fn point_visibility(li:u32,receiver:vec3<f32>,px:vec3<f32>,py:vec3<f32>)->f32 {
    let mode=u32(lights[li].pcss.w);
    let radius=length(receiver);
    if radius<lights[li].shadow_depth[0].x || radius>=lights[li].shadow_depth[0].y {return 1.0;}
    let direction=receiver/radius;
    let plane=cross(px,py);
    if mode==1u || (mode==3u && lights[li].pcss.x==0.0) {
        let depths=point_depths(li,direction,receiver,plane);
        return select(0.0,1.0,depths.y<=depths.x);
    }
    let up=select(vec3(0.0,0.0,1.0),vec3(0.0,1.0,0.0),abs(direction.z)>0.99);
    let tangent=normalize(cross(direction,up));
    let bitangent=cross(direction,tangent);
    let texel=2.0/f32(textureDimensions(shadow_maps).x);
    if mode==2u {
        var visibility=0.0;
        let side=u32(sqrt(lights[li].pcss_samples.z));
        let center=(f32(side)-1.0)*0.5;
        let step=texel*lights[li].shadow_params.w*2.0/max(f32(side)-1.0,1.0);
        for(var y=0u;y<side;y++) {
            for(var x=0u;x<side;x++) {
                visibility+=point_compare(li,direction+step*((f32(x)-center)*tangent+(f32(y)-center)*bitangent),receiver,plane);
            }
        }
        return visibility/lights[li].pcss_samples.z;
    }
    let near=lights[li].shadow_depth[0].z;
    let search=min(lights[li].pcss.x*max(radius-near,0.0)/(near*radius),texel*lights[li].pcss.y);
    let blocker_samples=u32(lights[li].pcss_samples.x);
    var sum=0.0;
    var count=0u;
    for(var i=0u;i<blocker_samples;i++) {
        let p=shadow_disk(i,blocker_samples)*search;
        let depths=point_depths(li,direction+p.x*tangent+p.y*bitangent,receiver,plane);
        if depths.x<1.0 && depths.x<depths.y {
            sum+=depths.x*lights[li].shadow_depth[0].y;
            count++;
        }
    }
    if count==0u {return 1.0;}
    let blocker=sum/f32(count);
    let penumbra=min(lights[li].pcss.x*max(radius-blocker,0.0)/(blocker*radius),texel*lights[li].pcss.z);
    let filter_samples=u32(lights[li].pcss_samples.y);
    var visibility=0.0;
    for(var i=0u;i<filter_samples;i++) {
        let p=shadow_disk(i,filter_samples)*penumbra;
        visibility+=point_compare(li,direction+p.x*tangent+p.y*bitangent,receiver,plane);
    }
    return visibility/f32(filter_samples);
}

fn linear_shadow_depth(li:u32,depth:f32,cascade:u32)->f32 {
    let near=lights[li].shadow_depth[cascade].x;
    let far=lights[li].shadow_depth[cascade].y;
    if lights[li].position_kind.w>0.5 {
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

fn pcss_visibility(li:u32,uv:vec2<f32>,depth:f32,gradient:vec2<f32>,cascade:u32)->f32 {
    let size=vec2<f32>(textureDimensions(shadow_maps));
    let layer=i32(lights[li].shadow_params.x)+i32(cascade);
    let receiver=linear_shadow_depth(li,clamp(depth,0.0,1.0),cascade);
    let near=lights[li].shadow_depth[cascade].x;
    var projection=lights[li].shadow_depth[cascade].zw;
    var search_world=lights[li].pcss.x*receiver;
    if lights[li].position_kind.w>0.5 {
        projection/=receiver;
        search_world=lights[li].pcss.x*max(receiver-near,0.0)/near;
    }
    let search=bounded_shadow_radius(search_world*projection,lights[li].pcss.y,size);
    let blocker_samples=u32(lights[li].pcss_samples.x);
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
            blocker_sum+=linear_shadow_depth(li,stored,cascade);
            blockers++;
        }
    }
    if blockers==0u {return 1.0;}
    let blocker=blocker_sum/f32(blockers);
    var penumbra=lights[li].pcss.x*max(receiver-blocker,0.0);
    if lights[li].position_kind.w>0.5 {penumbra/=blocker;}
    let radius=bounded_shadow_radius(penumbra*projection,lights[li].pcss.z,size);
    let filter_samples=u32(lights[li].pcss_samples.y);
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

fn planar_visibility(li:u32,position:vec3<f32>,normal:vec3<f32>,px:vec3<f32>,py:vec3<f32>,cascade:u32)->f32 {
    let mode=u32(lights[li].pcss.w);
    let matrix=lights[li].shadow_matrix[cascade];
    let clip=matrix*vec4(position+normal*lights[li].shadow_params.z,1.0);
    if clip.w<=0.0 {return 1.0;}
    let ndc=clip.xyz/clip.w;
    let uv=ndc.xy*vec2(0.5,-0.5)+vec2(0.5);
    if any(uv<vec2(0.0)) || any(uv>=vec2(1.0)) || ndc.z<0.0 || ndc.z>1.0 {return 1.0;}
    let layer=i32(lights[li].shadow_params.x)+i32(cascade);
    var depth=ndc.z-lights[li].shadow_params.y;
    if lights[li].position_kind.w==2.0 {
        // Perspective spot depth is nonlinear: apply the bias to linear depth as a fraction
        // of the far distance, like point cubemaps, so a near plane fitted close to the
        // light cannot stretch it over many metres.
        let near=lights[li].shadow_depth[cascade].x;
        let far=lights[li].shadow_depth[cascade].y;
        let z=linear_shadow_depth(li,clamp(ndc.z,0.0,1.0),cascade)-lights[li].shadow_params.y*far;
        depth=far*(z-near)/(max(z,near)*(far-near));
    }
    let size=textureDimensions(shadow_maps);
    let hx=matrix*vec4(px,0.0);
    let hy=matrix*vec4(py,0.0);
    let dx=(hx.xyz-ndc*hx.w)/clip.w;
    let dy=(hy.xyz-ndc*hy.w)/clip.w;
    let ux=dx.xy*vec2(0.5,-0.5);
    let uy=dy.xy*vec2(0.5,-0.5);
    let determinant=ux.x*uy.y-ux.y*uy.x;
    var gradient=vec2(0.0);
    if abs(determinant)>1e-12 {
        gradient=vec2(dx.z*uy.y-dy.z*ux.y,ux.x*dy.z-uy.x*dx.z)/determinant;
    }
    if mode==1u || (mode==3u && lights[li].pcss.x==0.0) {
        let pixel=clamp(vec2<i32>(uv*vec2<f32>(size)),vec2(0),vec2<i32>(size)-vec2(1));
        let offset=(vec2<f32>(pixel)+vec2(0.5))/vec2<f32>(size)-uv;
        return select(0.0,1.0,depth+dot(gradient,offset)<=textureLoad(shadow_maps,pixel,layer,0));
    }
    if mode==3u {return pcss_visibility(li,uv,depth,gradient,cascade);}
    var visibility=0.0;
    let side=u32(sqrt(lights[li].pcss_samples.z));
    let center=(f32(side)-1.0)*0.5;
    let step=lights[li].shadow_params.w/vec2<f32>(size)*2.0/max(f32(side)-1.0,1.0);
    // Correct the receiver plane per tap; cover the bilinear footprint's depth slope.
    let receiver_bias=dot(abs(gradient),1.0/vec2<f32>(size));
    for(var y=0u;y<side;y++) {
        for(var x=0u;x<side;x++) {
            let offset=(vec2<f32>(f32(x),f32(y))-vec2(center))*step;
            visibility+=textureSampleCompareLevel(shadow_maps,shadow_sampler,uv+offset,layer,depth+dot(gradient,offset)-receiver_bias);
        }
    }
    return visibility/lights[li].pcss_samples.z;
}

fn shadow_visibility(li:u32,position:vec3<f32>,normal:vec3<f32>,px:vec3<f32>,py:vec3<f32>)->f32 {
    if lights[li].pcss.w==0.0 || lights[li].shadow_params.x<0.0 {return 1.0;}
    // Point and area lights share one centre cubemap.
    if lights[li].position_kind.w==1.0 || lights[li].position_kind.w==3.0 {
        return point_visibility(li,position+normal*lights[li].shadow_params.z-lights[li].position_kind.xyz,px,py);
    }
    let count=u32(lights[li].pcss_samples.w);
    if count<=1u {return planar_visibility(li,position,normal,px,py,0u);}
    let depth=dot(camera.optical_depth,vec4(position,1.0));
    if depth<lights[li].cascade_params.x || depth>=lights[li].cascade_splits[count-1u] {return 1.0;}
    var cascade=0u;
    while cascade+1u<count && depth>lights[li].cascade_splits[cascade] {cascade++;}
    let visibility=planar_visibility(li,position,normal,px,py,cascade);
    if lights[li].cascade_params.y==0.0 {return visibility;}
    var begin=lights[li].cascade_params.x;
    if cascade>0u {begin=lights[li].cascade_splits[cascade-1u];}
    let end=lights[li].cascade_splits[cascade];
    let blend_start=end-(end-begin)*lights[li].cascade_params.y;
    if depth<=blend_start {return visibility;}
    var next=1.0;
    if cascade+1u<count {next=planar_visibility(li,position,normal,px,py,cascade+1u);}
    return mix(visibility,next,smoothstep(blend_start,end,depth));
}

// Bilinear texel-centre lookup at (roughness, sqrt(1 - NdotV)) on the 64x64 table.
fn ltc_lookup(rough:f32,nv:f32)->LtcEntry {
    let uv=clamp(vec2(rough,sqrt(1.0-nv)),vec2(0.0),vec2(1.0))*63.0;
    let p=min(vec2<u32>(floor(uv)),vec2(63u));
    let q=min(p+vec2(1u),vec2(63u));
    let f=fract(uv);
    let a=ltc_table[p.x+64u*p.y];
    let b=ltc_table[q.x+64u*p.y];
    let c=ltc_table[p.x+64u*q.y];
    let d=ltc_table[q.x+64u*q.y];
    return LtcEntry(mix(mix(a.matrix,b.matrix,f.x),mix(c.matrix,d.matrix,f.x),f.y),
        mix(mix(a.amplitude,b.amplitude,f.x),mix(c.amplitude,d.amplitude,f.x),f.y));
}

// Cosine-weighted edge integral; the polygon total is divided by 2*pi.
fn ltc_edge(a_in:vec3<f32>,b_in:vec3<f32>)->f32 {
    let a=normalize(a_in);
    let b=normalize(b_in);
    let c=cross(a,b);
    let s=length(c);
    if s<=1e-7 {return 0.0;}
    return c.z*atan2(s,dot(a,b))/s;
}

struct EdgeSum { first:vec3<f32>, previous:vec3<f32>, sum:f32, count:u32 };
fn edge_push(state:EdgeSum,p:vec3<f32>)->EdgeSum {
    var next=state;
    if next.count==0u {next.first=p;} else {next.sum+=ltc_edge(next.previous,p);}
    next.previous=p;
    next.count++;
    return next;
}

// Streams physically clipped vertices; the LTC copy is clipped again after transforming.
struct AreaIntegral { first:vec3<f32>, previous:vec3<f32>, count:u32, diffuse:f32, specular:EdgeSum };
fn area_append(state:AreaIntegral,p:vec3<f32>,m:mat3x3<f32>)->AreaIntegral {
    var next=state;
    if next.count==0u {
        next.first=p;
    } else {
        next.diffuse+=ltc_edge(next.previous,p);
        let a=m*next.previous;
        let b=m*p;
        if a.z>0.0 {next.specular=edge_push(next.specular,a);}
        if (a.z>0.0)!=(b.z>0.0) {next.specular=edge_push(next.specular,a+(b-a)*(a.z/(a.z-b.z)));}
    }
    next.previous=p;
    next.count++;
    return next;
}

fn perpendicular(d:vec3<f32>)->vec3<f32> {
    return normalize(cross(d,select(vec3(0.0,0.0,1.0),vec3(1.0,0.0,0.0),abs(d.z)>0.9)));
}

const AREA_CURVE_SEGMENTS:u32=64u;

// Unoccluded ltc-plausible-v1 emitter: Schlick LTC specular plus Lambert*(1-F0) diffuse.
fn area_lighting(li:u32,x:vec3<f32>,n:vec3<f32>,v:vec3<f32>,base:vec3<f32>,metal:f32,rough:f32)->vec3<f32> {
    let shape=u32(lights[li].emission_outer.w);
    var center=lights[li].position_kind.xyz-x;
    var normal=lights[li].direction_inner.xyz;
    var u=lights[li].area[0].xyz;
    var extent=lights[li].area[0].w;
    if shape==2u {
        let d2=dot(center,center);
        // Outward emission never reaches a receiver inside or on the sphere.
        if d2<=extent*extent {return vec3(0.0);}
        // Integrate the receiver-dependent tangent silhouette, not a disk at the centre.
        let factor=1.0-extent*extent/d2;
        normal=-center*inverseSqrt(d2);
        u=perpendicular(normal);
        center*=factor;
        extent*=sqrt(factor);
    } else {
        let side=-dot(normal,center);
        if side==0.0 || (side<0.0 && lights[li].direction_inner.w==0.0) {return vec3(0.0);}
    }
    let w=cross(normal,u);
    let nv=clamp(dot(n,v),1e-4,1.0);
    var t1=v-n*dot(v,n);
    t1=select(perpendicular(n),normalize(t1),dot(t1,t1)>1e-12);
    let t2=cross(n,t1);
    let entry=ltc_lookup(rough,nv);
    let t=entry.matrix;
    let m=mat3x3(vec3(t.x,0.0,t.y),vec3(0.0,1.0,0.0),vec3(t.z,0.0,t.w));
    let count=select(AREA_CURVE_SEGMENTS,4u,shape==0u);
    var integral:AreaIntegral;
    var previous=vec3(0.0);
    for(var i=0u;i<=count;i++) {
        var offset=vec3(0.0);
        if shape==0u {
            let k=i%4u;
            offset=select(-1.0,1.0,k==1u || k==2u)*extent*u+select(-1.0,1.0,k>=2u)*lights[li].area[1].x*w;
        } else {
            let angle=6.283185307*f32(i%count)/f32(count);
            offset=extent*(cos(angle)*u+sin(angle)*w);
        }
        let world=center+offset;
        let p=vec3(dot(world,t1),dot(world,t2),dot(world,n));
        if i>0u {
            if previous.z>0.0 {integral=area_append(integral,previous,m);}
            if (previous.z>0.0)!=(p.z>0.0) {
                integral=area_append(integral,previous+(p-previous)*(previous.z/(previous.z-p.z)),m);
            }
        }
        previous=p;
    }
    if integral.count==0u {return vec3(0.0);}
    integral=area_append(integral,integral.first,m);
    // The clipped horizon's closing edge has a nonzero azimuth integral.
    if integral.specular.count>0u {integral.specular=edge_push(integral.specular,integral.specular.first);}
    let lambert=abs(integral.diffuse)/6.283185307;
    let specular=abs(integral.specular.sum)/6.283185307;
    let f0=mix(vec3(0.04),base,metal);
    return lights[li].emission_outer.rgb*(specular*(f0*entry.amplitude.x+(vec3(1.0)-f0)*entry.amplitude.y)
        +base*(1.0-metal)*(vec3(1.0)-f0)*lambert);
}

// Unoccluded direct contribution of one light; visibility is applied by the caller.
fn direct_light(li:u32,x:vec3<f32>,n:vec3<f32>,v:vec3<f32>,base:vec3<f32>,metal:f32,rough:f32)->vec3<f32> {
    if lights[li].position_kind.w>2.5 {return area_lighting(li,x,n,v,base,metal,rough);}
    var l=-lights[li].direction_inner.xyz;
    var attenuation=1.0;
    if lights[li].position_kind.w>0.5 {
        let delta=lights[li].position_kind.xyz-x;
        let distance2=dot(delta,delta);
        // An ideal point source has no defined direction at its exact centre.
        if distance2==0.0 {return vec3(0.0);}
        l=delta*inverseSqrt(distance2);
        attenuation=1.0/distance2;
        if lights[li].position_kind.w>1.5 {
            let cosine=dot(-l,lights[li].direction_inner.xyz);
            if cosine<=lights[li].emission_outer.w {return vec3(0.0);}
            if cosine<lights[li].direction_inner.w {
                attenuation*=smoothstep(lights[li].emission_outer.w,lights[li].direction_inner.w,cosine);
            }
        }
    }
    if dot(n,l)<=0.0 {return vec3(0.0);}
    return lights[li].emission_outer.rgb*attenuation*brdf(n,v,l,base,metal,rough);
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
fn fs_wire() -> @location(0) vec4<f32> {
    return vec4(1.0);
}

// Ground decals: unlit, premultiplied over the opaque scene (the pipeline blends
// with ONE, ONE_MINUS_SRC_ALPHA). Sampling precedes discards for uniform control flow.
@fragment
fn fs_decal(surface:Surface,@builtin(front_facing) front_facing:bool) -> @location(0) vec4<f32> {
    let base = textureSample(base_map,base_sampler,surface.uv)*object.color*surface.color;
    let emissive = textureSample(emissive_map,emissive_sampler,surface.uv).rgb*object.emissive_cutoff.rgb;
    let front = select(front_facing,!front_facing,object.flags.w!=0u);
    if !front && object.flags.z==0u {discard;}
    if object.emissive_cutoff.w>=0.0 && base.a<object.emissive_cutoff.w {discard;}
    let alpha = clamp(base.a,0.0,1.0);
    return vec4(min(base.rgb+emissive,vec3(65504.0))*alpha,alpha);
}

@fragment
fn fs_main(surface:Surface,@builtin(front_facing) front_facing:bool) -> @location(0) vec4<f32> {
    if camera.options.w==2u {
        // Inspect interpolated vertex normals (face normals when absent), before normal
        // maps and independent of lights and sidedness.
        var n=surface.normal;
        if dot(n,n)<1e-12 {
            n=cross(dpdx(surface.position),dpdy(surface.position));
            n=select(n,-n,front_facing);
            n=select(n,-n,object.flags.w!=0u);
        }
        return vec4(normalize(n)*0.5+vec3(0.5),1.0);
    }
    if camera.options.w==3u { return vec4(vec3(surface.clip.z),1.0); }
    if camera.options.x==0u { return vec4(object.color.rgb,1.0); }
    let base = textureSample(base_map,base_sampler,surface.uv)*object.color*surface.color;
    let mr = textureSample(mr_map,mr_sampler,surface.uv);
    let normal_texel = textureSample(normal_map,normal_sampler,surface.uv).xyz*2.0-vec3(1.0);
    let emissive = textureSample(emissive_map,emissive_sampler,surface.uv).rgb*object.emissive_cutoff.rgb;
    // Occlusion maps affect indirect lighting, not punctual direct lights[li].
    let occlusion = textureSample(ao_map,ao_sampler,surface.uv).r;
    let px=dpdx(surface.position); let py=dpdy(surface.position);
    let tx=dpdx(surface.uv); let ty=dpdy(surface.uv);
    let front = select(front_facing,!front_facing,object.flags.w!=0u);
    if !front && object.flags.z==0u {discard;}
    if object.emissive_cutoff.w>=0.0 && base.a<object.emissive_cutoff.w {discard;}
    if object.flags.y!=0u {return vec4(min(base.rgb+emissive,vec3(65504.0)),1.0);}
    var v=-camera.optical_depth.xyz;
    if camera.options.z==0u {v=normalize(camera.eye.xyz-surface.position);}
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
                // Same frame as imported tangents: t along +U, orthogonal to n, and the
                // bitangent towards decreasing V, the texture's up (glTF normal-map +Y).
                let dpdu=(px*ty.y-py*tx.y)/determinant;
                let dpdv=(py*tx.x-px*ty.x)/determinant;
                let along=dpdu-n*dot(n,dpdu);
                if dot(along,along)>1e-20 {
                    t=normalize(along);
                    b=cross(n,t)*select(1.0,-1.0,dot(cross(n,t),dpdv)>0.0);
                }
            }
        }
        if dot(t,t)>1e-12 && dot(b,b)>1e-12 {
            n=normalize(mat3x3(t,b,n)*vec3(normal_texel.xy*object.factors.z,normal_texel.z));
        }
    }
    let metal=clamp(object.factors.x*mr.b,0.0,1.0);
    // Finite raster specular lobe; roughness zero is not a sampled delta mirror.
    let rough=clamp(object.factors.y*mr.g,0.045,1.0);
    let ao=mix(1.0,occlusion,object.factors.w);
    var radiance=emissive+environment_lighting(n,v,base.rgb,metal,rough,ao);
    for (var i=0u;i<camera.options.y;i++) {
        let shaded=direct_light(i,surface.position,n,v,base.rgb,metal,rough);
        if any(shaded>vec3(0.0)) {
            radiance+=shaded*shadow_visibility(i,surface.position,shadow_normal,px,py);
        }
    }
    return vec4(clamp(radiance,vec3(0.0),vec3(65504.0)),1.0);
}

// Shadow catcher: premultiplied black whose alpha is the luminance fraction of light
// removed by shadows, so "over" blending multiplies what lies behind by
// shadowed / unshadowed lights[li]. Computed in linear HDR before any tone mapping.
@fragment
fn fs_shadow_catcher(surface:Surface,@builtin(front_facing) front_facing:bool)->@location(0) vec4<f32> {
    let px=dpdx(surface.position); let py=dpdy(surface.position);
    let front=select(front_facing,!front_facing,object.flags.w!=0u);
    if !front && object.flags.z==0u {discard;}
    var v=-camera.optical_depth.xyz;
    if camera.options.z==0u {v=normalize(camera.eye.xyz-surface.position);}
    var n=surface.normal;
    if dot(n,n)<1e-12 {
        n=normalize(cross(px,py));
        n=select(-n,n,dot(n,v)>=0.0);
    } else {
        n=normalize(n);
        if !front {n=-n;}
    }
    let base=object.color.rgb;
    let metal=clamp(object.factors.x,0.0,1.0);
    let rough=clamp(object.factors.y,0.045,1.0);
    // Unoccluded environment light reaches shadowed and lit ground alike.
    let ambient=environment_lighting(n,v,base,metal,rough,1.0);
    var open=ambient;
    var lit=ambient;
    for (var i=0u;i<camera.options.y;i++) {
        let shaded=direct_light(i,surface.position,n,v,base,metal,rough);
        if any(shaded>vec3(0.0)) {
            open+=shaded;
            lit+=shaded*shadow_visibility(i,surface.position,n,px,py);
        }
    }
    let weights=vec3(0.2126,0.7152,0.0722);
    let total=dot(open,weights);
    if total<=0.0 {return vec4(0.0);}
    return vec4(0.0,0.0,0.0,1.0-clamp(dot(lit,weights)/total,0.0,1.0));
}

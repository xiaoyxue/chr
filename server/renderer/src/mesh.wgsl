struct Camera {
    clip_from_world: mat4x4<f32>, eye: vec4<f32>, options: vec4<u32>,
    optical_depth: vec4<f32>
};
// What a draw's instances share; uniform, so fragment stages branch on it uniformly.
// `specular` .. `uv` carry the glTF material extensions (mesh_renderer.rs `DrawUniform`).
struct Object {
    color: vec4<f32>, emissive_cutoff: vec4<f32>, factors: vec4<f32>, flags: vec4<u32>,
    specular: vec4<f32>, coat: vec4<f32>, sheen: vec4<f32>, iridescence: vec4<f32>,
    anisotropy: vec4<f32>, maps: vec4<u32>, uv: array<vec4<f32>,30>
};
// One instance's transform, at `instance_index` (counted from the draw's first instance).
struct Instance { model: mat4x4<f32>, normal: mat3x3<f32> };
struct Light {
    position_kind: vec4<f32>, direction_inner: vec4<f32>, emission_outer: vec4<f32>,
    shadow_matrix: array<mat4x4<f32>,4>, shadow_params: vec4<f32>,
    shadow_depth: array<vec4<f32>,4>, pcss: vec4<f32>, pcss_samples: vec4<f32>,
    cascade_splits: vec4<f32>, cascade_params: vec4<f32>, area: array<vec4<f32>,2>
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> object: Object;
@group(1) @binding(2) var<storage,read> instances: array<Instance>;
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
@group(2) @binding(10) var clearcoat_map: texture_2d<f32>;
@group(2) @binding(11) var clearcoat_sampler: sampler;
@group(2) @binding(12) var clearcoat_roughness_map: texture_2d<f32>;
@group(2) @binding(13) var clearcoat_roughness_sampler: sampler;
@group(2) @binding(14) var clearcoat_normal_map: texture_2d<f32>;
@group(2) @binding(15) var clearcoat_normal_sampler: sampler;
@group(2) @binding(16) var sheen_color_map: texture_2d<f32>;
@group(2) @binding(17) var sheen_color_sampler: sampler;
@group(2) @binding(18) var sheen_roughness_map: texture_2d<f32>;
@group(2) @binding(19) var sheen_roughness_sampler: sampler;
@group(2) @binding(20) var specular_map: texture_2d<f32>;
@group(2) @binding(21) var specular_sampler: sampler;
@group(2) @binding(22) var specular_color_map: texture_2d<f32>;
@group(2) @binding(23) var specular_color_sampler: sampler;
@group(2) @binding(24) var iridescence_map: texture_2d<f32>;
@group(2) @binding(25) var iridescence_sampler: sampler;
@group(2) @binding(26) var thickness_map: texture_2d<f32>;
@group(2) @binding(27) var thickness_sampler: sampler;
@group(2) @binding(28) var anisotropy_map: texture_2d<f32>;
@group(2) @binding(29) var anisotropy_sampler: sampler;
// Material map slots (scene `MapSlot`): bindings 2*slot and 2*slot+1, bit `slot` of
// `object.maps.x` and rows 2*slot, 2*slot+1 of `object.uv`.
const SLOT_BASE:u32=0u;
const SLOT_METAL_ROUGH:u32=1u;
const SLOT_NORMAL:u32=2u;
const SLOT_EMISSIVE:u32=3u;
const SLOT_OCCLUSION:u32=4u;
const SLOT_CLEARCOAT:u32=5u;
const SLOT_CLEARCOAT_ROUGH:u32=6u;
const SLOT_CLEARCOAT_NORMAL:u32=7u;
const SLOT_SHEEN_COLOR:u32=8u;
const SLOT_SHEEN_ROUGH:u32=9u;
const SLOT_SPECULAR:u32=10u;
const SLOT_SPECULAR_COLOR:u32=11u;
const SLOT_IRIDESCENCE:u32=12u;
const SLOT_THICKNESS:u32=13u;
const SLOT_ANISOTROPY:u32=14u;

// KHR_texture_transform of a slot: its affine rows applied to the texture coordinates.
fn map_uv(slot:u32,uv:vec2<f32>)->vec2<f32> {
    let p=vec3(uv,1.0);
    return vec2(dot(object.uv[2u*slot].xyz,p),dot(object.uv[2u*slot+1u].xyz,p));
}

fn has_map(slot:u32)->bool {
    return ((object.maps.x>>slot)&1u)!=0u;
}

// A tangent-space direction read from a transformed slot (normal-map XY, anisotropy
// direction) back in the untransformed texture frame the tangents follow. The texel's
// X runs along transformed +U and its Y against transformed V; the transform's
// rotation and mirroring are undone, its scale does not change directions.
fn map_direction(slot:u32,xy:vec2<f32>)->vec2<f32> {
    let u=normalize(vec2(object.uv[2u*slot].x,object.uv[2u*slot+1u].x));
    let v=normalize(vec2(object.uv[2u*slot].y,object.uv[2u*slot+1u].y));
    let d=vec2(xy.x,-xy.y);
    return vec2(dot(u,d),-dot(v,d));
}
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
fn vs_main(vertex: Vertex, @builtin(instance_index) index: u32) -> Surface {
    var out: Surface;
    let model = instances[index].model;
    let world = model * vec4(vertex.position,1.0);
    out.clip = camera.clip_from_world * world;
    out.position = world.xyz;
    out.normal = instances[index].normal * vertex.normal;
    out.uv = vertex.uv;
    out.tangent = vec4((model * vec4(vertex.tangent.xyz,0.0)).xyz,
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
fn vs_shadow(vertex:Vertex,@builtin(instance_index) index:u32)->ShadowSurface {
    var out:ShadowSurface;
    let world=instances[index].model*vec4(vertex.position,1.0);
    out.clip=camera.clip_from_world*world;
    out.uv=vertex.uv;
    out.alpha=vertex.color.a;
    out.position=world.xyz;
    return out;
}

fn shadow_alpha_test(surface:ShadowSurface,front_facing:bool) {
    let alpha=textureSample(base_map,base_sampler,map_uv(SLOT_BASE,surface.uv)).a*object.color.a*surface.alpha;
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

// Direction towards a punctual light and the radiance arriving from it (zero outside a
// spot cone or at a point light's exact centre); not for area lights.
struct Incident { l:vec3<f32>, radiance:vec3<f32> };
fn punctual_incident(li:u32,x:vec3<f32>)->Incident {
    var l=-lights[li].direction_inner.xyz;
    var attenuation=1.0;
    if lights[li].position_kind.w>0.5 {
        let delta=lights[li].position_kind.xyz-x;
        let distance2=dot(delta,delta);
        // An ideal point source has no defined direction at its exact centre.
        if distance2==0.0 {return Incident(l,vec3(0.0));}
        l=delta*inverseSqrt(distance2);
        attenuation=1.0/distance2;
        if lights[li].position_kind.w>1.5 {
            let cosine=dot(-l,lights[li].direction_inner.xyz);
            if cosine<=lights[li].emission_outer.w {return Incident(l,vec3(0.0));}
            if cosine<lights[li].direction_inner.w {
                attenuation*=smoothstep(lights[li].emission_outer.w,lights[li].direction_inner.w,cosine);
            }
        }
    }
    return Incident(l,lights[li].emission_outer.rgb*attenuation);
}

// Unoccluded direct contribution of one light; visibility is applied by the caller.
fn direct_light(li:u32,x:vec3<f32>,n:vec3<f32>,v:vec3<f32>,base:vec3<f32>,metal:f32,rough:f32)->vec3<f32> {
    if lights[li].position_kind.w>2.5 {return area_lighting(li,x,n,v,base,metal,rough);}
    let incident=punctual_incident(li,x);
    if dot(n,incident.l)<=0.0 {return vec3(0.0);}
    return incident.radiance*brdf(n,v,incident.l,base,metal,rough);
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

// glTF material extensions of one fragment (Khronos definitions), evaluated once and
// shared by every light and the environment. Defaults reproduce the plain base material.
struct Layers {
    // Dielectric reflectance at normal and grazing incidence (KHR_materials_ior and
    // KHR_materials_specular); 0.04 and 1 by default.
    f0:vec3<f32>, f90:vec3<f32>,
    // KHR_materials_iridescence: strength and thin-film Fresnel of the dielectric and the
    // metal at N.V (applied for every light, as the Khronos sample viewer does).
    iridescence:f32, iridescent:vec3<f32>, iridescent_metal:vec3<f32>,
    // KHR_materials_anisotropy: strength and the unit directions of increased (t) and
    // base (b) roughness, perpendicular to the shading normal.
    anisotropy:f32, at:vec3<f32>, ab:vec3<f32>,
    // KHR_materials_sheen: colour, roughness (at least 0.07) and directional albedo at N.V.
    sheen:vec3<f32>, sheen_rough:f32, sheen_albedo:f32,
    // KHR_materials_clearcoat: intensity, normal, perceptual roughness and the
    // fraction 1 - clearcoat * Fresnel(N.V) of light passing the coat.
    coat:f32, coat_n:vec3<f32>, coat_rough:f32, coat_pass:f32,
};

fn max3(c:vec3<f32>)->f32 { return max(c.x,max(c.y,c.z)); }

fn schlick(f0:vec3<f32>,f90:vec3<f32>,cosine:f32)->vec3<f32> {
    return f0+(f90-f0)*pow5(1.0-cosine);
}

// Thin-film interference Fresnel (Belcour and Barla 2017) in the approximation of
// KHR_materials_iridescence: Schlick reflectances at both interfaces and the analytic
// spectral integration of the first two orders, in linear BT.709. `thickness` is in nm.
fn iridescence_fresnel(film_ior:f32,cosine:f32,thickness:f32,base_f0:vec3<f32>)->vec3<f32> {
    let eta=film_ior;
    let sin2=(1.0-cosine*cosine)/(eta*eta);
    if sin2>=1.0 {return vec3(1.0);}
    let cos2=sqrt(1.0-sin2);
    let q0=(eta-1.0)/(eta+1.0);
    let r0=q0*q0;
    let r12=r0+(1.0-r0)*pow5(1.0-cosine);
    let t121=1.0-r12;
    let phi12=select(0.0,3.14159265,eta<1.0);
    let root=sqrt(clamp(base_f0,vec3(0.0),vec3(0.9999)));
    let base_ior=(vec3(1.0)+root)/(vec3(1.0)-root);
    let q1=(base_ior-vec3(eta))/(base_ior+vec3(eta));
    let r1=q1*q1;
    let r23=r1+(vec3(1.0)-r1)*pow5(1.0-cos2);
    let phi23=select(vec3(0.0),vec3(3.14159265),base_ior<vec3(eta));
    let opd=2.0*eta*thickness*cos2;
    let phi=vec3(3.14159265-phi12)+phi23;
    let r123=clamp(r12*r23,vec3(1e-5),vec3(0.9999));
    let r123_root=sqrt(r123);
    let rs=t121*t121*r23/(vec3(1.0)-r123);
    var result=r12+rs;
    var cm=rs-vec3(t121);
    for (var m=1;m<=2;m++) {
        cm*=r123_root;
        result+=cm*2.0*film_sensitivity(f32(m)*opd,f32(m)*phi);
    }
    return max(result,vec3(0.0));
}

// Fourier transform of the CIE XYZ matching functions as Gaussians, at optical path
// difference `opd` (nm) and phase `shift`, converted to linear BT.709.
fn film_sensitivity(opd:f32,shift:vec3<f32>)->vec3<f32> {
    let phase=2.0*3.14159265*opd*1.0e-9;
    let value=vec3(5.4856e-13,4.4201e-13,5.2481e-13);
    let position=vec3(1.6810e+06,1.7953e+06,2.2084e+06);
    let variance=vec3(4.3278e+09,9.3046e+09,6.6121e+09);
    var xyz=value*sqrt(2.0*3.14159265*variance)*cos(position*phase+shift)*exp(-phase*phase*variance);
    xyz.x+=9.7470e-14*sqrt(2.0*3.14159265*4.5282e+09)*cos(2.2399e+06*phase+shift.x)
        *exp(-4.5282e+09*phase*phase);
    xyz/=1.0685e-7;
    let xyz_to_rec709=mat3x3<f32>(
        vec3(3.2404542,-0.9692660,0.0556434),
        vec3(-1.5371385,1.8760108,-0.2040259),
        vec3(-0.4985314,0.0415560,1.0572252));
    return xyz_to_rec709*xyz;
}

// Light leaving the base material towards `v` for light arriving along `l` with unit
// radiance, cosine included: the shared GGX/Burley material of `brdf`, with the
// dielectric Fresnel of KHR_materials_ior/specular, thin-film Fresnel and anisotropic GGX.
// The diffuse lobe keeps the energy the dielectric's strongest Fresnel channel leaves.
fn layered_brdf(n:vec3<f32>,v:vec3<f32>,l:vec3<f32>,base:vec3<f32>,metal:f32,rough:f32,m:Layers)->vec3<f32> {
    let nl=max(dot(n,l),0.0);
    if nl<=0.0 {return vec3(0.0);}
    let nv=max(dot(n,v),0.0001);
    let h=normalize(v+l);
    let nh=max(dot(n,h),0.0);
    let vh=max(dot(v,h),0.0);
    let alpha=rough*rough;
    var specular=0.0;
    if m.anisotropy>0.0 {
        let at=mix(alpha,1.0,m.anisotropy*m.anisotropy);
        let ab=alpha;
        let a2=at*ab;
        let f=vec3(ab*dot(m.at,h),at*dot(m.ab,h),a2*nh);
        let w2=a2/dot(f,f);
        let distribution=a2*w2*w2/3.14159265;
        let view=nl*length(vec3(at*dot(m.at,v),ab*dot(m.ab,v),nv));
        let light=nv*length(vec3(at*dot(m.at,l),ab*dot(m.ab,l),nl));
        specular=distribution*clamp(0.5/(view+light),0.0,1.0);
    } else {
        let a2=alpha*alpha;
        let d0=nh*nh*(a2-1.0)+1.0;
        let distribution=a2/(3.14159265*d0*d0);
        specular=distribution*0.5/(nl*sqrt(nv*nv*(1.0-a2)+a2)+nv*sqrt(nl*nl*(1.0-a2)+a2));
    }
    let dielectric=mix(schlick(m.f0,m.f90,vh),m.iridescent,m.iridescence);
    let conductor=mix(schlick(base,vec3(1.0),vh),m.iridescent_metal,m.iridescence);
    let fresnel=mix(dielectric,conductor,metal);
    let fd90=0.5+2.0*rough*vh*vh;
    let diffuse=(1.0+(fd90-1.0)*pow5(1.0-nl))*(1.0+(fd90-1.0)*pow5(1.0-nv))/3.14159265;
    let kept=vec3(1.0)-mix(vec3(max3(dielectric)),conductor,metal);
    return nl*(base*(1.0-metal)*kept*diffuse+fresnel*specular);
}

// Charlie sheen lobe for unit incoming radiance, cosine included.
fn sheen_light(n:vec3<f32>,v:vec3<f32>,l:vec3<f32>,m:Layers)->vec3<f32> {
    let nl=dot(n,l);
    if nl<=0.0 || m.sheen_albedo<=0.0 {return vec3(0.0);}
    let nv=max(dot(n,v),0.0001);
    let alpha=m.sheen_rough*m.sheen_rough;
    let nh=max(dot(n,normalize(v+l)),0.0);
    return m.sheen*charlie_distribution(alpha,nh)*charlie_visibility(alpha,nv,nl)*nl;
}

// Clearcoat GGX lobe (F0 0.04) for unit incoming radiance, weighted by the coat.
fn coat_light(v:vec3<f32>,l:vec3<f32>,m:Layers)->vec3<f32> {
    let nl=dot(m.coat_n,l);
    if nl<=0.0 || m.coat<=0.0 {return vec3(0.0);}
    let n=m.coat_n;
    let nv=max(dot(n,v),0.0001);
    let h=normalize(v+l);
    let nh=max(dot(n,h),0.0);
    let a2=pow(m.coat_rough,4.0);
    let d0=nh*nh*(a2-1.0)+1.0;
    let distribution=a2/(3.14159265*d0*d0);
    let visibility=0.5/(nl*sqrt(nv*nv*(1.0-a2)+a2)+nv*sqrt(nl*nl*(1.0-a2)+a2));
    let fresnel=0.04+0.96*pow5(1.0-max(dot(v,h),0.0));
    return vec3(m.coat*fresnel*distribution*visibility*nl);
}

// One light on a layered material: the base (sheen-scaled) under the sheen lobe, both
// under the clearcoat. Area lights shade the base layer only (ltc-plausible-v1).
fn layered_light(li:u32,x:vec3<f32>,n:vec3<f32>,v:vec3<f32>,base:vec3<f32>,metal:f32,rough:f32,m:Layers)->vec3<f32> {
    let scale=m.coat_pass*(1.0-max3(m.sheen)*m.sheen_albedo);
    if lights[li].position_kind.w>2.5 {return area_lighting(li,x,n,v,base,metal,rough)*scale;}
    let incident=punctual_incident(li,x);
    if all(incident.radiance==vec3(0.0)) {return vec3(0.0);}
    let l=incident.l;
    return incident.radiance*(layered_brdf(n,v,l,base,metal,rough,m)*scale
        +sheen_light(n,v,l,m)*m.coat_pass+coat_light(v,l,m));
}

fn prefiltered_radiance(direction:vec3<f32>,rough:f32)->vec3<f32> {
    let uv=panorama_uv(environment_direction(direction));
    let level=rough*environment.info.w;
    let lower=i32(floor(level));
    let upper=min(lower+1,i32(environment.info.w));
    return mix(sample_float_layer(environment_filtered,uv,lower),
        sample_float_layer(environment_filtered,uv,upper),fract(level));
}

// Environment and constant fill on a layered material; with default layers this is
// `environment_lighting`. Sheen reads the GGX-prefiltered panorama at its roughness
// around the normal; anisotropy bends the reflected direction (KHR_materials_anisotropy).
fn layered_environment(n:vec3<f32>,v:vec3<f32>,base:vec3<f32>,metal:f32,rough:f32,ao:f32,m:Layers)->vec3<f32> {
    let sky=environment.flags.x!=0.0 && environment.info.x!=0.0;
    let constant=environment.constant.rgb;
    if !sky && all(constant==vec3(0.0)) {return vec3(0.0);}
    let nv=max(dot(n,v),0.0001);
    let gain=environment.flags.y*environment.info.x;
    var reflected=reflect(-v,n);
    if m.anisotropy>0.0 {
        var bent=normalize(cross(cross(m.ab,v),m.ab));
        let a=pow(1.0-m.anisotropy*(1.0-rough),4.0);
        bent=normalize(mix(bent,n,a));
        reflected=normalize(mix(reflect(-v,bent),bent,rough*rough));
    }
    let dielectric_f0=mix(m.f0,m.iridescent,m.iridescence);
    let f0=mix(dielectric_f0,mix(base,m.iridescent_metal,m.iridescence),metal);
    let f90=mix(m.f90,vec3(1.0),metal);
    let kept_f0=mix(vec3(max3(dielectric_f0)),base,metal);
    let kept_f90=mix(vec3(max3(m.f90)),vec3(1.0),metal);
    let fresnel=kept_f0+(max(vec3(1.0-rough)*kept_f90,kept_f0)-kept_f0)*pow5(1.0-nv);
    // A uniform radiance L is its own diffuse (SH) and prefiltered specular radiance.
    var irradiance=constant;
    var prefiltered=constant;
    if sky {
        irradiance+=environment_diffuse(environment_direction(n))*gain;
        prefiltered+=prefiltered_radiance(reflected,rough)*gain;
    }
    let dfg=sample_float_image(environment_brdf,vec2(nv,rough),false).rg;
    let diffuse=(vec3(1.0)-fresnel)*(1.0-metal)*base*irradiance;
    let specular=prefiltered*(f0*dfg.x+f90*dfg.y);
    var radiance=(diffuse+specular)*(1.0-max3(m.sheen)*m.sheen_albedo)*m.coat_pass;
    if m.sheen_albedo>0.0 {
        var sheen=constant;
        if sky {sheen+=prefiltered_radiance(n,m.sheen_rough)*gain;}
        radiance+=sheen*m.sheen*m.sheen_albedo*m.coat_pass;
    }
    if m.coat>0.0 {
        let coat_nv=max(dot(m.coat_n,v),0.0001);
        var coat=constant;
        if sky {coat+=prefiltered_radiance(reflect(-v,m.coat_n),m.coat_rough)*gain;}
        let coat_dfg=sample_float_image(environment_brdf,vec2(coat_nv,m.coat_rough),false).rg;
        radiance+=coat*(0.04*coat_dfg.x+coat_dfg.y)*m.coat;
    }
    return radiance*ao;
}

// Tangent and bitangent of the texture frame at a fragment: the interpolated tangent,
// or one derived from position and texture-coordinate derivatives. Zero when neither
// exists (no texture coordinates or a degenerate UV gradient).
struct Frame { t:vec3<f32>, b:vec3<f32> };
fn texture_frame(n:vec3<f32>,tangent:vec4<f32>,px:vec3<f32>,py:vec3<f32>,tx:vec2<f32>,ty:vec2<f32>)->Frame {
    var t=tangent.xyz;
    if dot(t,t)>1e-12 {
        t=normalize(t-n*dot(n,t));
        return Frame(t,cross(n,t)*tangent.w);
    }
    let determinant=tx.x*ty.y-tx.y*ty.x;
    if abs(determinant)>1e-10 {
        // Same frame as imported tangents: t along +U, orthogonal to n, and the
        // bitangent towards decreasing V, the texture's up (glTF normal-map +Y).
        let dpdu=(px*ty.y-py*tx.y)/determinant;
        let dpdv=(py*tx.x-px*ty.x)/determinant;
        let along=dpdu-n*dot(n,dpdu);
        if dot(along,along)>1e-20 {
            t=normalize(along);
            return Frame(t,cross(n,t)*select(1.0,-1.0,dot(cross(n,t),dpdv)>0.0));
        }
    }
    return Frame(vec3(0.0),vec3(0.0));
}

@fragment
fn fs_wire() -> @location(0) vec4<f32> {
    return vec4(1.0);
}

// Ground decals: unlit, premultiplied over the opaque scene (the pipeline blends
// with ONE, ONE_MINUS_SRC_ALPHA). Sampling precedes discards for uniform control flow.
@fragment
fn fs_decal(surface:Surface,@builtin(front_facing) front_facing:bool) -> @location(0) vec4<f32> {
    let base = textureSample(base_map,base_sampler,map_uv(SLOT_BASE,surface.uv))*object.color*surface.color;
    let emissive = textureSample(emissive_map,emissive_sampler,map_uv(SLOT_EMISSIVE,surface.uv)).rgb*object.emissive_cutoff.rgb;
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
    let base = textureSample(base_map,base_sampler,map_uv(SLOT_BASE,surface.uv))*object.color*surface.color;
    let mr = textureSample(mr_map,mr_sampler,map_uv(SLOT_METAL_ROUGH,surface.uv));
    let normal_texel = textureSample(normal_map,normal_sampler,map_uv(SLOT_NORMAL,surface.uv)).xyz*2.0-vec3(1.0);
    let emissive = textureSample(emissive_map,emissive_sampler,map_uv(SLOT_EMISSIVE,surface.uv)).rgb*object.emissive_cutoff.rgb;
    // Occlusion maps affect indirect lighting, not punctual direct lights[li].
    let occlusion = textureSample(ao_map,ao_sampler,map_uv(SLOT_OCCLUSION,surface.uv)).r;
    // Extension maps default to white (no effect); `object` is uniform, so these
    // branches keep sampling in uniform control flow, before any discard.
    var coat_texel=vec4(1.0); var coat_rough_texel=vec4(1.0); var coat_normal_texel=vec3(0.0,0.0,1.0);
    if object.coat.y>0.0 {
        coat_texel=textureSample(clearcoat_map,clearcoat_sampler,map_uv(SLOT_CLEARCOAT,surface.uv));
        coat_rough_texel=textureSample(clearcoat_roughness_map,clearcoat_roughness_sampler,
            map_uv(SLOT_CLEARCOAT_ROUGH,surface.uv));
        coat_normal_texel=textureSample(clearcoat_normal_map,clearcoat_normal_sampler,
            map_uv(SLOT_CLEARCOAT_NORMAL,surface.uv)).xyz*2.0-vec3(1.0);
    }
    var sheen_texel=vec4(1.0); var sheen_rough_texel=vec4(1.0);
    if any(object.sheen.rgb>vec3(0.0)) {
        sheen_texel=textureSample(sheen_color_map,sheen_color_sampler,map_uv(SLOT_SHEEN_COLOR,surface.uv));
        sheen_rough_texel=textureSample(sheen_roughness_map,sheen_roughness_sampler,
            map_uv(SLOT_SHEEN_ROUGH,surface.uv));
    }
    var specular_texel=vec4(1.0); var specular_color_texel=vec4(1.0);
    if has_map(SLOT_SPECULAR) || has_map(SLOT_SPECULAR_COLOR) {
        specular_texel=textureSample(specular_map,specular_sampler,map_uv(SLOT_SPECULAR,surface.uv));
        specular_color_texel=textureSample(specular_color_map,specular_color_sampler,
            map_uv(SLOT_SPECULAR_COLOR,surface.uv));
    }
    var iridescence_texel=vec4(1.0); var thickness_texel=vec4(1.0);
    if object.iridescence.x>0.0 {
        iridescence_texel=textureSample(iridescence_map,iridescence_sampler,map_uv(SLOT_IRIDESCENCE,surface.uv));
        thickness_texel=textureSample(thickness_map,thickness_sampler,map_uv(SLOT_THICKNESS,surface.uv));
    }
    // Without a texture the anisotropy direction is +X at full strength.
    var anisotropy_texel=vec4(1.0,0.5,1.0,1.0);
    if object.anisotropy.x>0.0 && has_map(SLOT_ANISOTROPY) {
        anisotropy_texel=textureSample(anisotropy_map,anisotropy_sampler,map_uv(SLOT_ANISOTROPY,surface.uv));
    }
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
    let coat_mapped=object.coat.y>0.0 && has_map(SLOT_CLEARCOAT_NORMAL);
    var frame=Frame(vec3(0.0),vec3(0.0));
    if object.flags.x!=0u || coat_mapped || object.anisotropy.x>0.0 {
        frame=texture_frame(n,surface.tangent,px,py,tx,ty);
    }
    let framed=dot(frame.t,frame.t)>1e-12 && dot(frame.b,frame.b)>1e-12;
    if object.flags.x!=0u && framed {
        let xy=map_direction(SLOT_NORMAL,normal_texel.xy);
        n=normalize(mat3x3(frame.t,frame.b,n)*vec3(xy*object.factors.z,normal_texel.z));
    }
    let metal=clamp(object.factors.x*mr.b,0.0,1.0);
    // Finite raster specular lobe; roughness zero is not a sampled delta mirror.
    var alpha2=pow(clamp(object.factors.y*mr.g,0.045,1.0),4.0);
    // Specular antialiasing (#177): normals varying within a pixel widen its lobe, so sub-pixel
    // curvature and bumps cannot reflect one bright texel or light at full strength. The
    // screen-space kernel (Tokuyoshi and Kaplanyan, Filament's variance 0.15 and threshold 0.2)
    // sees variation across pixels; a minified normal map's shortened average (Toksvig), the
    // variation inside its texels.
    let dndx=dpdx(n); let dndy=dpdy(n);
    alpha2+=min(0.3*(dot(dndx,dndx)+dot(dndy,dndy)),0.2);
    if object.flags.x!=0u {
        // 8-bit texels of unit normals fall short by up to sqrt(3)/255; that is not variance.
        let mean=clamp(length(normal_texel)+0.007,1e-3,1.0);
        alpha2+=(1.0-mean)/mean*min(object.factors.z*object.factors.z,1.0);
    }
    let rough=sqrt(sqrt(min(alpha2,1.0)));
    let ao=mix(1.0,occlusion,object.factors.w);
    let nv=max(dot(n,v),0.0001);
    var m:Layers;
    // KHR_materials_ior and KHR_materials_specular; IOR 0 makes the Fresnel term 1.
    let ior=object.coat.x;
    let strength=object.specular.w*specular_texel.a;
    let q=(ior-1.0)/(ior+1.0);
    let reflectance=select(q*q,1.0,ior==0.0);
    m.f0=min(vec3(reflectance)*object.specular.rgb*specular_color_texel.rgb,vec3(1.0))*strength;
    m.f90=select(vec3(strength),m.f0,ior==0.0);
    let thickness=mix(object.iridescence.z,object.iridescence.w,thickness_texel.g);
    // A film of zero thickness is no film.
    m.iridescence=select(object.iridescence.x*iridescence_texel.r,0.0,thickness<=0.0);
    if m.iridescence>0.0 {
        m.iridescent=iridescence_fresnel(object.iridescence.y,nv,thickness,m.f0);
        m.iridescent_metal=iridescence_fresnel(object.iridescence.y,nv,thickness,base.rgb);
    }
    m.anisotropy=object.anisotropy.x*anisotropy_texel.b;
    if m.anisotropy>0.0 && framed {
        var direction=vec2(1.0,0.0);
        if has_map(SLOT_ANISOTROPY) {
            direction=map_direction(SLOT_ANISOTROPY,normalize(anisotropy_texel.rg*2.0-vec2(1.0)));
        }
        let c=object.anisotropy.y; let s=object.anisotropy.z;
        direction=vec2(c*direction.x-s*direction.y,s*direction.x+c*direction.y);
        let along=frame.t*direction.x+frame.b*direction.y;
        m.at=normalize(along-n*dot(n,along));
        m.ab=cross(n,m.at);
    } else {
        m.anisotropy=0.0;
    }
    m.sheen=object.sheen.rgb*sheen_texel.rgb;
    if any(m.sheen>vec3(0.0)) {
        m.sheen_rough=max(object.sheen.w*sheen_rough_texel.a,0.07);
        m.sheen_albedo=sample_float_image(environment_brdf,vec2(nv,m.sheen_rough),false).b;
    }
    m.coat=object.coat.y*coat_texel.r;
    m.coat_n=shadow_normal;
    if coat_mapped && framed {
        let xy=map_direction(SLOT_CLEARCOAT_NORMAL,coat_normal_texel.xy);
        m.coat_n=normalize(mat3x3(frame.t,frame.b,shadow_normal)*vec3(xy*object.coat.w,coat_normal_texel.z));
    }
    // The coat's own lobe gets the same specular antialiasing as the base.
    let dcx=dpdx(m.coat_n); let dcy=dpdy(m.coat_n);
    let coat_alpha2=pow(clamp(object.coat.z*coat_rough_texel.g,0.045,1.0),4.0)
        +min(0.3*(dot(dcx,dcx)+dot(dcy,dcy)),0.2);
    m.coat_rough=sqrt(sqrt(min(coat_alpha2,1.0)));
    m.coat_pass=1.0-m.coat*(0.04+0.96*pow5(1.0-abs(dot(v,m.coat_n))));
    var radiance=emissive*m.coat_pass+layered_environment(n,v,base.rgb,metal,rough,ao,m);
    for (var i=0u;i<camera.options.y;i++) {
        let shaded=layered_light(i,surface.position,n,v,base.rgb,metal,rough,m);
        if any(shaded>vec3(0.0)) {
            radiance+=shaded*shadow_visibility(i,surface.position,shadow_normal,px,py);
        }
    }
    return vec4(clamp(radiance,vec3(0.0),vec3(65504.0)),1.0);
}

// Ground holdout: a catcher's depth only, drawn before opaque geometry.
@fragment
fn fs_holdout() -> @location(0) vec4<f32> {
    return vec4(0.0);
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

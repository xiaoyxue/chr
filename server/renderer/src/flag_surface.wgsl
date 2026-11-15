// Flag draws, appended to mesh.wgsl. Group 1 adds the compute pass's flag-local
// surface (flag.wgsl) to the object uniform; the fragment stages are mesh.wgsl's,
// except the simple look below.

struct FlagVertex { position: vec4<f32>, normal: vec4<f32>, tangent: vec4<f32> };
@group(1) @binding(1) var<storage,read> flag_vertices: array<FlagVertex>;

fn flag_uv(index:u32)->vec2<f32> {
    let columns=FLAG_COLUMNS+1u;
    return vec2(f32(index%columns)/f32(FLAG_COLUMNS),f32(index/columns)/f32(FLAG_ROWS));
}

@vertex
fn vs_flag(@builtin(vertex_index) index:u32)->Surface {
    let flag=flag_vertices[index];
    var out:Surface;
    let world=object.model*vec4(flag.position.xyz,1.0);
    out.clip=camera.clip_from_world*world;
    out.position=world.xyz;
    out.normal=object.normal*flag.normal.xyz;
    out.uv=flag_uv(index);
    out.tangent=vec4((object.model*vec4(flag.tangent.xyz,0.0)).xyz,
        flag.tangent.w*select(1.0,-1.0,object.flags.w!=0u));
    out.color=vec4(1.0);
    return out;
}

@vertex
fn vs_flag_shadow(@builtin(vertex_index) index:u32)->ShadowSurface {
    var out:ShadowSurface;
    let world=object.model*vec4(flag_vertices[index].position.xyz,1.0);
    out.clip=camera.clip_from_world*world;
    out.uv=flag_uv(index);
    out.alpha=1.0;
    out.position=world.xyz;
    return out;
}

// SimpleWave's look in scene-linear colour: two-sided diffuse, back light and a faint
// highlight under a key light above the viewer. Scene lights and shadows do not apply.
@fragment
fn fs_flag_simple(surface:Surface,@builtin(front_facing) front_facing:bool)->@location(0) vec4<f32> {
    let base=textureSample(base_map,base_sampler,surface.uv)*object.color;
    if object.emissive_cutoff.w>=0.0 && base.a<object.emissive_cutoff.w {discard;}
    let front=select(front_facing,!front_facing,object.flags.w!=0u);
    var v=-camera.optical_depth.xyz;
    if camera.options.z==0u {v=normalize(camera.eye.xyz-surface.position);}
    let n=normalize(surface.normal)*select(-1.0,1.0,front);
    let light=normalize(v+vec3(0.0,0.0,-1.5));
    let diffuse=max(dot(n,light),0.0);
    let back=max(-dot(n,light),0.0);
    let highlight=pow(max(dot(n,normalize(light+v)),0.0),35.0)*0.045;
    let lit=base.rgb*(0.55+0.48*diffuse+0.12*back)+vec3(highlight);
    return vec4(min(lit,vec3(65504.0)),1.0);
}

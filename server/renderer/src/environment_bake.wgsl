struct Params {roughness:f32,samples:u32,kind:u32,padding:u32};
@group(0) @binding(0) var input:texture_2d<f32>;
@group(0) @binding(1) var output:texture_storage_2d<rgba32float,write>;
@group(0) @binding(2) var<uniform> params:Params;

fn ggx_half(index:u32,count:u32,roughness:f32)->vec3<f32> {
    let u=f32(index)/f32(count);
    let phi=2.0*ENV_PI*f32(reverseBits(index))*2.3283064365386963e-10;
    let alpha=roughness*roughness;
    let z=sqrt((1.0-u)/(1.0+(alpha*alpha-1.0)*u));
    let r=sqrt(max(0.0,1.0-z*z));
    return vec3(r*cos(phi),r*sin(phi),z);
}

// Bilinear at `level`, trilinear between levels: reads the source panorama's box-filtered mips.
fn sample_mip(uv:vec2<f32>,level:i32)->vec3<f32> {
    let size=vec2<i32>(textureDimensions(input,level));
    let pixel=uv*vec2<f32>(size)-vec2(0.5);
    let base=vec2<i32>(floor(pixel));
    let f=fract(pixel);
    let a=textureLoad(input,image_pixel(base,size,true),level).rgb;
    let b=textureLoad(input,image_pixel(base+vec2(1,0),size,true),level).rgb;
    let c=textureLoad(input,image_pixel(base+vec2(0,1),size,true),level).rgb;
    let d=textureLoad(input,image_pixel(base+vec2(1,1),size,true),level).rgb;
    return mix(mix(a,b,f.x),mix(c,d,f.x),f.y);
}

fn sample_level(uv:vec2<f32>,level:f32)->vec3<f32> {
    let top=f32(textureNumLevels(input)-1u);
    let l=clamp(level,0.0,top);
    let a=floor(l);
    return mix(sample_mip(uv,i32(a)),sample_mip(uv,i32(min(a+1.0,top))),l-a);
}

// Directional albedo of the Charlie sheen lobe (KHR_materials_sheen, Conty and Kulla 2017)
// with its fitted visibility: the integral of BRDF times N.L over the hemisphere, by
// deterministic uniform sampling. Roughness below 0.07 shades as 0.07, as in mesh.wgsl.
fn sheen_albedo(nv:f32,roughness:f32)->f32 {
    let alpha=max(roughness,0.07)*max(roughness,0.07);
    let v=vec3(sqrt(1.0-nv*nv),0.0,nv);
    let count=4096u;
    var sum=0.0;
    for(var i=0u;i<count;i++) {
        let z=(f32(i)+0.5)/f32(count);
        let phi=2.0*ENV_PI*f32(reverseBits(i))*2.3283064365386963e-10;
        let r=sqrt(1.0-z*z);
        let l=vec3(r*cos(phi),r*sin(phi),z);
        let h=normalize(v+l);
        sum+=charlie_distribution(alpha,h.z)*charlie_visibility(alpha,nv,z)*z;
    }
    return sum*2.0*ENV_PI/f32(count);
}

@compute @workgroup_size(8,8)
fn main(@builtin(global_invocation_id) id:vec3<u32>) {
    let size=textureDimensions(output);
    if any(id.xy>=size) {return;}
    let uv=(vec2<f32>(id.xy)+vec2(0.5))/vec2<f32>(size);
    if params.kind==1u {
        let nv=max(uv.x,0.0001);
        let rough=max(uv.y,0.045);
        let v=vec3(sqrt(1.0-nv*nv),0.0,nv);
        let a2=rough*rough*rough*rough;
        var result=vec2(0.0);
        for(var i=0u;i<params.samples;i++) {
            let h=ggx_half(i,params.samples,rough);
            let vh=max(dot(v,h),0.0);
            let l=2.0*vh*h-v;
            let nl=l.z;
            if nl>0.0 {
                let vis=0.5/(nl*sqrt(nv*nv*(1.0-a2)+a2)+nv*sqrt(nl*nl*(1.0-a2)+a2));
                let weight=4.0*vis*nl*vh/max(h.z,0.0001);
                let fc=pow(1.0-vh,5.0);
                result+=vec2(1.0-fc,fc)*weight;
            }
        }
        textureStore(output,vec2<i32>(id.xy),vec4(result/f32(params.samples),sheen_albedo(nv,uv.y),1.0));
        return;
    }
    let n=panorama_direction(uv);
    // Filtered importance sampling (Krivanek and Colbert, #177): each sample reads the mip whose
    // texels match its share of the lobe's solid angle, never finer than the output texel's own,
    // so a sun of a few source texels is integrated rather than hit or missed.
    let source=vec2<f32>(textureDimensions(input));
    let footprint=log2(source.x/f32(size.x));
    if params.roughness==0.0 {
        textureStore(output,vec2<i32>(id.xy),vec4(sample_level(uv,footprint),1.0));
        return;
    }
    let equator_texel=2.0*ENV_PI*ENV_PI/(source.x*source.y);
    let alpha=params.roughness*params.roughness;
    let a2=alpha*alpha;
    let up=select(vec3(0.0,0.0,1.0),vec3(1.0,0.0,0.0),abs(n.z)>0.99);
    let t=normalize(cross(up,n));
    let b=cross(n,t);
    var radiance=vec3(0.0);
    var weight=0.0;
    for(var i=0u;i<params.samples;i++) {
        let local=ggx_half(i,params.samples,params.roughness);
        let h=t*local.x+b*local.y+n*local.z;
        let l=normalize(2.0*dot(n,h)*h-n);
        let nl=max(dot(n,l),0.0);
        if nl>0.0 {
            // With n = v the reflected direction's density is D(h) / 4.
            let q=local.z*local.z*(a2-1.0)+1.0;
            let pdf=a2/(4.0*ENV_PI*q*q);
            let sample_angle=1.0/(f32(params.samples)*pdf);
            let texel_angle=equator_texel*max(sqrt(max(1.0-l.z*l.z,0.0)),1e-4);
            let level=max(0.5*log2(sample_angle/texel_angle)+1.0,footprint);
            radiance+=sample_level(panorama_uv(l),level)*nl;
            weight+=nl;
        }
    }
    textureStore(output,vec2<i32>(id.xy),vec4(radiance/weight,1.0));
}

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
        textureStore(output,vec2<i32>(id.xy),vec4(result/f32(params.samples),0.0,1.0));
        return;
    }
    let n=panorama_direction(uv);
    if params.roughness==0.0 {
        textureStore(output,vec2<i32>(id.xy),vec4(sample_float_image(input,uv,true),1.0));
        return;
    }
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
            radiance+=sample_float_image(input,panorama_uv(l),true)*nl;
            weight+=nl;
        }
    }
    textureStore(output,vec2<i32>(id.xy),vec4(radiance/weight,1.0));
}

// Inverse-warps ideal premultiplied HDR CG (rendered over a guard band) into the
// distorted native canvas for `radial-poly-native-px-v1`. Coordinates are framebuffer
// sample positions, matching the rasterizer: CG texel k shades ideal coordinate k + 0.5.
struct Warp {
    // Rows of the inverse frame warp (frame SNC -> reference pixels), when present.
    inverse: array<vec4<f32>,3>,
    // Radial centre (px), scale (px), distorted normalised radius limit.
    radial: vec4<f32>,
    // k1, k2, k3, ideal normalised radius limit.
    k: vec4<f32>,
    // Native width, height, SNC scale sqrt(W H), frame-warp flag.
    native: vec4<f32>,
    // Guard band left/top in texels.
    margin: vec4<f32>,
};
@group(0) @binding(0) var source:texture_2d<f32>;
@group(0) @binding(1) var<uniform> warp:Warp;

@vertex fn vs_main(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32> {
    let p=array<vec2<f32>,6>(vec2(-1.0,-1.0),vec2(1.0,-1.0),vec2(-1.0,1.0),
        vec2(-1.0,1.0),vec2(1.0,-1.0),vec2(1.0,1.0))[index];
    return vec4(p,0.0,1.0);
}

fn texel(p:vec2<i32>)->vec4<f32> {
    let size=vec2<i32>(textureDimensions(source));
    if any(p<vec2(0)) || any(p>=size) {return vec4(0.0);}
    return textureLoad(source,p,0);
}

@fragment fn fs_main(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
    var r=position.xy;
    if warp.native.w>0.5 {
        let snc=vec3((2.0*position.xy-warp.native.xy)/warp.native.z,1.0);
        let h=vec3(dot(warp.inverse[0].xyz,snc),dot(warp.inverse[1].xyz,snc),dot(warp.inverse[2].xyz,snc));
        if abs(h.z)<=1e-7*max(abs(h.x),abs(h.y)) {return vec4(0.0);}
        r=h.xy/h.z;
    }
    let centre=warp.radial.xy;
    let wanted=length(r-centre)/warp.radial.z;
    // Beyond the monotonic branch there is no ideal pixel: leave the canvas untouched.
    if wanted>=warp.radial.w {return vec4(0.0);}
    var rho=min(wanted,0.999*warp.k.w);
    for(var i=0;i<8;i++) {
        let r2=rho*rho;
        let value=rho*(1.0+r2*(warp.k.x+r2*(warp.k.y+r2*warp.k.z)))-wanted;
        let slope=1.0+r2*(3.0*warp.k.x+r2*(5.0*warp.k.y+r2*7.0*warp.k.z));
        rho=clamp(rho-value/slope,0.0,warp.k.w);
    }
    var ideal=r;
    if wanted>0.0 {ideal=centre+(r-centre)*(rho/wanted);}
    let t=ideal+warp.margin.xy-vec2(0.5);
    let base=floor(t);
    let f=t-base;
    let b=vec2<i32>(base);
    return mix(mix(texel(b),texel(b+vec2(1,0)),f.x),mix(texel(b+vec2(0,1)),texel(b+vec2(1,1)),f.x),f.y);
}

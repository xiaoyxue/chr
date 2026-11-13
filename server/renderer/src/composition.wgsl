struct Params {rect:vec4<f32>,canvas:vec4<f32>,options:vec4<u32>,output:vec4<u32>};
@group(0) @binding(0) var source:texture_2d<f32>;
@group(0) @binding(1) var<uniform> params:Params;
struct Vertex { @builtin(position) clip:vec4<f32>, @location(0) uv:vec2<f32> };

@vertex fn vs_main(@builtin(vertex_index) index:u32)->Vertex {
    let uv=array<vec2<f32>,6>(vec2(0.0,0.0),vec2(1.0,0.0),vec2(0.0,1.0),
        vec2(0.0,1.0),vec2(1.0,0.0),vec2(1.0,1.0))[index];
    let pixel=params.rect.xy+uv*params.rect.zw;
    return Vertex(vec4((pixel/params.canvas.xy*2.0-vec2(1.0))*vec2(1.0,-1.0),0.0,1.0),uv);
}

fn decode(value:vec3<f32>,transfer:u32)->vec3<f32> {
    if transfer==1u {
        return select(pow((value+vec3(0.055))/1.055,vec3(2.4)),value/12.92,value<=vec3(0.04045));
    }
    if transfer==2u {
        return select(pow((value+vec3(0.099))/1.099,vec3(1.0/0.45)),value/4.5,value<vec3(0.081));
    }
    return value;
}

fn encode(value:vec3<f32>,transfer:u32)->vec3<f32> {
    if transfer==1u {
        return select(1.055*pow(value,vec3(1.0/2.4))-vec3(0.055),12.92*value,value<=vec3(0.0031308));
    }
    if transfer==2u {
        return select(1.099*pow(value,vec3(0.45))-vec3(0.099),4.5*value,value<vec3(0.018));
    }
    return value;
}

fn read_pixel(pixel:vec2<i32>,size:vec2<i32>)->vec4<f32> {
    let value=textureLoad(source,clamp(pixel,vec2(0),size-vec2(1)),0);
    var rgb=decode(max(value.rgb,vec3(0.0)),params.options.y);
    if params.options.z==0u {rgb*=value.a;}
    return vec4(min(rgb,vec3(65504.0*value.a)),value.a);
}

fn bilinear(pixel:vec2<f32>,size:vec2<i32>)->vec4<f32> {
    let base=vec2<i32>(floor(pixel));
    let f=fract(pixel);
    return mix(mix(read_pixel(base,size),read_pixel(base+vec2(1,0),size),f.x),
        mix(read_pixel(base+vec2(0,1),size),read_pixel(base+vec2(1,1),size),f.x),f.y);
}

@fragment fn fs_main(vertex:Vertex)->@location(0) vec4<f32> {
    let size=vec2<i32>(textureDimensions(source));
    let centre=vertex.uv*vec2<f32>(size);
    // Source texels per destination pixel. Plain bilinear skips texels when
    // minifying, which aliases fine detail; average a 4x4 grid of bilinear taps
    // over the destination pixel's footprint instead.
    let footprint=vec2<f32>(size)/params.rect.zw;
    var sampled=vec4(0.0);
    if all(footprint<=vec2(1.0)) {
        sampled=bilinear(centre-vec2(0.5),size);
    } else {
        let step=max(footprint,vec2(1.0))/4.0;
        for(var y=0;y<4;y++) {
            for(var x=0;x<4;x++) {
                let offset=(vec2(f32(x),f32(y))-vec2(1.5))*step;
                sampled+=bilinear(centre+offset-vec2(0.5),size);
            }
        }
        sampled/=16.0;
    }
    let value=sampled*params.canvas.w;
    if params.options.x==1u {
        if value.a==0.0 {return vec4(0.0);}
        let linear=min(value.rgb/value.a*params.canvas.z,vec3(65504.0));
        var mapped=linear/(vec3(1.0)+linear);
        if params.options.w==1u {
            mapped=clamp((linear*(2.51*linear+vec3(0.03)))/
                (linear*(2.43*linear+vec3(0.59))+vec3(0.14)),vec3(0.0),vec3(1.0));
        }
        return vec4(mapped*value.a,value.a);
    }
    if params.options.x==2u {
        var rgb=value.rgb;
        if params.output.y==0u && value.a>0.0 {rgb/=value.a;}
        return vec4(encode(rgb,params.output.x),value.a);
    }
    return value;
}

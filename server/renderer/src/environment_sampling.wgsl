const ENV_PI:f32=3.141592653589793;

fn panorama_uv(d:vec3<f32>)->vec2<f32> {
    return vec2(atan2(d.y,d.x)/(2.0*ENV_PI)+0.5,acos(clamp(-d.z,-1.0,1.0))/ENV_PI);
}

fn panorama_direction(uv:vec2<f32>)->vec3<f32> {
    let phi=(uv.x-0.5)*2.0*ENV_PI;
    let theta=uv.y*ENV_PI;
    return vec3(sin(theta)*cos(phi),sin(theta)*sin(phi),-cos(theta));
}

fn image_pixel(p:vec2<i32>,size:vec2<i32>,wrap_x:bool)->vec2<i32> {
    var x=clamp(p.x,0,size.x-1);
    if wrap_x {x=((p.x%size.x)+size.x)%size.x;}
    return vec2(x,clamp(p.y,0,size.y-1));
}

fn sample_float_image(image:texture_2d<f32>,uv:vec2<f32>,wrap_x:bool)->vec3<f32> {
    let size=vec2<i32>(textureDimensions(image));
    let pixel=uv*vec2<f32>(size)-vec2(0.5);
    let base=vec2<i32>(floor(pixel));
    let f=fract(pixel);
    let a=textureLoad(image,image_pixel(base,size,wrap_x),0).rgb;
    let b=textureLoad(image,image_pixel(base+vec2(1,0),size,wrap_x),0).rgb;
    let c=textureLoad(image,image_pixel(base+vec2(0,1),size,wrap_x),0).rgb;
    let d=textureLoad(image,image_pixel(base+vec2(1,1),size,wrap_x),0).rgb;
    return mix(mix(a,b,f.x),mix(c,d,f.x),f.y);
}

fn sample_float_layer(image:texture_2d_array<f32>,uv:vec2<f32>,layer:i32)->vec3<f32> {
    let size=vec2<i32>(textureDimensions(image));
    let pixel=uv*vec2<f32>(size)-vec2(0.5);
    let base=vec2<i32>(floor(pixel));
    let f=fract(pixel);
    let a=textureLoad(image,image_pixel(base,size,true),layer,0).rgb;
    let b=textureLoad(image,image_pixel(base+vec2(1,0),size,true),layer,0).rgb;
    let c=textureLoad(image,image_pixel(base+vec2(0,1),size,true),layer,0).rgb;
    let d=textureLoad(image,image_pixel(base+vec2(1,1),size,true),layer,0).rgb;
    return mix(mix(a,b,f.x),mix(c,d,f.x),f.y);
}

// Charlie sheen distribution and its fitted visibility (KHR_materials_sheen); `alpha` is
// the squared sheen roughness. Shared by the albedo bake and mesh shading.
fn charlie_distribution(alpha:f32,nh:f32)->f32 {
    let inverse=1.0/alpha;
    let sin2=max(1.0-nh*nh,0.0);
    return (2.0+inverse)*pow(sin2,inverse*0.5)/(2.0*ENV_PI);
}

fn charlie_fit(x:f32,alpha:f32)->f32 {
    let t=(1.0-alpha)*(1.0-alpha);
    let a=mix(21.5473,25.3245,t);
    let b=mix(3.82987,3.32435,t);
    let c=mix(0.19823,0.16801,t);
    let d=mix(-1.97760,-1.27393,t);
    let e=mix(-4.32054,-4.85967,t);
    return a/(1.0+b*pow(x,c))+d*x+e;
}

fn charlie_lambda(cosine:f32,alpha:f32)->f32 {
    if abs(cosine)<0.5 {return exp(charlie_fit(cosine,alpha));}
    return exp(2.0*charlie_fit(0.5,alpha)-charlie_fit(1.0-cosine,alpha));
}

fn charlie_visibility(alpha:f32,nv:f32,nl:f32)->f32 {
    return 1.0/((1.0+charlie_lambda(nv,alpha)+charlie_lambda(nl,alpha))*(4.0*nv*nl));
}

// Experimental LTC polygon integration; see LICENSE.ltc and the method report.
struct Params { shape: u32, lights: u32, size: u32, sides: u32 };
struct Entry { matrix: vec4<f32>, amplitude: vec4<f32> };
@group(0) @binding(0) var<storage, read> lut: array<Entry>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
const PI: f32 = 3.141592653589793;

fn edge(a_in: vec3<f32>, b_in: vec3<f32>) -> f32 {
    let a = normalize(a_in);
    let b = normalize(b_in);
    let c = cross(a,b);
    let s = length(c);
    if s <= 1e-7 { return 0.0; }
    return c.z*atan2(s,dot(a,b))/s;
}
struct EdgeSum { first: vec3<f32>, previous: vec3<f32>, sum: f32, count: u32 };
fn push(state: EdgeSum, p: vec3<f32>) -> EdgeSum {
    var next = state;
    if next.count == 0u { next.first = p; }
    else { next.sum += edge(next.previous,p); }
    next.previous = p;
    next.count++;
    return next;
}
struct Integral { first: vec3<f32>, previous: vec3<f32>, diffuse: f32, count: u32, specular: EdgeSum };
fn append(state: Integral, p: vec3<f32>, matrix: mat3x3<f32>) -> Integral {
    var next = state;
    if next.count == 0u {
        next.first = p;
    } else {
        next.diffuse += edge(next.previous,p);
        let a = matrix*next.previous;
        let b = matrix*p;
        if a.z > 0.0 { next.specular = push(next.specular,a); }
        if (a.z > 0.0) != (b.z > 0.0) {
            next.specular = push(next.specular,a+(b-a)*(a.z/(a.z-b.z)));
        }
    }
    next.previous = p;
    next.count++;
    return next;
}
fn vertex(i: u32, center: vec3<f32>, u: vec3<f32>, v: vec3<f32>, radius: f32) -> vec3<f32> {
    if params.shape == 0u {
        let x = select(-1.0,1.0,i==1u || i==2u);
        let y = select(-0.6,0.6,i>=2u);
        return center+x*u+y*v;
    }
    let angle = 2.0*PI*f32(i)/f32(params.sides);
    return center+radius*(u*cos(angle)+v*sin(angle));
}
fn lookup(rough: f32, nv: f32) -> Entry {
    let uv = vec2(rough,sqrt(1.0-nv))*63.0;
    let p = vec2<u32>(floor(uv));
    let f = fract(uv);
    let a = lut[p.x+64u*p.y];
    let b = lut[min(p.x+1u,63u)+64u*p.y];
    let c = lut[p.x+64u*min(p.y+1u,63u)];
    let d = lut[min(p.x+1u,63u)+64u*min(p.y+1u,63u)];
    return Entry(
        mix(mix(a.matrix,b.matrix,f.x),mix(c.matrix,d.matrix,f.x),f.y),
        mix(mix(a.amplitude,b.amplitude,f.x),mix(c.amplitude,d.amplitude,f.x),f.y));
}
@compute @workgroup_size(8,8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.size || id.y >= params.size { return; }
    let uv = (vec2<f32>(id.xy)+0.5)/f32(params.size);
    let receiver = vec3((uv-0.5)*4.0,0.0);
    let rough = 0.15+0.7*uv.x;
    let nv = 0.3+0.65*uv.y;
    let entry = lookup(rough,nv);
    let t = entry.matrix;
    let matrix = mat3x3(vec3(t.x,0.0,t.y),vec3(0.0,1.0,0.0),vec3(t.z,0.0,t.w));
    var total = vec4(0.0);
    for (var light = 0u; light < params.lights; light++) {
        var center = vec3(f32(light)*0.25,0.0,3.0)-receiver;
        var u = vec3(1.0,0.0,0.0);
        var v = vec3(0.0,1.0,0.0);
        var radius = 1.0;
        if params.shape == 2u {
            let d = length(center);
            let w = center/d;
            u = normalize(cross(vec3(0.0,1.0,0.0),w));
            v = cross(w,u);
            let factor = 1.0-1.0/(d*d);
            center *= factor;
            radius = sqrt(factor);
        }
        let count = select(params.sides,4u,params.shape==0u);
        var integral: Integral;
        // Stream physically clipped vertices; no large per-invocation arrays.
        for (var i = 0u; i < count; i++) {
            let a = vertex(i,center,u,v,radius);
            let b = vertex((i+1u)%count,center,u,v,radius);
            if a.z > 0.0 { integral = append(integral,a,matrix); }
            if (a.z > 0.0) != (b.z > 0.0) {
                integral = append(integral,a+(b-a)*(a.z/(a.z-b.z)),matrix);
            }
        }
        if integral.count > 0u { integral = append(integral,integral.first,matrix); }
        // The clipped horizon's closing edge has a nonzero azimuth integral.
        if integral.specular.count > 0u {
            integral.specular = push(integral.specular,integral.specular.first);
        }
        let values = abs(vec2(integral.diffuse,integral.specular.sum))/(2.0*PI);
        let lambert = values.x;
        let specular = values.y*(0.04*entry.amplitude.x+0.96*entry.amplitude.y);
        let diffuse = lambert*0.7*0.96;
        total += vec4(specular+diffuse,lambert,specular,diffuse);
    }
    output[id.x+params.size*id.y] = total;
}

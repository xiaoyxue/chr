@compute @workgroup_size(8,8)
fn comparison_candidate(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.size || id.y >= params.size { return; }
    let p = comparison_surface(id.xy,params.size);
    let emitter = comparison_emitter(params.shape,p);
    if emitter >= 0.0 {
        output[id.x+params.size*id.y] = vec4(vec3(emitter),0.0);
        return;
    }
    let entry = lookup(COMPARE_ROUGH,COMPARE_NV);
    let t = entry.matrix;
    let matrix = mat3x3(vec3(t.x,0.0,t.y),vec3(0.0,1.0,0.0),vec3(t.z,0.0,t.w));
    var center = vec3(0.0,0.0,2.0)-p;
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
    for (var i=0u; i<count; i++) {
        let a = vertex(i,center,u,v,radius);
        let b = vertex((i+1u)%count,center,u,v,radius);
        if a.z>0.0 { integral=append(integral,a,matrix); }
        if (a.z>0.0)!=(b.z>0.0) {
            integral=append(integral,a+(b-a)*(a.z/(a.z-b.z)),matrix);
        }
    }
    if integral.count>0u { integral=append(integral,integral.first,matrix); }
    if integral.specular.count>0u {
        integral.specular=push(integral.specular,integral.specular.first);
    }
    let specular = abs(integral.specular.sum)/(2.0*PI)
        *(0.7*entry.amplitude.x+0.3*entry.amplitude.y);
    output[id.x+params.size*id.y] = vec4(vec3(specular),1.0);
}

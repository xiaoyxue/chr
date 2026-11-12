//! Synthetic scalar method gate, not a renderer light API or production integrator.
//! LTC tables and parameterization: Heitz et al., SIGGRAPH 2016, official reference
//! revision 31e5e96b54f98f33098f8503003119ba2231a1c6. See LICENSE.ltc and the report.

use std::{error::Error, f64::consts::PI, fs, io::Write};

mod comparison;
mod gpu;
mod profile;
mod profile_gpu;

#[derive(Clone, Copy, Debug)]
struct V(f64, f64, f64);
impl V {
    fn dot(self, b: Self) -> f64 {
        self.0 * b.0 + self.1 * b.1 + self.2 * b.2
    }
    fn cross(self, b: Self) -> Self {
        Self(
            self.1 * b.2 - self.2 * b.1,
            self.2 * b.0 - self.0 * b.2,
            self.0 * b.1 - self.1 * b.0,
        )
    }
    fn length(self) -> f64 {
        self.dot(self).sqrt()
    }
    fn unit(self) -> Self {
        self * (1.0 / self.length())
    }
}
impl std::ops::Add for V {
    type Output = Self;
    fn add(self, b: Self) -> Self {
        Self(self.0 + b.0, self.1 + b.1, self.2 + b.2)
    }
}
impl std::ops::Sub for V {
    type Output = Self;
    fn sub(self, b: Self) -> Self {
        self + b * -1.0
    }
}
impl std::ops::Mul<f64> for V {
    type Output = Self;
    fn mul(self, b: f64) -> Self {
        Self(self.0 * b, self.1 * b, self.2 * b)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    Rectangle,
    Disk,
    Sphere,
}

#[derive(Clone, Copy)]
struct Light {
    shape: Shape,
    center: V,
    radius: f64,
    half_height: f64,
    tilt: f64,
    two_sided: bool,
    radiance: f64,
}
impl Light {
    fn validate(self) -> Result<(), &'static str> {
        if ![
            self.center.0,
            self.center.1,
            self.center.2,
            self.radius,
            self.half_height,
            self.tilt,
            self.radiance,
        ]
        .iter()
        .all(|v| v.is_finite())
            || self.radius <= 0.0
            || self.half_height <= 0.0
            || self.radiance < 0.0
        {
            return Err("invalid geometry/emission");
        }
        if self.shape == Shape::Sphere {
            if self.two_sided {
                return Err("spheres emit outward only");
            }
            if (self.center.length() - self.radius).abs() < 1e-10 {
                return Err("receiver on emitter");
            }
        } else if self.normal().dot(self.center).abs() < 1e-10 {
            return Err("coplanar receiver is outside this experiment");
        }
        Ok(())
    }
    fn axes(self) -> (V, V) {
        (V(self.tilt.cos(), 0.0, -self.tilt.sin()), V(0.0, 1.0, 0.0))
    }
    fn normal(self) -> V {
        let (u, v) = self.axes();
        u.cross(v) * -1.0
    }
    fn area(self) -> f64 {
        match self.shape {
            Shape::Rectangle => 4.0 * self.radius * self.half_height,
            Shape::Disk => PI * self.radius * self.radius,
            Shape::Sphere => 4.0 * PI * self.radius * self.radius,
        }
    }
    fn flux(self) -> f64 {
        self.radiance * PI * self.area() * if self.two_sided { 2.0 } else { 1.0 }
    }
    fn inside(self) -> bool {
        self.shape == Shape::Sphere && self.center.length() < self.radius
    }
    fn sample(self, u: f64, v: f64) -> (V, V) {
        let (x, y) = self.axes();
        match self.shape {
            Shape::Rectangle => (
                self.center
                    + x * ((2.0 * u - 1.0) * self.radius)
                    + y * ((2.0 * v - 1.0) * self.half_height),
                self.normal(),
            ),
            Shape::Disk => {
                let r = self.radius * u.sqrt();
                let p = 2.0 * PI * v;
                (
                    self.center + x * (r * p.cos()) + y * (r * p.sin()),
                    self.normal(),
                )
            }
            Shape::Sphere => {
                let z = 1.0 - 2.0 * u;
                let r = (1.0 - z * z).sqrt();
                let p = 2.0 * PI * v;
                let n = V(r * p.cos(), r * p.sin(), z);
                (self.center + n * self.radius, n)
            }
        }
    }
    fn polygon(self, sides: usize) -> Vec<V> {
        if self.inside()
            || (self.shape != Shape::Sphere
                && !self.two_sided
                && self.normal().dot(self.center) >= 0.0)
        {
            return vec![];
        }
        let (mut u, mut v) = self.axes();
        let mut center = self.center;
        let mut radius = self.radius;
        if self.shape == Shape::Sphere {
            // The tangent silhouette, NOT a disk of radius r at the sphere centre.
            let d = center.length();
            let w = center * (1.0 / d);
            u = V(0.0, 1.0, 0.0).cross(w).unit();
            v = w.cross(u);
            let factor = 1.0 - (radius / d).powi(2);
            center = center * factor;
            radius *= factor.sqrt();
        }
        if self.shape == Shape::Rectangle {
            [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                .map(|(x, y)| center + u * (x * radius) + v * (y * self.half_height))
                .to_vec()
        } else {
            (0..sides)
                .map(|i| {
                    let p = 2.0 * PI * i as f64 / sides as f64;
                    center + u * (radius * p.cos()) + v * (radius * p.sin())
                })
                .collect()
        }
    }
}

#[derive(Clone, Copy)]
struct Material {
    roughness: f64,
    metal: f64,
    nv: f64,
}
impl Material {
    fn view(self) -> V {
        V((1.0 - self.nv * self.nv).sqrt(), 0.0, self.nv)
    }
    fn f0(self) -> f64 {
        0.04 * (1.0 - self.metal) + 0.7 * self.metal
    }
    // Returns BRDF*cos_receiver, matching mesh.wgsl (cosine is included once).
    fn brdf_cos(self, l: V) -> [f64; 3] {
        if l.2 <= 0.0 {
            return [0.0; 3];
        }
        let h = (self.view() + l).unit();
        let vh = self.view().dot(h).max(0.0);
        let a2 = self.roughness.powi(4);
        let d = a2 / (PI * (h.2 * h.2 * (a2 - 1.0) + 1.0).powi(2));
        let vis = 0.5
            / (l.2 * (self.nv * self.nv * (1.0 - a2) + a2).sqrt()
                + self.nv * (l.2 * l.2 * (1.0 - a2) + a2).sqrt());
        let f = self.f0() + (1.0 - self.f0()) * (1.0 - vh).powi(5);
        let fd90 = 0.5 + 2.0 * self.roughness * vh * vh;
        let diffuse = (1.0 + (fd90 - 1.0) * (1.0 - l.2).powi(5))
            * (1.0 + (fd90 - 1.0) * (1.0 - self.nv).powi(5))
            / PI;
        [
            l.2 * d * vis,
            l.2 * d * vis * f,
            l.2 * diffuse * 0.7 * (1.0 - self.metal) * (1.0 - f),
        ]
    }
}

struct Table(Vec<[f64; 6]>);
impl Table {
    fn load() -> Self {
        Self(
            include_bytes!("ltc.bin")
                .chunks_exact(24)
                .map(|bytes| {
                    std::array::from_fn(|i| {
                        let k = 4 * i;
                        f32::from_le_bytes([bytes[k], bytes[k + 1], bytes[k + 2], bytes[k + 3]])
                            as f64
                    })
                })
                .collect(),
        )
    }
    fn lookup(&self, m: Material) -> Ltc {
        let x = m.roughness.clamp(0.0, 1.0) * 63.0;
        let y = (1.0 - m.nv.clamp(0.0, 1.0)).sqrt() * 63.0;
        let ix = x.floor() as usize;
        let iy = y.floor() as usize;
        let values = std::array::from_fn(|k| {
            let a = self.0[ix + 64 * iy][k];
            let b = self.0[(ix + 1).min(63) + 64 * iy][k];
            let c = self.0[ix + 64 * (iy + 1).min(63)][k];
            let d = self.0[(ix + 1).min(63) + 64 * (iy + 1).min(63)][k];
            let top = a + (b - a) * (x - ix as f64);
            let bottom = c + (d - c) * (x - ix as f64);
            top + (bottom - top) * (y - iy as f64)
        });
        Ltc(values)
    }
}
#[derive(Clone, Copy)]
struct Ltc([f64; 6]);
impl Ltc {
    fn transform(self, p: V) -> V {
        let t = self.0;
        V(t[0] * p.0 + t[2] * p.2, p.1, t[1] * p.0 + t[3] * p.2)
    }
    fn density(self, l: V) -> f64 {
        if l.2 <= 0.0 {
            return 0.0;
        }
        let q = self.transform(l);
        let det = (self.0[0] * self.0[3] - self.0[1] * self.0[2]).abs();
        det * q.2.max(0.0) / (PI * q.dot(q).powi(2))
    }
    fn amplitude(self, m: Material) -> f64 {
        m.f0() * self.0[4] + (1.0 - m.f0()) * self.0[5]
    }
}

fn clip(poly: &[V]) -> Vec<V> {
    let mut result = Vec::new();
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        if a.2 > 0.0 {
            result.push(a);
        }
        if (a.2 > 0.0) != (b.2 > 0.0) {
            result.push(a + (b - a) * (a.2 / (a.2 - b.2)));
        }
    }
    result
}

fn cosine_polygon(poly: &[V]) -> f64 {
    let p: Vec<_> = clip(poly).iter().map(|p| p.unit()).collect();
    let mut sum = 0.0;
    for i in 0..p.len() {
        let a = p[i];
        let b = p[(i + 1) % p.len()];
        let cross = a.cross(b);
        let sine = cross.length();
        if sine > 1e-14 {
            sum += cross.2 * sine.atan2(a.dot(b)) / sine;
        }
    }
    sum.abs() / (2.0 * PI)
}

// [Lambert unit albedo, F=1 GGX, Schlick specular, approximate diffuse].
fn candidate(light: Light, m: Material, ltc: Ltc, sides: usize) -> [f64; 4] {
    let poly = clip(&light.polygon(sides));
    let lambert = cosine_polygon(&poly) * light.radiance;
    let transformed: Vec<_> = poly.iter().map(|&p| ltc.transform(p)).collect();
    let spec = cosine_polygon(&transformed) * light.radiance;
    [
        lambert,
        spec * ltc.0[4],
        spec * ltc.amplitude(m),
        lambert * 0.7 * (1.0 - m.metal) * (1.0 - m.f0()),
    ]
}

// Uniform surface PDF = 1/area. The fifth channel integrates the fitted density
// over the actual curved shape, separating polygon discretization from BRDF fit.
fn reference(light: Light, m: Material, ltc: Ltc, n: usize) -> [f64; 5] {
    if light.inside() {
        return [0.0; 5];
    }
    let mut sum = [0.0; 5];
    for i in 0..n {
        for j in 0..n {
            let (p, normal) =
                light.sample((i as f64 + 0.5) / n as f64, (j as f64 + 0.5) / n as f64);
            let r2 = p.dot(p);
            let l = p * (1.0 / r2.sqrt());
            let cosine = -normal.dot(l);
            let emitter_cos = if light.two_sided {
                cosine.abs()
            } else {
                cosine.max(0.0)
            };
            if emitter_cos == 0.0 || l.2 <= 0.0 {
                continue;
            }
            let weight = light.radiance * light.area() * emitter_cos / r2 / (n * n) as f64;
            let b = m.brdf_cos(l);
            let terms = [
                l.2 / PI,
                b[0],
                b[1],
                b[2],
                ltc.density(l) * ltc.amplitude(m),
            ];
            for k in 0..5 {
                sum[k] += weight * terms[k];
            }
        }
    }
    sum
}

// Independent solid-angle check for an outward sphere or an axial facing disk.
// This does not replace the required surface-PDF reference.
fn cone_reference(light: Light, m: Material, n: usize) -> Option<[f64; 3]> {
    if light.inside() {
        return Some([0.0; 3]);
    }
    let d = light.center.length();
    let cos_max = match light.shape {
        Shape::Sphere => (1.0 - (light.radius / d).powi(2)).sqrt(),
        Shape::Disk if light.center.0 == 0.0 && light.tilt == 0.0 => {
            d / (d * d + light.radius * light.radius).sqrt()
        }
        _ => return None,
    };
    let w = light.center * (1.0 / d);
    let u = V(0.0, 1.0, 0.0).cross(w).unit();
    let v = w.cross(u);
    let weight = light.radiance * 2.0 * PI * (1.0 - cos_max) / (n * n) as f64;
    let mut sum = [0.0; 3];
    for i in 0..n {
        let c = 1.0 - (1.0 - cos_max) * (i as f64 + 0.5) / n as f64;
        let s = (1.0 - c * c).sqrt();
        for j in 0..n {
            let phi = 2.0 * PI * (j as f64 + 0.5) / n as f64;
            let l = w * c + u * (s * phi.cos()) + v * (s * phi.sin());
            let b = m.brdf_cos(l);
            for (dst, term) in sum.iter_mut().zip([l.2.max(0.0) / PI, b[1], b[2]]) {
                *dst += weight * term;
            }
        }
    }
    Some(sum)
}

fn default_light(shape: Shape) -> Light {
    Light {
        shape,
        center: V(0.0, 0.0, 3.0),
        radius: 1.0,
        half_height: 0.6,
        tilt: 0.0,
        two_sided: false,
        radiance: 1.0,
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().nth(1).as_deref() == Some("--license") {
        println!("{}", include_str!("LICENSE.ltc"));
        return Ok(());
    }
    let table = Table::load();
    if std::env::args().nth(1).as_deref() == Some("--images") {
        return pollster::block_on(comparison::run(&table));
    }
    if std::env::args().nth(1).as_deref() == Some("--profile") {
        return profile::run(&table);
    }
    if std::env::args().nth(1).as_deref() == Some("--profile-gpu") {
        return pollster::block_on(profile_gpu::run(&table));
    }
    if std::env::args().nth(1).as_deref() == Some("--gpu") {
        return pollster::block_on(gpu::run(&table));
    }
    fs::create_dir_all("output/area-light-methods")?;
    let mut csv = fs::File::create("output/area-light-methods/scalar.csv")?;
    writeln!(
        csv,
        "shape,case,roughness,metal,nv,area,flux,n,ref_lambert,ref_ggx,ref_spec,ref_diff,ltc_surface_spec,c16_lambert,c16_ggx,c16_spec,c16_diff,c64_spec,c256_spec,ref_total,abs_error,relative_error,unit_radiance_error,convergence_total,convergence_spec,polygon_spec_error,fit_spec_error,diffuse_error,cone_total,cone_delta,surface_cone_delta,c64_total,c256_total,abs64,relative64"
    )?;
    let mut count = 0;
    let mut max_abs: f64 = 0.0;
    let mut max_conv: f64 = 0.0;
    for shape in [Shape::Rectangle, Shape::Disk, Shape::Sphere] {
        for case in [
            "axial",
            "offset",
            "grazing",
            "horizon",
            "near",
            "back",
            "two_sided",
            "small",
            "large",
            "large_flux",
            "inside",
        ] {
            if case == "inside" && shape != Shape::Sphere {
                continue;
            }
            if case == "two_sided" && shape == Shape::Sphere {
                continue;
            }
            for roughness in [0.045, 0.15, 0.4, 0.85] {
                for metal in [0.0, 1.0] {
                    let mut light = default_light(shape);
                    let mut nv = 0.8;
                    match case {
                        "offset" => light.center.0 = -2.0,
                        "grazing" => {
                            light.center.0 = -3.0;
                            nv = 0.05;
                        }
                        "horizon" => {
                            light.center = V(-3.0, 0.0, 0.2);
                            light.tilt = -0.5;
                            nv = 0.05;
                        }
                        "near" => light.center.2 = if shape == Shape::Sphere { 1.02 } else { 0.05 },
                        "back" => light.tilt = PI,
                        "two_sided" => {
                            light.tilt = PI;
                            light.two_sided = true;
                        }
                        "small" => {
                            light.radius *= 0.1;
                            light.half_height *= 0.1;
                        }
                        "large_flux" => {
                            let flux = light.flux();
                            light.radius *= 2.0;
                            light.half_height *= 2.0;
                            light.radiance *= flux / light.flux();
                        }
                        "large" => {
                            light.radius *= 2.0;
                            light.half_height *= 2.0;
                        }
                        "inside" => light.center.2 = 0.5,
                        _ => {}
                    }
                    light.validate()?;
                    let m = Material {
                        roughness,
                        metal,
                        nv,
                    };
                    let ltc = table.lookup(m);
                    let low = reference(light, m, ltc, 512);
                    let high = reference(light, m, ltc, 1024);
                    let c = candidate(light, m, ltc, 16);
                    let c64 = candidate(light, m, ltc, 64);
                    let c256 = candidate(light, m, ltc, 256);
                    let total = high[2] + high[3];
                    let error = (c[2] + c[3] - total).abs();
                    let relative = if total > 0.0 {
                        format!("{:.9e}", error / total)
                    } else {
                        "undefined".into()
                    };
                    let convergence = (total - low[2] - low[3]).abs();
                    let total64 = c64[2] + c64[3];
                    let total256 = c256[2] + c256[3];
                    let error64 = (total64 - total).abs();
                    let relative64 = if total > 0.0 {
                        format!("{:.9e}", error64 / total)
                    } else {
                        "undefined".into()
                    };
                    let cone = cone_reference(light, m, 1024);
                    let cone_low = cone_reference(light, m, 512);
                    let cone_fields = match (cone, cone_low) {
                        (Some(a), Some(b)) => format!(
                            "{:.9e},{:.9e},{:.9e}",
                            a[1] + a[2],
                            (a[1] + a[2] - b[1] - b[2]).abs(),
                            (total - a[1] - a[2]).abs()
                        ),
                        _ => "unavailable,unavailable,unavailable".into(),
                    };
                    max_abs = max_abs.max(error);
                    max_conv = max_conv.max(convergence);
                    writeln!(
                        csv,
                        "{shape:?},{case},{roughness},{metal},{nv},{:.9e},{:.9e},1024,{:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{total:.9e},{error:.9e},{relative},{:.9e},{convergence:.9e},{:.9e},{:.9e},{:.9e},{:.9e},{cone_fields},{total64:.9e},{total256:.9e},{error64:.9e},{relative64}",
                        light.area(),
                        light.flux(),
                        high[0],
                        high[1],
                        high[2],
                        high[3],
                        high[4],
                        c[0],
                        c[1],
                        c[2],
                        c[3],
                        c64[2],
                        c256[2],
                        error / light.radiance,
                        (high[2] - low[2]).abs(),
                        c[2] - high[4],
                        high[4] - high[2],
                        c[3] - high[3]
                    )?;
                    count += 1;
                }
            }
        }
    }
    println!(
        "scalar rows={count} max_absolute_error={max_abs:.9e} max_reference_delta={max_conv:.9e}"
    );
    println!(
        "All rows retained; reference deltas are empirical, not universal convergence bounds."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn material() -> Material {
        Material {
            roughness: 0.4,
            metal: 0.0,
            nv: 1.0,
        }
    }
    #[test]
    fn analytic_lambert_and_sphere_silhouette() {
        let m = material();
        let ltc = Table::load().lookup(m);
        for shape in [Shape::Disk, Shape::Sphere] {
            let light = default_light(shape);
            let expected = if shape == Shape::Disk { 0.1 } else { 1.0 / 9.0 };
            assert!((reference(light, m, ltc, 512)[0] - expected).abs() < 2e-6);
            assert!((candidate(light, m, ltc, 256)[0] - expected).abs() < 2e-5);
        }
        let rectangle = default_light(Shape::Rectangle);
        assert!(
            (reference(rectangle, m, ltc, 256)[0] - candidate(rectangle, m, ltc, 4)[0]).abs()
                < 1e-6
        );
        let broad_disk = Light {
            radius: 100.0,
            center: V(0.0, 0.0, 1.0),
            ..default_light(Shape::Disk)
        };
        assert!((candidate(broad_disk, m, ltc, 256)[0] - 10000.0 / 10001.0).abs() < 1e-7);
    }
    #[test]
    fn sidedness_and_outward_sphere() {
        let m = material();
        let ltc = Table::load().lookup(m);
        for shape in [Shape::Rectangle, Shape::Disk] {
            let light = default_light(shape);
            let back = Light { tilt: PI, ..light };
            assert_eq!(reference(back, m, ltc, 64), [0.0; 5]);
            assert_eq!(candidate(back, m, ltc, 16), [0.0; 4]);
            let two = Light {
                two_sided: true,
                ..back
            };
            assert!(
                (reference(two, m, ltc, 64)[0] - reference(light, m, ltc, 64)[0]).abs() < 1e-12
            );
            assert!(
                (candidate(two, m, ltc, 64)[2] - candidate(light, m, ltc, 64)[2]).abs() < 1e-12
            );
        }
        let inside = Light {
            center: V(0.0, 0.0, 0.5),
            ..default_light(Shape::Sphere)
        };
        assert_eq!(reference(inside, m, ltc, 64), [0.0; 5]);
        assert_eq!(candidate(inside, m, ltc, 64), [0.0; 4]);
    }
    #[test]
    fn flux_size_distance_and_zero_emission() {
        let m = material();
        let ltc = Table::load().lookup(m);
        for shape in [Shape::Rectangle, Shape::Disk, Shape::Sphere] {
            let light = default_light(shape);
            let scaled = Light {
                radius: 2.0,
                half_height: 1.2,
                center: light.center * 2.0,
                ..light
            };
            assert!((scaled.flux() / light.flux() - 4.0).abs() < 1e-12);
            assert!(
                (reference(scaled, m, ltc, 64)[2] - reference(light, m, ltc, 64)[2]).abs() < 1e-12
            );
            let fixed_power = Light {
                radiance: 0.25,
                ..scaled
            };
            let enlarged = Light {
                center: light.center,
                ..scaled
            };
            assert!(candidate(enlarged, m, ltc, 64)[0] > candidate(light, m, ltc, 64)[0]);
            assert!((fixed_power.flux() - light.flux()).abs() < 1e-12);
            assert!(
                (candidate(fixed_power, m, ltc, 64)[2] * 4.0 - candidate(light, m, ltc, 64)[2])
                    .abs()
                    < 1e-12
            );
            let off = Light {
                radiance: 0.0,
                ..light
            };
            assert_eq!(reference(off, m, ltc, 16), [0.0; 5]);
            assert_eq!(candidate(off, m, ltc, 16), [0.0; 4]);
        }
    }
    #[test]
    fn validation_and_horizon_clipping() {
        let light = default_light(Shape::Rectangle);
        assert!(
            Light {
                radius: 0.0,
                ..light
            }
            .validate()
            .is_err()
        );
        assert!(
            Light {
                radiance: -1.0,
                ..light
            }
            .validate()
            .is_err()
        );
        assert!(
            Light {
                radiance: f64::NAN,
                ..light
            }
            .validate()
            .is_err()
        );
        assert!(
            Light {
                center: V(0.0, 0.0, 0.0),
                ..light
            }
            .validate()
            .is_err()
        );
        let m = material();
        let ltc = Table::load().lookup(m);
        let tilted = Light {
            center: V(3.0, 0.0, 0.2),
            tilt: 0.5,
            ..light
        };
        assert!((reference(tilted, m, ltc, 512)[0] - candidate(tilted, m, ltc, 4)[0]).abs() < 2e-6);
    }
    #[test]
    fn ltc_integral_matches_its_surface_density() {
        let m = Material {
            nv: 0.3,
            ..material()
        };
        let ltc = Table::load().lookup(m);
        let light = Light {
            center: V(-2.0, 0.0, 3.0),
            ..default_light(Shape::Rectangle)
        };
        let sampled = reference(light, m, ltc, 256);
        assert!((sampled[4] - candidate(light, m, ltc, 4)[2]).abs() < 1e-6);
    }
    #[test]
    fn surface_and_solid_angle_agree() {
        let m = material();
        let ltc = Table::load().lookup(m);
        for shape in [Shape::Disk, Shape::Sphere] {
            let light = default_light(shape);
            let surface = reference(light, m, ltc, 512);
            let cone = cone_reference(light, m, 512).unwrap();
            for (a, b) in [surface[0], surface[2], surface[3]].into_iter().zip(cone) {
                assert!((a - b).abs() < 2e-6);
            }
        }
        let near = Light {
            center: V(0.0, 0.0, 1.02),
            ..default_light(Shape::Sphere)
        };
        let cone = cone_reference(near, m, 512).unwrap();
        assert!((cone[0] - 1.0 / 1.02_f64.powi(2)).abs() < 1e-10);
        assert_eq!(m.brdf_cos(V(0.0, 0.0, -1.0)), [0.0; 3]);
    }
}

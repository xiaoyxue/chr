//! Synthetic distant-environment visibility experiment, not a renderer path.
//! Run from the workspace root; stdout is CSV. See the accompanying evaluation.

use std::f64::consts::{PI, TAU};

type Vec3 = [f64; 3];
const UP: Vec3 = [0.0, 0.0, -1.0];
const ORIGIN: Vec3 = [0.0; 3];
const PROBE_Z: usize = 32;
const PROBE_PHI: usize = 64;

fn dot(a: Vec3, b: Vec3) -> f64 {
    a.into_iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalized(a: Vec3) -> Vec3 {
    let length = dot(a, a).sqrt();
    a.map(|x| x / length)
}

#[derive(Clone, Copy)]
struct Rect {
    axis: usize,
    plane: f64,
    bounds: [[f64; 2]; 2],
}

impl Rect {
    fn hit(self, origin: Vec3, direction: Vec3) -> bool {
        if direction[self.axis].abs() < 1e-12 {
            return false;
        }
        let t = (self.plane - origin[self.axis]) / direction[self.axis];
        t > 1e-8
            && (0..2).all(|i| {
                let axis = (self.axis + 1 + i) % 3;
                let p = origin[axis] + t * direction[axis];
                p >= self.bounds[i][0] && p <= self.bounds[i][1]
            })
    }
}

fn visible(rects: &[Rect], origin: Vec3, direction: Vec3) -> f64 {
    f64::from(!rects.iter().any(|rect| rect.hit(origin, direction)))
}

struct Case {
    name: &'static str,
    rects: Vec<Rect>,
    normal: Vec3,
    grazing_view: bool,
}

fn roof(x: [f64; 2], y: [f64; 2], height: f64) -> Rect {
    Rect {
        axis: 2,
        plane: -height,
        bounds: [x, y],
    }
}

fn cases() -> Vec<Case> {
    let enclosure = (0..3)
        .flat_map(|axis| {
            [-2.0, 2.0].map(|plane| Rect {
                axis,
                plane,
                bounds: [[-2.0, 2.0]; 2],
            })
        })
        .collect();
    [
        ("open", vec![], UP, false),
        ("enclosure", enclosure, UP, false),
        ("roof", vec![roof([-0.8, 0.8], [-0.8, 0.8], 1.0)], UP, false),
        (
            "multiple",
            vec![
                roof([-1.0, -0.2], [-1.0, 0.6], 0.6),
                roof([0.1, 0.9], [-0.5, 1.0], 1.4),
                Rect {
                    axis: 0,
                    plane: 1.2,
                    bounds: [[-1.0, 1.0], [-1.5, -0.1]],
                },
            ],
            UP,
            false,
        ),
        (
            "offscreen",
            vec![roof([2.0, 3.0], [-1.0, 1.0], 1.0)],
            normalized([2.5, 0.0, -(7.25_f64.sqrt() + 1.0)]),
            false,
        ),
        (
            "thin",
            vec![roof([-0.015, 0.015], [-1.5, 1.5], 0.4)],
            UP,
            false,
        ),
        (
            "grazing_receiver",
            vec![roof([0.1, 1.0], [-0.8, 0.8], 0.4)],
            normalized([1.0, 0.0, -0.2]),
            false,
        ),
        (
            "grazing_view",
            vec![roof([-1.0, 1.0], [-0.8, 0.8], 0.4)],
            UP,
            true,
        ),
    ]
    .into_iter()
    .map(|(name, rects, normal, grazing_view)| Case {
        name,
        rects,
        normal,
        grazing_view,
    })
    .collect()
}

#[derive(Clone, Copy)]
struct Sample {
    direction: Vec3,
    weight: f64,
}

fn direction(z: f64, phi: f64) -> Vec3 {
    let r = (1.0 - z * z).max(0.0).sqrt();
    [r * phi.cos(), r * phi.sin(), z]
}

fn grid(nz: usize, nphi: usize) -> Vec<Sample> {
    (0..nz)
        .flat_map(|iz| {
            (0..nphi).map(move |ip| Sample {
                direction: direction(
                    -1.0 + 2.0 * (iz as f64 + 0.5) / nz as f64,
                    TAU * (ip as f64 + 0.5) / nphi as f64,
                ),
                weight: 4.0 * PI / (nz * nphi) as f64,
            })
        })
        .collect()
}

fn oracle_grid(ntheta: usize, nphi: usize) -> Vec<Sample> {
    (0..ntheta)
        .flat_map(|it| {
            let lo = PI * it as f64 / ntheta as f64;
            let hi = PI * (it + 1) as f64 / ntheta as f64;
            (0..nphi).map(move |ip| Sample {
                direction: direction(
                    -((lo + hi) * 0.5).cos(),
                    TAU * (ip as f64 + 0.5) / nphi as f64,
                ),
                weight: (lo.cos() - hi.cos()) * TAU / nphi as f64,
            })
        })
        .collect()
}

fn fibonacci(count: usize) -> Vec<Sample> {
    (0..count)
        .map(|i| Sample {
            direction: direction(
                -1.0 + 2.0 * (i as f64 + 0.5) / count as f64,
                TAU * (i as f64 * 0.618_033_988_749_894_9).fract(),
            ),
            weight: 4.0 * PI / count as f64,
        })
        .collect()
}

fn radiance(direction: Vec3, hdr: bool) -> f64 {
    if hdr {
        // A smooth bright cap avoids an oracle dominated by a discontinuous HDR texel.
        let alignment = dot(direction, normalized([0.3, 0.0, -1.0]));
        0.05 + 40.0 * (80.0 * (alignment - 1.0)).exp()
    } else {
        1.0
    }
}

fn environment_samples(count: usize, hdr: bool) -> Vec<Sample> {
    let cells = grid(64, 128);
    let total: f64 = cells.iter().map(|s| radiance(s.direction, hdr)).sum();
    let masses: Vec<_> = cells
        .iter()
        .map(|s| 0.5 / cells.len() as f64 + 0.5 * radiance(s.direction, hdr) / total)
        .collect();
    let rows: Vec<f64> = masses.chunks(128).map(|row| row.iter().sum()).collect();
    let choose = |weights: &[f64], u: f64| {
        let mut sum = 0.0;
        weights
            .iter()
            .position(|w| {
                sum += w;
                sum > u
            })
            .unwrap_or(weights.len() - 1)
    };
    (0..count)
        .map(|i| {
            let row = choose(&rows, (i as f64 + 0.5) / count as f64);
            let phi_u = (i as u32).reverse_bits() as f64 / 4_294_967_296.0;
            let col = choose(&masses[row * 128..(row + 1) * 128], phi_u * rows[row]);
            let index = row * 128 + col;
            Sample {
                direction: cells[index].direction,
                weight: cells[index].weight / (count as f64 * masses[index]),
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Lobe {
    Diffuse,
    Ggx(f64),
}

impl Lobe {
    fn name(self) -> &'static str {
        match self {
            Self::Diffuse => "diffuse",
            Self::Ggx(r) if r > 0.3 => "ggx_0.5",
            Self::Ggx(_) => "ggx_0.2",
        }
    }

    fn kernel(self, n: Vec3, v: Vec3, l: Vec3) -> f64 {
        let nl = dot(n, l);
        let nv = dot(n, v);
        if nl <= 0.0 || nv <= 0.0 {
            return 0.0;
        }
        let Self::Ggx(roughness) = self else {
            return nl / PI;
        };
        let h = normalized(std::array::from_fn(|i| v[i] + l[i]));
        let nh = dot(n, h).max(0.0);
        let vh = dot(v, h).max(0.0);
        let a2 = roughness.powi(4);
        let d = a2 / (PI * (nh * nh * (a2 - 1.0) + 1.0).powi(2));
        let smith = 0.5
            / (nl * (nv * nv * (1.0 - a2) + a2).sqrt() + nv * (nl * nl * (1.0 - a2) + a2).sqrt());
        let fresnel = 0.04 + 0.96 * (1.0 - vh).powi(5);
        d * smith * fresnel * nl
    }
}

struct Mask(Vec<f64>);

impl Mask {
    fn capture(rects: &[Rect], origin: Vec3) -> Self {
        Self(
            grid(PROBE_Z, PROBE_PHI)
                .iter()
                .map(|s| visible(rects, origin, s.direction))
                .collect(),
        )
    }

    fn lookup(&self, l: Vec3) -> f64 {
        let iz = (((l[2] + 1.0) * 0.5 * PROBE_Z as f64) as usize).min(PROBE_Z - 1);
        let ip = ((l[1].atan2(l[0]).rem_euclid(TAU) / TAU * PROBE_PHI as f64) as usize)
            .min(PROBE_PHI - 1);
        self.0[iz * PROBE_PHI + ip]
    }
}

struct Surface {
    p: Vec3,
    n: Vec3,
    v: Vec3,
    lobe: Lobe,
    hdr: bool,
}

fn integrate(samples: &[Sample], surface: &Surface, visibility: impl Fn(Vec3) -> f64) -> f64 {
    samples
        .iter()
        .map(|s| {
            s.weight
                * radiance(s.direction, surface.hdr)
                * surface.lobe.kernel(surface.n, surface.v, s.direction)
                * visibility(s.direction)
        })
        .sum()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let coarse = oracle_grid(256, 512);
    let fine = oracle_grid(512, 1024);
    let mut max_delta: f64 = 0.0;
    println!(
        "scene,hdr,lobe,frame,method,estimate,reference,unoccluded,normalized_error,oracle_delta"
    );
    for hdr in [false, true] {
        let directional = [
            ("uniform32", fibonacci(32)),
            ("uniform128", fibonacci(128)),
            ("env32", environment_samples(32, hdr)),
            ("env128", environment_samples(128, hdr)),
        ];
        for case in cases() {
            let shared = Mask::capture(&case.rects, ORIGIN);
            for frame in -2..=2 {
                let t = f64::from(frame);
                let p = [0.25 * t, 0.0, 0.0];
                let local = Mask::capture(&case.rects, p);
                let v = if case.grazing_view {
                    normalized([1.0, 0.1 * t, -0.2])
                } else if case.name == "offscreen" {
                    normalized([0.3 * t, 0.0, -1.0])
                } else {
                    normalized([case.normal[0] + 0.3 * t, 0.0, case.normal[2]])
                };
                let ao_surface = Surface {
                    p,
                    n: case.normal,
                    v,
                    lobe: Lobe::Diffuse,
                    hdr: false,
                };
                let ao = integrate(&fine, &ao_surface, |l| visible(&case.rects, p, l))
                    / integrate(&fine, &ao_surface, |_| 1.0);
                for lobe in [Lobe::Diffuse, Lobe::Ggx(0.5), Lobe::Ggx(0.2)] {
                    let surface = Surface {
                        lobe,
                        hdr,
                        ..ao_surface
                    };
                    let truth = |l| visible(&case.rects, surface.p, l);
                    let reference = integrate(&fine, &surface, truth);
                    let unoccluded = integrate(&fine, &surface, |_| 1.0);
                    let delta =
                        (integrate(&coarse, &surface, truth) - reference).abs() / unoccluded;
                    max_delta = max_delta.max(delta);
                    let report = |name: &str, estimate: f64| {
                        println!(
                            "{},{hdr},{},{frame},{name},{estimate:.9},{reference:.9},{unoccluded:.9},{:.9},{delta:.9}",
                            case.name,
                            lobe.name(),
                            (estimate - reference).abs() / unoccluded
                        );
                    };
                    for (name, samples) in &directional {
                        report(name, integrate(samples, &surface, truth));
                    }
                    report(
                        "local_mask",
                        integrate(&fine, &surface, |l| local.lookup(l)),
                    );
                    report(
                        "shared_mask",
                        integrate(&fine, &surface, |l| shared.lookup(l)),
                    );
                    report("scalar_ao", ao * unoccluded);
                    if case.name == "offscreen" {
                        report("missing_blocker", unoccluded);
                    }
                }
            }
        }
    }
    eprintln!("maximum normalized oracle delta: {max_delta:.9}");
    if max_delta > 0.01 {
        return Err("oracle convergence exceeds the evaluation's 1% limit".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(lobe: Lobe) -> Surface {
        Surface {
            p: ORIGIN,
            n: UP,
            v: UP,
            lobe,
            hdr: false,
        }
    }

    #[test]
    fn solid_angle_and_lambertian_normalization() {
        let samples = grid(32, 64);
        assert!((samples.iter().map(|s| s.weight).sum::<f64>() - 4.0 * PI).abs() < 1e-10);
        assert!((integrate(&samples, &surface(Lobe::Diffuse), |_| 1.0) - 1.0).abs() < 1e-10);
        let tilted = Surface {
            n: normalized([1.0, 0.0, -0.2]),
            ..surface(Lobe::Diffuse)
        };
        assert!((integrate(&samples, &tilted, |_| 1.0) - 1.0).abs() < 0.005);
        assert!(
            (integrate(&oracle_grid(128, 256), &surface(Lobe::Diffuse), |_| 1.0) - 1.0).abs()
                < 0.0001
        );
    }

    #[test]
    fn ray_rect_rejects_behind_parallel_and_outside_but_keeps_thin_blockers() {
        let rect = roof([-0.015, 0.015], [-1.0, 1.0], 0.4);
        assert!(rect.hit(ORIGIN, UP));
        assert!(!rect.hit(ORIGIN, [0.0, 0.0, 1.0]));
        assert!(!rect.hit(ORIGIN, [1.0, 0.0, 0.0]));
        assert!(!rect.hit(ORIGIN, normalized([1.0, 0.0, -1.0])));
        assert_eq!(visible(&[rect], [0.02, 0.0, 0.0], UP), 1.0);
    }

    #[test]
    fn ggx_kernel_is_reciprocal_and_finite_at_grazing() {
        let n = UP;
        let v = normalized([1.0, 0.0, -0.01]);
        let l = normalized([-0.7, 0.2, -0.4]);
        for roughness in [0.2, 0.5] {
            let brdf = Lobe::Ggx(roughness);
            let a = brdf.kernel(n, v, l) / dot(n, l);
            let b = brdf.kernel(n, l, v) / dot(n, v);
            assert!(a.is_finite() && a > 0.0);
            assert!((a - b).abs() < 1e-12);
            assert_eq!(brdf.kernel(n, v, [0.0, 0.0, 1.0]), 0.0);
        }
    }

    #[test]
    fn enclosure_and_offscreen_blocker_affect_both_lobes() {
        let cases = cases();
        let samples = grid(128, 256);
        for lobe in [Lobe::Diffuse, Lobe::Ggx(0.5)] {
            let s = surface(lobe);
            assert_eq!(
                integrate(&samples, &s, |l| visible(&cases[1].rects, ORIGIN, l)),
                0.0
            );
            let open = integrate(&samples, &s, |_| 1.0);
            let blocked = integrate(&samples, &s, |l| visible(&cases[4].rects, ORIGIN, l));
            assert!(blocked < open * 0.999);
        }
        // Eye (0,0,-4), optical axis +Z, 40-degree horizontal FOV:
        // the offscreen quad's nearest x=2 exceeds the frustum edge at its z.
        assert!(2.0 > 3.0 * 20_f64.to_radians().tan());
        let tilted_mirror = Surface {
            n: cases[4].normal,
            ..surface(Lobe::Ggx(0.2))
        };
        let open = integrate(&samples, &tilted_mirror, |_| 1.0);
        let blocked = integrate(&samples, &tilted_mirror, |l| {
            visible(&cases[4].rects, ORIGIN, l)
        });
        assert!(blocked < open * 0.5);
    }

    #[test]
    fn directional_mask_exposes_parallax_and_ao_specular_mismatch() {
        let samples = grid(128, 256);
        let rects = [roof([-0.15, 0.15], [-0.15, 0.15], 0.5)];
        let mask = Mask::capture(&rects, ORIGIN);
        assert_eq!(visible(&rects, [0.5, 0.0, 0.0], UP), 1.0);
        assert_eq!(mask.lookup(UP), 0.0);
        let diffuse = surface(Lobe::Diffuse);
        let glossy = surface(Lobe::Ggx(0.2));
        let ao = integrate(&samples, &diffuse, |l| visible(&rects, ORIGIN, l));
        let gloss_ratio = integrate(&samples, &glossy, |l| visible(&rects, ORIGIN, l))
            / integrate(&samples, &glossy, |_| 1.0);
        assert!(ao > 0.8 && gloss_ratio < 0.2);
    }

    #[test]
    fn importance_weights_reconstruct_radiance_and_replay_deterministically() {
        // Integrate a piecewise-constant sphere, independently of BRDF/visibility.
        let expected: f64 = grid(64, 128)
            .iter()
            .map(|s| s.weight * radiance(s.direction, true))
            .sum();
        let estimate: f64 = environment_samples(16384, true)
            .iter()
            .map(|s| s.weight * radiance(s.direction, true))
            .sum();
        assert!((estimate - expected).abs() / expected < 0.005);
        let a = environment_samples(128, true);
        let b = environment_samples(128, true);
        assert!(a.iter().zip(b).all(|(a, b)| {
            a.direction == b.direction && a.weight.to_bits() == b.weight.to_bits()
        }));
    }
}

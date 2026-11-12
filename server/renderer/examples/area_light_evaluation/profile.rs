//! Necessary-condition screen before integrating the approved experimental path.
//! Pure metals isolate the existing specular fit: Burley contributes exactly zero.

use super::*;

fn bounding_radius(light: Light) -> f64 {
    match light.shape {
        Shape::Rectangle => light.radius.hypot(light.half_height),
        Shape::Disk | Shape::Sphere => light.radius,
    }
}

fn admitted(light: Light, m: Material) -> bool {
    let radius = bounding_radius(light);
    light.validate().is_ok()
        && (0.15..=0.85).contains(&m.roughness)
        && (0.3..=1.0).contains(&m.nv)
        && (0.0..=1.0).contains(&m.metal)
        && light.center.length() >= 1.5 * radius
        && (light.shape == Shape::Sphere || light.normal().dot(light.center).abs() >= radius)
}

// Sample a solid-angle cone enclosing the emitter, then independently intersect
// rays with its geometry. There is no area PDF or surface Jacobian in this oracle.
pub(super) fn angular_reference(light: Light, m: Material, n: usize) -> f64 {
    let d = light.center.length();
    let w = light.center * (1.0 / d);
    let u = V(0.0, 1.0, 0.0).cross(w).unit();
    let v = w.cross(u);
    let cosine_max = (1.0 - (bounding_radius(light) / d).powi(2)).sqrt();
    let weight = light.radiance * 2.0 * PI * (1.0 - cosine_max) / (n * n) as f64;
    let (a, b) = light.axes();
    let normal = light.normal();
    let mut total = 0.0;
    for i in 0..n {
        let cosine = 1.0 - (1.0 - cosine_max) * (i as f64 + 0.5) / n as f64;
        let sine = (1.0 - cosine * cosine).sqrt();
        for j in 0..n {
            let phi = 2.0 * PI * (j as f64 + 0.5) / n as f64;
            let l = w * cosine + u * (sine * phi.cos()) + v * (sine * phi.sin());
            if light.shape != Shape::Sphere {
                let denominator = normal.dot(l);
                let distance = normal.dot(light.center) / denominator;
                if distance <= 0.0 || (!light.two_sided && denominator >= 0.0) {
                    continue;
                }
                let p = l * distance - light.center;
                let x = p.dot(a);
                let y = p.dot(b);
                let hit = match light.shape {
                    Shape::Rectangle => x.abs() <= light.radius && y.abs() <= light.half_height,
                    Shape::Disk => x * x + y * y <= light.radius * light.radius,
                    Shape::Sphere => unreachable!(),
                };
                if !hit {
                    continue;
                }
            }
            let brdf = m.brdf_cos(l);
            total += weight * (brdf[1] + brdf[2]);
        }
    }
    total
}

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    light: Light,
    material: Material,
}

fn cases() -> Vec<Case> {
    profile_cases(&[1.0], false)
}

// Polar 80 degrees at distance 8 keeps planar separation above the bounding radius.
const HORIZON: [(&str, V); 2] = [
    ("horizon_mirror_side", V(-7.878462, 0.0, 1.389185)),
    ("horizon_view_side", V(7.878462, 0.0, 1.389185)),
];

fn profile_cases(metals: &[f64], horizon: bool) -> Vec<Case> {
    let mut cases = Vec::new();
    for &metal in metals {
        for shape in [Shape::Rectangle, Shape::Disk, Shape::Sphere] {
            for roughness in [0.15, 0.3, 0.5, 0.85] {
                for nv in [0.3_f64, 0.5, 0.8, 1.0] {
                    let mirror_x = -3.0 * (1.0 - nv * nv).sqrt() / nv;
                    let mut centers = vec![
                        ("axial", V(0.0, 0.0, 3.0)),
                        ("mirror", V(mirror_x, 0.0, 3.0)),
                        ("edge", V(mirror_x + 1.0, 0.0, 3.0)),
                        ("off_axis", V(mirror_x, 1.0, 3.0)),
                        ("near_admitted", V(0.0, 0.0, 2.0)),
                    ];
                    if horizon {
                        centers.extend(HORIZON);
                    }
                    for (name, center) in centers {
                        cases.push(Case {
                            name,
                            light: Light {
                                center,
                                ..default_light(shape)
                            },
                            material: Material {
                                roughness,
                                nv,
                                metal,
                            },
                        });
                    }
                }
            }
        }
    }
    cases
}

/// Owner-approved `ltc-plausible-v1` approximation profile (2026-11-12): per emitter,
/// |candidate - reference| <= 0.02 * Le + 0.30 * reference. Set from measured LTC
/// behaviour; it bounds an accepted approximation, not reference accuracy.
pub(super) fn plausible_tolerance(reference: f64, radiance: f64) -> f64 {
    0.02 * radiance + 0.30 * reference
}

// The original reference-accuracy criteria, which the pinned fit fails.
fn tolerance(reference: f64, radiance: f64) -> f64 {
    let absolute = 0.005 * radiance;
    if reference >= 1e-4 * radiance {
        absolute.min(0.03 * reference)
    } else {
        absolute
    }
}

fn surface(case: Case, table: &Table, n: usize) -> f64 {
    let r = reference(case.light, case.material, table.lookup(case.material), n);
    r[2] + r[3]
}

pub(super) fn counterexample(shape: Shape, roughness: f64) -> (Light, Material) {
    (
        Light {
            center: V(0.0, 0.0, 2.0),
            ..default_light(shape)
        },
        Material {
            roughness,
            nv: 0.3,
            metal: 1.0,
        },
    )
}

pub(super) fn run(table: &Table) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all("output/area-light-native")?;
    let mut csv = fs::File::create("output/area-light-native/profile.csv")?;
    writeln!(
        csv,
        "shape,case,roughness,nv,metal,center_x,center_y,center_z,radius_bound,reference256,reference512,candidate64,absolute_error,relative_error,surface_delta,tolerance,screen"
    )?;
    let mut failures = Vec::new();
    let mut unresolved = 0;
    for case in cases() {
        if !admitted(case.light, case.material) {
            return Err("profile fixture is outside the approved envelope".into());
        }
        let low = surface(case, table, 256);
        let high = surface(case, table, 512);
        let c = candidate(case.light, case.material, table.lookup(case.material), 64);
        let value = c[2] + c[3];
        let error = (value - high).abs();
        let delta = (high - low).abs();
        let limit = tolerance(high, case.light.radiance);
        let screen = if delta > limit / 16.0 {
            unresolved += 1;
            "unresolved"
        } else if error > limit {
            failures.push((error / limit, error, case));
            "candidate_failure"
        } else {
            "within_sampled_thresholds"
        };
        let relative = if high > 0.0 {
            format!("{:.12e}", error / high)
        } else {
            "undefined".into()
        };
        writeln!(
            csv,
            "{:?},{},{},{},1,{:.12e},{:.12e},{:.12e},{:.12e},{low:.12e},{high:.12e},{value:.12e},{error:.12e},{relative},{delta:.12e},{limit:.12e},{screen}",
            case.light.shape,
            case.name,
            case.material.roughness,
            case.material.nv,
            case.light.center.0,
            case.light.center.1,
            case.light.center.2,
            bounding_radius(case.light),
        )?;
    }
    println!(
        "profile rows=240 initial_candidate_failures={} unresolved={unresolved}",
        failures.len()
    );
    // Refine the worst normalized and absolute failures per shape. This is a falsification
    // test, not a way to certify the unrefined rows or the continuous envelope.
    let mut refinement = fs::File::create("output/area-light-native/refinement.csv")?;
    writeln!(
        refinement,
        "shape,selection,case,roughness,nv,surface1024,surface2048,angular1024,angular2048,candidate64,candidate256,surface_delta,angular_delta,cross_delta,absolute_error,relative_error,tolerance,confirmed_failure"
    )?;
    for shape in [Shape::Rectangle, Shape::Disk, Shape::Sphere] {
        for selection in ["normalized", "absolute"] {
            let Some((_, _, case)) = failures
                .iter()
                .filter(|(_, _, c)| c.light.shape == shape)
                .max_by(|a, b| {
                    if selection == "absolute" {
                        a.1.total_cmp(&b.1)
                    } else {
                        a.0.total_cmp(&b.0)
                    }
                })
            else {
                continue;
            };
            let case = *case;
            let s1 = surface(case, table, 1024);
            let s2 = surface(case, table, 2048);
            let a1 = angular_reference(case.light, case.material, 1024);
            let a2 = angular_reference(case.light, case.material, 2048);
            let c = candidate(case.light, case.material, table.lookup(case.material), 64);
            let c256 = candidate(case.light, case.material, table.lookup(case.material), 256);
            let c = c[2] + c[3];
            let c256 = c256[2] + c256[3];
            let limit = tolerance(s2, case.light.radiance);
            let ds = (s2 - s1).abs();
            let da = (a2 - a1).abs();
            let cross = (s2 - a2).abs();
            let uncertainty = ds.max(da).max(cross);
            let error = (c - s2).abs();
            let confirmed = uncertainty <= limit / 16.0 && error - uncertainty > limit;
            writeln!(
                refinement,
                "{shape:?},{selection},{},{},{},{s1:.12e},{s2:.12e},{a1:.12e},{a2:.12e},{c:.12e},{c256:.12e},{ds:.12e},{da:.12e},{cross:.12e},{error:.12e},{:.12e},{limit:.12e},{confirmed}",
                case.name,
                case.material.roughness,
                case.material.nv,
                error / s2,
            )?;
            println!(
                "{shape:?}/{} roughness={} nv={} candidate={c:.9e} reference={s2:.9e} relative_error={:.4}% reference_discrepancy={uncertainty:.3e} confirmed_failure={confirmed}",
                case.name,
                case.material.roughness,
                case.material.nv,
                100.0 * error / s2,
            );
        }
    }
    println!("Evaluation completion is not quality acceptance; inspect all CSV rows.");
    Ok(())
}

// Outward spheres over the upper hemisphere (y >= 0 by symmetry), from small far
// emitters to the admitted limit centre distance = 1.5 radius, including caps that
// cross the horizon. Unit radius; distance = 1 / sin(half angle).
fn sphere_sweep(metals: &[f64]) -> Vec<Case> {
    let mut cases = Vec::new();
    for &metal in metals {
        for roughness in [0.15, 0.3, 0.5, 0.85] {
            for nv in [0.3, 0.5, 0.8, 1.0] {
                for distance in [
                    1.0 / 3_f64.to_radians().sin(),
                    1.0 / 8_f64.to_radians().sin(),
                    1.0 / 15_f64.to_radians().sin(),
                    1.0 / 25_f64.to_radians().sin(),
                    // The admitted limit; the epsilon absorbs rounding in the unit direction.
                    1.5 + 1e-9,
                ] {
                    for polar_step in 0..12 {
                        let polar = (7.5 * polar_step as f64).to_radians();
                        let azimuths: &[f64] = if polar_step == 0 {
                            &[0.0]
                        } else {
                            &[0.0, 45.0, 90.0, 135.0, 180.0]
                        };
                        for azimuth in azimuths.iter().map(|a| a.to_radians()) {
                            cases.push(Case {
                                name: "sphere_sweep",
                                light: Light {
                                    center: V(
                                        polar.sin() * azimuth.cos(),
                                        polar.sin() * azimuth.sin(),
                                        polar.cos(),
                                    ) * distance,
                                    ..default_light(Shape::Sphere)
                                },
                                material: Material {
                                    roughness,
                                    nv,
                                    metal,
                                },
                            });
                        }
                    }
                }
            }
        }
    }
    cases
}

struct Resolved {
    reference: f64,
    discrepancy: f64,
    oracle: &'static str,
}

// Area-sampled reference first; under-resolved rows fall back to the independent
// angular oracle. Either must agree with its own refinement within 1/16 of the limit.
fn plausible_reference(case: Case, table: &Table) -> Option<Resolved> {
    let low = surface(case, table, 256);
    let high = surface(case, table, 512);
    let radiance = case.light.radiance;
    if (high - low).abs() <= plausible_tolerance(high, radiance) / 16.0 {
        return Some(Resolved {
            reference: high,
            discrepancy: (high - low).abs(),
            oracle: "surface512",
        });
    }
    let coarse = angular_reference(case.light, case.material, 1024);
    let fine = angular_reference(case.light, case.material, 2048);
    let discrepancy = (fine - coarse).abs();
    (discrepancy <= plausible_tolerance(fine, radiance) / 16.0).then_some(Resolved {
        reference: fine,
        discrepancy,
        oracle: "angular2048",
    })
}

pub(super) fn run_plausible(table: &Table) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all("output/area-light-native")?;
    let mut csv = fs::File::create("output/area-light-native/plausible.csv")?;
    writeln!(
        csv,
        "shape,case,roughness,nv,metal,center_x,center_y,center_z,oracle,reference,discrepancy,candidate64,absolute_error,relative_error,tolerance,error_over_tolerance,result"
    )?;
    let (mut rows, mut failures, mut unresolved, mut worst) = (0, 0, 0, 0.0_f64);
    for case in profile_cases(&[0.0, 1.0], true)
        .into_iter()
        .chain(sphere_sweep(&[0.0, 1.0]))
    {
        if !admitted(case.light, case.material) {
            return Err("plausible fixture is outside the approved envelope".into());
        }
        rows += 1;
        let c = candidate(case.light, case.material, table.lookup(case.material), 64);
        let value = c[2] + c[3];
        let (reference, discrepancy, oracle, result, limit) = match plausible_reference(case, table)
        {
            Some(r) => {
                let limit = plausible_tolerance(r.reference, case.light.radiance);
                let error = (value - r.reference).abs();
                worst = worst.max((error + r.discrepancy) / limit);
                let result = if error + r.discrepancy <= limit {
                    "pass"
                } else {
                    failures += 1;
                    "fail"
                };
                (r.reference, r.discrepancy, r.oracle, result, limit)
            }
            None => {
                unresolved += 1;
                (f64::NAN, f64::NAN, "none", "unresolved", f64::NAN)
            }
        };
        let error = (value - reference).abs();
        let relative = if reference > 0.0 {
            format!("{:.12e}", error / reference)
        } else {
            "undefined".into()
        };
        writeln!(
            csv,
            "{:?},{},{},{},{},{:.12e},{:.12e},{:.12e},{oracle},{reference:.12e},{discrepancy:.12e},{value:.12e},{error:.12e},{relative},{limit:.12e},{:.12e},{result}",
            case.light.shape,
            case.name,
            case.material.roughness,
            case.material.nv,
            case.material.metal,
            case.light.center.0,
            case.light.center.1,
            case.light.center.2,
            error / limit,
        )?;
    }
    println!(
        "ltc-plausible-v1 rows={rows} failures={failures} unresolved={unresolved} max_(error+discrepancy)/tolerance={worst:.4}"
    );
    if failures > 0 || unresolved > 0 {
        return Err("ltc-plausible-v1 screen did not pass; inspect plausible.csv".into());
    }
    println!(
        "Sampled rows pass; this does not certify the continuous envelope or temporal behaviour."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_enforces_geometry_and_material_boundaries() {
        for case in cases() {
            assert!(admitted(case.light, case.material));
        }
        let case = cases()[0];
        assert!(!admitted(
            case.light,
            Material {
                nv: 0.299,
                ..case.material
            }
        ));
        assert!(!admitted(
            case.light,
            Material {
                roughness: 0.149,
                ..case.material
            }
        ));
        assert!(!admitted(
            Light {
                center: V(0.0, 0.0, 1.1),
                ..case.light
            },
            case.material
        ));
        assert!(!admitted(
            Light {
                tilt: PI / 2.0,
                ..case.light
            },
            case.material
        ));
    }

    #[test]
    fn independent_angular_oracle_matches_surface_transport() {
        let table = Table::load();
        for shape in [Shape::Rectangle, Shape::Disk, Shape::Sphere] {
            let case = Case {
                name: "oracle",
                light: default_light(shape),
                material: Material {
                    roughness: 0.5,
                    metal: 1.0,
                    nv: 0.8,
                },
            };
            let area = surface(case, &table, 512);
            let angle = angular_reference(case.light, case.material, 1024);
            assert!(
                (area - angle).abs() < tolerance(area, case.light.radiance) / 16.0,
                "{shape:?}: {area} {angle}"
            );
        }
    }

    #[test]
    fn admitted_specular_counterexamples_fail_quality_gates() {
        let table = Table::load();
        for shape in [Shape::Rectangle, Shape::Disk, Shape::Sphere] {
            for roughness in [0.15, 0.5] {
                let (light, material) = counterexample(shape, roughness);
                assert!(admitted(light, material));
                let case = Case {
                    name: "near_admitted",
                    light,
                    material,
                };
                let fine = surface(case, &table, 1024);
                let coarse = surface(case, &table, 512);
                let angular = angular_reference(light, material, 1024);
                let limit = tolerance(fine, light.radiance);
                assert!((fine - coarse).abs() < limit / 16.0);
                assert!((fine - angular).abs() < limit / 16.0);
                let c = candidate(light, material, table.lookup(material), 64);
                assert_eq!(c[3], 0.0);
                let error = (c[2] - fine).abs();
                assert!(fine >= 1e-4 * light.radiance);
                if roughness == 0.15 {
                    assert!(error < 0.005 * light.radiance);
                } else {
                    assert!(error > 0.005 * light.radiance);
                }
                assert!(error > limit);
            }
        }
    }

    #[test]
    fn counterexamples_meet_plausible_profile_but_not_half_energy() {
        let table = Table::load();
        for shape in [Shape::Rectangle, Shape::Disk, Shape::Sphere] {
            for roughness in [0.15, 0.5] {
                let (light, material) = counterexample(shape, roughness);
                let case = Case {
                    name: "near_admitted",
                    light,
                    material,
                };
                let fine = surface(case, &table, 1024);
                let angular = angular_reference(light, material, 1024);
                let limit = plausible_tolerance(fine, light.radiance);
                let discrepancy = (fine - angular).abs();
                assert!(discrepancy < limit / 16.0);
                let c = candidate(light, material, table.lookup(material), 64)[2];
                assert!(
                    (c - fine).abs() + discrepancy <= limit,
                    "{shape:?} {roughness}"
                );
            }
            // Not vacuous: losing half of a bright highlight (reference > 0.08 Le) fails.
            let mirror = profile_cases(&[1.0], false)
                .into_iter()
                .find(|c| {
                    c.light.shape == shape
                        && c.name == "mirror"
                        && c.material.roughness == 0.5
                        && c.material.nv == 0.8
                })
                .expect("mirror fixture");
            let bright = surface(mirror, &table, 512);
            assert!(
                0.5 * bright > plausible_tolerance(bright, 1.0),
                "{shape:?} {bright}"
            );
        }
    }

    #[test]
    fn horizon_fixtures_are_admitted_near_the_horizon() {
        for (_, center) in HORIZON {
            assert!((center.length() - 8.0).abs() < 1e-5);
            assert!((center.2 / center.length() - 80_f64.to_radians().cos()).abs() < 1e-6);
        }
        assert_eq!(profile_cases(&[0.0, 1.0], true).len(), 672);
        let sweep = sphere_sweep(&[1.0]);
        assert_eq!(sweep.len(), 16 * 280);
        assert!(sweep.iter().all(|c| admitted(c.light, c.material)));
    }
}

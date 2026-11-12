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
    let mut cases = Vec::new();
    for shape in [Shape::Rectangle, Shape::Disk, Shape::Sphere] {
        for roughness in [0.15, 0.3, 0.5, 0.85] {
            for nv in [0.3_f64, 0.5, 0.8, 1.0] {
                let mirror_x = -3.0 * (1.0 - nv * nv).sqrt() / nv;
                for (name, center) in [
                    ("axial", V(0.0, 0.0, 3.0)),
                    ("mirror", V(mirror_x, 0.0, 3.0)),
                    ("edge", V(mirror_x + 1.0, 0.0, 3.0)),
                    ("off_axis", V(mirror_x, 1.0, 3.0)),
                    ("near_admitted", V(0.0, 0.0, 2.0)),
                ] {
                    cases.push(Case {
                        name,
                        light: Light {
                            center,
                            ..default_light(shape)
                        },
                        material: Material {
                            roughness,
                            nv,
                            metal: 1.0,
                        },
                    });
                }
            }
        }
    }
    cases
}

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
}

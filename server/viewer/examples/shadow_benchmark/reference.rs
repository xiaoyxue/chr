use chr_renderer::{
    camera::Camera,
    coordinates::ImageExtent,
    scene::{InstanceId, Light, Lighting, MeshData, ModelMatrix, PbrMaterial, Scene},
};
use serde::Serialize;

use super::Result;

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Directional,
    Spot,
    Point,
}

impl Kind {
    pub fn layers(self) -> u32 {
        if matches!(self, Self::Point) { 6 } else { 1 }
    }
}

#[derive(Clone, Copy)]
pub struct Blocker {
    pub center: [f64; 3],
}

impl Blocker {
    fn intersects(self, origin: [f64; 3], direction: [f64; 3], max_t: f64) -> bool {
        let t = (self.center[2] - origin[2]) / direction[2];
        t > 0.0
            && t < max_t
            && (0..2).all(|i| (origin[i] + t * direction[i] - self.center[i]).abs() <= 0.5)
    }

    fn model(self) -> Result<ModelMatrix> {
        let mut rows = ModelMatrix::IDENTITY.rows();
        for (row, value) in rows.iter_mut().zip(self.center) {
            row[3] = value;
        }
        Ok(ModelMatrix::new(rows)?)
    }
}

pub struct Fixture {
    pub scene: Scene,
    pub camera: Camera,
    pub kind: Kind,
    pub radius: f64,
    pub blockers: Vec<Blocker>,
    caster: InstanceId,
}

impl Fixture {
    pub fn new(kind: Kind, name: &str) -> Result<Self> {
        let height = if name == "contact" { 0.3 } else { 1.5 };
        let radius = if matches!(kind, Kind::Directional) {
            0.12
        } else {
            0.4
        } * if name == "large_penumbra" { 2.0 } else { 1.0 };
        let mut blockers = vec![Blocker {
            center: [0.0, 0.0, -height],
        }];
        if name == "multiple_casters" {
            blockers.push(Blocker {
                center: [0.9, 0.35, -0.8],
            });
        }
        let extent = ImageExtent::new(384, 384)?;
        let (eye, target) = if name == "grazing_view" {
            ([0.0, -5.0, -2.5], [1.0, 0.0, 0.0])
        } else {
            ([0.0, 0.0, -5.0], [0.0; 3])
        };
        let camera = Camera::look_at(extent, eye, target, [0.0, -1.0, 0.0], 1.0, 0.1, 100.0)?;
        let mut scene = Scene::new()?;
        let mesh = scene.add_mesh(MeshData::new(
            vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
            vec![0, 2, 1, 0, 3, 2],
            std::iter::once(0..6).collect(),
        )?)?;
        let material = scene.add_material(PbrMaterial {
            roughness: 1.0,
            ..Default::default()
        })?;
        let mut ground = ModelMatrix::IDENTITY.rows();
        ground[0][0] = 8.0;
        ground[1][1] = 8.0;
        scene.add_instance(mesh, vec![material], ModelMatrix::new(ground)?)?;
        let caster = scene.add_instance(mesh, vec![material], blockers[0].model()?)?;
        for blocker in &blockers[1..] {
            scene.add_instance(mesh, vec![material], blocker.model()?)?;
        }
        let light = match kind {
            Kind::Directional => Light::Directional {
                direction: [0.7, 0.0, 1.0],
                irradiance: [3.0; 3],
                angular_radius: radius as f32,
                shadow_mode: None,
            },
            Kind::Spot => Light::Spot {
                position: [-3.0, 0.0, -5.0],
                direction: [0.6, 0.0, 1.0],
                intensity: [100.0; 3],
                inner_angle: 0.7,
                outer_angle: 1.0,
                source_radius: radius as f32,
                shadow_mode: None,
            },
            Kind::Point => Light::Point {
                position: [-3.0, 0.0, -5.0],
                intensity: [100.0; 3],
                source_radius: radius as f32,
                shadow_mode: None,
            },
        };
        scene.set_lighting(Lighting::new(vec![light], None)?)?;
        Ok(Self {
            scene,
            camera,
            kind,
            radius,
            blockers,
            caster,
        })
    }

    /// Publish a fresh scene revision with identical geometry, invalidating depth reuse
    /// without changing pixels, map dimensions, pipelines or filtering parameters.
    pub fn invalidate(&mut self) -> Result<()> {
        self.scene
            .set_model(self.caster, self.blockers[0].model()?)?;
        Ok(())
    }

    fn visibility(&self, receiver: [f64; 3], radius: f64, side: u32) -> f64 {
        let slope: f64 = if matches!(self.kind, Kind::Directional) {
            0.7
        } else {
            0.6
        };
        let length = (1.0 + slope * slope).sqrt();
        let mut visible = 0;
        let mut count = 0;
        for row in 0..side {
            for col in 0..side {
                let u = (f64::from(col) + 0.5) * 2.0 / f64::from(side) - 1.0;
                let v = (f64::from(row) + 0.5) * 2.0 / f64::from(side) - 1.0;
                if u * u + v * v > 1.0 {
                    continue;
                }
                let (direction, max_t) = if matches!(self.kind, Kind::Directional) {
                    (
                        [
                            -slope / length - radius.tan() * u / length,
                            -radius.tan() * v,
                            -1.0 / length + radius.tan() * u * slope / length,
                        ],
                        f64::INFINITY,
                    )
                } else {
                    let emitter = [
                        -3.0 + radius * u / length,
                        radius * v,
                        -5.0 - radius * u * slope / length,
                    ];
                    (std::array::from_fn(|i| emitter[i] - receiver[i]), 1.0)
                };
                visible += u32::from(
                    !self
                        .blockers
                        .iter()
                        .any(|blocker| blocker.intersects(receiver, direction, max_t)),
                );
                count += 1;
            }
        }
        f64::from(visible) / f64::from(count)
    }

    pub fn probes(&self) -> Result<Vec<Probe>> {
        let mut probes = Vec::new();
        // A receiver-only strip around the right shadow edge, sampled at distinct
        // raster pixel centres. Exclude foreground caster coverage analytically.
        for y in (0..384).step_by(3) {
            for x in (0..384).step_by(2) {
                let ray = self.camera.ray([f64::from(x) + 0.5, f64::from(y) + 0.5])?;
                let t = -ray.origin[2] / ray.direction[2];
                if t <= 0.0
                    || self
                        .blockers
                        .iter()
                        .any(|b| b.intersects(ray.origin, ray.direction, t))
                {
                    continue;
                }
                let p = std::array::from_fn(|i| ray.origin[i] + t * ray.direction[i]);
                let height = -self.blockers[0].center[2];
                let edge = if matches!(self.kind, Kind::Directional) {
                    0.7 * height + 0.5
                } else {
                    (3.0 * height + 2.5) / (5.0 - height)
                };
                if (p[0] - edge).abs() > 0.45 || p[1].abs() > 0.3 {
                    continue;
                }
                probes.push(Probe {
                    offset: (y as usize * 384 + x as usize) * 4,
                    finite: self.visibility(p, self.radius, 48),
                    finer: self.visibility(p, self.radius, 96),
                    point: self.visibility(p, 0.0, 1),
                });
            }
        }
        if probes.len() < 20 {
            return Err("receiver strip has too few unoccluded probes".into());
        }
        Ok(probes)
    }
}

pub struct Probe {
    pub offset: usize,
    pub finite: f64,
    pub finer: f64,
    pub point: f64,
}

#[derive(Debug, Serialize)]
pub struct Quality {
    pub receiver_pixels: usize,
    pub partial_pixels: usize,
    pub reference_partial_pixels: usize,
    pub finite_emitter_mae: f64,
    pub point_emitter_mae: f64,
    pub reference_48_vs_96_mae: f64,
    pub max_visibility: f64,
}

fn radiance(value: u8) -> Result<f64> {
    if value == 255 {
        return Err("saturated reference pixel cannot be inverted".into());
    }
    let encoded = f64::from(value) / 255.0;
    let linear = if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    };
    Ok(linear / (1.0 - linear))
}

pub fn quality(probes: &[Probe], off: &[u8], shadowed: &[u8]) -> Result<Quality> {
    let mut result = Quality {
        receiver_pixels: probes.len(),
        partial_pixels: 0,
        reference_partial_pixels: 0,
        finite_emitter_mae: 0.0,
        point_emitter_mae: 0.0,
        reference_48_vs_96_mae: 0.0,
        max_visibility: 0.0,
    };
    if probes.is_empty() {
        return Err("empty quality reference".into());
    }
    for probe in probes {
        let lit = radiance(*off.get(probe.offset).ok_or("short baseline image")?)?;
        if lit < 0.01 {
            return Err("reference pixel has insufficient direct radiance".into());
        }
        let visibility = radiance(*shadowed.get(probe.offset).ok_or("short shadow image")?)? / lit;
        result.partial_pixels += usize::from((0.1..0.9).contains(&visibility));
        result.reference_partial_pixels += usize::from((0.1..0.9).contains(&probe.finite));
        result.finite_emitter_mae += (visibility - probe.finite).abs();
        result.point_emitter_mae += (visibility - probe.point).abs();
        result.reference_48_vs_96_mae += (probe.finite - probe.finer).abs();
        result.max_visibility = result.max_visibility.max(visibility);
    }
    let count = probes.len() as f64;
    result.finite_emitter_mae /= count;
    result.point_emitter_mae /= count;
    result.reference_48_vs_96_mae /= count;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analytic_visibility_covers_contact_multiple_and_source_size() {
        for kind in [Kind::Directional, Kind::Spot, Kind::Point] {
            let fixture = Fixture::new(kind, "separated").unwrap();
            assert_eq!(fixture.visibility([0.9, 0.0, 0.0], 0.0, 1), 0.0);
            assert_eq!(fixture.visibility([3.0, 0.0, 0.0], 0.0, 1), 1.0);
            let probes = fixture.probes().unwrap();
            assert!(probes.iter().any(|p| p.finite > 0.1 && p.finite < 0.9));
            let convergence = probes
                .iter()
                .map(|p| (p.finite - p.finer).abs())
                .sum::<f64>()
                / probes.len() as f64;
            assert!(convergence < 0.005);
            let large = Fixture::new(kind, "large_penumbra")
                .unwrap()
                .probes()
                .unwrap();
            let partial = |p: &&Probe| p.finite > 0.1 && p.finite < 0.9;
            assert!(large.iter().filter(partial).count() > probes.iter().filter(partial).count());
            for name in ["contact", "multiple_casters", "grazing_view"] {
                assert!(Fixture::new(kind, name).unwrap().probes().unwrap().len() >= 20);
            }
        }
        let blocker = Blocker {
            center: [0.9, 0.35, -0.8],
        };
        assert!(blocker.intersects([0.9, 0.35, 0.0], [0.0, 0.0, -1.0], 1.0));
        assert!(!blocker.intersects([0.9, 0.35, 0.0], [0.0, 0.0, -1.0], 0.5));
    }

    #[test]
    fn quality_detects_wrong_visibility_and_rejects_invalid_baselines() {
        let probes = [Probe {
            offset: 0,
            finite: 0.0,
            finer: 0.0,
            point: 0.0,
        }];
        assert_eq!(
            quality(&probes, &[180], &[0]).unwrap().finite_emitter_mae,
            0.0
        );
        assert_eq!(
            quality(&probes, &[180], &[180]).unwrap().finite_emitter_mae,
            1.0
        );
        assert!(quality(&probes, &[255], &[180]).is_err());
        assert!(quality(&probes, &[0], &[0]).is_err());
        assert!(quality(&probes, &[], &[0]).is_err());
        assert!(quality(&[], &[], &[]).is_err());
    }
}

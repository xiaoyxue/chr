use std::sync::Arc;

use chr_renderer::{
    coordinates::ImageExtent,
    scene::{
        AreaShape, EnvironmentMap, FLAG_VERTICES, Flag, FlagMotion, FlagShading, Light, Lighting,
        MeshData, ModelMatrix, PbrMaterial, Scene, SceneError, SkyLight, Wave,
    },
};

fn mesh() -> MeshData {
    MeshData::new(
        vec![
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, 1.0],
            [0.0, 1.0, 0.0],
            [100.0; 3],
        ],
        vec![0, 1, 2, 2, 1, 0],
        vec![0..3, 3..6],
    )
    .unwrap()
}

#[test]
fn transforms_preserve_parts_normals_and_negative_z_grounding() {
    let mut scene = Scene::new().unwrap();
    let mesh_id = scene.add_mesh(mesh()).unwrap();
    let material = scene.add_material(PbrMaterial::default()).unwrap();
    let a = scene
        .add_instance(mesh_id, vec![material; 2], ModelMatrix::IDENTITY)
        .unwrap();
    let b = scene
        .add_instance(mesh_id, vec![material; 2], ModelMatrix::IDENTITY)
        .unwrap();
    let before = scene.snapshot();
    let model = ModelMatrix::new([
        [0.0, -2.0, 0.0, 1.0],
        [-3.0, 0.0, 0.0, 2.0],
        [0.0, 0.0, 4.0, 5.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
    .unwrap()
    .grounded(before.mesh(mesh_id).unwrap().bounds())
    .unwrap();
    scene.set_model(a, model).unwrap();
    scene.set_visible(a, false).unwrap();
    let after = scene.snapshot();
    assert!(model.is_mirrored());
    assert_eq!(before.mesh(mesh_id).unwrap().bounds().max(), [1.0; 3]);
    assert_eq!(after.instance(a).unwrap().bounds().max()[2], 0.0);
    assert_eq!(after.instance(a).unwrap().bounds().min()[2], -8.0);
    assert_eq!(after.instance(a).unwrap().materials(), &[material; 2]);
    assert!(!after.instance(a).unwrap().visible());
    assert_eq!(after.instance(b).unwrap().model(), ModelMatrix::IDENTITY);
    assert_eq!(before.instance(a).unwrap().model(), ModelMatrix::IDENTITY);
    assert!(std::ptr::eq(
        before.mesh(mesh_id).unwrap(),
        after.mesh(mesh_id).unwrap()
    ));

    let normal = model.transform_normal([1.0, 1.0, 1.0]).unwrap();
    let tangent = [2.0, -3.0, 0.0];
    let dot: f64 = normal.into_iter().zip(tangent).map(|(a, b)| a * b).sum();
    assert!(dot.abs() < 1e-12);
    assert!((normal.into_iter().map(|v| v * v).sum::<f64>() - 1.0).abs() < 1e-12);
}

#[test]
fn a_view_with_moved_instances_leaves_the_original_and_shares_meshes() {
    let mut scene = Scene::new().unwrap();
    let mesh_id = scene.add_mesh(mesh()).unwrap();
    let material = scene.add_material(PbrMaterial::default()).unwrap();
    let moving = scene
        .add_instance(mesh_id, vec![material; 2], ModelMatrix::IDENTITY)
        .unwrap();
    let still = scene
        .add_instance(mesh_id, vec![material; 2], ModelMatrix::IDENTITY)
        .unwrap();
    let view = scene.snapshot();
    let mut rows = ModelMatrix::IDENTITY.rows();
    rows[0][3] = 10.0;
    let moved = view
        .with_models([(moving, ModelMatrix::new(rows).unwrap())])
        .unwrap();
    assert_eq!(
        moved.instance(moving).unwrap().bounds().max(),
        [11.0, 1.0, 1.0]
    );
    assert_eq!(view.instance(moving).unwrap().bounds().max(), [1.0; 3]);
    assert_eq!(
        moved.instance(still).unwrap().model(),
        ModelMatrix::IDENTITY
    );
    assert!(std::ptr::eq(
        view.mesh(mesh_id).unwrap(),
        moved.mesh(mesh_id).unwrap()
    ));
}

#[test]
fn resource_reuse_and_updates_do_not_rewrite_old_views() {
    let mut scene = Scene::new().unwrap();
    let mesh_id = scene.add_mesh(mesh()).unwrap();
    let mat = scene.add_material(PbrMaterial::default()).unwrap();
    let instance = scene
        .add_instance(mesh_id, vec![mat; 2], ModelMatrix::IDENTITY)
        .unwrap();
    let old = scene.snapshot();
    assert_eq!(scene.remove_mesh(mesh_id), Err(SceneError::ResourceInUse));
    assert_eq!(scene.remove_material(mat), Err(SceneError::ResourceInUse));
    assert_eq!(scene.snapshot().revision(), old.revision());
    let changed = PbrMaterial {
        metallic: 1.0,
        ..PbrMaterial::default()
    };
    scene.set_material(mat, changed).unwrap();
    assert_eq!(old.material(mat).unwrap().metallic, 0.0);
    assert_eq!(scene.snapshot().material(mat).unwrap().metallic, 1.0);
    scene.remove_instance(instance).unwrap();
    scene.remove_mesh(mesh_id).unwrap();
    scene.remove_material(mat).unwrap();
    let next_mesh = scene.add_mesh(mesh()).unwrap();
    let next_mat = scene.add_material(changed).unwrap();
    let next_instance = scene
        .add_instance(next_mesh, vec![next_mat; 2], ModelMatrix::IDENTITY)
        .unwrap();
    assert_ne!(next_mesh, mesh_id);
    assert_ne!(next_mat, mat);
    assert_ne!(next_instance, instance);
    let current = scene.snapshot();
    assert!(matches!(
        current.mesh(mesh_id),
        Err(SceneError::InvalidHandle)
    ));
    assert!(matches!(
        current.material(mat),
        Err(SceneError::InvalidHandle)
    ));
    assert!(matches!(
        current.instance(instance),
        Err(SceneError::InvalidHandle)
    ));
    drop(scene);
    assert_eq!(old.instance(instance).unwrap().mesh(), mesh_id);
    assert_eq!(old.mesh(mesh_id).unwrap().parts(), &[0..3, 3..6]);
}

#[test]
fn invalid_updates_leave_the_scene_unchanged() {
    let mut scene = Scene::new().unwrap();
    let mesh_id = scene.add_mesh(mesh()).unwrap();
    let mat = scene.add_material(PbrMaterial::default()).unwrap();
    let instance = scene
        .add_instance(mesh_id, vec![mat; 2], ModelMatrix::IDENTITY)
        .unwrap();
    let old = scene.snapshot();
    let mut other = Scene::new().unwrap();
    let foreign_mat = other.add_material(PbrMaterial::default()).unwrap();
    assert_eq!(
        scene.set_material_bindings(instance, vec![mat]),
        Err(SceneError::MaterialBindings)
    );
    assert_eq!(
        scene.set_material_bindings(instance, vec![foreign_mat; 2]),
        Err(SceneError::InvalidHandle)
    );
    assert!(matches!(
        scene.add_instance(mesh_id, vec![foreign_mat; 2], ModelMatrix::IDENTITY),
        Err(SceneError::InvalidHandle)
    ));
    assert_eq!(
        scene.set_material(
            mat,
            PbrMaterial {
                roughness: f32::NAN,
                ..PbrMaterial::default()
            }
        ),
        Err(SceneError::InvalidMaterial)
    );
    let huge = ModelMatrix::new([
        [1e308, 0.0, 0.0, 1e308],
        [0.0, 1e-308, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
    .unwrap();
    assert_eq!(scene.set_model(instance, huge), Err(SceneError::NonFinite));
    assert_eq!(scene.snapshot().revision(), old.revision());
    assert_eq!(
        scene.snapshot().instance(instance).unwrap().model(),
        ModelMatrix::IDENTITY
    );
}

#[test]
fn flags_wave_only_the_shared_grid_and_widen_their_bounds() {
    let mut scene = Scene::new().unwrap();
    let grid = MeshData::flag_grid();
    assert!(grid.is_flag_grid() && !mesh().is_flag_grid());
    assert_eq!(grid.positions().len(), FLAG_VERTICES as usize);
    assert_eq!(
        MeshData::flag_grid().positions().as_ptr(),
        grid.positions().as_ptr()
    );
    let material = scene.add_material(PbrMaterial::default()).unwrap();
    let grid = scene.add_mesh(grid).unwrap();
    let other = scene.add_mesh(mesh()).unwrap();
    let model = ModelMatrix::new([
        [1.0, 0.0, 0.0, 10.0],
        [0.0, 0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0, -4.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
    .unwrap();
    let flag_id = scene.add_instance(grid, vec![material], model).unwrap();
    let other_id = scene
        .add_instance(other, vec![material, material], model)
        .unwrap();
    let flat = scene.snapshot();
    let flag = Flag {
        width: 3.2,
        height: 2.0,
        motion: FlagMotion::Wave(Wave {
            amplitude: 0.5,
            frequency: 1.35,
            speed: 1.0,
        }),
        shading: FlagShading::Pbr,
        artwork_turns: 0,
        revealed: 1.0,
        reveal_direction: Default::default(),
    };
    assert_eq!(
        scene.set_flag(other_id, Some(flag)),
        Err(SceneError::FlagMesh)
    );
    let invalid = Flag {
        height: -1.0,
        ..flag
    };
    assert_eq!(
        scene.set_flag(flag_id, Some(invalid)),
        Err(SceneError::InvalidFlag)
    );
    scene.set_flag(flag_id, Some(flag)).unwrap();
    let waving = scene.snapshot();
    let instance = waving.instance(flag_id).unwrap();
    assert_eq!(instance.flag(), Some(flag));
    // Local X 0..3.2 to world X, local Y (sag included) down to world Z, and the
    // 1.53 m-per-metre normal reach to world -Y.
    let bounds = instance.bounds();
    assert_eq!(bounds.min(), [10.0, -0.765, -4.0]);
    let max = bounds.max();
    assert!((max[0] - 13.2).abs() < 1e-6 && (max[1] - 0.765).abs() < 1e-6);
    assert!((max[2] - (-4.0 + 2.0 + 0.22 * 0.5)).abs() < 1e-6);
    assert_eq!(flat.instance(flag_id).unwrap().flag(), None);
    scene.set_flag(flag_id, None).unwrap();
    assert_eq!(
        scene.snapshot().instance(flag_id).unwrap().bounds().max()[0],
        11.0
    );
}

#[test]
fn malformed_meshes_and_models_are_rejected_at_construction() {
    let vertices = vec![[0.0; 3], [1.0; 3], [2.0; 3]];
    for indices in [vec![], vec![0, 1], vec![0, 1, 3]] {
        assert!(matches!(
            MeshData::new(vertices.clone(), indices, std::iter::once(0..3).collect()),
            Err(SceneError::InvalidMesh)
        ));
    }
    for parts in [
        vec![],
        vec![0..2, 2..3],
        std::iter::once(1..3).collect(),
        std::iter::once(0..6).collect(),
    ] {
        assert!(matches!(
            MeshData::new(vertices.clone(), vec![0, 1, 2], parts),
            Err(SceneError::InvalidMeshParts)
        ));
    }
    let mut singular = ModelMatrix::IDENTITY.rows();
    singular[2][2] = 0.0;
    assert_eq!(ModelMatrix::new(singular), Err(SceneError::SingularModel));
    let mut projective = ModelMatrix::IDENTITY.rows();
    projective[3][2] = 1.0;
    assert_eq!(
        ModelMatrix::new(projective),
        Err(SceneError::NonAffineModel)
    );
    assert_eq!(
        ModelMatrix::IDENTITY.transform_normal([0.0; 3]),
        Err(SceneError::ZeroDirection)
    );
}

#[test]
fn lighting_is_validated_and_owned_by_each_snapshot() {
    let environment = Arc::new(
        EnvironmentMap::new(ImageExtent::new(2, 1).unwrap(), vec![[4.0, 2.0, 1.0]; 2]).unwrap(),
    );
    let lighting = Lighting::new(
        vec![Light::Point {
            position: [1.0, 2.0, -35.0],
            intensity: [10.0; 3],
            source_radius: 0.0,
            shadow_mode: None,
        }],
        Some(SkyLight {
            environment,
            gain: 2.0,
            rotation: 0.5,
            visible: false,
            max_radiance: None,
        }),
    )
    .unwrap();
    let mut scene = Scene::new().unwrap();
    scene.set_lighting(lighting).unwrap();
    let old = scene.snapshot();
    scene.set_lighting(Lighting::default()).unwrap();
    assert_eq!(old.lighting().lights().len(), 1);
    assert_eq!(
        old.lighting().sky().unwrap().environment.pixels()[0][0],
        4.0
    );
    assert!(!old.lighting().sky().unwrap().visible);
    assert!(scene.snapshot().lighting().lights().is_empty());
    assert!(scene.snapshot().lighting().sky().is_none());
    let sky = old.lighting().sky().unwrap();
    for cap in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let capped = SkyLight {
            max_radiance: Some(cap),
            ..sky.clone()
        };
        assert!(
            matches!(
                Lighting::new(Vec::new(), Some(capped)),
                Err(SceneError::InvalidLighting)
            ),
            "max_radiance {cap}"
        );
    }
    for light in [
        Light::Directional {
            direction: [0.0; 3],
            irradiance: [1.0; 3],
            angular_radius: 0.0,
            shadow_mode: None,
        },
        Light::Point {
            position: [0.0; 3],
            intensity: [-1.0; 3],
            source_radius: 0.0,
            shadow_mode: None,
        },
        Light::Spot {
            position: [0.0; 3],
            direction: [0.0, 0.0, 1.0],
            intensity: [1.0; 3],
            inner_angle: 0.5,
            outer_angle: 0.4,
            source_radius: 0.0,
            shadow_mode: None,
        },
    ] {
        assert!(matches!(
            Lighting::new(vec![light], None),
            Err(SceneError::InvalidLighting)
        ));
    }
    assert!(matches!(
        EnvironmentMap::new(ImageExtent::new(1, 1).unwrap(), vec![[f32::INFINITY; 3]]),
        Err(SceneError::InvalidEnvironment)
    ));
    for radius in [-0.1, f32::INFINITY, f32::NAN] {
        for light in [
            Light::Directional {
                direction: [0.0, 0.0, 1.0],
                irradiance: [1.0; 3],
                angular_radius: radius,
                shadow_mode: None,
            },
            Light::Spot {
                position: [0.0, 0.0, -1.0],
                direction: [0.0, 0.0, 1.0],
                intensity: [1.0; 3],
                inner_angle: 0.2,
                outer_angle: 0.5,
                source_radius: radius,
                shadow_mode: None,
            },
        ] {
            assert!(matches!(
                Lighting::new(vec![light], None),
                Err(SceneError::InvalidLighting)
            ));
        }
    }
}

#[derive(Clone, Copy)]
struct Area {
    position: [f64; 3],
    normal: [f64; 3],
    tangent: [f64; 3],
    shape: AreaShape,
    radiance: [f32; 3],
    two_sided: bool,
}

impl Area {
    fn new(shape: AreaShape) -> Self {
        Self {
            position: [0.0, 0.0, -3.0],
            normal: [0.0, 0.0, 1.0],
            tangent: [1.0, 0.0, 0.0],
            shape,
            radiance: [2.0; 3],
            two_sided: false,
        }
    }

    fn light(self) -> Light {
        Light::Area {
            position: self.position,
            normal: self.normal,
            tangent: self.tangent,
            shape: self.shape,
            radiance: self.radiance,
            two_sided: self.two_sided,
            shadow_mode: None,
        }
    }
}

#[test]
fn area_lights_validate_geometry_emission_and_sidedness() {
    let rectangle = AreaShape::Rectangle {
        half_width: 1.0,
        half_height: 0.5,
    };
    let sphere = AreaShape::Sphere { radius: 0.5 };
    let valid = [
        Area::new(rectangle),
        Area::new(AreaShape::Disk { radius: 2.0 }),
        Area::new(sphere),
        // Non-unit, non-perpendicular axes are accepted and orthonormalized for shading.
        Area {
            normal: [0.0, 1e3, 1e3],
            tangent: [2.0, 0.0, 1.0],
            two_sided: true,
            ..Area::new(rectangle)
        },
    ];
    assert!(Lighting::new(valid.iter().map(|a| a.light()).collect(), None).is_ok());
    for area in [
        Area::new(AreaShape::Rectangle {
            half_width: 0.0,
            half_height: 0.5,
        }),
        Area::new(AreaShape::Disk { radius: -1.0 }),
        Area::new(AreaShape::Sphere { radius: f32::NAN }),
        Area {
            radiance: [1.0, -1.0, 1.0],
            ..Area::new(rectangle)
        },
        Area {
            position: [f64::INFINITY, 0.0, 0.0],
            ..Area::new(rectangle)
        },
        Area {
            normal: [0.0; 3],
            ..Area::new(rectangle)
        },
        Area {
            tangent: [0.0, 0.0, -4.0],
            ..Area::new(rectangle)
        },
        Area {
            two_sided: true,
            ..Area::new(sphere)
        },
    ] {
        let light = area.light();
        assert!(
            matches!(
                Lighting::new(vec![light], None),
                Err(SceneError::InvalidLighting)
            ),
            "{light:?}"
        );
    }
}

#[test]
fn area_radiance_separates_fixed_radiance_from_fixed_power() {
    let disk = AreaShape::Disk { radius: 1.0 };
    // flux = pi * area * radiance, and the unit disk has area pi.
    let flux = [std::f32::consts::PI * std::f32::consts::PI; 3];
    assert!((disk.radiance_from_flux(flux, false)[0] - 1.0).abs() < 1e-6);
    // Two emitting faces share the power; a sphere has one outward face.
    assert!((disk.radiance_from_flux(flux, true)[0] - 0.5).abs() < 1e-6);
    let sphere = AreaShape::Sphere { radius: 0.5 };
    assert!((sphere.area() - std::f64::consts::PI).abs() < 1e-12);
    assert_eq!(
        sphere.radiance_from_flux(flux, true),
        sphere.radiance_from_flux(flux, false)
    );
    let doubled = AreaShape::Disk { radius: 2.0 };
    assert!((doubled.radiance_from_flux(flux, false)[0] - 0.25).abs() < 1e-6);
}

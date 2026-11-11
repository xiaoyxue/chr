use chr_renderer::{
    camera::{Camera, CameraError, CameraView, Intrinsics, OrthographicCamera},
    coordinates::{ImageExtent, Ti},
};

fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-7, "{a} != {b}");
}

#[test]
fn calibrated_pixels_rays_and_gpu_clip_agree() {
    for (w, h) in [(1280, 720), (720, 1280)] {
        let extent = ImageExtent::new(w, h).unwrap();
        let k = Intrinsics {
            fx: 850.0,
            fy: 930.0,
            skew: 13.0,
            cx: 311.0,
            cy: 277.0,
        };
        let camera = Camera::from_pose(
            extent,
            k,
            [
                [1.0, 0.0, 0.0, 2.0],
                [0.0, 1.0, 0.0, 3.0],
                [0.0, 0.0, 1.0, -10.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            1.0,
            100.0,
        )
        .unwrap();
        let point = [3.0, 5.0, 0.0];
        let (pixel, depth) = camera.project(point).unwrap();
        close(pixel[0], 398.6);
        close(pixel[1], 463.0);
        let ray = camera.ray(pixel).unwrap();
        let t = (point[2] - ray.origin[2]) / ray.direction[2];
        for (i, coordinate) in point.into_iter().enumerate() {
            close(ray.origin[i] + t * ray.direction[i], coordinate);
        }
        let m = camera.clip_from_world().unwrap();
        let p = [point[0], point[1], point[2], 1.0];
        let clip: [f64; 4] =
            std::array::from_fn(|i| (0..4).map(|j| f64::from(m[j][i]) * p[j]).sum());
        assert!(((clip[0] / clip[3] + 1.0) * f64::from(w) / 2.0 - pixel[0]).abs() < 1e-4);
        assert!(((1.0 - clip[1] / clip[3]) * f64::from(h) / 2.0 - pixel[1]).abs() < 1e-4);
        assert!((clip[2] / clip[3] - depth).abs() < 1e-6);
        close(camera.project([2.0, 3.0, -9.0]).unwrap().1, 0.0);
        close(camera.project([2.0, 3.0, 90.0]).unwrap().1, 1.0);
        assert_eq!(
            camera.project([2.0, 3.0, -11.0]),
            Err(CameraError::OutsideDepth)
        );
    }
}

#[test]
fn ti_decomposition_preserves_ground_elevated_points_and_signed_scale() {
    let extent = ImageExtent::new(1280, 720).unwrap();
    let center = [2.0, 4.0, -10.0];
    let rows = [[0.08, 0.01, 0.2], [0.0, 0.1, -0.3], [0.002, 0.001, 1.0]];
    for scale in [1.0, -3.0, 0.1] {
        let ti = Ti::new(rows.map(|row| row.map(|v| v * scale))).unwrap();
        let camera = Camera::from_ti(extent, ti, center, 0.01, 10000.0).unwrap();
        assert_eq!(camera.source_ti().unwrap().rows(), ti.rows());
        for world in [[0.0, 0.0, 0.0], [3.0, 5.0, -2.0], [-2.0, 1.0, -5.0]] {
            let expected = extent.snc_to_pixel(ti.project_world(world, center).unwrap());
            let (actual, _) = camera.project(world).unwrap();
            for (a, b) in actual.into_iter().zip(expected) {
                close(a, b);
            }
        }
    }
}

#[test]
fn look_at_fov_and_invalid_inputs() {
    let extent = ImageExtent::new(960, 640).unwrap();
    let camera = Camera::look_at(
        extent,
        [0.0, -80.0, -60.0],
        [0.0; 3],
        [0.0, 0.0, -1.0],
        1.0,
        0.1,
        1000.0,
    )
    .unwrap();
    let (pixel, _) = camera.project([0.0; 3]).unwrap();
    close(pixel[0], 480.0);
    close(pixel[1], 320.0);
    let k = Intrinsics::from_fov(extent, 1.0).unwrap();
    assert_eq!(k, camera.intrinsics());
    let equivalent = Camera::from_pose(
        extent,
        k,
        [
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, -0.6, 0.8, -80.0],
            [0.0, 0.8, 0.6, -60.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
        0.1,
        1000.0,
    )
    .unwrap();
    let world = [20.0, 10.0, -5.0];
    let (a, _) = camera.project(world).unwrap();
    let (b, _) = equivalent.project(world).unwrap();
    for (a, b) in a.into_iter().zip(b) {
        close(a, b);
    }
    assert!(Intrinsics::from_fov(extent, 0.0).is_err());
    assert!(
        Camera::look_at(
            extent,
            [0.0; 3],
            [0.0; 3],
            [0.0, 0.0, -1.0],
            1.0,
            0.1,
            100.0
        )
        .is_err()
    );
    let pose = [
        [2.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    assert!(matches!(
        Camera::from_pose(extent, k, pose, 0.1, 100.0),
        Err(CameraError::InvalidPose)
    ));
    assert!(camera.ray([f64::NAN, 0.0]).is_err());
}

#[test]
fn orthographic_pose_spans_rays_and_linear_clip_agree_at_every_depth() {
    // A translated, rotated optical frame; optical +Z is world +Y.
    let pose = [
        [-1.0, 0.0, 0.0, 2.0],
        [0.0, 0.0, 1.0, 3.0],
        [0.0, 1.0, 0.0, -10.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    for (w, h) in [(800, 400), (400, 800)] {
        let extent = ImageExtent::new(w, h).unwrap();
        let camera = OrthographicCamera::from_pose(extent, [8.0, 4.0], pose, 1.0, 21.0).unwrap();
        let ray = camera
            .ray([0.75 * f64::from(w), 0.75 * f64::from(h)])
            .unwrap();
        assert_eq!(ray.origin, [0.0, 3.0, -9.0]);
        assert_eq!(ray.direction, [0.0, 1.0, 0.0]);
        let corner = camera.ray([0.0, 0.0]).unwrap();
        assert_eq!(corner.origin, [6.0, 3.0, -12.0]);
        assert_eq!(corner.direction, ray.direction);
        let matrix = camera.clip_from_world().unwrap();
        for depth in [1.0, 4.0, 11.0, 21.0] {
            let world = [0.0, 3.0 + depth, -9.0];
            let (pixel, z) = camera.project(world).unwrap();
            close(pixel[0], 0.75 * f64::from(w));
            close(pixel[1], 0.75 * f64::from(h));
            close(z, (depth - 1.0) / 20.0);
            let (other, _) = camera.project([1.0, 3.0 + depth, -9.0]).unwrap();
            close(pixel[0] - other[0], f64::from(w) / 8.0);
            let p = [world[0], world[1], world[2], 1.0];
            let clip: [f64; 4] =
                std::array::from_fn(|r| (0..4).map(|c| f64::from(matrix[c][r]) * p[c]).sum());
            close(clip[0], 0.5);
            close(clip[1], -0.5);
            close(clip[2], z);
            close(clip[3], 1.0);
        }
        for depth in [-1.0, 0.99, 21.01] {
            assert_eq!(
                camera.project([2.0, 3.0 + depth, -10.0]),
                Err(CameraError::OutsideDepth)
            );
        }
    }
}

#[test]
fn orthographic_rejects_invalid_spans_pose_clips_and_nonfinite_operations() {
    let extent = ImageExtent::new(64, 64).unwrap();
    let pose = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    for value in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        for span in [[value, 4.0], [4.0, value]] {
            assert!(matches!(
                OrthographicCamera::from_pose(extent, span, pose, 1.0, 10.0),
                Err(CameraError::InvalidSpan)
            ));
        }
    }
    for (near, far) in [
        (0.0, 10.0),
        (-1.0, 10.0),
        (1.0, 1.0),
        (2.0, 1.0),
        (f64::NAN, 10.0),
        (1.0, f64::INFINITY),
    ] {
        assert!(matches!(
            OrthographicCamera::from_pose(extent, [4.0; 2], pose, near, far),
            Err(CameraError::InvalidClip)
        ));
    }
    for (row, col, value) in [(0, 0, 2.0), (0, 0, -1.0), (3, 0, 1.0), (0, 3, f64::NAN)] {
        let mut invalid = pose;
        invalid[row][col] = value;
        assert!(matches!(
            OrthographicCamera::from_pose(extent, [4.0; 2], invalid, 1.0, 10.0),
            Err(CameraError::InvalidPose)
        ));
    }
    let camera = OrthographicCamera::from_pose(extent, [4.0; 2], pose, 1.0, 10.0).unwrap();
    assert!(camera.ray([f64::NAN, 0.0]).is_err());
    assert!(camera.ray([0.0, f64::INFINITY]).is_err());
    assert_eq!(
        camera.project([f64::NAN, 0.0, 2.0]),
        Err(CameraError::InvalidGeometry)
    );
    let tiny =
        OrthographicCamera::from_pose(extent, [f64::MIN_POSITIVE; 2], pose, 1.0, 10.0).unwrap();
    assert!(
        tiny.clip_from_world().is_err(),
        "GPU-unrepresentable matrices fail explicitly"
    );
}

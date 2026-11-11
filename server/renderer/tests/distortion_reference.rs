//! Independent CPU reference for #33, not a renderer API or a production warp.
//! Model, domain proof and limits: server/docs/distortion-reference.md.

use chr_renderer::{
    camera::Intrinsics,
    coordinates::{ImageExtent, Ti},
};
use thiserror::Error;

type Point = [f64; 2];

#[derive(Debug, Error, PartialEq, Eq)]
enum ReferenceError {
    #[error("parameters must be finite with positive focal lengths and radius")]
    InvalidParameters,
    #[error("the calibrated disk is not certified by this conservative reference")]
    UncertifiedDomain,
    #[error("nonfinite input or numerical evaluation")]
    NonFinite,
    #[error("ideal coordinate lies outside the calibrated disk")]
    OutsideDomain,
    #[error("inverse did not converge within the bounded solver")]
    NoConvergence,
    #[error("inverse source lies outside the image's pixel-centre rectangle")]
    OutsideImage,
    #[error("invalid bounded solver settings")]
    InvalidSolver,
}

#[derive(Clone, Copy, Debug)]
struct Lens {
    extent: ImageExtent,
    intrinsics: Intrinsics,
    radial: [f64; 3],
    tangential: [f64; 2],
    radius: f64,
    jacobian_lower_bound: f64,
}

#[derive(Clone, Copy, Debug)]
struct Evaluation {
    point: Point,
    jacobian: [[f64; 2]; 2],
}

#[derive(Clone, Copy, Debug)]
struct Solver {
    iterations: u32,
    backtracks: u32,
    tolerance: f64,
}

impl Solver {
    fn new(iterations: u32, backtracks: u32, tolerance: f64) -> Result<Self, ReferenceError> {
        if !(1..=64).contains(&iterations)
            || !(1..=32).contains(&backtracks)
            || !tolerance.is_finite()
            || tolerance <= 0.0
        {
            return Err(ReferenceError::InvalidSolver);
        }
        Ok(Self {
            iterations,
            backtracks,
            tolerance,
        })
    }
}

impl Lens {
    fn new(
        extent: ImageExtent,
        intrinsics: Intrinsics,
        radial: [f64; 3],
        tangential: [f64; 2],
        radius: f64,
    ) -> Result<Self, ReferenceError> {
        if ![
            intrinsics.fx,
            intrinsics.fy,
            intrinsics.skew,
            intrinsics.cx,
            intrinsics.cy,
            radius,
        ]
        .into_iter()
        .chain(radial)
        .chain(tangential)
        .all(f64::is_finite)
            || intrinsics.fx <= 0.0
            || intrinsics.fy <= 0.0
            || radius <= 0.0
        {
            return Err(ReferenceError::InvalidParameters);
        }
        let r2 = radius * radius;
        let r4 = r2 * r2;
        let r6 = r4 * r2;
        // Negative radial terms lose at most (2n+1)|kn|R^(2n);
        // the tangential Jacobian loses at most 6|p|R on the disk.
        let lower_bound = 1.0
            - 3.0 * (-radial[0]).max(0.0) * r2
            - 5.0 * (-radial[1]).max(0.0) * r4
            - 7.0 * (-radial[2]).max(0.0) * r6
            - 6.0 * tangential[0].hypot(tangential[1]) * radius;
        if !lower_bound.is_finite() || lower_bound <= 1e-6 {
            return Err(ReferenceError::UncertifiedDomain);
        }
        Ok(Self {
            extent,
            intrinsics,
            radial,
            tangential,
            radius,
            jacobian_lower_bound: lower_bound,
        })
    }

    fn evaluate(self, [x, y]: Point) -> Result<Evaluation, ReferenceError> {
        finite([x, y])?;
        if x.hypot(y) > self.radius {
            return Err(ReferenceError::OutsideDomain);
        }
        let [k1, k2, k3] = self.radial;
        let [p1, p2] = self.tangential;
        let r2 = x * x + y * y;
        let radial = 1.0 + r2 * (k1 + r2 * (k2 + r2 * k3));
        let slope = k1 + r2 * (2.0 * k2 + 3.0 * r2 * k3);
        let point = [
            x * radial + 2.0 * p1 * x * y + p2 * (r2 + 2.0 * x * x),
            y * radial + p1 * (r2 + 2.0 * y * y) + 2.0 * p2 * x * y,
        ];
        let cross = 2.0 * x * y * slope + 2.0 * p1 * x + 2.0 * p2 * y;
        let jacobian = [
            [
                radial + 2.0 * x * x * slope + 2.0 * p1 * y + 6.0 * p2 * x,
                cross,
            ],
            [
                cross,
                radial + 2.0 * y * y * slope + 6.0 * p1 * y + 2.0 * p2 * x,
            ],
        ];
        if !point
            .into_iter()
            .chain(jacobian.into_iter().flatten())
            .all(f64::is_finite)
        {
            return Err(ReferenceError::NonFinite);
        }
        Ok(Evaluation { point, jacobian })
    }

    fn normalized(self, [u, v]: Point) -> Point {
        let k = self.intrinsics;
        let y = (v - k.cy) / k.fy;
        [(u - k.cx - k.skew * y) / k.fx, y]
    }

    fn pixel_displacement(self, [x, y]: Point) -> Point {
        let k = self.intrinsics;
        [k.fx * x + k.skew * y, k.fy * y]
    }

    fn distort_pixel(self, pixel: Point) -> Result<Point, ReferenceError> {
        let ideal = self.normalized(pixel);
        let distorted = self.evaluate(ideal)?.point;
        let delta = self.pixel_displacement(sub(distorted, ideal));
        // Displacements preserve zero-distortion pixel centres exactly.
        let result = [pixel[0] + delta[0], pixel[1] + delta[1]];
        finite(result)?;
        Ok(result)
    }

    fn inverse(self, target: Point, solver: Solver) -> Result<(Point, u32), ReferenceError> {
        finite(target)?;
        let radius = target[0].hypot(target[1]);
        if !radius.is_finite() {
            return Err(ReferenceError::NonFinite);
        }
        let mut point = if radius > self.radius {
            target.map(|value| value * (self.radius / radius))
        } else {
            target
        };
        for iteration in 0..=solver.iterations {
            let value = self.evaluate(point)?;
            let residual = sub(value.point, target);
            let error = residual[0].hypot(residual[1]);
            if !error.is_finite() {
                return Err(ReferenceError::NonFinite);
            }
            if error <= solver.tolerance {
                return Ok((point, iteration));
            }
            if iteration == solver.iterations {
                break;
            }
            let [[a, b], [c, d]] = value.jacobian;
            let determinant = a * d - b * c;
            if !determinant.is_finite() || determinant <= 0.0 {
                return Err(ReferenceError::NoConvergence);
            }
            let step = [
                (d * residual[0] - b * residual[1]) / determinant,
                (a * residual[1] - c * residual[0]) / determinant,
            ];
            finite(step)?;
            let mut scale = 1.0;
            let mut accepted = None;
            for _ in 0..solver.backtracks {
                let candidate = [point[0] - scale * step[0], point[1] - scale * step[1]];
                match self.evaluate(candidate) {
                    Ok(next) => {
                        let residual = sub(next.point, target);
                        if residual[0].hypot(residual[1]) < error {
                            accepted = Some(candidate);
                            break;
                        }
                    }
                    Err(ReferenceError::OutsideDomain) => {}
                    Err(error) => return Err(error),
                }
                scale *= 0.5;
            }
            point = accepted.ok_or(ReferenceError::NoConvergence)?;
        }
        Err(ReferenceError::NoConvergence)
    }

    fn source_pixel(
        self,
        destination: Point,
        solver: Solver,
    ) -> Result<(Point, u32), ReferenceError> {
        let target = self.normalized(destination);
        let (ideal, iterations) = self.inverse(target, solver)?;
        let delta = self.pixel_displacement(sub(ideal, target));
        let source = [destination[0] + delta[0], destination[1] + delta[1]];
        finite(source)?;
        if !(0.0..=f64::from(self.extent.width() - 1)).contains(&source[0])
            || !(0.0..=f64::from(self.extent.height() - 1)).contains(&source[1])
        {
            return Err(ReferenceError::OutsideImage);
        }
        Ok((source, iterations))
    }
}

fn finite(point: Point) -> Result<(), ReferenceError> {
    if point.into_iter().all(f64::is_finite) {
        Ok(())
    } else {
        Err(ReferenceError::NonFinite)
    }
}

fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1]]
}

fn fixture(radial: [f64; 3], tangential: [f64; 2]) -> Lens {
    Lens::new(
        ImageExtent::new(1280, 720).unwrap(),
        Intrinsics {
            fx: 900.0,
            fy: 950.0,
            skew: 23.0,
            cx: 640.0,
            cy: 360.0,
        },
        radial,
        tangential,
        1.2,
    )
    .expect("synthetic certified lens")
}

fn solver() -> Solver {
    Solver::new(32, 16, 1e-12).expect("bounded reference settings")
}

fn close(actual: Point, expected: Point, tolerance: f64) {
    assert!(
        actual
            .into_iter()
            .zip(expected)
            .all(|(a, b)| (a - b).abs() <= tolerance),
        "{actual:?} != {expected:?}"
    );
}

#[test]
fn mixed_forward_matches_hand_calculated_coordinates() {
    let lens = fixture([0.1, -0.02, 0.003], [0.004, -0.005]);
    close(
        lens.evaluate([0.2, -0.3]).unwrap().point,
        [0.2010037182, -0.3019605773],
        1e-14,
    );
}

#[test]
fn analytic_jacobian_matches_central_differences() {
    let lens = fixture([0.1, -0.02, 0.003], [0.004, -0.005]);
    let h = 1e-6;
    for point in [[0.0, 0.0], [0.2, -0.3], [-0.7, 0.4]] {
        let analytic = lens.evaluate(point).unwrap().jacobian;
        for axis in 0..2 {
            let mut before = point;
            let mut after = point;
            before[axis] -= h;
            after[axis] += h;
            let delta = sub(
                lens.evaluate(after).unwrap().point,
                lens.evaluate(before).unwrap().point,
            );
            close(
                [analytic[0][axis], analytic[1][axis]],
                delta.map(|value| value / (2.0 * h)),
                1e-9,
            );
        }
    }
}

#[test]
fn zero_distortion_preserves_native_centres_including_borders() {
    for (width, height) in [(1280, 720), (720, 1280), (641, 481)] {
        let mut lens = fixture([0.0; 3], [0.0; 2]);
        lens.extent = ImageExtent::new(width, height).unwrap();
        lens.intrinsics.cx = f64::from(width) / 2.0;
        lens.intrinsics.cy = f64::from(height) / 2.0;
        for pixel in [
            [0.0, 0.0],
            [f64::from(width - 1), f64::from(height - 1)],
            [17.25, 30.75],
        ] {
            assert_eq!(lens.distort_pixel(pixel).unwrap(), pixel);
            assert_eq!(lens.source_pixel(pixel, solver()).unwrap(), (pixel, 0));
        }
    }
}

#[test]
fn radial_tangential_and_mixed_grids_round_trip() {
    let profiles = [
        ([-0.12, 0.015, 0.0], [0.0, 0.0]),
        ([0.25, 0.02, 0.001], [0.0, 0.0]),
        ([0.0; 3], [0.008, 0.0]),
        ([0.0; 3], [0.0, -0.008]),
        ([0.1, -0.02, 0.003], [0.004, -0.005]),
    ];
    let mut max_error = 0.0_f64;
    let mut max_iterations = 0;
    let mut samples = 0;
    for (radial, tangential) in profiles {
        let lens = fixture(radial, tangential);
        assert!(lens.jacobian_lower_bound > 1e-6);
        for row in 0..9 {
            for column in 0..13 {
                let pixel = [
                    1279.0 * (f64::from(column) + 0.5) / 13.0,
                    719.0 * (f64::from(row) + 0.5) / 9.0,
                ];
                let distorted = lens.distort_pixel(pixel).unwrap();
                let (recovered, iterations) = lens.source_pixel(distorted, solver()).unwrap();
                close(recovered, pixel, 1e-7);
                let error = sub(recovered, pixel);
                max_error = max_error.max(error[0].hypot(error[1]));
                max_iterations = max_iterations.max(iterations);
                samples += 1;
            }
        }
    }
    eprintln!(
        "{samples} synthetic round trips: max_error_px={max_error:.3e}, max_iterations={max_iterations}"
    );
}

#[test]
fn inverse_can_start_outside_the_ideal_disk_without_clamping_its_result() {
    let lens = Lens::new(
        ImageExtent::new(1280, 720).unwrap(),
        fixture([0.0; 3], [0.0; 2]).intrinsics,
        [1.0, 0.0, 0.0],
        [0.0; 2],
        1.0,
    )
    .unwrap();
    let target = lens.evaluate([0.9, 0.0]).unwrap().point;
    assert!(target[0] > lens.radius);
    close(lens.inverse(target, solver()).unwrap().0, [0.9, 0.0], 1e-12);
}

#[test]
fn invalid_models_and_uncertified_domains_are_explicit() {
    let identity = fixture([0.0; 3], [0.0; 2]);
    let mut invalid_k = identity.intrinsics;
    invalid_k.fx = 0.0;
    assert_eq!(
        Lens::new(identity.extent, invalid_k, [0.0; 3], [0.0; 2], 1.0).unwrap_err(),
        ReferenceError::InvalidParameters
    );
    for (radial, tangential, radius, expected) in [
        (
            [f64::NAN, 0.0, 0.0],
            [0.0; 2],
            1.0,
            ReferenceError::InvalidParameters,
        ),
        ([0.0; 3], [0.0; 2], 0.0, ReferenceError::InvalidParameters),
        (
            [-1.0, 0.0, 0.0],
            [0.0; 2],
            1.0,
            ReferenceError::UncertifiedDomain,
        ),
        ([0.0; 3], [0.2, 0.0], 1.0, ReferenceError::UncertifiedDomain),
    ] {
        assert_eq!(
            Lens::new(
                identity.extent,
                identity.intrinsics,
                radial,
                tangential,
                radius
            )
            .unwrap_err(),
            expected
        );
    }
    assert_eq!(
        identity.evaluate([f64::INFINITY, 0.0]).unwrap_err(),
        ReferenceError::NonFinite
    );
    assert_eq!(
        identity.evaluate([2.0, 0.0]).unwrap_err(),
        ReferenceError::OutsideDomain
    );
}

#[test]
fn nonconvergence_and_source_coverage_do_not_fabricate_edge_samples() {
    let identity = fixture([0.0; 3], [0.0; 2]);
    assert_eq!(
        identity.inverse([2.0, 0.0], solver()).unwrap_err(),
        ReferenceError::NoConvergence
    );
    assert_eq!(
        identity.source_pixel([-0.25, 100.0], solver()).unwrap_err(),
        ReferenceError::OutsideImage
    );
    let barrel = fixture([-0.1, 0.0, 0.0], [0.0; 2]);
    assert_eq!(
        barrel.source_pixel([0.0, 0.0], solver()).unwrap_err(),
        ReferenceError::OutsideImage
    );
    let nonlinear = fixture([0.3, 0.02, 0.0], [0.0; 2]);
    let target = nonlinear.evaluate([0.8, 0.3]).unwrap().point;
    assert_eq!(
        nonlinear
            .inverse(target, Solver::new(1, 16, 1e-12).unwrap())
            .unwrap_err(),
        ReferenceError::NoConvergence
    );
    assert_eq!(
        Solver::new(0, 16, 1e-12).unwrap_err(),
        ReferenceError::InvalidSolver
    );
}

#[test]
fn elevated_ti_projection_distorts_k_normalized_coordinates_not_snc() {
    let extent = ImageExtent::new(1280, 720).unwrap();
    let ti = Ti::new([[0.01, 0.0, 0.0], [0.0, 0.01, 0.0], [0.0, 0.0, 1.0]]).unwrap();
    let snc = ti
        .project_world([10.0, 0.0, -5.0], [0.0, 0.0, -10.0])
        .unwrap();
    let ideal_pixel = extent.snc_to_pixel(snc);
    let lens = Lens::new(
        extent,
        Intrinsics {
            fx: 800.0,
            fy: 800.0,
            skew: 0.0,
            cx: 640.0,
            cy: 360.0,
        },
        [0.1, 0.0, 0.0],
        [0.0; 2],
        1.0,
    )
    .unwrap();
    close(ideal_pixel, [736.0, 360.0], 1e-12);
    let distorted = lens.distort_pixel(ideal_pixel).unwrap();
    close(distorted, [736.13824, 360.0], 1e-10);
    close(
        lens.source_pixel(distorted, solver()).unwrap().0,
        ideal_pixel,
        1e-9,
    );
}

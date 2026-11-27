//! Independent synthetic f64 oracle for the proposed atmosphere method profile.
//! No renderer calls, LUT implementation, assets, or production atmosphere API.

use std::f64::consts::PI;

fn midpoint_integral(length: f64, steps: u32, value: impl Fn(f64) -> f64) -> f64 {
    let step = length / f64::from(steps);
    (0..steps)
        .map(|i| value((f64::from(i) + 0.5) * step) * step)
        .sum()
}

fn transmittance(length: f64, steps: u32, extinction: impl Fn(f64) -> f64) -> f64 {
    (-midpoint_integral(length, steps, extinction)).exp()
}

fn segment_radiance(extinction: f64, source_per_metre: f64, length: f64) -> f64 {
    if extinction == 0.0 {
        source_per_metre * length
    } else {
        source_per_metre * -(-extinction * length).exp_m1() / extinction
    }
}

fn ozone_density(altitude: f64) -> f64 {
    (1.0 - (altitude - 25_000.0).abs() / 15_000.0).max(0.0)
}

fn shell_exit(bottom: f64, top: f64, altitude: f64, zenith_cosine: f64) -> f64 {
    let radius = bottom + altitude;
    let projection = radius * zenith_cosine;
    // Rationalize the outward root to avoid subtracting nearly equal radii.
    let difference = (top - radius) * (top + radius);
    let root = (projection * projection + difference).sqrt();
    if projection >= 0.0 {
        difference / (root + projection)
    } else {
        root - projection
    }
}

fn ray_altitude(bottom: f64, altitude: f64, cosine: f64, distance: f64) -> f64 {
    let radius = bottom + altitude;
    (radius * radius + distance * distance + 2.0 * radius * cosine * distance).sqrt() - bottom
}

#[test]
fn homogeneous_extinction_and_partition_match_beer_lambert() {
    for length in [0.0_f64, 1.0, 100_000.0] {
        for coefficient in [0.0, 1e-12, 1e-5, 0.01] {
            let expected = (-coefficient * length).exp();
            let actual = transmittance(length, 128, |_| coefficient);
            assert!((actual - expected).abs() < 2e-14);
            let split = transmittance(length * 0.3, 32, |_| coefficient)
                * transmittance(length * 0.7, 32, |_| coefficient);
            assert!((actual - split).abs() < 2e-14);
        }
    }
}

#[test]
fn constant_source_has_finite_vacuum_limit_and_zero_emission() {
    let length = 2000.0;
    for extinction in [0.0, 1e-16, 1e-8, 0.01] {
        assert_eq!(segment_radiance(extinction, 0.0, length), 0.0);
        let actual = segment_radiance(extinction, 0.25, length);
        let numerical = midpoint_integral(length, 65_536, |s| 0.25 * (-extinction * s).exp());
        assert!((actual - numerical).abs() / actual < 1e-8);
    }
    assert_eq!(segment_radiance(0.0, 0.25, length), 500.0);
}

#[test]
fn vertical_exponential_and_ozone_columns_match_closed_forms() {
    for scale_height in [1200.0, 8000.0] {
        let length = 100_000.0;
        let numerical = midpoint_integral(length, 65_536, |h| (-h / scale_height).exp());
        let analytic = scale_height * (1.0 - (-length / scale_height).exp());
        assert!((numerical - analytic).abs() / analytic < 1e-7);
    }
    let column = midpoint_integral(100_000.0, 100_000, ozone_density);
    assert!((column - 15_000.0).abs() < 1e-8);
    let extinction_per_metre = 1.881e-6;
    let actual = transmittance(100_000.0, 100_000, |h| {
        extinction_per_metre * ozone_density(h)
    });
    assert!((actual - (-extinction_per_metre * 15_000.0).exp()).abs() < 1e-12);
}

#[test]
fn synthetic_spherical_paths_converge_across_altitude_and_elevation() {
    let bottom = 6_360_000.0;
    let top = 6_460_000.0;
    let mut worst_difference = 0.0_f64;
    let mut previous_zenith = 0.0;
    for altitude in [0.0, 2.0, 1000.0, 10_000.0, 20_000.0] {
        let mut previous = 0.0;
        for elevation in [0.0_f64, 1.0, 5.0, 30.0, 90.0] {
            let cosine = elevation.to_radians().sin();
            let distance = shell_exit(bottom, top, altitude, cosine);
            let extinction = |s| {
                let h = ray_altitude(bottom, altitude, cosine, s);
                13.558e-6 * (-h / 8000.0).exp()
                    + 4.440e-6 * (-h / 1200.0).exp()
                    + 1.881e-6 * ozone_density(h)
            };
            let reference = transmittance(distance, 8192, extinction);
            let coarse = transmittance(distance, 4096, extinction);
            worst_difference = worst_difference.max((coarse - reference).abs());
            assert!((0.0..=1.0).contains(&reference));
            assert!(reference >= previous);
            previous = reference;
        }
        assert!(previous > previous_zenith);
        previous_zenith = previous;
        assert!((shell_exit(bottom, top, altitude, 1.0) - (top - bottom - altitude)).abs() < 1e-8);
    }
    println!("25 synthetic rays: max |T4096-T8192| = {worst_difference:.9e}");
    assert!(worst_difference < 2e-6);
}

#[test]
fn finite_disk_quadrature_preserves_normal_irradiance() {
    // Integrate solid angle explicitly, not projected area or a delta light.
    for radius in [0.001_f64, 0.004675, 0.01] {
        let emission = 2.0;
        let radiance = emission / (PI * radius.sin().powi(2));
        let irradiance = 2.0
            * PI
            * midpoint_integral(radius, 4096, |theta| radiance * theta.cos() * theta.sin());
        assert!((irradiance - emission).abs() < 1e-10);
        let attenuation = transmittance(10_000.0, 64, |_| 1e-5);
        assert!((irradiance * attenuation - emission * (-0.1_f64).exp()).abs() < 1e-10);
    }
}

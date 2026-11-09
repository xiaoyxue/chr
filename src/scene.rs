use crate::media::FrameInfo;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
pub struct Level {
    pub width: u32,
    pub height: u32,
    pub offset: u64,
}

#[derive(Deserialize)]
pub struct Artwork {
    pub path: PathBuf,
    pub levels: Vec<Level>,
}

#[derive(Deserialize)]
pub struct Quad {
    pub valid: bool,
    pub texture: u32,
    #[serde(default)]
    pub inverse: [f32; 9],
    #[serde(default)]
    pub bounds: [i32; 4],
}

#[derive(Deserialize)]
pub struct Frame {
    pub clip_pts: i64,
    pub time_base_num: i64,
    pub time_base_den: i64,
    pub present_index: i64,
    pub ti_row_index: Option<u32>,
    pub quads: Vec<Quad>,
}

#[derive(Deserialize)]
pub struct Pins {
    pub video_sha256: String,
}

#[derive(Deserialize)]
pub struct Scene {
    pub version: u32,
    pub video: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub opacity: f32,
    pub textures: Vec<Artwork>,
    pub mask_path: PathBuf,
    pub frames: Vec<Frame>,
    pub pins: Pins,
}

impl Scene {
    pub fn load(path: &Path) -> Result<Self> {
        let scene: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(scene.version == 1, "Unsupported scene manifest version");
        ensure!(
            scene.fps_num == 30 && scene.fps_den == 1,
            "Expected 30 fps NDF"
        );
        ensure!(
            scene.width > 0
                && scene.height > 0
                && scene.width.is_multiple_of(4)
                && scene.height.is_multiple_of(2),
            "NV12 packing requires width divisible by four and even height"
        );
        ensure!(
            !scene.frames.is_empty() && scene.textures.len() == 3,
            "Expected the three-texture SC1464 scene"
        );
        ensure!(scene.opacity.is_finite() && (0.0..=1.0).contains(&scene.opacity));
        for frame in &scene.frames {
            ensure!(frame.quads.len() == 6, "Expected six ordered quads");
            for quad in &frame.quads {
                ensure!(quad.texture < 3, "Invalid texture index");
                ensure!(quad.inverse.iter().all(|value| value.is_finite()));
            }
        }
        let expected = u64::from(scene.width) * u64::from(scene.height) * scene.frames.len() as u64;
        ensure!(
            std::fs::metadata(&scene.mask_path)?.len() == expected,
            "Invalid mask cache size"
        );
        crate::media::verify_source(&scene.video, &scene.pins.video_sha256)?;
        Ok(scene)
    }

    pub fn frame_index(&self, info: &FrameInfo) -> Result<usize> {
        ensure!(
            (info.width, info.height) == (self.width, self.height),
            "Decoded raster differs from camera geometry"
        );
        ensure!(info.pts >= 0 && info.time_base_num > 0 && info.time_base_den > 0);
        let numerator = i128::from(info.pts) * i128::from(info.time_base_num) * 30;
        let denominator = i128::from(info.time_base_den);
        ensure!(
            numerator % denominator == 0,
            "Frame PTS is off the source frame grid"
        );
        let index = usize::try_from(numerator / denominator)?;
        let row = self
            .frames
            .get(index)
            .context("Frame PTS is outside the source clip")?;
        ensure!(
            i128::from(info.pts) * i128::from(info.time_base_num) * i128::from(row.time_base_den)
                == i128::from(row.clip_pts) * i128::from(row.time_base_num) * denominator,
            "Decoded PTS does not match the frame-map entry"
        );
        Ok(index)
    }
}

pub fn timecode(sequence: u64) -> String {
    let frames = sequence % (24 * 60 * 60 * 30);
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        frames / 108_000,
        frames / 1_800 % 60,
        frames / 30 % 60,
        frames % 30
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecode_is_continuous_across_clip_loop_and_wraps_at_midnight() {
        assert_eq!(timecode(1481), "00:00:49:11");
        assert_eq!(timecode(1482), "00:00:49:12");
        assert_eq!(timecode(24 * 60 * 60 * 30), "00:00:00:00");
    }

    #[test]
    fn association_uses_source_pts_and_rejects_off_grid_or_out_of_range_frames() {
        let scene = Scene {
            version: 1,
            video: PathBuf::new(),
            width: 4,
            height: 2,
            fps_num: 30,
            fps_den: 1,
            opacity: 0.5,
            textures: vec![],
            mask_path: PathBuf::new(),
            pins: Pins {
                video_sha256: String::new(),
            },
            frames: (0..3)
                .map(|i| Frame {
                    clip_pts: i * 512,
                    time_base_num: 1,
                    time_base_den: 15360,
                    present_index: 100 + i,
                    ti_row_index: (i == 2).then_some(0),
                    quads: vec![],
                })
                .collect(),
        };
        let mut frame = FrameInfo {
            width: 4,
            height: 2,
            time_base_num: 1,
            time_base_den: 90000,
            pts: 6000,
            ..Default::default()
        };
        assert_eq!(scene.frame_index(&frame).unwrap(), 2);
        assert_eq!(scene.frames[2].ti_row_index, Some(0));
        frame.pts = 6001;
        assert!(scene.frame_index(&frame).is_err());
        frame.pts = 9000;
        assert!(scene.frame_index(&frame).is_err());
    }
}

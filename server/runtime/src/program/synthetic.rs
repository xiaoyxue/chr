//! Named, trusted-local synthetic profile; none of these capacities are production defaults.
use super::Error;
use crate::{
    frame_resources::{FrameMetadata, FrameResources, ResourceConfig},
    frame_submissions::{FrameSubmissions, MonotonicClock},
    retained::{Registrations, SceneAdapter, SceneControl},
    sdi::VideoMode,
};
use chr_common::scene_frame as w;
use chr_renderer::{
    glb::{GlbAsset, ImportedMaterial},
    scene::{MeshData, PbrMaterial},
};
use serde_json::json;
use std::{
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

struct Clock(Instant);
impl MonotonicClock for Clock {
    fn now_ms(&self) -> u64 {
        self.0.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
    }
}

pub fn id(text: impl Into<String>) -> Result<w::Id, Error> {
    Ok(w::Id::try_from(text.into())?)
}

/// Fresh run namespace, never a static fixture ID. A new stream derives from each run.
pub fn runtime_id() -> Result<w::Id, Error> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static RUN: AtomicU64 = AtomicU64::new(0);
    let counter = RUN
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| "run namespace exhausted")?;
    id(format!(
        "synthetic-{}-{}-{counter}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ))
}

pub struct Synthetic {
    pub configuration: w::Configuration,
    pub scheduler: FrameSubmissions,
    pub scene_id: w::Id,
    pub stream_id: w::Id,
    pub video_lens: Option<Arc<crate::calibration::replay::VideoLens>>,
    pixels: Vec<u8>,
    next: u64,
    pending: Vec<w::Id>,
    initial_state: w::SceneState,
}

impl Synthetic {
    /// Small uncalibrated overlay for the opt-in software video demonstration.
    pub fn install_video(mode: VideoMode, runtime: w::Id) -> Result<(Self, SceneAdapter), Error> {
        Self::install_kind(mode, runtime, true)
    }

    pub(super) fn configuration(
        mode: VideoMode,
        runtime: &w::Id,
    ) -> Result<w::Configuration, Error> {
        if mode.width > 1280 || mode.height > 720 {
            return Err("synthetic-program-v1 supports at most 1280x720".into());
        }
        let configuration: w::Configuration = serde_json::from_value(json!({
            "runtime_id":runtime, "configuration_id":format!("{}-configuration", runtime.as_str()),
            "profile":"scene_frame_v1",
            "native_extent":{"width":mode.width,"height":mode.height},
            "output_extent":{"width":mode.width,"height":mode.height},
            "output_rate":{"numerator":1,"denominator":mode.fps},
            "fit":"contain","input_stage":"display_background","output_transfer":"bt709",
            "exposure":1,"tone_map":"aces","sample_count":1,
            "shadows":{"resolution":256,"filter_radius":2,"pcf_samples":9,
                "depth_bias":0.001,"normal_bias":0.01,"max_bytes":"16777216","max_samples_per_fragment":"1024",
                "pcss":{"blocker_samples":16,"filter_samples":16,"max_search_radius":8,"max_filter_radius":8},
                "cascades":{"count":1,"max_distance":100,"split_lambda":0.5,"blend_fraction":0.1}},
            "environment":{"resolution":64,"samples":16,"max_bytes":"1048576"},
            "max_intermediate_bytes":"16777216",
            "limits":{"scenes":4,"instances_per_scene":8,"lights_per_scene":4,"operations_per_mutation":8,
                "published_frames":8,"frames_per_submission":4,"inflight_frames":4,"retained_submissions":8,"render_timeout_ms":5000}
        }))?;
        Ok(configuration)
    }

    pub fn install(mode: VideoMode, runtime: w::Id) -> Result<(Self, SceneAdapter), Error> {
        Self::install_kind(mode, runtime, false)
    }

    fn install_kind(
        mode: VideoMode,
        runtime: w::Id,
        video: bool,
    ) -> Result<(Self, SceneAdapter), Error> {
        let configuration = Self::configuration(mode, &runtime)?;
        let mut registrations = Registrations::default();
        registrations
            .register_world(serde_json::from_value(
                json!({"id":"synthetic-world","revision":"1"}),
            )?)
            .map_err(|e| e.to_string())?;
        // Native -Z-up ground receiver plus an elevated red triangle. No asset files.
        let mesh = if video {
            MeshData::new(
                vec![[-1.2, -0.8, -1.], [0., 1.2, -1.], [1.2, -0.8, -1.]],
                vec![0, 1, 2],
                std::iter::once(0..3).collect(),
            )?
        } else {
            MeshData::new(
                vec![
                    [-4., -3., 0.],
                    [4., -3., 0.],
                    [4., 3., 0.],
                    [-4., 3., 0.],
                    [-1.2, -0.8, -1.],
                    [0., 1.2, -1.],
                    [1.2, -0.8, -1.],
                ],
                vec![0, 2, 1, 0, 3, 2, 4, 5, 6],
                vec![0..6, 6..9],
            )?
        };
        let material = |color| ImportedMaterial {
            factors: PbrMaterial {
                base_color: color,
                roughness: 0.75,
                double_sided: true,
                ..Default::default()
            },
            maps: Default::default(),
        };
        registrations
            .register_model(
                id("synthetic-mesh")?,
                GlbAsset {
                    mesh,
                    materials: if video {
                        vec![material([0.8, 0.08, 0.02, 1.])]
                    } else {
                        vec![
                            material([0.55, 0.55, 0.55, 1.]),
                            material([0.8, 0.08, 0.02, 1.]),
                        ]
                    },
                    warnings: Vec::new(),
                },
            )
            .map_err(|e| e.to_string())?;
        if video {
            registrations
                .register_model(
                    id("synthetic-ground")?,
                    GlbAsset {
                        mesh: MeshData::new(
                            vec![[-4., -3., 0.], [4., -3., 0.], [4., 3., 0.], [-4., 3., 0.]],
                            vec![0, 2, 1, 0, 3, 2],
                            std::iter::once(0..6).collect(),
                        )?,
                        materials: vec![material([0.55, 0.55, 0.55, 1.])],
                        warnings: Vec::new(),
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        let e = &configuration.native_extent;
        let mut state: w::SceneState = serde_json::from_value(json!({
                "world":{"id":"synthetic-world","revision":"1"},"configuration_id":configuration.configuration_id,
                "camera":{"mode":"manual","camera":{"kind":"intrinsics_pose","extent":e,"near_m":0.1,"far_m":100,
                    "intrinsics":{"fx":e.width as f64 * 0.8,"fy":e.width as f64 * 0.8,"skew":0,"cx":e.width as f64 / 2.,"cy":e.height as f64 / 2.},
                    "camera_to_world":[[1,0,0,0],[0,1,0,0],[0,0,1,-8],[0,0,0,1]]}},
                "instances":[{"instance_id":"mesh","asset_ref":"synthetic-mesh","model":[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],
                    "visible":true,"material_overrides":[]}],
                "lighting":{"lights":[{"light_id":"sun","kind":"directional","direction":[0.4,0.2,1],
                    "irradiance":[3,3,3],"angular_radius":0.00465,"shadow_mode":null}],"environment":null},
                "shadow_mode":"pcf"
        }))?;
        if video {
            let mut ground = state.instances[0].clone();
            ground.instance_id = id("ground")?;
            ground.asset_ref = id("synthetic-ground")?;
            state.instances.push(ground);
            for instance in &mut state.instances {
                for axis in 0..3 {
                    instance.model[axis][axis] = 0.25.try_into()?;
                }
            }
        }
        Self::install_registered(configuration, registrations, state)
    }

    /// Shared native image producer and scheduler for explicitly installed local demos.
    pub(super) fn install_registered(
        configuration: w::Configuration,
        registrations: Registrations,
        initial_state: w::SceneState,
    ) -> Result<(Self, SceneAdapter), Error> {
        super::validate_output(&configuration)?;
        let runtime = &configuration.runtime_id;
        let stream_id = id(format!("{}-input", runtime.as_str()))?;
        let e = configuration.native_extent;
        let resources = FrameResources::new(
            ResourceConfig {
                runtime_id: runtime.clone(),
                native_extent: e,
                max_frames: 8,
                max_frame_bytes: e.width as usize * e.height as usize * 8 * 8,
                max_assets: 1,
                max_asset_bytes: 4,
                max_producers: 1,
            },
            vec![stream_id.clone()],
        )?;
        let producer = resources.producer(&stream_id)?;
        let scheduler = FrameSubmissions::new(
            configuration.clone(),
            resources,
            producer,
            Arc::new(Clock(Instant::now())),
        )
        .map_err(|e| e.to_string())?;
        let adapter = SceneAdapter::new(
            configuration.clone(),
            registrations,
            Box::new(scheduler.clone()),
        )
        .map_err(|e| e.to_string())?;
        let scene_id = id(format!("{}-scene", runtime.as_str()))?;
        Ok((
            Self {
                configuration,
                scheduler,
                scene_id,
                stream_id,
                video_lens: None,
                pixels: [32, 64, 96, 255].repeat(e.width as usize * e.height as usize),
                next: 0,
                pending: Vec::new(),
                initial_state,
            },
            adapter,
        ))
    }

    pub async fn create_scene(&self, control: &SceneControl) -> Result<(), Error> {
        self.create_scene_with(control, &self.initial_state).await
    }

    /// Creates the program scene from `state` instead of the initial one, through
    /// the same validating `scene.create` as both IPCs.
    pub async fn create_scene_with(
        &self,
        control: &SceneControl,
        state: &w::SceneState,
    ) -> Result<(), Error> {
        control.call(w::SCENE_CREATE, Some(json!({
            "runtime_id":self.configuration.runtime_id,"scene_id":self.scene_id,"state":state,
        }))).await.map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The scene [`Self::create_scene`] creates.
    pub fn initial_state(&self) -> &w::SceneState {
        &self.initial_state
    }

    /// The synthetic native input image (RGBA8, BT.709 code values) that
    /// [`Self::inject`] submits.
    pub fn native_pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// One native frame, submitted through the same strict control queue as both IPCs.
    /// Terminal native-generated receipts are explicitly released; external receipts are not touched.
    pub async fn inject(&mut self, control: &SceneControl) -> Result<Option<w::Id>, Error> {
        self.inject_inner(control, None).await
    }

    /// Software capture assigns sequence before its bounded queue can drop a frame.
    pub async fn inject_image(
        &mut self,
        control: &SceneControl,
        sequence: u64,
        pixels: &[u8],
    ) -> Result<Option<w::Id>, Error> {
        if sequence < self.next {
            return Err("software input frame sequence went backwards".into());
        }
        self.inject_inner(control, Some((sequence, pixels))).await
    }

    async fn inject_inner(
        &mut self,
        control: &SceneControl,
        input: Option<(u64, &[u8])>,
    ) -> Result<Option<w::Id>, Error> {
        let runtime = &self.configuration.runtime_id;
        let mut retained = Vec::new();
        for submission in self.pending.drain(..) {
            let target = json!({"runtime_id":runtime,"submission_id":submission});
            let snapshot: w::SubmissionSnapshot = serde_json::from_value(
                control
                    .call(w::RENDER_GET, Some(target.clone()))
                    .await
                    .map_err(|e| e.to_string())?,
            )?;
            if snapshot.frames.iter().all(|frame| {
                matches!(
                    frame,
                    w::FrameResult::Failed { .. }
                        | w::FrameResult::Rendered {
                            delivery: w::Delivery::Transmitted { .. } | w::Delivery::Dropped { .. },
                            ..
                        }
                )
            }) {
                control
                    .call(w::RENDER_RELEASE, Some(target))
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                retained.push(submission);
            }
        }
        self.pending = retained;
        if self.pending.len() >= 4 {
            return Ok(None);
        }
        let sequence = input.map_or(self.next, |(sequence, _)| sequence);
        self.next = sequence
            .checked_add(1)
            .ok_or("synthetic sequence exhausted")?;
        let timeline = id(format!("{}-timeline", runtime.as_str()))?;
        let metadata = FrameMetadata {
            frame: w::FrameKey {
                stream_id: self.stream_id.clone(),
                sequence: sequence.into(),
            },
            timeline: w::Timeline {
                timeline_id: timeline.clone(),
                time_base: self.configuration.output_rate,
            },
            position: w::Position {
                timeline_id: timeline,
                ticks: sequence.into(),
            },
            image: w::ImageDescription {
                extent: self.configuration.native_extent,
                format: w::ImageFormat::Rgba8Unorm,
                transfer: w::Transfer::Bt709,
                alpha: w::Alpha::Straight,
            },
        };
        let (ticket, producer) = self
            .scheduler
            .register(
                metadata.clone(),
                input.map_or(self.pixels.as_slice(), |(_, pixels)| pixels),
            )
            .map_err(|e| e.to_string())?;
        producer.ready()?;
        producer.complete();
        let published = control.call(w::FRAME_PUBLISH, Some(json!({
            "runtime_id":runtime,"producer_ticket":ticket.id(),"frame":metadata.frame,
            "timeline":metadata.timeline,"position":metadata.position,"image":metadata.image,
        }))).await;
        let published: w::PublishedFrame = match published {
            Ok(value) => serde_json::from_value(value)?,
            Err(error) => {
                self.scheduler
                    .cancel_write(&ticket)
                    .map_err(|e| e.to_string())?;
                return Err(error.to_string().into());
            }
        };
        let submission = id(format!("{}-submission-{sequence}", runtime.as_str()))?;
        let submitted = control.call(w::RENDER_SUBMIT, Some(json!({
            "runtime_id":runtime,"scene_id":self.scene_id,"submission_id":submission,"expected_scene_revision":null,
            "frames":[{"frame":published.frame,"image_ref":published.image_ref,"position":published.position,"geometry":{"kind":"scene_camera"}}],
        }))).await;
        control
            .call(
                w::FRAME_RELEASE,
                Some(json!({"runtime_id":runtime,"image_ref":published.image_ref})),
            )
            .await
            .map_err(|e| e.to_string())?;
        submitted.map_err(|e| e.to_string())?;
        self.pending.push(submission.clone());
        Ok(Some(submission))
    }
}

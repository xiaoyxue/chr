//! One program frame rendered to an image, as `chr-renderer-cli render` does.
//!
//! The frame takes the live path: the program is installed from its
//! specification, its scene is created through `scene.create`, one native input
//! frame is published and submitted, and the scheduler's execution is composed by
//! [`ProgramRenderer`]. Only the end differs: the BT.709 RGBA program image is read
//! back instead of being converted to UYVY and handed to the SDI port.
use super::{Error, ProgramRenderer, spec::ProgramSpec, synthetic::Synthetic};
use crate::{frame_submissions::Execution, retained::SceneControl, sdi::VideoMode};
use chr_common::scene_frame as w;
use chr_renderer::Gpu;
use std::time::Instant;

pub struct Still {
    pub spec: ProgramSpec,
    pub mode: VideoMode,
    /// Replaces the program's initial scene. Its `configuration_id` is replaced by
    /// the program's, which is only known after installation.
    pub state: Option<w::SceneState>,
    /// Native input image. Default: the program's synthetic input.
    pub background: Option<Background>,
    /// Input frame sequence; it selects the lens model of a calibrated video scene
    /// and the replayed camera of a tracked scene.
    pub frame: u64,
}

/// Tightly packed RGBA8 BT.709 code values, straight alpha.
pub struct Background {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A still whose frame is admitted and ready for the GPU.
pub struct Prepared {
    program: Synthetic,
    control: SceneControl,
    owner: tokio::task::JoinHandle<()>,
    execution: Execution,
}

/// Installs the program and admits its frame; validates every input without a GPU.
pub async fn prepare(still: Still) -> Result<Prepared, Error> {
    let installed = still
        .spec
        .install(still.mode, super::synthetic::runtime_id()?)?;
    let mut program = installed.program;
    let (control, owner) = SceneControl::spawn(installed.adapter, 8).map_err(|e| e.to_string())?;
    match admit(&mut program, &control, still).await {
        Ok(execution) => Ok(Prepared {
            program,
            control,
            owner,
            execution,
        }),
        Err(error) => {
            control.shutdown().await;
            owner.await?;
            Err(error)
        }
    }
}

async fn admit(
    program: &mut Synthetic,
    control: &SceneControl,
    still: Still,
) -> Result<Execution, Error> {
    match still.state {
        Some(mut state) => {
            state.configuration_id = program.configuration.configuration_id.clone();
            program.create_scene_with(control, &state).await?;
        }
        None => program.create_scene(control).await?,
    }
    let pixels = match still.background {
        Some(background) => {
            let native = program.configuration.native_extent;
            if (background.width, background.height) != (native.width, native.height) {
                return Err(format!(
                    "the background is {}x{}, the program's native extent is {}x{}",
                    background.width, background.height, native.width, native.height
                )
                .into());
            }
            background.rgba
        }
        None => program.native_pixels().to_vec(),
    };
    let geometry = match &program.tracking {
        Some(tracked) => tracked.geometry(
            &tracked
                .replay
                .as_ref()
                .ok_or("a still of a tracked program needs a reference camera")?
                .frame(still.frame),
        ),
        None => w::FrameGeometry::SceneCamera {},
    };
    program
        .inject_with(control, still.frame, &pixels, geometry)
        .await?
        .ok_or("the input frame was not admitted")?;
    Ok(program
        .scheduler
        .next_ready()
        .map_err(|e| e.to_string())?
        .ok_or("no execution is ready")?)
}

impl Prepared {
    /// Output width and height of the rendered image.
    pub fn extent(&self) -> (u32, u32) {
        let extent = self.program.configuration.output_extent;
        (extent.width, extent.height)
    }

    /// Renders the BT.709 RGBA8 program image: tightly packed, top-down,
    /// `width * height * 4` bytes of [`Self::extent`].
    pub async fn render(self, gpu: &Gpu) -> Result<Vec<u8>, Error> {
        let rendered =
            ProgramRenderer::new(gpu, &self.program.configuration).and_then(|mut renderer| {
                renderer.set_video_lens(self.program.video_lens.clone());
                renderer.render_rgba(gpu, self.execution)
            });
        self.control.shutdown().await;
        self.owner.await?;
        rendered
    }

    /// Renders the admitted frame `frames` times the way a render worker does
    /// (composition, UYVY conversion and readback) and times each frame. GPU time
    /// needs a device with timestamp queries inside encoders; otherwise it is `None`.
    /// Nothing is transmitted.
    pub async fn bench(self, gpu: &Gpu, frames: u32) -> Result<Vec<FrameTiming>, Error> {
        let timings = (|| {
            let mut renderer = ProgramRenderer::new(gpu, &self.program.configuration)?;
            renderer.set_video_lens(self.program.video_lens.clone());
            let timer = Timer::new(gpu);
            renderer.set_timestamp_queries(gpu, timer.as_ref().map(|t| t.queries.clone()))?;
            let frame = super::Frame::of(&self.execution)?;
            (0..frames)
                .map(|_| {
                    let start = Instant::now();
                    renderer.render_frame(gpu, &frame)?;
                    let wall_ms = start.elapsed().as_secs_f64() * 1e3;
                    let gpu_ms = timer.as_ref().map(|t| t.read(gpu)).transpose()?;
                    Ok(FrameTiming { gpu_ms, wall_ms })
                })
                .collect::<Result<Vec<_>, Error>>()
        })();
        self.execution.failed().map_err(|e| e.to_string())?;
        self.control.shutdown().await;
        self.owner.await?;
        timings
    }
}

/// One benchmarked frame, in milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct FrameTiming {
    /// From the first command to the end of the UYVY conversion on the GPU.
    pub gpu_ms: Option<f64>,
    /// The whole call on the CPU, including upload, submission and readback.
    pub wall_ms: f64,
}

/// The two timestamps [`ProgramRenderer`] writes around a frame, read back after it.
struct Timer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
}

impl Timer {
    fn new(gpu: &Gpu) -> Option<Self> {
        let required =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        gpu.device.features().contains(required).then(|| {
            let buffer = |label, usage| {
                gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: 16,
                    usage,
                    mapped_at_creation: false,
                })
            };
            Self {
                queries: gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("bench timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                }),
                resolve: buffer(
                    "bench timestamp resolve",
                    wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                ),
                readback: buffer(
                    "bench timestamp readback",
                    wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                ),
            }
        })
    }

    fn read(&self, gpu: &Gpu) -> Result<f64, Error> {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.resolve_query_set(&self.queries, 0..2, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, 16);
        gpu.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| e.to_string())?;
        receiver.recv()?.map_err(|e| e.to_string())?;
        let ticks = {
            let bytes = self
                .readback
                .slice(..)
                .get_mapped_range()
                .map_err(|e| e.to_string())?;
            let stamp = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().expect("8 bytes"));
            stamp(8).saturating_sub(stamp(0))
        };
        self.readback.unmap();
        Ok(ticks as f64 * f64::from(gpu.queue.get_timestamp_period()) / 1e6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODE: VideoMode = VideoMode {
        width: 64,
        height: 32,
        fps: chr_common::rate::Rate::fps(50),
    };

    fn still() -> Still {
        Still {
            spec: ProgramSpec::Synthetic,
            mode: MODE,
            state: None,
            background: None,
            frame: 0,
        }
    }

    async fn initial_state() -> w::SceneState {
        prepare(still())
            .await
            .unwrap()
            .program
            .initial_state()
            .clone()
    }

    #[tokio::test]
    async fn admits_the_initial_scene_or_a_replacement_state_and_frame() {
        let prepared = prepare(still()).await.unwrap();
        assert_eq!(prepared.extent(), (64, 32));
        assert_eq!(prepared.execution.provenance.input.frame.sequence.get(), 0);

        let mut state = initial_state().await;
        state.configuration_id = w::Id::try_from("from-another-run".to_owned()).unwrap();
        state.instances.clear();
        let prepared = prepare(Still {
            state: Some(state),
            frame: 7,
            ..still()
        })
        .await
        .unwrap();
        assert_eq!(prepared.execution.provenance.input.frame.sequence.get(), 7);
        assert!(
            prepared
                .execution
                .scene
                .snapshot()
                .state
                .instances
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rejects_invalid_inputs_before_rendering() {
        let wrong_size = Still {
            background: Some(Background {
                width: 32,
                height: 64,
                rgba: vec![0; 32 * 64 * 4],
            }),
            ..still()
        };
        let error = prepare(wrong_size).await.err().unwrap().to_string();
        assert!(error.contains("native extent is 64x32"), "{error}");

        let mut state = initial_state().await;
        state.instances[0].asset_ref = w::Id::try_from("missing-model".to_owned()).unwrap();
        assert!(
            prepare(Still {
                state: Some(state),
                ..still()
            })
            .await
            .is_err()
        );

        let too_large = VideoMode {
            width: 1920,
            height: 1080,
            fps: chr_common::rate::Rate::fps(50),
        };
        assert!(
            prepare(Still {
                mode: too_large,
                ..still()
            })
            .await
            .is_err()
        );

        let missing = ProgramSpec::VideoScene {
            path: "/nonexistent/scene.json".into(),
        };
        assert!(
            prepare(Still {
                spec: missing,
                ..still()
            })
            .await
            .is_err()
        );
    }
}

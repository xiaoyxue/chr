//! The render pipeline: owns the GPU and the scene, applies commands between frames,
//! and paces frames with the monotonic clock.
//!
//! Its only product is the program output in SDI form: every frame is rendered, converted
//! to UYVY on the GPU, read back, and queued for the SDI port. Everything clients see is
//! derived from what that port transmits (the program tap read by `chr-monitor`).

use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use chr_common::methods::triangle::TriangleState;
use chr_renderer::{
    Gpu, OffscreenTarget, TriangleRenderer, UyvyConverter, coordinates::ImageExtent,
};
use tokio::sync::{mpsc, oneshot};

use crate::{
    scene::Scene,
    sdi::{SdiFrame, SdiInput},
};

type BoxError = Box<dyn Error + Send + Sync>;

/// Commands queued for the render worker. Each carries its reply channel.
#[derive(Debug)]
pub enum Command {
    StartRotation {
        angular_velocity_rad_per_s: Option<f64>,
        reply: oneshot::Sender<Result<TriangleState, String>>,
    },
    StopRotation {
        reply: oneshot::Sender<TriangleState>,
    },
    GetState {
        reply: oneshot::Sender<TriangleState>,
    },
}

/// Bounded so a flood of commands is refused instead of growing memory.
pub const COMMAND_QUEUE_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct RenderConfig {
    pub extent: ImageExtent,
    pub fps: u32,
}

pub struct RenderWorker {
    pub commands: mpsc::Sender<Command>,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Result<(), String>>,
}

impl RenderWorker {
    /// Starts the worker and waits until the GPU, renderer, and encoder are ready.
    pub fn spawn(config: RenderConfig, sdi: SdiInput) -> Result<Self, BoxError> {
        Self::spawn_inner(config, sdi, None, None)
    }

    pub fn spawn_program(
        config: RenderConfig,
        sdi: SdiInput,
        profile: chr_common::scene_frame::Configuration,
        scheduler: crate::frame_submissions::FrameSubmissions,
        lens: Option<Arc<crate::calibration::replay::VideoLens>>,
    ) -> Result<Self, BoxError> {
        Self::spawn_inner(config, sdi, Some((profile, scheduler)), lens)
    }

    fn spawn_inner(
        config: RenderConfig,
        sdi: SdiInput,
        program: Option<(
            chr_common::scene_frame::Configuration,
            crate::frame_submissions::FrameSubmissions,
        )>,
        lens: Option<Arc<crate::calibration::replay::VideoLens>>,
    ) -> Result<Self, BoxError> {
        let (commands, receiver) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("chr-render".into())
            .spawn(move || {
                let mut worker =
                    match Worker::new(config, sdi, receiver, thread_stop, program, lens) {
                        Ok(worker) => {
                            let _ = ready_tx.send(Ok(()));
                            worker
                        }
                        Err(error) => {
                            let message = error.to_string();
                            let _ = ready_tx.send(Err(message.clone()));
                            return Err(message);
                        }
                    };
                worker.run().map_err(|error| error.to_string())
            })?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                stop,
                thread,
            }),
            Ok(Err(message)) => Err(message.into()),
            Err(_) => Err("render thread exited during startup".into()),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Asks the worker to stop after the current frame and waits for it.
    pub fn shutdown(self) -> Result<(), String> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .join()
            .map_err(|_| "render thread panicked".to_owned())?
    }
}

struct Worker {
    gpu: Gpu,
    target: OffscreenTarget,
    renderer: TriangleRenderer,
    uyvy: UyvyConverter,
    sdi: SdiInput,
    commands: mpsc::Receiver<Command>,
    stop: Arc<AtomicBool>,
    scene: Scene,
    fps: u32,
    program: Option<(
        crate::program::ProgramRenderer,
        crate::frame_submissions::FrameSubmissions,
    )>,
}

impl Worker {
    fn new(
        config: RenderConfig,
        sdi: SdiInput,
        commands: mpsc::Receiver<Command>,
        stop: Arc<AtomicBool>,
        program: Option<(
            chr_common::scene_frame::Configuration,
            crate::frame_submissions::FrameSubmissions,
        )>,
        lens: Option<Arc<crate::calibration::replay::VideoLens>>,
    ) -> Result<Self, BoxError> {
        let instance = Gpu::instance();
        let gpu =
            pollster::block_on(Gpu::request(&instance, None)).map_err(|error| error.to_string())?;
        let target = OffscreenTarget::new(&gpu, config.extent)?;
        let renderer = TriangleRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
        renderer.resize(&gpu.queue, config.extent);
        let uyvy = UyvyConverter::new(&gpu, &target)?;
        let program = program
            .map(|(profile, scheduler)| {
                crate::program::ProgramRenderer::new(&gpu, &profile).map(|mut renderer| {
                    renderer.set_video_lens(lens);
                    (renderer, scheduler)
                })
            })
            .transpose()?;
        Ok(Self {
            gpu,
            target,
            renderer,
            uyvy,
            sdi,
            commands,
            stop,
            scene: Scene::new(config.fps),
            fps: config.fps,
            program,
        })
    }

    fn run(&mut self) -> Result<(), BoxError> {
        let frame_duration = Duration::from_secs_f64(1.0 / f64::from(self.fps));
        let mut deadline = Instant::now();
        while !self.stop.load(Ordering::Relaxed) {
            if !self.apply_commands() {
                break;
            }
            self.render_frame()?;
            self.scene.advance();

            deadline += frame_duration;
            let now = Instant::now();
            if let Some(wait) = deadline.checked_duration_since(now) {
                std::thread::sleep(wait);
            } else if now.duration_since(deadline) > frame_duration {
                // Too far behind to catch up: drop the backlog instead of bursting frames.
                log::debug!("render loop behind by {:?}", now.duration_since(deadline));
                deadline = now;
            }
        }
        Ok(())
    }

    /// Applies queued commands before the next frame. Returns false once no sender is left.
    fn apply_commands(&mut self) -> bool {
        loop {
            match self.commands.try_recv() {
                Ok(Command::StartRotation {
                    angular_velocity_rad_per_s,
                    reply,
                }) => {
                    let _ = reply.send(self.scene.start_rotation(angular_velocity_rad_per_s));
                }
                Ok(Command::StopRotation { reply }) => {
                    let _ = reply.send(self.scene.stop_rotation());
                }
                Ok(Command::GetState { reply }) => {
                    let _ = reply.send(self.scene.state());
                }
                Err(mpsc::error::TryRecvError::Empty) => return true,
                Err(mpsc::error::TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn render_frame(&mut self) -> Result<(), BoxError> {
        if let Some((renderer, scheduler)) = &mut self.program {
            return renderer.drive(&self.gpu, scheduler, &mut self.sdi);
        }
        self.renderer
            .set_rotation(&self.gpu.queue, self.scene.angle_rad());
        let renderer = &self.renderer;
        self.target
            .render(&self.gpu, |encoder, view| renderer.encode(encoder, view))?;
        let mut uyvy = Vec::new();
        self.uyvy.convert(&self.gpu, &mut uyvy)?;
        self.sdi.submit(SdiFrame {
            source_frame: self.scene.frame(),
            uyvy: Arc::new(uyvy),
            program: None,
        });
        Ok(())
    }
}

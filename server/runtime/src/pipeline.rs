//! The render pipeline: owns the scene and the output clock, applies commands between
//! frames, and paces frames with the monotonic clock. It holds no GPU: every frame is
//! rendered by the supervised `chr-renderer-cli worker` (see [`crate::worker`]).
//!
//! Its only product is the program output in SDI form, queued for the SDI port.
//! Everything clients see is derived from what that port transmits (the program tap
//! read by `chr-monitor`). A frame the worker does not deliver in time is
//! transmitted as its clean feed: the input image without CG, scaled to the
//! output. A tracked frame without a camera is clean feed too ([`CleanFrame`]).

use std::{
    collections::VecDeque,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use chr_common::{methods::triangle::TriangleState, scene_frame as w};
use chr_renderer::{ChannelOrder, coordinates::ImageExtent, uyvy_from_bt709};
use tokio::sync::{mpsc, oneshot};

use crate::{
    frame_submissions::{Execution, FrameSubmissions},
    scene::Scene,
    sdi::{SdiFrame, SdiInput, VideoMode},
    worker::{
        supervisor::{Miss, StatusHandle, Supervisor, WorkerConfig},
        transport::SharedPixels,
    },
};

type BoxError = Box<dyn Error + Send + Sync>;

/// Commands queued for the render loop. Each carries its reply channel.
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

/// Answers the triangle's commands without a render loop, for chr-server's
/// GStreamer core (`--pipeline gst`), which renders only its program: the
/// triangle's state is kept and reported but never drawn.
pub async fn answer_commands(mut commands: mpsc::Receiver<Command>, fps: u32) {
    let mut scene = Scene::new(fps);
    while let Some(command) = commands.recv().await {
        match command {
            Command::StartRotation {
                angular_velocity_rad_per_s,
                reply,
            } => {
                let _ = reply.send(scene.start_rotation(angular_velocity_rad_per_s));
            }
            Command::StopRotation { reply } => {
                let _ = reply.send(scene.stop_rotation());
            }
            Command::GetState { reply } => {
                let _ = reply.send(scene.state());
            }
        }
    }
}

/// A captured frame without a camera, transmitted as its clean feed in its
/// place among the program frames.
pub struct CleanFrame {
    /// The program input sequence the frame takes.
    pub sequence: u64,
    /// RGBA8 BT.709 code values at `extent`, tightly packed.
    pub rgba: SharedPixels,
    pub extent: w::Extent,
}

/// Clean frames that may wait for the render loop.
pub const CLEAN_QUEUE: usize = 4;

#[derive(Debug, Clone, Copy)]
pub struct RenderConfig {
    pub mode: VideoMode,
    /// How long a frame may take in the worker before it becomes clean feed; the
    /// wait never extends past the loop's next frame boundary.
    pub render_deadline: Duration,
}

pub struct RenderWorker {
    pub commands: mpsc::Sender<Command>,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Result<(), String>>,
}

impl RenderWorker {
    /// Starts the render loop for the default triangle. The worker process starts
    /// asynchronously; until it is ready the port repeats its last frame.
    pub fn spawn(
        config: RenderConfig,
        sdi: SdiInput,
        worker: WorkerConfig,
        status: StatusHandle,
    ) -> Result<Self, BoxError> {
        Self::spawn_inner(config, sdi, worker, status, None, None)
    }

    /// Starts the render loop for an installed program; until the worker is ready
    /// its frames are clean feed. `clean` delivers the frames without a camera
    /// of a tracked program.
    pub fn spawn_program(
        config: RenderConfig,
        sdi: SdiInput,
        worker: WorkerConfig,
        status: StatusHandle,
        scheduler: FrameSubmissions,
        clean: Option<Receiver<CleanFrame>>,
    ) -> Result<Self, BoxError> {
        Self::spawn_inner(config, sdi, worker, status, Some(scheduler), clean)
    }

    fn spawn_inner(
        config: RenderConfig,
        sdi: SdiInput,
        worker: WorkerConfig,
        status: StatusHandle,
        scheduler: Option<FrameSubmissions>,
        clean: Option<Receiver<CleanFrame>>,
    ) -> Result<Self, BoxError> {
        let supervisor = Supervisor::new(worker, Arc::clone(&status))?;
        let (commands, receiver) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let mut pipeline = Pipeline {
            supervisor,
            status,
            sdi,
            commands: receiver,
            stop: Arc::clone(&stop),
            scene: Scene::new(config.mode.fps),
            config,
            scheduler,
            clean,
            clean_pending: VecDeque::new(),
        };
        let thread = std::thread::Builder::new()
            .name("chr-render".into())
            .spawn(move || pipeline.run().map_err(|error| error.to_string()))?;
        Ok(Self {
            commands,
            stop,
            thread,
        })
    }

    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Asks the loop to stop after the current frame, waits for it and stops the worker.
    pub fn shutdown(self) -> Result<(), String> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .join()
            .map_err(|_| "render thread panicked".to_owned())?
    }
}

struct Pipeline {
    supervisor: Supervisor,
    status: StatusHandle,
    sdi: SdiInput,
    commands: mpsc::Receiver<Command>,
    stop: Arc<AtomicBool>,
    scene: Scene,
    config: RenderConfig,
    scheduler: Option<FrameSubmissions>,
    clean: Option<Receiver<CleanFrame>>,
    /// Clean frames received but not transmitted yet, in sequence order.
    clean_pending: VecDeque<CleanFrame>,
}

impl Pipeline {
    fn run(&mut self) -> Result<(), BoxError> {
        let frame_duration = self.config.mode.frame_duration();
        let mut deadline = Instant::now();
        while !self.stop.load(Ordering::Relaxed) {
            if !self.apply_commands() {
                break;
            }
            deadline += frame_duration;
            // The worker may use the frame's render deadline, but never the time the
            // loop needs to reach the next frame.
            let render_deadline = (Instant::now() + self.config.render_deadline).min(deadline);
            self.render_frame(render_deadline)?;
            self.scene.advance();

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

    fn render_frame(&mut self, deadline: Instant) -> Result<(), BoxError> {
        self.supervisor.maintain();
        let Some(scheduler) = self.scheduler.clone() else {
            match self
                .supervisor
                .triangle(self.scene.frame(), self.scene.angle_rad(), deadline)
            {
                Ok(uyvy) => self.sdi.submit(SdiFrame {
                    source_frame: self.scene.frame(),
                    uyvy: Arc::new(uyvy),
                    program: None,
                }),
                // The triangle has no input to fall back to: the port repeats.
                Err(miss) => self.missed(&miss, false),
            }
            return Ok(());
        };
        scheduler
            .handoff(&mut self.sdi)
            .map_err(|e| e.to_string())?;
        if let Some(clean) = &self.clean {
            self.clean_pending.extend(clean.try_iter());
        }
        // Clean frames keep their place: those before the next program frame
        // go first, the others once no program frame waits.
        match scheduler.next_ready().map_err(|e| e.to_string())? {
            Some(execution) => {
                let sequence = execution.provenance.input.frame.sequence.get();
                self.transmit_clean(Some(sequence));
                self.render_program(execution, deadline);
            }
            None => self.transmit_clean(None),
        }
        scheduler
            .handoff(&mut self.sdi)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Transmits the pending clean frames before `before`, or all of them.
    fn transmit_clean(&mut self, before: Option<u64>) {
        while self
            .clean_pending
            .front()
            .is_some_and(|clean| before.is_none_or(|before| clean.sequence < before))
        {
            let clean = self.clean_pending.pop_front().expect("front exists");
            match clean_uyvy(
                &clean.rgba,
                clean.extent,
                ChannelOrder::Rgba,
                self.config.mode,
            ) {
                Some(uyvy) => self.sdi.submit(SdiFrame {
                    source_frame: clean.sequence,
                    uyvy: Arc::new(uyvy),
                    program: None,
                }),
                None => log::warn!("clean frame {} could not be converted", clean.sequence),
            }
        }
    }

    fn render_program(&mut self, execution: Execution, deadline: Instant) {
        let result = match execution.input.stored() {
            Ok(pixels) => self.supervisor.render(
                &execution.input.metadata().image,
                pixels,
                &execution.provenance.input,
                execution.scene.snapshot(),
                deadline,
            ),
            Err(error) => Err(Miss::Failed(error.to_string())),
        };
        let miss = match result {
            Ok(uyvy) => match execution.rendered(Arc::new(uyvy)) {
                Ok(()) => return,
                Err(error) => {
                    log::warn!("rendered frame was not accepted: {error:?}");
                    return;
                }
            },
            Err(miss) => miss,
        };
        // A worker still rendering the input holds its own reference to the
        // shared memory, so the input can be released here.
        let clean = clean_feed(&execution, self.config.mode);
        let source_frame = execution.provenance.input.frame.sequence.get();
        if let Err(error) = execution.failed() {
            log::warn!("failed frame was not recorded: {error:?}");
        }
        match clean {
            Some(uyvy) => {
                self.sdi.submit(SdiFrame {
                    source_frame,
                    uyvy: Arc::new(uyvy),
                    program: None,
                });
                self.missed(&miss, true);
            }
            None => self.missed(&miss, false),
        }
    }

    fn missed(&self, miss: &Miss, clean_feed: bool) {
        log::debug!("frame without CG: {miss:?}");
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if clean_feed {
            status.clean_feed_frames += 1;
        } else {
            status.repeated_for_unavailable_clean_feed += 1;
        }
    }
}

/// The input image converted to the output format, when it is 8-bit BT.709;
/// otherwise `None` and the port repeats its previous frame.
fn clean_feed(execution: &Execution, mode: VideoMode) -> Option<Vec<u8>> {
    let image = &execution.input.metadata().image;
    let order = match image.format {
        w::ImageFormat::Rgba8Unorm => ChannelOrder::Rgba,
        w::ImageFormat::Bgra8Unorm => ChannelOrder::Bgra,
        w::ImageFormat::Rgba16Float => return None,
    };
    if image.transfer != w::Transfer::Bt709 {
        return None;
    }
    clean_uyvy(execution.input.pixels().ok()?, image.extent, order, mode)
}

/// 8-bit BT.709 pixels at `extent` as UYVY at the output extent, scaled
/// bilinearly when the extents differ (the output keeps the camera's aspect).
fn clean_uyvy(
    pixels: &[u8],
    extent: w::Extent,
    order: ChannelOrder,
    mode: VideoMode,
) -> Option<Vec<u8>> {
    let scaled;
    let pixels = if (extent.width, extent.height) == (mode.width, mode.height) {
        pixels
    } else {
        scaled = resize_rgba8(pixels, extent, mode)?;
        &scaled
    };
    let mut uyvy = Vec::new();
    uyvy_from_bt709(
        pixels,
        ImageExtent::new(mode.width, mode.height).ok()?,
        order,
        &mut uyvy,
    )
    .ok()?;
    Some(uyvy)
}

/// Bilinear resampling of tightly packed 8-bit four-channel pixels, sampling
/// at output pixel centres. The clean feed is for display only, not geometry.
fn resize_rgba8(pixels: &[u8], from: w::Extent, to: VideoMode) -> Option<Vec<u8>> {
    let (sw, sh) = (from.width as usize, from.height as usize);
    let (dw, dh) = (to.width as usize, to.height as usize);
    if sw == 0 || sh == 0 || pixels.len() != sw * sh * 4 {
        return None;
    }
    let taps = |dst: usize, src: usize| -> Vec<(usize, usize, f32)> {
        let scale = src as f32 / dst as f32;
        (0..dst)
            .map(|i| {
                let at = ((i as f32 + 0.5) * scale - 0.5).clamp(0.0, (src - 1) as f32);
                let low = at as usize;
                (low, (low + 1).min(src - 1), at - low as f32)
            })
            .collect()
    };
    let (columns, rows) = (taps(dw, sw), taps(dh, sh));
    let mut out = Vec::with_capacity(dw * dh * 4);
    for &(top, bottom, wy) in &rows {
        let (top, bottom) = (&pixels[top * sw * 4..], &pixels[bottom * sw * 4..]);
        for &(left, right, wx) in &columns {
            for channel in 0..4 {
                let sample = |row: &[u8], x: usize| f32::from(row[x * 4 + channel]);
                let upper = sample(top, left) + (sample(top, right) - sample(top, left)) * wx;
                let lower =
                    sample(bottom, left) + (sample(bottom, right) - sample(bottom, left)) * wx;
                out.push((upper + (lower - upper) * wy).round() as u8);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_feed_scales_to_the_output() {
        let from = w::Extent {
            width: 4,
            height: 2,
        };
        let mode = VideoMode {
            width: 2,
            height: 2,
            fps: 30,
        };
        // Columns 0..4 have red 0, 100, 200, 250; the output samples between pairs.
        let pixels: Vec<u8> = (0..2)
            .flat_map(|_| {
                [0_u8, 100, 200, 250]
                    .into_iter()
                    .flat_map(|r| [r, 0, 0, 255])
            })
            .collect();
        let resized = resize_rgba8(&pixels, from, mode).unwrap();
        assert_eq!(resized, [50, 0, 0, 255, 225, 0, 0, 255].repeat(2));
        assert_eq!(
            resize_rgba8(&pixels[4..], from, mode),
            None,
            "a short frame"
        );
        let uyvy = clean_uyvy(&pixels, from, ChannelOrder::Rgba, mode).unwrap();
        assert_eq!(uyvy.len(), 2 * 2 * 2);
    }
}

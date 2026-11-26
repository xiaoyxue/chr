//! The worker's side: `chr-renderer-cli worker` runs [`serve`] on the two sockets
//! that chr-server names on its command line.
//!
//! The worker holds the GPU and nothing authoritative. It installs the program
//! from the server's specification and runtime ID, so registrations and IDs match;
//! each request carries the bound scene snapshot and the frame, and its input
//! image is the request's payload, mapped from the server's memory without a
//! copy. Requests are handled in order.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::Arc,
    time::Duration,
};

use chr_common::scene_frame as w;
use chr_renderer::{
    Gpu, OffscreenTarget, TriangleRenderer, UyvyConverter, coordinates::ImageExtent,
};

use super::{
    protocol::{Output, Reply, Request, VERSION},
    transport::{self, Incoming, Receiver, Sender, SharedPixels},
};
use crate::{
    program::{Frame, ProgramRenderer, assets::Imported},
    retained::{BoundScene, SceneAdapter},
    sdi::VideoMode,
};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Test-only: delays every reply by this many milliseconds.
pub const DELAY_ENV: &str = "CHR_WORKER_TEST_DELAY_MS";
/// Test-only: answers every request with a solid UYVY frame of this luma
/// (0-255) instead of rendering, without a GPU device: a stand-in for fault and
/// timing tests that must not need, or compete for, the GPU (#154).
pub const SOLID_ENV: &str = "CHR_WORKER_TEST_SOLID";

/// How long an exiting worker waits for its last replies, such as `fatal`, to
/// reach the server.
const FINISH: Duration = Duration::from_secs(2);

/// Serves one server until its input stream ends: requests arrive on `input`,
/// replies leave on `output`. Startup failures are reported as `fatal` and returned.
pub fn serve(input: &Path, output: &Path) -> Result<(), Error> {
    let (requests, replies) = transport::worker_endpoints(input, output)?;
    let result = serve_on(&requests, &replies);
    replies.finish(FINISH);
    result
}

fn serve_on(requests: &Receiver, replies: &Sender) -> Result<(), Error> {
    if let Ok(luma) = std::env::var(SOLID_ENV) {
        return serve_solid(requests, replies, luma.parse()?);
    }
    let mut worker = match start(requests) {
        Ok(worker) => worker,
        Err(error) => {
            let _ = replies.send(
                &Reply::Fatal {
                    message: error.to_string(),
                },
                None,
            );
            return Err(error);
        }
    };
    replies.send(
        &Reply::Ready {
            adapter: worker.adapter.clone(),
        },
        None,
    )?;
    let delay = test_delay()?;
    while let Some(request) = requests.recv::<Request>()? {
        let (id, result) = worker.handle(&request)?;
        // The input image goes back to the server before the reply does.
        drop(request);
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
        match result {
            Ok(uyvy) => replies.send(&Reply::Done { id }, Some(uyvy.memory()))?,
            Err(error) => replies.send(
                &Reply::Failed {
                    id,
                    message: error.to_string(),
                },
                None,
            )?,
        }
    }
    Ok(())
}

fn test_delay() -> Result<Option<Duration>, Error> {
    Ok(std::env::var(DELAY_ENV)
        .ok()
        .map(|ms| ms.parse().map(Duration::from_millis))
        .transpose()?)
}

/// [`SOLID_ENV`]: every request is done with the same solid frame.
fn serve_solid(requests: &Receiver, replies: &Sender, luma: u8) -> Result<(), Error> {
    let Some(Incoming {
        message: Request::Hello {
            version, output, ..
        },
        ..
    }) = requests.recv()?
    else {
        return Err("the first message must be hello".into());
    };
    if version != VERSION {
        return Err(format!("protocol version {version} is not {VERSION}").into());
    }
    let frame = [128, luma, 128, luma].repeat(output.width as usize * output.height as usize / 2);
    replies.send(
        &Reply::Ready {
            adapter: format!("solid test frames of luma {luma}, no GPU"),
        },
        None,
    )?;
    let delay = test_delay()?;
    while let Some(request) = requests.recv::<Request>()? {
        let id = match request.message {
            Request::Render { id, .. } | Request::Triangle { id, .. } => id,
            Request::Hello { .. } => return Err("unexpected second hello".into()),
        };
        drop(request);
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
        let uyvy = SharedPixels::copy_from(&frame)?;
        replies.send(&Reply::Done { id }, Some(uyvy.memory()))?;
    }
    Ok(())
}

struct Worker {
    gpu: Gpu,
    adapter: String,
    renderer: Renderer,
    uyvy: Vec<u8>,
}

enum Renderer {
    Triangle(Box<Triangle>),
    Program(Box<Program>),
}

struct Triangle {
    target: OffscreenTarget,
    renderer: TriangleRenderer,
    converter: UyvyConverter,
}

struct Program {
    adapter: SceneAdapter,
    renderer: ProgramRenderer,
    /// The last bound scene; reused while requests carry the same snapshot.
    scene: Option<Arc<BoundScene>>,
    /// Media seconds of the first frame rendered with `scene`: its motion's time zero,
    /// so a newly loaded scene plays from its start (#157).
    started: f64,
    /// Each cued flag instance's cue count and the scene seconds its reveal started.
    cues: HashMap<w::Id, (u32, f64)>,
}

fn start(requests: &Receiver) -> Result<Worker, Error> {
    let Some(Incoming {
        message:
            Request::Hello {
                version,
                program,
                runtime_id,
                output,
                assets,
            },
        ..
    }) = requests.recv()?
    else {
        return Err("the first message must be hello".into());
    };
    if version != VERSION {
        return Err(format!("protocol version {version} is not {VERSION}").into());
    }
    let Output { width, height, fps } = output;
    let extent = ImageExtent::new(width, height)?;
    let gpu =
        pollster::block_on(Gpu::request(&Gpu::instance(), None)).map_err(|e| e.to_string())?;
    let info = gpu.adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu {
        return Err(format!("software GPU adapter {} is not accepted", info.name).into());
    }
    let adapter = format!("{} ({:?}, {:?})", info.name, info.device_type, info.backend);
    // A lost device cannot render again: exit, so the server replaces the worker.
    gpu.device.set_device_lost_callback(|reason, message| {
        if reason != wgpu::DeviceLostReason::Destroyed {
            log::error!("GPU device lost ({reason:?}): {message}");
            std::process::exit(1);
        }
    });
    log::info!("render worker GPU adapter: {adapter}");
    let renderer = match program.spec() {
        None => {
            let target = OffscreenTarget::new(&gpu, extent)?;
            let renderer = TriangleRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
            renderer.resize(&gpu.queue, extent);
            let converter = UyvyConverter::new(&gpu, &target)?;
            Renderer::Triangle(Box::new(Triangle {
                target,
                renderer,
                converter,
            }))
        }
        Some(spec) => {
            let mut installed = spec.install(VideoMode { width, height, fps }, runtime_id)?;
            for (id, asset) in crate::program::description::import_for_worker(&assets)? {
                match asset {
                    Imported::Model(glb) => installed.adapter.register_model(id, glb),
                    Imported::Environment(map) => installed.adapter.register_environment(id, map),
                }
                .map_err(|e| e.to_string())?;
            }
            let mut renderer = ProgramRenderer::new(&gpu, &installed.program.configuration)?;
            renderer.set_video_lens(installed.program.video_lens.clone());
            Renderer::Program(Box::new(Program {
                adapter: installed.adapter,
                renderer,
                scene: None,
                started: 0.0,
                cues: HashMap::new(),
            }))
        }
    };
    Ok(Worker {
        gpu,
        adapter,
        renderer,
        uyvy: Vec::new(),
    })
}

impl Worker {
    /// Handles one request and returns its UYVY frame in shared memory. Errors of
    /// the request are returned with its ID; an error of this function itself ends
    /// the worker.
    fn handle(
        &mut self,
        request: &Incoming<Request>,
    ) -> Result<(u64, Result<SharedPixels, Error>), Error> {
        let (id, result) = match &request.message {
            Request::Hello { .. } => return Err("unexpected second hello".into()),
            Request::Triangle { id, angle_rad, .. } => (*id, self.triangle(*angle_rad)),
            Request::Render {
                id,
                image,
                frame,
                scene,
                time_base,
                cues,
            } => (
                *id,
                self.render(request, image, frame, scene, *time_base, cues),
            ),
        };
        Ok((
            id,
            result.and_then(|()| SharedPixels::copy_from(&self.uyvy)),
        ))
    }

    fn triangle(&mut self, angle_rad: f64) -> Result<(), Error> {
        let Renderer::Triangle(triangle) = &self.renderer else {
            return Err("this worker renders a program, not the triangle".into());
        };
        let Triangle {
            target,
            renderer,
            converter,
        } = &**triangle;
        renderer.set_rotation(&self.gpu.queue, angle_rad);
        target.render(&self.gpu, |encoder, view| renderer.encode(encoder, view))?;
        converter.convert(&self.gpu, &mut self.uyvy)
    }

    fn render(
        &mut self,
        request: &Incoming<Request>,
        image: &w::ImageDescription,
        frame: &w::FrameInput,
        snapshot: &w::SceneSnapshot,
        time_base: w::TimeBase,
        cues: &BTreeMap<w::Id, u32>,
    ) -> Result<(), Error> {
        let Renderer::Program(program) = &mut self.renderer else {
            return Err("this worker renders the triangle, not a program".into());
        };
        let Program {
            adapter,
            renderer,
            scene,
            started,
            cues: started_cues,
        } = &mut **program;
        let seconds = crate::program::media_seconds(time_base, frame.position.ticks.get());
        let pixels = request.payload()?;
        if pixels.len() != image_bytes(image) {
            return Err("input image size does not match its description".into());
        }
        // A released and recreated scene restarts its revisions, so the whole
        // snapshot (including its runtime-wide effect) identifies the binding.
        let cached = scene
            .as_ref()
            .is_some_and(|bound| bound.snapshot() == snapshot);
        if !cached {
            *scene = Some(Arc::new(
                adapter.bind(snapshot.clone()).map_err(|e| e.to_string())?,
            ));
            *started = seconds;
            started_cues.clear();
        }
        let motion_seconds = seconds - *started;
        for (instance, &count) in cues {
            if started_cues.get(instance).map(|&(seen, _)| seen) != Some(count) {
                started_cues.insert(instance.clone(), (count, motion_seconds));
            }
        }
        let bound = scene.as_ref().expect("bound above");
        let camera = adapter
            .frame_camera(bound, &frame.geometry)
            .map_err(|e| e.to_string())?;
        self.uyvy = renderer.render_frame(
            &self.gpu,
            &Frame {
                scene: bound,
                camera: &camera,
                image,
                pixels: &pixels,
                key: &frame.frame,
                lens: crate::program::frame_lens(&frame.geometry)?,
                frame_warp: crate::program::frame_warp(&frame.geometry),
                seconds,
                motion_seconds,
                cues: started_cues
                    .iter()
                    .map(|(instance, &(_, start))| (instance.clone(), start))
                    .collect(),
            },
        )?;
        Ok(())
    }
}

/// Bytes of a tightly packed image as `image` describes it.
pub fn image_bytes(image: &w::ImageDescription) -> usize {
    let texel = match image.format {
        w::ImageFormat::Rgba8Unorm | w::ImageFormat::Bgra8Unorm => 4,
        w::ImageFormat::Rgba16Float => 8,
    };
    image.extent.width as usize * image.extent.height as usize * texel
}

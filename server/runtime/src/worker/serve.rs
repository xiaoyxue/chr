//! The worker's side: `chr-renderer-cli worker` runs [`serve`] on its stdin/stdout.
//!
//! The worker holds the GPU and nothing authoritative. It installs the program
//! from the server's specification and runtime ID, so registrations and IDs match;
//! each request carries the bound scene snapshot and the frame, and its input
//! image arrives through the input ring. Requests are handled in order.

use std::{
    io::{BufRead, Write},
    sync::Arc,
    time::Duration,
};

use chr_common::scene_frame as w;
use chr_renderer::{
    Gpu, OffscreenTarget, TriangleRenderer, UyvyConverter, coordinates::ImageExtent,
};
use chr_shm::{Layout, Read, Reader, SlotHeader, Writer};

use super::protocol::{self, Output, RING_SLOTS, Reply, Request, VERSION};
use crate::{
    program::{Frame, ProgramRenderer},
    retained::{BoundScene, SceneAdapter},
    sdi::VideoMode,
};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Test-only: delays every reply by this many milliseconds.
pub const DELAY_ENV: &str = "CHR_WORKER_TEST_DELAY_MS";

/// Serves one server until EOF on `input`. Startup failures are reported as
/// `fatal` and returned.
pub fn serve(mut input: impl BufRead, mut output: impl Write) -> Result<(), Error> {
    let mut worker = match start(&mut input) {
        Ok(worker) => worker,
        Err(error) => {
            let _ = protocol::write_line(
                &mut output,
                &Reply::Fatal {
                    message: error.to_string(),
                },
            );
            return Err(error);
        }
    };
    protocol::write_line(
        &mut output,
        &Reply::Ready {
            adapter: worker.adapter.clone(),
        },
    )?;
    let delay = std::env::var(DELAY_ENV)
        .ok()
        .map(|ms| ms.parse().map(Duration::from_millis))
        .transpose()?;
    while let Some(request) = protocol::read_line::<Request>(&mut input)? {
        let (id, result) = worker.handle(request)?;
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
        let reply = match result {
            Ok(()) => Reply::Done { id },
            Err(error) => Reply::Failed {
                id,
                message: error.to_string(),
            },
        };
        protocol::write_line(&mut output, &reply)?;
    }
    Ok(())
}

struct Worker {
    gpu: Gpu,
    adapter: String,
    renderer: Renderer,
    input: Option<Reader>,
    output: Writer,
    payload: Vec<u8>,
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
}

fn start(input: &mut impl BufRead) -> Result<Worker, Error> {
    let Some(Request::Hello {
        version,
        program,
        runtime_id,
        output,
        input_ring,
        output_ring,
        assets,
    }) = protocol::read_line(input)?
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
                installed
                    .adapter
                    .register_model(id, asset)
                    .map_err(|e| e.to_string())?;
            }
            let mut renderer = ProgramRenderer::new(&gpu, &installed.program.configuration)?;
            renderer.set_lenses(&installed.program);
            Renderer::Program(Box::new(Program {
                adapter: installed.adapter,
                renderer,
                scene: None,
            }))
        }
    };
    let input = match (&renderer, input_ring) {
        (Renderer::Program(_), Some(path)) => Some(Reader::open(&path)?),
        (Renderer::Program(_), None) => return Err("a program needs an input ring".into()),
        (Renderer::Triangle(_), _) => None,
    };
    let output = Writer::create(
        &output_ring,
        Layout {
            slot_count: RING_SLOTS,
            slot_bytes: u64::from(width) * u64::from(height) * 2,
        },
    )?;
    Ok(Worker {
        gpu,
        adapter,
        renderer,
        input,
        output,
        payload: Vec::new(),
        uyvy: Vec::new(),
    })
}

impl Worker {
    /// Handles one request. Errors of the request are returned with its ID; an
    /// error of this function itself ends the worker.
    fn handle(&mut self, request: Request) -> Result<(u64, Result<(), Error>), Error> {
        let (id, output_slot, result) = match request {
            Request::Hello { .. } => return Err("unexpected second hello".into()),
            Request::Triangle {
                id,
                output_slot,
                angle_rad,
                ..
            } => (id, output_slot, self.triangle(angle_rad)),
            Request::Render {
                id,
                input_slot,
                output_slot,
                image,
                frame,
                scene,
            } => (
                id,
                output_slot,
                self.render(id, input_slot, &image, &frame, *scene),
            ),
        };
        let result = result.and_then(|()| {
            let header = SlotHeader {
                index: id,
                meta: [0; 4],
            };
            Ok(self.output.write(output_slot, header, &self.uyvy)?)
        });
        Ok((id, result))
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
        id: u64,
        input_slot: u32,
        image: &w::ImageDescription,
        frame: &w::FrameInput,
        snapshot: w::SceneSnapshot,
    ) -> Result<(), Error> {
        let Renderer::Program(program) = &mut self.renderer else {
            return Err("this worker renders the triangle, not a program".into());
        };
        let Program {
            adapter,
            renderer,
            scene,
        } = &mut **program;
        let input = self.input.as_ref().ok_or("no input ring")?;
        match input.read(input_slot, &mut self.payload)? {
            Read::Frame(header) if header.index == id => {}
            other => {
                return Err(format!(
                    "input slot {input_slot} does not hold request {id}: {other:?}"
                )
                .into());
            }
        }
        if self.payload.len() != image_bytes(image) {
            return Err("input image size does not match its description".into());
        }
        // A released and recreated scene restarts its revisions, so the whole
        // snapshot (including its runtime-wide effect) identifies the binding.
        let cached = scene
            .as_ref()
            .is_some_and(|bound| *bound.snapshot() == snapshot);
        if !cached {
            *scene = Some(Arc::new(adapter.bind(snapshot).map_err(|e| e.to_string())?));
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
                pixels: &self.payload,
                key: &frame.frame,
                frame_warp: crate::program::frame_warp(&frame.geometry),
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

use std::{
    error::Error,
    sync::Arc,
    time::{Duration, Instant},
};

use chr_core::{Gpu, TriangleRenderer, coordinates::ImageExtent};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowId},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

mod capture;

struct WindowState {
    surface: wgpu::Surface<'static>,
    window: Arc<Window>,
    instance: wgpu::Instance,
    gpu: Gpu,
    config: wgpu::SurfaceConfiguration,
    renderer: TriangleRenderer,
    drawable: bool,
}

impl WindowState {
    async fn new(window: Arc<Window>) -> Result<Self> {
        let instance = Gpu::instance();
        let surface = instance.create_surface(window.clone())?;
        let gpu = Gpu::request(&instance, Some(&surface)).await?;
        capture::write_triangle_png(&gpu)?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .ok_or("adapter has no compatible surface configuration")?;
        if let Some(format) = surface
            .get_capabilities(&gpu.adapter)
            .formats
            .into_iter()
            .find(wgpu::TextureFormat::is_srgb)
        {
            config.format = format;
        }
        config.present_mode = wgpu::PresentMode::AutoVsync;
        let renderer = TriangleRenderer::new(&gpu.device, config.format);
        let mut state = Self {
            surface,
            window,
            instance,
            gpu,
            config,
            renderer,
            drawable: false,
        };
        state.resize(size);
        Ok(state)
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        let Ok(extent) = ImageExtent::new(size.width, size.height) else {
            self.drawable = false;
            return;
        };
        self.drawable = true;
        self.config.width = extent.width();
        self.config.height = extent.height();
        self.surface.configure(&self.gpu.device, &self.config);
        self.renderer.resize(&self.gpu.queue, extent);
        self.window.request_redraw();
    }

    fn render(&mut self) -> Result<bool> {
        if !self.drawable {
            return Ok(false);
        }
        let (frame, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.resize(self.window.inner_size());
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self.instance.create_surface(self.window.clone())?;
                self.resize(self.window.inner_size());
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("surface texture validation failed".into());
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        self.renderer.encode(&mut encoder, &view);
        self.gpu.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.gpu.queue.present(frame);
        if suboptimal {
            self.resize(self.window.inner_size());
        }
        Ok(true)
    }
}

#[derive(Default)]
struct App {
    state: Option<WindowState>,
    error: Option<Box<dyn Error>>,
    smoke_test: bool,
    presented: u32,
    smoke_deadline: Option<Instant>,
}

impl App {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: Box<dyn Error>) {
        self.error = Some(error);
        event_loop.exit();
    }
}

impl ApplicationHandler for App {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if self
            .smoke_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.fail(event_loop, "smoke test timed out".into());
            return;
        }
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            event_loop.set_control_flow(ControlFlow::Wait);
            if let Some(state) = &self.state {
                state.window.request_redraw();
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let result = (|| {
            let window = event_loop.create_window(
                Window::default_attributes()
                    .with_title("chr - Hello triangle")
                    .with_inner_size(LogicalSize::new(960, 640)),
            )?;
            pollster::block_on(WindowState::new(Arc::new(window)))
        })();
        match result {
            Ok(state) => self.state = Some(state),
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.state = None;
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut().filter(|s| s.window.id() == window_id) else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::Occluded(false) => state.window.request_redraw(),
            WindowEvent::RedrawRequested => match state.render() {
                Ok(presented) => {
                    if presented {
                        self.presented += 1;
                        if self.smoke_test && self.presented == 1 {
                            let _ = state.window.request_inner_size(PhysicalSize::new(640, 480));
                        }
                        if self.smoke_test && self.presented >= 3 {
                            log::info!("smoke test: presented three frames");
                            event_loop.exit();
                        }
                    }
                    if state.drawable || self.smoke_test {
                        event_loop.set_control_flow(ControlFlow::WaitUntil(
                            Instant::now() + Duration::from_millis(16),
                        ));
                    }
                }
                Err(error) => self.fail(event_loop, error),
            },
            _ => {}
        }
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut app = App::default();
    let mut headless = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--headless" => headless = true,
            "--smoke-test" => {
                app.smoke_test = true;
                app.smoke_deadline = Some(Instant::now() + Duration::from_secs(30));
            }
            "--help" | "-h" => {
                println!(
                    "Usage: chr-app [--headless | --smoke-test]\n\
                     Writes output/render/triangle.png under the checkout root\n\
                     (or the working directory outside a checkout). Escape closes the window."
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if headless {
        if app.smoke_test {
            return Err("--headless and --smoke-test are mutually exclusive".into());
        }
        let instance = Gpu::instance();
        let gpu = pollster::block_on(Gpu::request(&instance, None))?;
        return capture::write_triangle_png(&gpu);
    }
    EventLoop::new()?.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error);
    }
    if app.smoke_test && app.presented < 3 {
        return Err("smoke test closed before three frames were presented".into());
    }
    Ok(())
}

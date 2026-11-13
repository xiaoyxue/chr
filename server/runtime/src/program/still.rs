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

pub struct Still {
    pub spec: ProgramSpec,
    pub mode: VideoMode,
    /// Replaces the program's initial scene. Its `configuration_id` is replaced by
    /// the program's, which is only known after installation.
    pub state: Option<w::SceneState>,
    /// Native input image. Default: the program's synthetic input.
    pub background: Option<Background>,
    /// Input frame sequence; it selects the lens model of a calibrated video scene.
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
    program
        .inject_image(control, still.frame, &pixels)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODE: VideoMode = VideoMode {
        width: 64,
        height: 32,
        fps: 50,
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
            fps: 50,
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

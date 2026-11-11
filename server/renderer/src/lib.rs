//! Shared rendering independent of window ownership and operating system.

pub mod camera;
pub mod coordinates;
pub mod glb;
mod gpu;
mod material_maps;
mod mesh_renderer;
mod offscreen;
pub mod scene;
mod sdi;
mod shadow;
mod triangle;

pub use gpu::Gpu;
pub use mesh_renderer::{MeshRenderer, PbrSettings, ToneMap};
pub use offscreen::{OffscreenTarget, render_rotated_triangle_rgba, render_triangle_rgba};
pub use sdi::UyvyConverter;
pub use shadow::{CascadeSettings, PcssSettings, ShadowMode, ShadowSettings};
pub use triangle::TriangleRenderer;

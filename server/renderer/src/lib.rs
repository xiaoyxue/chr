//! Shared rendering independent of window ownership and operating system.

pub mod camera;
pub mod coordinates;
mod gpu;
mod offscreen;
pub mod scene;
mod sdi;
mod triangle;

pub use gpu::Gpu;
pub use offscreen::{OffscreenTarget, render_rotated_triangle_rgba, render_triangle_rgba};
pub use sdi::UyvyConverter;
pub use triangle::TriangleRenderer;

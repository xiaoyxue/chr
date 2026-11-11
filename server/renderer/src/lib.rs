//! Shared rendering independent of window ownership and operating system.

pub mod coordinates;
mod gpu;
mod offscreen;
mod sdi;
mod triangle;

pub use gpu::Gpu;
pub use offscreen::{OffscreenTarget, render_rotated_triangle_rgba, render_triangle_rgba};
pub use sdi::UyvyConverter;
pub use triangle::TriangleRenderer;

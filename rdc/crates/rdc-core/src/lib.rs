//! Shared rendering independent of window ownership and operating system.

pub mod coordinates;
mod gpu;
mod offscreen;
mod triangle;

pub use gpu::Gpu;
pub use offscreen::render_triangle_rgba;
pub use triangle::TriangleRenderer;

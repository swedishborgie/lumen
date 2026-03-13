//! lumen-compositor — Smithay-based Wayland compositor.
//!
//! Captures rendered frames and injects input events into client applications.
//! Communicates via Tokio channels so it can run on a dedicated OS thread
//! while the rest of the application uses an async runtime.

pub mod compositor;
pub mod handlers;
pub mod input;
pub mod render;
pub mod state;
pub mod types;

pub use compositor::Compositor;
pub use input::InputEvent;
pub use types::{CapturedFrame, CompositorConfig};

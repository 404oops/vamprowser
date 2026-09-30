// Compile the production policy against GPUI's public types without the
// upstream lib-test suite's unpublished font fixtures.
pub use gpui::{RequestFrameOptions, ThermalState};

#[path = "../src/window/frame_pacing.rs"]
mod frame_pacing;

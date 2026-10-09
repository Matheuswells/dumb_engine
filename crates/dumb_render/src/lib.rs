//! Vulkan renderer for Dumb Engine.
//!
//! * Vulkan 1.3 with dynamic rendering and synchronization2 (no render pass objects).
//! * WGSL shaders compiled to SPIR-V at build time by naga — no Vulkan SDK needed.
//! * Every 3D view (scene viewport, game view, model/material/animation viewers) renders into
//!   an offscreen target that egui displays as an image; the swapchain only receives egui.
//! * Forward PBR with GPU skinning, frustum culling, debug lines, wireframe.

mod assets;
pub mod extract;
mod pipelines;
mod renderer;
pub mod shadows;
mod types;
mod vk;

pub use extract::{camera_view, extract_world, find_primary_camera, push_model, screen_size, skeleton_lines, ExtractOptions};
pub use renderer::{EguiFrame, Renderer};
pub use types::*;

/// GPU resources whose destruction is deferred until in-flight frames finish.
pub(crate) enum Garbage {
    Model(assets::GpuModel),
    Image(vk::Image),
    #[allow(dead_code)]
    Buffer(vk::Buffer),
    Target(vk::Image, vk::Image, ash::vk::DescriptorSet),
    EguiTexture(Option<vk::Image>, ash::vk::DescriptorSet),
}

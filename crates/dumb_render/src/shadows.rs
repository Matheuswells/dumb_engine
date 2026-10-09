//! Cascaded shadow maps for the sun.
//!
//! The camera frustum (up to `SHADOW_DISTANCE`) is split into `CASCADES` slices; each slice gets
//! an orthographic light projection fitted around its bounding sphere and snapped to whole
//! shadow-map texels, so shadows don't shimmer when the camera moves. One depth array is shared
//! by all views: each view renders its cascades right before its own main pass.

use crate::vk::Context;
use ash::vk;
use dumb_core::{Mat4, Vec3, Vec4};
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};
use gpu_allocator::MemoryLocation;

pub const CASCADES: usize = 4;
pub const SHADOW_SIZE: u32 = 2048;
pub const SHADOW_FORMAT: vk::Format = vk::Format::D32_SFLOAT;
/// How far from the camera shadows are drawn (meters).
pub const SHADOW_DISTANCE: f32 = 160.0;

pub struct ShadowMap {
    pub image: vk::Image,
    alloc: Option<Allocation>,
    /// All cascades, for sampling.
    pub array_view: vk::ImageView,
    /// One view per cascade, for rendering.
    pub layer_views: Vec<vk::ImageView>,
    /// Comparison sampler (hardware PCF).
    pub sampler: vk::Sampler,
}

impl ShadowMap {
    pub fn new(ctx: &mut Context) -> Self {
        unsafe {
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(SHADOW_FORMAT)
                .extent(vk::Extent3D { width: SHADOW_SIZE, height: SHADOW_SIZE, depth: 1 })
                .mip_levels(1)
                .array_layers(CASCADES as u32)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT | vk::ImageUsageFlags::SAMPLED)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            let image = ctx.device.create_image(&info, None).expect("shadow image");
            let req = ctx.device.get_image_memory_requirements(image);
            let alloc = ctx
                .allocator
                .allocate(&AllocationCreateDesc {
                    name: "shadow cascades",
                    requirements: req,
                    location: MemoryLocation::GpuOnly,
                    linear: false,
                    allocation_scheme: AllocationScheme::GpuAllocatorManaged,
                })
                .expect("shadow memory");
            ctx.device.bind_image_memory(image, alloc.memory(), alloc.offset()).unwrap();
            let view = |ty: vk::ImageViewType, base: u32, count: u32| {
                ctx.device
                    .create_image_view(
                        &vk::ImageViewCreateInfo::default().image(image).view_type(ty).format(SHADOW_FORMAT).subresource_range(
                            vk::ImageSubresourceRange::default()
                                .aspect_mask(vk::ImageAspectFlags::DEPTH)
                                .base_mip_level(0)
                                .level_count(1)
                                .base_array_layer(base)
                                .layer_count(count),
                        ),
                        None,
                    )
                    .expect("shadow view")
            };
            let array_view = view(vk::ImageViewType::TYPE_2D_ARRAY, 0, CASCADES as u32);
            let layer_views = (0..CASCADES as u32).map(|i| view(vk::ImageViewType::TYPE_2D, i, 1)).collect();
            let sampler = ctx
                .device
                .create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::LINEAR)
                        .min_filter(vk::Filter::LINEAR)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_BORDER)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_BORDER)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_BORDER)
                        .border_color(vk::BorderColor::FLOAT_OPAQUE_WHITE)
                        .compare_enable(true)
                        .compare_op(vk::CompareOp::LESS_OR_EQUAL),
                    None,
                )
                .expect("shadow sampler");
            ShadowMap { image, alloc: Some(alloc), array_view, layer_views, sampler }
        }
    }

    pub fn destroy(&mut self, ctx: &mut Context) {
        unsafe {
            for v in self.layer_views.drain(..) {
                ctx.device.destroy_image_view(v, None);
            }
            ctx.device.destroy_image_view(self.array_view, None);
            ctx.device.destroy_sampler(self.sampler, None);
            ctx.device.destroy_image(self.image, None);
        }
        if let Some(a) = self.alloc.take() {
            let _ = ctx.allocator.free(a);
        }
    }
}

/// Light view-projection per cascade and the far distance of each split.
#[derive(Clone, Copy, Debug)]
pub struct Cascades {
    pub view_proj: [Mat4; CASCADES],
    /// View-space distance where each cascade ends.
    pub splits: [f32; CASCADES],
    /// World size of one shadow texel per cascade (for normal-offset bias).
    pub texel: [f32; CASCADES],
}

/// Fit cascades to the camera. `view`/`proj` are the camera matrices (Vulkan depth 0..1),
/// `light_dir` the direction light travels.
pub fn fit_cascades(view: Mat4, proj: Mat4, light_dir: Vec3, near: f32, far: f32) -> Cascades {
    let far = far.max(near + 1.0);
    // Practical split scheme: blend of logarithmic and uniform.
    let lambda = 0.75;
    let mut splits = [0.0; CASCADES];
    for (i, s) in splits.iter_mut().enumerate() {
        let p = (i + 1) as f32 / CASCADES as f32;
        let log = near * (far / near).powf(p);
        let uni = near + (far - near) * p;
        *s = lambda * log + (1.0 - lambda) * uni;
    }
    // Frustum corners in view space from the projection's field of view.
    let tan_y = 1.0 / proj.y_axis.y.abs();
    let tan_x = 1.0 / proj.x_axis.x.abs();
    let inv_view = view.inverse();
    let light_dir = light_dir.normalize_or(Vec3::NEG_Y);
    let up = if light_dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };

    let mut out = Cascades { view_proj: [Mat4::IDENTITY; CASCADES], splits, texel: [0.0; CASCADES] };
    let mut prev = near;
    for (i, &split) in splits.iter().enumerate() {
        let (n, f) = (prev, split);
        prev = split;
        // Bounding sphere of the slice (stable under camera rotation).
        let mut corners = Vec::with_capacity(8);
        for z in [n, f] {
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let p = Vec4::new(sx * tan_x * z, sy * tan_y * z, -z, 1.0);
                corners.push((inv_view * p).truncate());
            }
        }
        let center = corners.iter().fold(Vec3::ZERO, |a, c| a + *c) / 8.0;
        let radius = corners.iter().map(|c| c.distance(center)).fold(0.0f32, f32::max).max(0.5);
        let radius = (radius * 16.0).ceil() / 16.0;
        let texel = 2.0 * radius / SHADOW_SIZE as f32;

        // Snap the center to the texel grid in light space.
        let light_view = Mat4::look_to_rh(Vec3::ZERO, light_dir, up);
        let mut c = light_view.transform_point3(center);
        c.x = (c.x / texel).floor() * texel;
        c.y = (c.y / texel).floor() * texel;
        let center = light_view.inverse().transform_point3(c);

        // Pull the eye back so casters behind the slice (e.g. tall buildings) still cast.
        let back = radius + 200.0;
        let eye = center - light_dir * back;
        let lv = Mat4::look_to_rh(eye, light_dir, up);
        let lp = Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.1, back + radius);
        out.view_proj[i] = lp * lv;
        out.texel[i] = texel;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cascades_cover_the_frustum_and_grow() {
        let view = Mat4::look_at_rh(Vec3::new(0.0, 5.0, 10.0), Vec3::ZERO, Vec3::Y);
        let proj = Mat4::perspective_rh(60f32.to_radians(), 16.0 / 9.0, 0.1, 1000.0);
        let c = fit_cascades(view, proj, Vec3::new(-0.4, -1.0, -0.3), 0.1, 150.0);
        assert!(c.splits.windows(2).all(|w| w[0] < w[1]));
        assert!((c.splits[3] - 150.0).abs() < 1e-3);
        assert!(c.texel.windows(2).all(|w| w[0] < w[1]), "farther cascades cover more: {:?}", c.texel);
        // A point in front of the camera at 5 m lands inside cascade 0's clip volume.
        let p = Mat4::look_at_rh(Vec3::new(0.0, 5.0, 10.0), Vec3::ZERO, Vec3::Y).inverse().transform_point3(Vec3::new(0.0, 0.0, -5.0));
        let clip = c.view_proj[0] * p.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0 && (0.0..=1.0).contains(&ndc.z), "{ndc:?}");
    }

    #[test]
    fn snapping_keeps_texel_alignment_when_moving() {
        let proj = Mat4::perspective_rh(60f32.to_radians(), 1.0, 0.1, 1000.0);
        let a = fit_cascades(Mat4::look_at_rh(Vec3::new(0.0, 2.0, 0.0), Vec3::new(0.0, 2.0, -1.0), Vec3::Y), proj, Vec3::NEG_Y, 0.1, 100.0);
        let b = fit_cascades(Mat4::look_at_rh(Vec3::new(0.013, 2.0, 0.0), Vec3::new(0.013, 2.0, -1.0), Vec3::Y), proj, Vec3::NEG_Y, 0.1, 100.0);
        // The same world point maps to positions differing by whole texels.
        let p = Vec3::new(3.0, 0.0, -10.0).extend(1.0);
        let (pa, pb) = (a.view_proj[1] * p, b.view_proj[1] * p);
        let texels = (pa.x - pb.x) * 0.5 * SHADOW_SIZE as f32;
        assert!((texels - texels.round()).abs() < 1e-2, "moved by {texels} texels");
    }
}

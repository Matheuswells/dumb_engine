use crate::assets::{material_flags, GpuAssets};
use crate::shadows::{ShadowMap, CASCADES, SHADOW_SIZE};
use crate::pipelines::{MeshVariant, Pipelines, COLOR_FORMAT, DEPTH_FORMAT};
use crate::types::*;
use crate::vk::{barrier, range, vkerr, Buffer, Context, Image, VkResult};
use crate::Garbage;
use ash::vk;
use dumb_asset::{AlphaMode, AssetDatabase};
use gpu_allocator::MemoryLocation;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::collections::HashMap;

const FRAMES: usize = 2;
const VIEW_STRIDE: u64 = 1024;
const MAX_VIEWS: u64 = 32;
const MAX_JOINTS: u64 = 65536;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewUniforms {
    view_proj: [f32; 16],
    inv_view_proj: [f32; 16],
    camera_pos: [f32; 4],
    light_dir: [f32; 4],
    light_color: [f32; 4],
    ambient_sky: [f32; 4],
    ambient_ground: [f32; 4],
    params: [f32; 4],
    points: [[f32; 8]; 8],
    // Sun shadows.
    shadow_mats: [[f32; 16]; CASCADES],
    shadow_splits: [f32; 4],
    shadow_texel: [f32; 4],
    /// x = enabled, y = strength, z = cascade blend, w = unused.
    shadow_params: [f32; 4],
    /// Camera forward (for picking the cascade by view depth).
    cam_forward: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<ViewUniforms>() as u64 <= VIEW_STRIDE);

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
/// Per-batch material parameters (push constants).
struct MeshPush {
    albedo: [f32; 4],
    emission: [f32; 4],
    pbr: [f32; 4],
    /// uv tiling (xy), uv offset (zw).
    uv: [f32; 4],
    /// material flags, alpha cutoff bits, unused, unused.
    misc: [u32; 4],
}

const _: () = assert!(std::mem::size_of::<MeshPush>() == 80);

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
/// Per-instance data in the instance storage buffer, read with `instance_index`.
struct InstanceData {
    /// First three rows of the (affine) model matrix.
    model_rows: [[f32; 4]; 3],
    tint: [f32; 4],
    /// joint offset, instance flags (skinned, selected), unused, unused.
    misc: [u32; 4],
}

const INSTANCE_SIZE: u64 = std::mem::size_of::<InstanceData>() as u64;
const INITIAL_INSTANCES: u64 = 16384;

/// One instanced draw: consecutive draws of the same primitive, material and LOD.
struct Batch {
    /// Index into the view's `draws` of the first member (for model/material lookup).
    first_draw: usize,
    first_instance: u32,
    count: u32,
}

struct Frame {
    pool: vk::CommandPool,
    cb: vk::CommandBuffer,
    fence: vk::Fence,
    image_available: vk::Semaphore,
    view_ubo: Buffer,
    joints: Buffer,
    /// Per-instance transforms/tints for every view this frame (binding 4).
    instances: Buffer,
    /// Two timestamps (start, end of the command buffer) and the profiler frame they belong to.
    timestamps: vk::QueryPool,
    timed_frame: Option<u64>,
    set0: vk::DescriptorSet,
    egui_vb: Buffer,
    egui_ib: Buffer,
    line_vb: Buffer,
}

struct Swapchain {
    handle: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    render_finished: Vec<vk::Semaphore>,
    format: vk::Format,
    extent: vk::Extent2D,
}

pub struct RenderTarget {
    pub color: Image,
    pub depth: Image,
    pub egui_id: egui::TextureId,
    egui_set: vk::DescriptorSet,
}

struct EguiTexture {
    image: Option<Image>,
    set: vk::DescriptorSet,
}

/// The Vulkan renderer. Owns the device, swapchain, GPU asset caches and offscreen targets.
pub struct Renderer {
    ctx: Context,
    swapchain: Swapchain,
    frames: Vec<Frame>,
    frame_index: usize,
    frame_number: u64,
    pipelines: Pipelines,
    assets: GpuAssets,
    targets: HashMap<RenderTargetId, RenderTarget>,
    next_target: u32,
    egui_textures: HashMap<egui::TextureId, EguiTexture>,
    egui_pool: vk::DescriptorPool,
    egui_sampler: vk::Sampler,
    frame_pool: vk::DescriptorPool,
    garbage: Vec<(u64, Garbage)>,
    pending_garbage: Vec<Garbage>,
    pending_egui_free: Vec<egui::TextureId>,
    window_size: [u32; 2],
    needs_resize: bool,
    pub vsync: bool,
    pub stats: RenderStats,
    sky: Sky,
    shadow_map: ShadowMap,
    /// GPU time of the last measured frame (ms).
    pub gpu_ms: f32,
}

/// The built-in skybox: an equirectangular panorama with a full mip chain (for rough reflections).
struct Sky {
    image: Image,
    sampler: vk::Sampler,
    /// Average colour of the upper and lower halves (linear), used as hemisphere ambient light.
    ambient_up: [f32; 4],
    ambient_down: [f32; 4],
    mips: f32,
}

static SKY_PNG: &[u8] = include_bytes!("../assets/sky_97_2k.png");

fn srgb_to_linear(c: u8) -> f32 {
    (c as f32 / 255.0).powf(2.2)
}

fn load_sky(ctx: &mut Context) -> VkResult<Sky> {
    let tex = dumb_asset::TextureData::decode("sky", SKY_PNG)?;
    let (w, h) = (tex.width as usize, tex.height as usize);
    let average = |rows: std::ops::Range<usize>| {
        let mut sum = [0f64; 3];
        let mut n = 0f64;
        for y in rows.step_by(4) {
            for x in (0..w).step_by(4) {
                let p = &tex.rgba8[(y * w + x) * 4..];
                for c in 0..3 {
                    sum[c] += srgb_to_linear(p[c]) as f64;
                }
                n += 1.0;
            }
        }
        // A sky average is brighter than the light that actually bounces around; scale it down.
        let k = 0.7;
        [(sum[0] / n) as f32 * k, (sum[1] / n) as f32 * k, (sum[2] / n) as f32 * k, 1.0]
    };
    let ambient_up = average(0..h * 2 / 5);
    let ambient_down = average(h * 3 / 5..h);
    let image = ctx.create_texture_rgba8(tex.width, tex.height, &tex.rgba8, true, "skybox");
    let mips = image.mip_levels as f32 - 1.0;
    let sampler = unsafe {
        ctx.device
            .create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::REPEAT)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .max_lod(vk::LOD_CLAMP_NONE),
                None,
            )
            .map_err(vkerr("sky sampler"))?
    };
    Ok(Sky { image, sampler, ambient_up, ambient_down, mips })
}

/// egui output for one frame.
pub struct EguiFrame<'a> {
    pub primitives: &'a [egui::ClippedPrimitive],
    pub textures: &'a egui::TexturesDelta,
    pub pixels_per_point: f32,
}

impl Renderer {
    pub fn new(display: RawDisplayHandle, window: RawWindowHandle, size: [u32; 2]) -> VkResult<Self> {
        let mut ctx = Context::new(display, window)?;
        let swapchain = create_swapchain(&ctx, size, true, vk::SwapchainKHR::null())?;
        let assets = GpuAssets::new(&mut ctx);
        let sky = load_sky(&mut ctx)?;
        let shadow_map = ShadowMap::new(&mut ctx);
        let pipelines = Pipelines::new(&ctx, assets.material_layout, swapchain.format);
        unsafe {
            let frame_pool = ctx
                .device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default().max_sets(FRAMES as u32).pool_sizes(&[
                        vk::DescriptorPoolSize { ty: vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC, descriptor_count: FRAMES as u32 },
                        vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_BUFFER, descriptor_count: 2 * FRAMES as u32 },
                        vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLED_IMAGE, descriptor_count: 2 * FRAMES as u32 },
                        vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLER, descriptor_count: 2 * FRAMES as u32 },
                    ]),
                    None,
                )
                .map_err(vkerr("frame pool"))?;
            let egui_pool = ctx
                .device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(1024)
                        .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                        .pool_sizes(&[
                            vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLED_IMAGE, descriptor_count: 1024 },
                            vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLER, descriptor_count: 1024 },
                        ]),
                    None,
                )
                .map_err(vkerr("egui pool"))?;
            let egui_sampler = ctx
                .device
                .create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::LINEAR)
                        .min_filter(vk::Filter::LINEAR)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                    None,
                )
                .map_err(vkerr("sampler"))?;

            let mut frames = Vec::new();
            for _ in 0..FRAMES {
                let pool = ctx
                    .device
                    .create_command_pool(
                        &vk::CommandPoolCreateInfo::default()
                            .queue_family_index(ctx.queue_family)
                            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                        None,
                    )
                    .map_err(vkerr("command pool"))?;
                let cb = ctx
                    .device
                    .allocate_command_buffers(
                        &vk::CommandBufferAllocateInfo::default().command_pool(pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1),
                    )
                    .map_err(vkerr("command buffer"))?[0];
                let fence = ctx
                    .device
                    .create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None)
                    .map_err(vkerr("fence"))?;
                let image_available =
                    ctx.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(vkerr("semaphore"))?;
                let view_ubo = ctx.create_buffer(VIEW_STRIDE * MAX_VIEWS, vk::BufferUsageFlags::UNIFORM_BUFFER, MemoryLocation::CpuToGpu, "view ubo");
                let joints = ctx.create_buffer(64 * MAX_JOINTS, vk::BufferUsageFlags::STORAGE_BUFFER, MemoryLocation::CpuToGpu, "joints");
                let instances = ctx.create_buffer(INSTANCE_SIZE * INITIAL_INSTANCES, vk::BufferUsageFlags::STORAGE_BUFFER, MemoryLocation::CpuToGpu, "instances");
                let layouts = [pipelines.view_layout];
                let set0 = ctx
                    .device
                    .allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(frame_pool).set_layouts(&layouts))
                    .map_err(vkerr("frame set"))?[0];
                let ubo_info = [vk::DescriptorBufferInfo { buffer: view_ubo.buffer, offset: 0, range: std::mem::size_of::<ViewUniforms>() as u64 }];
                let joint_info = [vk::DescriptorBufferInfo { buffer: joints.buffer, offset: 0, range: vk::WHOLE_SIZE }];
                let instance_info = [vk::DescriptorBufferInfo { buffer: instances.buffer, offset: 0, range: vk::WHOLE_SIZE }];
                let sky_view = sky.image.srgb_view.unwrap_or(sky.image.view);
                let sky_img = [vk::DescriptorImageInfo::default().image_view(sky_view).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
                let sky_smp = [vk::DescriptorImageInfo::default().sampler(sky.sampler)];
                let shadow_img = [vk::DescriptorImageInfo::default().image_view(shadow_map.array_view).image_layout(vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL)];
                let shadow_smp = [vk::DescriptorImageInfo::default().sampler(shadow_map.sampler)];
                ctx.device.update_descriptor_sets(
                    &[
                        vk::WriteDescriptorSet::default()
                            .dst_set(set0)
                            .dst_binding(0)
                            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                            .buffer_info(&ubo_info),
                        vk::WriteDescriptorSet::default()
                            .dst_set(set0)
                            .dst_binding(1)
                            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                            .buffer_info(&joint_info),
                        vk::WriteDescriptorSet::default()
                            .dst_set(set0)
                            .dst_binding(4)
                            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                            .buffer_info(&instance_info),
                        vk::WriteDescriptorSet::default().dst_set(set0).dst_binding(2).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE).image_info(&sky_img),
                        vk::WriteDescriptorSet::default().dst_set(set0).dst_binding(3).descriptor_type(vk::DescriptorType::SAMPLER).image_info(&sky_smp),
                        vk::WriteDescriptorSet::default().dst_set(set0).dst_binding(5).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE).image_info(&shadow_img),
                        vk::WriteDescriptorSet::default().dst_set(set0).dst_binding(6).descriptor_type(vk::DescriptorType::SAMPLER).image_info(&shadow_smp),
                    ],
                    &[],
                );
                let egui_vb = ctx.create_buffer(1 << 20, vk::BufferUsageFlags::VERTEX_BUFFER, MemoryLocation::CpuToGpu, "egui vb");
                let egui_ib = ctx.create_buffer(1 << 20, vk::BufferUsageFlags::INDEX_BUFFER, MemoryLocation::CpuToGpu, "egui ib");
                let line_vb = ctx.create_buffer(1 << 20, vk::BufferUsageFlags::VERTEX_BUFFER, MemoryLocation::CpuToGpu, "line vb");
                let timestamps = ctx
                    .device
                    .create_query_pool(&vk::QueryPoolCreateInfo::default().query_type(vk::QueryType::TIMESTAMP).query_count(2), None)
                    .map_err(vkerr("query pool"))?;
                frames.push(Frame { pool, cb, fence, image_available, view_ubo, joints, instances, timestamps, timed_frame: None, set0, egui_vb, egui_ib, line_vb });
            }

            Ok(Renderer {
                ctx,
                swapchain,
                frames,
                frame_index: 0,
                frame_number: 0,
                pipelines,
                assets,
                targets: HashMap::new(),
                next_target: 1,
                egui_textures: HashMap::new(),
                egui_pool,
                egui_sampler,
                frame_pool,
                garbage: Vec::new(),
                pending_garbage: Vec::new(),
                pending_egui_free: Vec::new(),
                window_size: size,
                needs_resize: false,
                vsync: true,
                stats: RenderStats::default(),
                sky,
                shadow_map,
                gpu_ms: 0.0,
            })
        }
    }

    pub fn device_name(&self) -> &str {
        &self.ctx.device_name
    }

    pub fn wireframe_supported(&self) -> bool {
        self.ctx.wireframe_supported
    }

    pub fn resize(&mut self, size: [u32; 2]) {
        if size != self.window_size {
            self.window_size = size;
            self.needs_resize = true;
        }
    }

    pub fn set_vsync(&mut self, on: bool) {
        if on != self.vsync {
            self.vsync = on;
            self.needs_resize = true;
        }
    }

    fn recreate_swapchain(&mut self) {
        if self.window_size[0] == 0 || self.window_size[1] == 0 {
            return;
        }
        unsafe { self.ctx.device.device_wait_idle().ok() };
        let old = self.swapchain.handle;
        match create_swapchain(&self.ctx, self.window_size, self.vsync, old) {
            Ok(new) => {
                let old_sc = std::mem::replace(&mut self.swapchain, new);
                destroy_swapchain(&self.ctx, old_sc);
            }
            Err(e) => log::error!("swapchain recreation failed: {e}"),
        }
        self.needs_resize = false;
    }

    // ------------------------------------------------------------------ render targets

    pub fn create_target(&mut self, width: u32, height: u32) -> RenderTargetId {
        let id = RenderTargetId(self.next_target);
        self.next_target += 1;
        let t = self.make_target(width, height, egui::TextureId::User(id.0 as u64));
        self.targets.insert(id, t);
        id
    }

    fn make_target(&mut self, width: u32, height: u32, egui_id: egui::TextureId) -> RenderTarget {
        let extent = vk::Extent2D { width: width.max(1), height: height.max(1) };
        let color = self.ctx.create_image(
            extent,
            COLOR_FORMAT,
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC,
            1,
            false,
            "target color",
        );
        let depth = self.ctx.create_image(extent, DEPTH_FORMAT, vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT, 1, false, "target depth");
        // Put the color image in a readable layout so it can be shown before the first render.
        let (img, device) = (color.image, self.ctx.device.clone());
        self.ctx.immediate(|cb| {
            barrier(&device, cb, img, vk::ImageAspectFlags::COLOR, 0, 1, vk::ImageLayout::UNDEFINED, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        });
        let egui_set = self.alloc_egui_set(color.view);
        RenderTarget { color, depth, egui_id, egui_set }
    }

    /// Resize a target if its size changed. Cheap no-op otherwise.
    pub fn resize_target(&mut self, id: RenderTargetId, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        let Some(t) = self.targets.get(&id) else { return };
        if t.color.extent.width == width && t.color.extent.height == height {
            return;
        }
        let egui_id = t.egui_id;
        let new = self.make_target(width, height, egui_id);
        if let Some(old) = self.targets.insert(id, new) {
            self.pending_garbage.push(Garbage::Target(old.color, old.depth, old.egui_set));
        }
    }

    pub fn destroy_target(&mut self, id: RenderTargetId) {
        if let Some(old) = self.targets.remove(&id) {
            self.pending_garbage.push(Garbage::Target(old.color, old.depth, old.egui_set));
        }
    }

    pub fn target_texture(&self, id: RenderTargetId) -> Option<egui::TextureId> {
        self.targets.get(&id).map(|t| t.egui_id)
    }

    pub fn target_size(&self, id: RenderTargetId) -> Option<[u32; 2]> {
        self.targets.get(&id).map(|t| [t.color.extent.width, t.color.extent.height])
    }

    fn alloc_egui_set(&mut self, view: vk::ImageView) -> vk::DescriptorSet {
        unsafe {
            let layouts = [self.pipelines.egui_layout];
            let set = self
                .ctx
                .device
                .allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.egui_pool).set_layouts(&layouts))
                .expect("egui descriptor pool exhausted")[0];
            let img = [vk::DescriptorImageInfo::default().image_view(view).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let smp = [vk::DescriptorImageInfo::default().sampler(self.egui_sampler)];
            self.ctx.device.update_descriptor_sets(
                &[
                    vk::WriteDescriptorSet::default().dst_set(set).dst_binding(0).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE).image_info(&img),
                    vk::WriteDescriptorSet::default().dst_set(set).dst_binding(1).descriptor_type(vk::DescriptorType::SAMPLER).image_info(&smp),
                ],
                &[],
            );
            set
        }
    }

    /// Register an RGBA8 image (e.g. a texture preview) as an egui texture.
    pub fn register_egui_image(&mut self, id: egui::TextureId, width: u32, height: u32, rgba: &[u8]) {
        let image = self.ctx.create_texture_rgba8(width, height, rgba, false, "egui user image");
        let set = self.alloc_egui_set(image.view);
        if let Some(old) = self.egui_textures.insert(id, EguiTexture { image: Some(image), set }) {
            self.pending_garbage.push(Garbage::EguiTexture(old.image, old.set));
        }
    }

    pub fn free_egui_image(&mut self, id: egui::TextureId) {
        if let Some(old) = self.egui_textures.remove(&id) {
            self.pending_garbage.push(Garbage::EguiTexture(old.image, old.set));
        }
    }

    // ------------------------------------------------------------------ egui textures

    fn update_egui_textures(&mut self, delta: &egui::TexturesDelta) {
        for (id, d) in delta.set.iter().flat_map(|(id, ds)| ds.iter().map(move |d| (id, d))) {
            let egui::ImageData::Color(img) = &d.image;
            let [w, h] = img.size;
            let pixels: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            match (d.pos, self.egui_textures.get(id)) {
                (Some([x, y]), Some(EguiTexture { image: Some(existing), .. })) => {
                    // Borrow dance: copy the handle info we need.
                    let existing = Image {
                        image: existing.image,
                        view: existing.view,
                        srgb_view: None,
                        alloc: None,
                        extent: existing.extent,
                        format: existing.format,
                        mip_levels: 1,
                    };
                    self.ctx.update_texture_region(&existing, x as u32, y as u32, w as u32, h as u32, &pixels);
                }
                _ => {
                    let image = self.ctx.create_texture_rgba8(w as u32, h as u32, &pixels, false, "egui texture");
                    let set = self.alloc_egui_set(image.view);
                    if let Some(old) = self.egui_textures.insert(*id, EguiTexture { image: Some(image), set }) {
                        self.pending_garbage.push(Garbage::EguiTexture(old.image, old.set));
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------ frame

    fn collect_garbage(&mut self, all: bool) {
        let done = self.frame_number;
        let mut keep = Vec::new();
        for (frame, g) in std::mem::take(&mut self.garbage) {
            if all || frame + FRAMES as u64 <= done {
                self.destroy_garbage(g);
            } else {
                keep.push((frame, g));
            }
        }
        self.garbage = keep;
    }

    fn destroy_garbage(&mut self, g: Garbage) {
        match g {
            Garbage::Model(m) => {
                for p in m.meshes.into_iter().flatten() {
                    self.ctx.destroy_buffer(p.vertex);
                    self.ctx.destroy_buffer(p.index);
                }
            }
            Garbage::Image(i) => self.ctx.destroy_image(i),
            Garbage::Buffer(b) => self.ctx.destroy_buffer(b),
            Garbage::Target(c, d, set) => {
                self.ctx.destroy_image(c);
                self.ctx.destroy_image(d);
                unsafe { self.ctx.device.free_descriptor_sets(self.egui_pool, &[set]).ok() };
            }
            Garbage::EguiTexture(img, set) => {
                if let Some(i) = img {
                    self.ctx.destroy_image(i);
                }
                unsafe { self.ctx.device.free_descriptor_sets(self.egui_pool, &[set]).ok() };
            }
        }
    }

    /// Grow a per-frame host buffer if needed.
    fn ensure_capacity(&mut self, which: u8, needed: u64) {
        let f = &self.frames[self.frame_index];
        let cur = match which {
            0 => f.egui_vb.size,
            1 => f.egui_ib.size,
            _ => f.line_vb.size,
        };
        if needed <= cur {
            return;
        }
        let size = needed.next_power_of_two();
        let usage = if which == 1 { vk::BufferUsageFlags::INDEX_BUFFER } else { vk::BufferUsageFlags::VERTEX_BUFFER };
        let nb = self.ctx.create_buffer(size, usage, MemoryLocation::CpuToGpu, "dynamic");
        let f = &mut self.frames[self.frame_index];
        let old = match which {
            0 => std::mem::replace(&mut f.egui_vb, nb),
            1 => std::mem::replace(&mut f.egui_ib, nb),
            _ => std::mem::replace(&mut f.line_vb, nb),
        };
        // This frame's fence was waited on, so the old buffer is idle.
        self.ctx.destroy_buffer(old);
    }

    /// Grow this frame's instance buffer (and re-point its descriptor) to hold `count` instances.
    fn ensure_instances(&mut self, count: u64) {
        let needed = count * INSTANCE_SIZE;
        if needed <= self.frames[self.frame_index].instances.size {
            return;
        }
        let nb = self.ctx.create_buffer(needed.next_power_of_two(), vk::BufferUsageFlags::STORAGE_BUFFER, MemoryLocation::CpuToGpu, "instances");
        let f = &mut self.frames[self.frame_index];
        let info = [vk::DescriptorBufferInfo { buffer: nb.buffer, offset: 0, range: vk::WHOLE_SIZE }];
        unsafe {
            self.ctx.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default().dst_set(f.set0).dst_binding(4).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&info)],
                &[],
            );
        }
        let old = std::mem::replace(&mut f.instances, nb);
        // This frame's fence was waited on, so the old buffer is idle.
        self.ctx.destroy_buffer(old);
    }

    /// Render all views into their targets, then egui into the window, and present.
    pub fn draw_frame(&mut self, mut db: Option<&mut AssetDatabase>, views: &[RenderView], egui: Option<EguiFrame>) {
        // Texture deltas must be applied even if this frame is skipped.
        if let Some(e) = &egui {
            // Frees requested last frame happen now, after that frame was recorded.
            for id in std::mem::take(&mut self.pending_egui_free) {
                if let Some(old) = self.egui_textures.remove(&id) {
                    self.pending_garbage.push(Garbage::EguiTexture(old.image, old.set));
                }
            }
            self.update_egui_textures(e.textures);
            self.pending_egui_free.extend(e.textures.free.iter().copied());
        }
        if self.window_size[0] == 0 || self.window_size[1] == 0 {
            return;
        }
        if self.needs_resize {
            self.recreate_swapchain();
        }
        let fi = self.frame_index;
        let wait_scope = dumb_core::profiler::Scope::new("gpu wait (vsync)");
        unsafe {
            let fence = self.frames[fi].fence;
            self.ctx.device.wait_for_fences(&[fence], true, u64::MAX).ok();
        }
        // This frame slot's previous submission is done: read its GPU time.
        if let Some(idx) = self.frames[fi].timed_frame.take() {
            let mut ts = [0u64; 2];
            let ok = unsafe { self.ctx.device.get_query_pool_results(self.frames[fi].timestamps, 0, &mut ts, vk::QueryResultFlags::TYPE_64) };
            if ok.is_ok() && ts[1] > ts[0] {
                let ms = (ts[1] - ts[0]) as f64 * self.ctx.limits.timestamp_period as f64 / 1e6;
                self.gpu_ms = ms as f32;
                dumb_core::profiler::report_gpu(idx, ms as f32);
            }
        }
        self.collect_garbage(false);

        let (image_index, _suboptimal) = unsafe {
            match self.ctx.swapchain_fn.acquire_next_image(
                self.swapchain.handle,
                u64::MAX,
                self.frames[fi].image_available,
                vk::Fence::null(),
            ) {
                Ok(r) => r,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    self.needs_resize = true;
                    return;
                }
                Err(e) => {
                    log::error!("acquire failed: {e:?}");
                    return;
                }
            }
        };
        unsafe { self.ctx.device.reset_fences(&[self.frames[fi].fence]).ok() };
        drop(wait_scope);

        // ---- prepare assets (may upload) before recording
        let mut stats = RenderStats { views: views.len() as u32, ..Default::default() };
        let mut new_garbage = Vec::new();
        let mut material_sets: HashMap<dumb_core::AssetId, (vk::DescriptorSet, dumb_asset::MaterialData)> = HashMap::new();
        self.assets.begin_frame(crate::assets::UPLOAD_BUDGET);
        // Without a database (no project open) only egui is drawn.
        let prepare: &[RenderView] = if db.is_some() { views } else { &[] };
        for v in prepare {
            let db = db.as_deref_mut().unwrap();
            for d in &v.draws {
                self.assets.ensure_model(&mut self.ctx, db, d.model, &mut new_garbage);
                if let std::collections::hash_map::Entry::Vacant(e) = material_sets.entry(d.material) {
                    let m = self.assets.ensure_material(&mut self.ctx, db, d.material, self.frame_number, &mut new_garbage);
                    e.insert((m.set, m.data.clone()));
                }
            }
        }
        for g in new_garbage {
            self.pending_garbage.push(g);
        }
        stats.deferred_uploads = self.assets.deferred;

        // ---- upload per-frame data
        let mut joint_base = Vec::with_capacity(views.len());
        {
            let mut joints_written = 0u64;
            let frame = &mut self.frames[fi];
            for (vi, v) in views.iter().enumerate().take(MAX_VIEWS as usize) {
                let u = view_uniforms(v, &self.sky);
                frame.view_ubo.write(vi as u64 * VIEW_STRIDE, bytemuck::bytes_of(&u));
                let n = v.joints.len() as u64;
                if joints_written + n <= MAX_JOINTS {
                    let flat: Vec<f32> = v.joints.iter().flat_map(|m| m.to_cols_array()).collect();
                    frame.joints.write(joints_written * 64, bytemuck::cast_slice(&flat));
                    joint_base.push(Some(joints_written as u32));
                    joints_written += n;
                } else {
                    joint_base.push(None);
                }
            }
        }
        // ---- instance batches: sort each view's draws and merge runs that share a primitive,
        // material and LOD into one instanced draw.
        let mut instances: Vec<InstanceData> = Vec::new();
        let mut view_batches: Vec<Vec<Batch>> = Vec::with_capacity(views.len());
        let mut view_shadow_batches: Vec<[Vec<Batch>; CASCADES]> = Vec::with_capacity(views.len());
        for (vi, v) in views.iter().enumerate().take(MAX_VIEWS as usize) {
            let blended = |d: &DrawItem| material_sets.get(&d.material).is_some_and(|(_, m)| m.alpha_mode == AlphaMode::Blend);
            let mut order: Vec<(bool, usize)> = v.draws.iter().enumerate().map(|(i, d)| (blended(d), i)).collect();
            // Opaque first, grouped by material and mesh; blended last, back to front.
            order.sort_by(|&(ba, a), &(bb, b)| {
                let (da, db) = (&v.draws[a], &v.draws[b]);
                ba.cmp(&bb).then_with(|| {
                    if ba {
                        let za = (v.camera_pos - da.transform.w_axis.truncate()).length_squared();
                        let zb = (v.camera_pos - db.transform.w_axis.truncate()).length_squared();
                        zb.total_cmp(&za)
                    } else {
                        (da.material, da.model, da.mesh, da.primitive, da.lod).cmp(&(db.material, db.model, db.mesh, db.primitive, db.lod))
                    }
                })
            });
            let mut batches: Vec<Batch> = Vec::new();
            let mut prev: Option<usize> = None;
            for (_, i) in order {
                let d = &v.draws[i];
                instances.push(instance_of(d, joint_base[vi]));
                let same = prev.is_some_and(|p| {
                    let a = &v.draws[p];
                    a.model == d.model && a.mesh == d.mesh && a.primitive == d.primitive && a.material == d.material && a.lod == d.lod
                });
                match batches.last_mut() {
                    Some(b) if same => b.count += 1,
                    _ => batches.push(Batch { first_draw: i, first_instance: (instances.len() - 1) as u32, count: 1 }),
                }
                prev = Some(i);
            }
            view_batches.push(batches);

            // Shadow casters: opaque and cut-out draws with `cast_shadows`, culled per cascade
            // against the cascade's light frustum and grouped the same way as the main pass.
            let mut shadow_batches: [Vec<Batch>; CASCADES] = Default::default();
            if shadows_enabled(v) {
                let l = &v.lighting;
                let (z, w) = (v.proj.z_axis.z, v.proj.w_axis.z);
                let near = if z.abs() > 1e-6 { (w / z).abs().max(0.01) } else { 0.1 };
                let cascades = crate::shadows::fit_cascades(v.view, v.proj, l.sun_dir, near, crate::shadows::SHADOW_DISTANCE);
                let mut casters: Vec<usize> = (0..v.draws.len()).filter(|&i| v.draws[i].cast_shadows && !blended(&v.draws[i])).collect();
                casters.sort_by_key(|&i| {
                    let d = &v.draws[i];
                    (d.material, d.model, d.mesh, d.primitive, d.lod)
                });
                for (c, batches) in shadow_batches.iter_mut().enumerate() {
                    let m = cascades.view_proj[c];
                    let scale = |r: dumb_core::Vec4| dumb_core::Vec3::new(r.x, r.y, r.z).length();
                    let (sx, sy, sz) = (scale(m.row(0)), scale(m.row(1)), scale(m.row(2)));
                    let mut prev: Option<usize> = None;
                    for &i in &casters {
                        let d = &v.draws[i];
                        let s = d.sphere;
                        if s.w > 0.0 {
                            let p = m * s.truncate().extend(1.0);
                            let r = s.w;
                            // Orthographic: outside the slab in x/y, or past the far plane.
                            if p.x.abs() > 1.0 + r * sx || p.y.abs() > 1.0 + r * sy || p.z - r * sz > 1.0 || p.z + r * sz < 0.0 {
                                continue;
                            }
                        }
                        instances.push(instance_of(d, joint_base[vi]));
                        let same = prev.is_some_and(|p| {
                            let a = &v.draws[p];
                            a.model == d.model && a.mesh == d.mesh && a.primitive == d.primitive && a.material == d.material && a.lod == d.lod
                        });
                        match batches.last_mut() {
                            Some(b) if same => b.count += 1,
                            _ => batches.push(Batch { first_draw: i, first_instance: (instances.len() - 1) as u32, count: 1 }),
                        }
                        prev = Some(i);
                    }
                }
            }
            view_shadow_batches.push(shadow_batches);
        }
        self.ensure_instances(instances.len() as u64);
        self.frames[fi].instances.write(0, bytemuck::cast_slice(&instances));

        let total_lines: usize = views.iter().map(|v| v.lines.len() + v.overlay_lines.len()).sum();
        self.ensure_capacity(2, (total_lines * std::mem::size_of::<LineVertex>()) as u64);
        let mut line_ranges = Vec::new();
        {
            let frame = &mut self.frames[fi];
            let mut off = 0u64;
            for v in views {
                let a = off;
                frame.line_vb.write(off, bytemuck::cast_slice(&v.lines));
                off += std::mem::size_of_val(v.lines.as_slice()) as u64;
                let b = off;
                frame.line_vb.write(off, bytemuck::cast_slice(&v.overlay_lines));
                off += std::mem::size_of_val(v.overlay_lines.as_slice()) as u64;
                line_ranges.push((a, v.lines.len() as u32, b, v.overlay_lines.len() as u32));
            }
            stats.lines = (total_lines / 2) as u32;
        }

        // egui geometry
        let mut egui_draws = Vec::new();
        if let Some(e) = &egui {
            let (mut vcount, mut icount) = (0usize, 0usize);
            for p in e.primitives {
                if let egui::epaint::Primitive::Mesh(m) = &p.primitive {
                    vcount += m.vertices.len();
                    icount += m.indices.len();
                }
            }
            self.ensure_capacity(0, (vcount * 20) as u64);
            self.ensure_capacity(1, (icount * 4) as u64);
            let frame = &mut self.frames[fi];
            let (mut voff, mut ioff) = (0usize, 0usize);
            for p in e.primitives {
                if let egui::epaint::Primitive::Mesh(m) = &p.primitive {
                    if m.indices.is_empty() {
                        continue;
                    }
                    let vbytes = unsafe { std::slice::from_raw_parts(m.vertices.as_ptr() as *const u8, m.vertices.len() * 20) };
                    frame.egui_vb.write((voff * 20) as u64, vbytes);
                    frame.egui_ib.write((ioff * 4) as u64, bytemuck::cast_slice(&m.indices));
                    egui_draws.push((p.clip_rect, m.texture_id, ioff as u32, m.indices.len() as u32, voff as i32));
                    voff += m.vertices.len();
                    ioff += m.indices.len();
                }
            }
        }

        // ---- record
        let device = self.ctx.device.clone();
        let frame = &self.frames[fi];
        let cb = frame.cb;
        unsafe {
            device.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty()).ok();
            device.begin_command_buffer(cb, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)).ok();
            if self.ctx.timestamps {
                device.cmd_reset_query_pool(cb, frame.timestamps, 0, 2);
                device.cmd_write_timestamp(cb, vk::PipelineStageFlags::TOP_OF_PIPE, frame.timestamps, 0);
            }
            // Views without shadows still bind the map, so keep it in a readable layout.
            shadow_barrier(&device, cb, self.shadow_map.image, vk::ImageLayout::UNDEFINED, vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL);

            for (vi, v) in views.iter().enumerate().take(MAX_VIEWS as usize) {
                let Some(target) = self.targets.get(&v.target) else { continue };
                let extent = target.color.extent;
                let dyn_off = [vi as u32 * VIEW_STRIDE as u32];

                // ---- sun shadow cascades for this view (the map is shared, so it's redrawn
                // right before each view that uses it)
                if shadows_enabled(v) {
                    let sm = &self.shadow_map;
                    shadow_barrier(&device, cb, sm.image, vk::ImageLayout::UNDEFINED, vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL);
                    let ext = vk::Extent2D { width: SHADOW_SIZE, height: SHADOW_SIZE };
                    for (c, layer) in sm.layer_views.iter().enumerate() {
                        let depth = vk::RenderingAttachmentInfo::default()
                            .image_view(*layer)
                            .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                            .load_op(vk::AttachmentLoadOp::CLEAR)
                            .store_op(vk::AttachmentStoreOp::STORE)
                            .clear_value(vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 } });
                        device.cmd_begin_rendering(
                            cb,
                            &vk::RenderingInfo::default().render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent: ext }).layer_count(1).depth_attachment(&depth),
                        );
                        device.cmd_set_viewport(cb, 0, &[vk::Viewport { x: 0.0, y: 0.0, width: ext.width as f32, height: ext.height as f32, min_depth: 0.0, max_depth: 1.0 }]);
                        device.cmd_set_scissor(cb, 0, &[vk::Rect2D { offset: vk::Offset2D::default(), extent: ext }]);
                        device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, self.pipelines.shadow);
                        device.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::GRAPHICS, self.pipelines.scene_layout, 0, &[frame.set0], &dyn_off);
                        let mut bound_material = vk::DescriptorSet::null();
                        let mut bound_vertex = vk::Buffer::null();
                        for b in &view_shadow_batches[vi][c] {
                            let d = &v.draws[b.first_draw];
                            let Some(gm) = self.assets.models.get(&d.model) else { continue };
                            let Some(prim) = gm.meshes.get(d.mesh as usize).and_then(|m| m.get(d.primitive as usize)) else { continue };
                            let Some((set, mat)) = material_sets.get(&d.material) else { continue };
                            if *set != bound_material {
                                device.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::GRAPHICS, self.pipelines.scene_layout, 1, &[*set], &[]);
                                bound_material = *set;
                            }
                            let a = mat.albedo;
                            let push = MeshPush {
                                albedo: [a.r, a.g, a.b, a.a],
                                emission: [0.0; 4],
                                pbr: [0.0; 4],
                                uv: [mat.uv_tiling.x, mat.uv_tiling.y, mat.uv_offset.x, mat.uv_offset.y],
                                misc: [material_flags(mat), mat.alpha_cutoff.to_bits(), c as u32, 0],
                            };
                            device.cmd_push_constants(
                                cb,
                                self.pipelines.scene_layout,
                                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                                0,
                                bytemuck::bytes_of(&push),
                            );
                            if prim.vertex.buffer != bound_vertex {
                                device.cmd_bind_vertex_buffers(cb, 0, &[prim.vertex.buffer], &[0]);
                                device.cmd_bind_index_buffer(cb, prim.index.buffer, 0, vk::IndexType::UINT32);
                                bound_vertex = prim.vertex.buffer;
                            }
                            let (first, count) = prim.lods[(d.lod as usize).min(prim.lods.len() - 1)];
                            device.cmd_draw_indexed(cb, count, b.count, first, 0, b.first_instance);
                            stats.shadow_draws += 1;
                        }
                        device.cmd_end_rendering(cb);
                    }
                    shadow_barrier(&device, cb, sm.image, vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL, vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL);
                }

                barrier(&device, cb, target.color.image, vk::ImageAspectFlags::COLOR, 0, 1, vk::ImageLayout::UNDEFINED, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
                barrier(&device, cb, target.depth.image, vk::ImageAspectFlags::DEPTH, 0, 1, vk::ImageLayout::UNDEFINED, vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL);
                let c = v.clear_color;
                let color_att = [vk::RenderingAttachmentInfo::default()
                    .image_view(target.color.view)
                    .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                    .load_op(vk::AttachmentLoadOp::CLEAR)
                    .store_op(vk::AttachmentStoreOp::STORE)
                    .clear_value(vk::ClearValue { color: vk::ClearColorValue { float32: [c.r, c.g, c.b, 1.0] } })];
                let depth_att = vk::RenderingAttachmentInfo::default()
                    .image_view(target.depth.view)
                    .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                    .load_op(vk::AttachmentLoadOp::CLEAR)
                    .store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .clear_value(vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 } });
                device.cmd_begin_rendering(
                    cb,
                    &vk::RenderingInfo::default()
                        .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent })
                        .layer_count(1)
                        .color_attachments(&color_att)
                        .depth_attachment(&depth_att),
                );
                device.cmd_set_viewport(
                    cb,
                    0,
                    &[vk::Viewport { x: 0.0, y: 0.0, width: extent.width as f32, height: extent.height as f32, min_depth: 0.0, max_depth: 1.0 }],
                );
                device.cmd_set_scissor(cb, 0, &[vk::Rect2D { offset: vk::Offset2D::default(), extent }]);
                device.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::GRAPHICS, self.pipelines.scene_layout, 0, &[frame.set0], &dyn_off);
                if v.sky {
                    device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, self.pipelines.sky);
                    device.cmd_draw(cb, 3, 1, 0, 0);
                }

                // One instanced draw per batch (built before recording).
                let mut bound_pipeline = vk::Pipeline::null();
                let mut bound_material = vk::DescriptorSet::null();
                let mut bound_vertex = vk::Buffer::null();
                for b in &view_batches[vi] {
                    let d = &v.draws[b.first_draw];
                    let Some(gm) = self.assets.models.get(&d.model) else { continue };
                    let Some(prim) = gm.meshes.get(d.mesh as usize).and_then(|m| m.get(d.primitive as usize)) else { continue };
                    let Some((set, mat)) = material_sets.get(&d.material) else { continue };
                    let variant = MeshVariant {
                        double_sided: mat.double_sided,
                        wireframe: v.wireframe,
                        blend: mat.alpha_mode == AlphaMode::Blend,
                    };
                    let pipe = self.pipelines.mesh_pipeline(variant);
                    if pipe != bound_pipeline {
                        device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, pipe);
                        bound_pipeline = pipe;
                    }
                    if *set != bound_material {
                        device.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::GRAPHICS, self.pipelines.scene_layout, 1, &[*set], &[]);
                        bound_material = *set;
                    }
                    let a = mat.albedo;
                    let push = MeshPush {
                        albedo: [a.r, a.g, a.b, a.a],
                        emission: [mat.emission.r, mat.emission.g, mat.emission.b, mat.emission_strength],
                        pbr: [mat.metallic, mat.roughness, mat.normal_scale, mat.ao_strength],
                        uv: [mat.uv_tiling.x, mat.uv_tiling.y, mat.uv_offset.x, mat.uv_offset.y],
                        misc: [material_flags(mat), mat.alpha_cutoff.to_bits(), 0, 0],
                    };
                    device.cmd_push_constants(
                        cb,
                        self.pipelines.scene_layout,
                        vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                        0,
                        bytemuck::bytes_of(&push),
                    );
                    if prim.vertex.buffer != bound_vertex {
                        device.cmd_bind_vertex_buffers(cb, 0, &[prim.vertex.buffer], &[0]);
                        device.cmd_bind_index_buffer(cb, prim.index.buffer, 0, vk::IndexType::UINT32);
                        bound_vertex = prim.vertex.buffer;
                    }
                    let (first, count) = prim.lods[(d.lod as usize).min(prim.lods.len() - 1)];
                    device.cmd_draw_indexed(cb, count, b.count, first, 0, b.first_instance);
                    stats.draw_calls += 1;
                    stats.instances += b.count;
                    stats.triangles += (count / 3) as u64 * b.count as u64;
                }

                let (la, ln, lb, lon) = line_ranges[vi];
                if ln > 0 {
                    device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, self.pipelines.line);
                    device.cmd_bind_vertex_buffers(cb, 0, &[frame.line_vb.buffer], &[la]);
                    device.cmd_draw(cb, ln, 1, 0, 0);
                }
                if lon > 0 {
                    device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, self.pipelines.line_overlay);
                    device.cmd_bind_vertex_buffers(cb, 0, &[frame.line_vb.buffer], &[lb]);
                    device.cmd_draw(cb, lon, 1, 0, 0);
                }

                device.cmd_end_rendering(cb);
                barrier(&device, cb, target.color.image, vk::ImageAspectFlags::COLOR, 0, 1, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            }

            // ---- swapchain pass (egui)
            let sc_image = self.swapchain.images[image_index as usize];
            let sc_view = self.swapchain.views[image_index as usize];
            let extent = self.swapchain.extent;
            barrier(&device, cb, sc_image, vk::ImageAspectFlags::COLOR, 0, 1, vk::ImageLayout::UNDEFINED, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            let color_att = [vk::RenderingAttachmentInfo::default()
                .image_view(sc_view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue { color: vk::ClearColorValue { float32: [0.05, 0.05, 0.06, 1.0] } })];
            device.cmd_begin_rendering(
                cb,
                &vk::RenderingInfo::default()
                    .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent })
                    .layer_count(1)
                    .color_attachments(&color_att),
            );
            if let Some(e) = &egui {
                let ppp = e.pixels_per_point;
                device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, self.pipelines.egui);
                device.cmd_set_viewport(
                    cb,
                    0,
                    &[vk::Viewport { x: 0.0, y: 0.0, width: extent.width as f32, height: extent.height as f32, min_depth: 0.0, max_depth: 1.0 }],
                );
                device.cmd_bind_vertex_buffers(cb, 0, &[frame.egui_vb.buffer], &[0]);
                device.cmd_bind_index_buffer(cb, frame.egui_ib.buffer, 0, vk::IndexType::UINT32);
                let screen = [extent.width as f32 / ppp, extent.height as f32 / ppp];
                device.cmd_push_constants(cb, self.pipelines.egui_pipeline_layout, vk::ShaderStageFlags::VERTEX, 0, bytemuck::cast_slice(&screen));
                for (clip, tex, first, count, voff) in &egui_draws {
                    let set = match tex {
                        egui::TextureId::User(_) => self
                            .targets
                            .values()
                            .find(|t| t.egui_id == *tex)
                            .map(|t| t.egui_set)
                            .or_else(|| self.egui_textures.get(tex).map(|t| t.set)),
                        _ => self.egui_textures.get(tex).map(|t| t.set),
                    };
                    let Some(set) = set else { continue };
                    let x0 = (clip.min.x * ppp).round().clamp(0.0, extent.width as f32) as i32;
                    let y0 = (clip.min.y * ppp).round().clamp(0.0, extent.height as f32) as i32;
                    let x1 = (clip.max.x * ppp).round().clamp(0.0, extent.width as f32) as i32;
                    let y1 = (clip.max.y * ppp).round().clamp(0.0, extent.height as f32) as i32;
                    if x1 <= x0 || y1 <= y0 {
                        continue;
                    }
                    device.cmd_set_scissor(
                        cb,
                        0,
                        &[vk::Rect2D {
                            offset: vk::Offset2D { x: x0, y: y0 },
                            extent: vk::Extent2D { width: (x1 - x0) as u32, height: (y1 - y0) as u32 },
                        }],
                    );
                    device.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::GRAPHICS, self.pipelines.egui_pipeline_layout, 0, &[set], &[]);
                    device.cmd_draw_indexed(cb, *count, 1, *first, *voff, 0);
                }
            }
            device.cmd_end_rendering(cb);
            barrier(&device, cb, sc_image, vk::ImageAspectFlags::COLOR, 0, 1, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR);
            if self.ctx.timestamps {
                device.cmd_write_timestamp(cb, vk::PipelineStageFlags::BOTTOM_OF_PIPE, frame.timestamps, 1);
            }
            device.end_command_buffer(cb).ok();

            // ---- submit & present
            let wait = [vk::SemaphoreSubmitInfo::default()
                .semaphore(frame.image_available)
                .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)];
            let signal_sem = self.swapchain.render_finished[image_index as usize];
            let signal = [vk::SemaphoreSubmitInfo::default().semaphore(signal_sem).stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let cbs = [vk::CommandBufferSubmitInfo::default().command_buffer(cb)];
            let submit = vk::SubmitInfo2::default().wait_semaphore_infos(&wait).signal_semaphore_infos(&signal).command_buffer_infos(&cbs);
            if let Err(e) = device.queue_submit2(self.ctx.queue, &[submit], frame.fence) {
                log::error!("queue submit failed: {e:?}");
            }
            let swapchains = [self.swapchain.handle];
            let indices = [image_index];
            let wait_present = [signal_sem];
            match self.ctx.swapchain_fn.queue_present(
                self.ctx.queue,
                &vk::PresentInfoKHR::default().wait_semaphores(&wait_present).swapchains(&swapchains).image_indices(&indices),
            ) {
                Ok(false) => {}
                Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.needs_resize = true,
                Err(e) => log::error!("present failed: {e:?}"),
            }
        }

        for g in std::mem::take(&mut self.pending_garbage) {
            self.garbage.push((self.frame_number, g));
        }
        if self.ctx.timestamps {
            self.frames[fi].timed_frame = Some(dumb_core::profiler::current_frame());
        }
        stats.gpu_ms = self.gpu_ms;
        stats.gpu_models = self.assets.models.len() as u32;
        stats.gpu_textures = self.assets.textures.len() as u32;
        stats.gpu_materials = self.assets.materials.len() as u32;
        self.stats = stats;
        self.frame_number += 1;
        self.frame_index = (self.frame_index + 1) % FRAMES;
    }
}

fn view_uniforms(v: &RenderView, sky: &Sky) -> ViewUniforms {
    let l = &v.lighting;
    // Cascades from the camera near plane to the shadow distance.
    let (z, w) = (v.proj.z_axis.z, v.proj.w_axis.z);
    let near = if z.abs() > 1e-6 { (w / z).abs().max(0.01) } else { 0.1 };
    let shadows_on = shadows_enabled(v);
    let cascades = crate::shadows::fit_cascades(v.view, v.proj, l.sun_dir, near, crate::shadows::SHADOW_DISTANCE);
    let fwd = -v.view.inverse().z_axis.truncate().normalize_or(dumb_core::Vec3::NEG_Z);
    let mut points = [[0f32; 8]; 8];
    for (i, p) in l.points.iter().take(8).enumerate() {
        points[i] = [p.position.x, p.position.y, p.position.z, p.range, p.color.r, p.color.g, p.color.b, p.intensity];
    }
    ViewUniforms {
        view_proj: (v.proj * v.view).to_cols_array(),
        inv_view_proj: (v.proj * v.view).inverse().to_cols_array(),
        camera_pos: v.camera_pos.extend(1.0).to_array(),
        light_dir: l.sun_dir.normalize_or(dumb_core::Vec3::NEG_Y).extend(l.sun_intensity).to_array(),
        light_color: l.sun_color.to_array(),
        // With the skybox on, ambient light comes from the sky itself.
        ambient_sky: if v.sky { sky.ambient_up } else { l.ambient_sky.to_array() },
        ambient_ground: if v.sky { sky.ambient_down } else { l.ambient_ground.to_array() },
        params: [l.points.len().min(8) as f32, v.time, l.exposure, if v.sky { sky.mips } else { 0.0 }],
        points,
        shadow_mats: cascades.view_proj.map(|m| m.to_cols_array()),
        shadow_splits: cascades.splits,
        shadow_texel: cascades.texel,
        shadow_params: [if shadows_on { 1.0 } else { 0.0 }, 1.0, 0.0, 0.0],
        cam_forward: fwd.extend(0.0).to_array(),
    }
}

fn create_swapchain(ctx: &Context, size: [u32; 2], vsync: bool, old: vk::SwapchainKHR) -> VkResult<Swapchain> {
    unsafe {
        let caps = ctx
            .surface_fn
            .get_physical_device_surface_capabilities(ctx.physical, ctx.surface)
            .map_err(vkerr("surface caps"))?;
        let formats = ctx.surface_fn.get_physical_device_surface_formats(ctx.physical, ctx.surface).map_err(vkerr("surface formats"))?;
        let format = formats
            .iter()
            .find(|f| f.format == vk::Format::B8G8R8A8_UNORM)
            .or_else(|| formats.iter().find(|f| f.format == vk::Format::R8G8B8A8_UNORM))
            .copied()
            .unwrap_or(formats[0]);
        let modes = ctx.surface_fn.get_physical_device_surface_present_modes(ctx.physical, ctx.surface).unwrap_or_default();
        let present_mode = if vsync {
            vk::PresentModeKHR::FIFO
        } else if modes.contains(&vk::PresentModeKHR::MAILBOX) {
            vk::PresentModeKHR::MAILBOX
        } else if modes.contains(&vk::PresentModeKHR::IMMEDIATE) {
            vk::PresentModeKHR::IMMEDIATE
        } else {
            vk::PresentModeKHR::FIFO
        };
        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: size[0].clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: size[1].clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        let mut image_count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            image_count = image_count.min(caps.max_image_count);
        }
        let handle = ctx
            .swapchain_fn
            .create_swapchain(
                &vk::SwapchainCreateInfoKHR::default()
                    .surface(ctx.surface)
                    .min_image_count(image_count)
                    .image_format(format.format)
                    .image_color_space(format.color_space)
                    .image_extent(extent)
                    .image_array_layers(1)
                    .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                    .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .pre_transform(caps.current_transform)
                    .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                    .present_mode(present_mode)
                    .clipped(true)
                    .old_swapchain(old),
                None,
            )
            .map_err(vkerr("create swapchain"))?;
        let images = ctx.swapchain_fn.get_swapchain_images(handle).map_err(vkerr("swapchain images"))?;
        let views = images
            .iter()
            .map(|i| {
                ctx.device
                    .create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(*i)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(format.format)
                            .subresource_range(range(vk::ImageAspectFlags::COLOR, 1)),
                        None,
                    )
                    .unwrap()
            })
            .collect();
        let render_finished = images.iter().map(|_| ctx.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).unwrap()).collect();
        Ok(Swapchain { handle, images, views, render_finished, format: format.format, extent })
    }
}

fn destroy_swapchain(ctx: &Context, sc: Swapchain) {
    unsafe {
        for v in sc.views {
            ctx.device.destroy_image_view(v, None);
        }
        for s in sc.render_finished {
            ctx.device.destroy_semaphore(s, None);
        }
        ctx.swapchain_fn.destroy_swapchain(sc.handle, None);
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe { self.ctx.device.device_wait_idle().ok() };
        self.collect_garbage(true);
        for g in std::mem::take(&mut self.pending_garbage) {
            self.destroy_garbage(g);
        }
        for (_, t) in std::mem::take(&mut self.targets) {
            self.ctx.destroy_image(t.color);
            self.ctx.destroy_image(t.depth);
        }
        for (_, t) in std::mem::take(&mut self.egui_textures) {
            if let Some(i) = t.image {
                self.ctx.destroy_image(i);
            }
        }
        for f in std::mem::take(&mut self.frames) {
            unsafe {
                self.ctx.device.destroy_command_pool(f.pool, None);
                self.ctx.device.destroy_fence(f.fence, None);
                self.ctx.device.destroy_query_pool(f.timestamps, None);
                self.ctx.device.destroy_semaphore(f.image_available, None);
            }
            self.ctx.destroy_buffer(f.view_ubo);
            self.ctx.destroy_buffer(f.joints);
            self.ctx.destroy_buffer(f.instances);
            self.ctx.destroy_buffer(f.egui_vb);
            self.ctx.destroy_buffer(f.egui_ib);
            self.ctx.destroy_buffer(f.line_vb);
        }
        self.assets.destroy(&mut self.ctx);
        unsafe { self.ctx.device.destroy_sampler(self.sky.sampler, None) };
        let sky_img = std::mem::replace(
            &mut self.sky.image,
            Image { image: vk::Image::null(), view: vk::ImageView::null(), srgb_view: None, alloc: None, extent: vk::Extent2D::default(), format: vk::Format::UNDEFINED, mip_levels: 1 },
        );
        self.ctx.destroy_image(sky_img);
        self.shadow_map.destroy(&mut self.ctx);
        self.pipelines.destroy(&self.ctx);
        unsafe {
            self.ctx.device.destroy_descriptor_pool(self.frame_pool, None);
            self.ctx.device.destroy_descriptor_pool(self.egui_pool, None);
            self.ctx.device.destroy_sampler(self.egui_sampler, None);
        }
        let sc = std::mem::replace(
            &mut self.swapchain,
            Swapchain { handle: vk::SwapchainKHR::null(), images: vec![], views: vec![], render_finished: vec![], format: vk::Format::UNDEFINED, extent: vk::Extent2D::default() },
        );
        destroy_swapchain(&self.ctx, sc);
    }
}

/// Whether a view draws sun shadows (perspective views with a sun).
fn shadows_enabled(v: &RenderView) -> bool {
    v.shadows && v.lighting.sun_intensity > 0.0 && v.proj.w_axis.w == 0.0
}

/// Layout transition of every cascade of the shadow map.
fn shadow_barrier(device: &ash::Device, cb: vk::CommandBuffer, image: vk::Image, old: vk::ImageLayout, new: vk::ImageLayout) {
    let b = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
        .old_layout(old)
        .new_layout(new)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::DEPTH)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(CASCADES as u32),
        );
    let barriers = [b];
    unsafe { device.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().image_memory_barriers(&barriers)) };
}

fn instance_of(d: &DrawItem, joint_base: Option<u32>) -> InstanceData {
    let mut flags = 0u32;
    let mut joint_offset = 0;
    if let (Some(off), Some(base)) = (d.joint_offset, joint_base) {
        flags |= FLAG_SKINNED;
        joint_offset = base + off;
    }
    if d.selected {
        flags |= FLAG_SELECTED;
    }
    let t = &d.transform;
    InstanceData {
        model_rows: [t.row(0).to_array(), t.row(1).to_array(), t.row(2).to_array()],
        tint: [d.tint.r, d.tint.g, d.tint.b, d.tint.a],
        misc: [joint_offset, flags, 0, 0],
    }
}

impl Renderer {
    /// (VRAM used, VRAM budget) in bytes. "Used" covers every process when the driver reports
    /// it (VK_EXT_memory_budget), otherwise this engine's allocations.
    pub fn vram(&self) -> (u64, u64) {
        self.ctx.vram()
    }

    /// VRAM allocated by this engine.
    pub fn engine_vram(&self) -> u64 {
        self.ctx.engine_vram()
    }

    /// Copy a render target's last frame back to the CPU as tightly packed RGBA8
    /// (width, height, pixels). Waits for the GPU, so it is for tools, not per-frame use.
    pub fn read_target(&mut self, id: RenderTargetId) -> Option<(u32, u32, Vec<u8>)> {
        let (image, extent) = {
            let t = self.targets.get(&id)?;
            (t.color.image, t.color.extent)
        };
        debug_assert_eq!(COLOR_FORMAT, vk::Format::R8G8B8A8_UNORM);
        let size = extent.width as u64 * extent.height as u64 * 4;
        let buf = self.ctx.create_buffer(size, vk::BufferUsageFlags::TRANSFER_DST, MemoryLocation::GpuToCpu, "target readback");
        unsafe { self.ctx.device.device_wait_idle().ok() };
        let (device, dst) = (self.ctx.device.clone(), buf.buffer);
        self.ctx.immediate(|cb| {
            let color = vk::ImageAspectFlags::COLOR;
            barrier(&device, cb, image, color, 0, 1, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(color).mip_level(0).base_array_layer(0).layer_count(1))
                .image_extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 });
            unsafe { device.cmd_copy_image_to_buffer(cb, image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, dst, &[region]) };
            barrier(&device, cb, image, color, 0, 1, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        });
        let pixels = buf.alloc.as_ref().and_then(|a| a.mapped_slice()).map(|s| s[..size as usize].to_vec());
        self.ctx.destroy_buffer(buf);
        Some((extent.width, extent.height, pixels?))
    }
}

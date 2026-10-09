//! Vulkan device context and resource helpers.

use ash::vk;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme, Allocator, AllocatorCreateDesc};
use gpu_allocator::MemoryLocation;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::ffi::{c_char, CStr};
use std::mem::ManuallyDrop;

pub type VkResult<T> = Result<T, String>;

pub(crate) fn vkerr(ctx: &str) -> impl Fn(vk::Result) -> String + '_ {
    move |e| format!("{ctx}: {e:?}")
}

#[allow(dead_code)]
pub struct Context {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    pub surface_fn: ash::khr::surface::Instance,
    pub surface: vk::SurfaceKHR,
    pub physical: vk::PhysicalDevice,
    pub device: ash::Device,
    pub swapchain_fn: ash::khr::swapchain::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    /// VK_EXT_memory_budget is enabled (VRAM usage queries).
    pub memory_budget: bool,
    /// GPU timestamps work on the graphics queue.
    pub timestamps: bool,
    pub allocator: ManuallyDrop<Allocator>,
    pub upload_pool: vk::CommandPool,
    pub limits: vk::PhysicalDeviceLimits,
    pub wireframe_supported: bool,
    pub anisotropy: Option<f32>,
    pub device_name: String,
}

unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _ty: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut std::ffi::c_void,
) -> vk::Bool32 {
    let msg = if data.is_null() || (*data).p_message.is_null() {
        "<null>".into()
    } else {
        CStr::from_ptr((*data).p_message).to_string_lossy()
    };
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        log::error!("[vulkan] {msg}");
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        log::warn!("[vulkan] {msg}");
    } else {
        log::debug!("[vulkan] {msg}");
    }
    vk::FALSE
}

impl Context {
    pub fn new(display: RawDisplayHandle, window: RawWindowHandle) -> VkResult<Self> {
        unsafe {
            let entry = ash::Entry::load().map_err(|e| format!("Vulkan loader not found: {e}"))?;
            let app_name = c"Dumb Engine";
            let app = vk::ApplicationInfo::default()
                .application_name(app_name)
                .engine_name(app_name)
                .api_version(vk::API_VERSION_1_3);

            let mut exts: Vec<*const c_char> =
                ash_window::enumerate_required_extensions(display).map_err(vkerr("surface extensions"))?.to_vec();
            let available_layers = entry.enumerate_instance_layer_properties().unwrap_or_default();
            let available_exts = entry.enumerate_instance_extension_properties(None).unwrap_or_default();
            let has_ext = |n: &CStr| available_exts.iter().any(|e| e.extension_name_as_c_str() == Ok(n));
            let debug_utils = has_ext(ash::ext::debug_utils::NAME);
            if debug_utils {
                exts.push(ash::ext::debug_utils::NAME.as_ptr());
            }
            let validation = c"VK_LAYER_KHRONOS_validation";
            let want_validation = std::env::var("DUMB_VALIDATION").is_ok_and(|v| v != "0");
            let mut layers = Vec::new();
            if want_validation {
                if available_layers.iter().any(|l| l.layer_name_as_c_str() == Ok(validation)) {
                    layers.push(validation.as_ptr());
                } else {
                    log::warn!("DUMB_VALIDATION set but the validation layer is not installed");
                }
            }
            let instance = entry
                .create_instance(
                    &vk::InstanceCreateInfo::default()
                        .application_info(&app)
                        .enabled_extension_names(&exts)
                        .enabled_layer_names(&layers),
                    None,
                )
                .map_err(vkerr("create instance"))?;

            let debug = if debug_utils && want_validation {
                let loader = ash::ext::debug_utils::Instance::new(&entry, &instance);
                let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                    .message_severity(
                        vk::DebugUtilsMessageSeverityFlagsEXT::ERROR | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                    )
                    .message_type(
                        vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                            | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                            | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                    )
                    .pfn_user_callback(Some(debug_callback));
                loader.create_debug_utils_messenger(&info, None).ok().map(|m| (loader, m))
            } else {
                None
            };

            let surface = ash_window::create_surface(&entry, &instance, display, window, None)
                .map_err(vkerr("create surface"))?;
            let surface_fn = ash::khr::surface::Instance::new(&entry, &instance);

            // Pick a device: prefer discrete GPUs that can present and support Vulkan 1.3.
            let mut best: Option<(vk::PhysicalDevice, u32, i32)> = None;
            for pd in instance.enumerate_physical_devices().map_err(vkerr("enumerate devices"))? {
                let props = instance.get_physical_device_properties(pd);
                if props.api_version < vk::API_VERSION_1_3 {
                    continue;
                }
                let families = instance.get_physical_device_queue_family_properties(pd);
                for (i, f) in families.iter().enumerate() {
                    let present = surface_fn.get_physical_device_surface_support(pd, i as u32, surface).unwrap_or(false);
                    if f.queue_flags.contains(vk::QueueFlags::GRAPHICS) && present {
                        let score = match props.device_type {
                            vk::PhysicalDeviceType::DISCRETE_GPU => 3,
                            vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
                            _ => 1,
                        };
                        if best.is_none_or(|b| score > b.2) {
                            best = Some((pd, i as u32, score));
                        }
                        break;
                    }
                }
            }
            let (physical, queue_family, _) = best.ok_or("no Vulkan 1.3 GPU with presentation support found")?;
            let props = instance.get_physical_device_properties(physical);
            let device_name = props.device_name_as_c_str().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            log::info!("GPU: {device_name}");

            let supported = instance.get_physical_device_features(physical);
            let features = vk::PhysicalDeviceFeatures::default()
                .fill_mode_non_solid(supported.fill_mode_non_solid == vk::TRUE)
                .sampler_anisotropy(supported.sampler_anisotropy == vk::TRUE);
            let mut f13 = vk::PhysicalDeviceVulkan13Features::default().dynamic_rendering(true).synchronization2(true);
            let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
            let mut features2 = vk::PhysicalDeviceFeatures2::default().features(features).push_next(&mut f13).push_next(&mut f12);
            let priorities = [1.0];
            let queue_info = [vk::DeviceQueueCreateInfo::default().queue_family_index(queue_family).queue_priorities(&priorities)];
            let available: Vec<String> = instance
                .enumerate_device_extension_properties(physical)
                .unwrap_or_default()
                .iter()
                .filter_map(|e| e.extension_name_as_c_str().ok().map(|s| s.to_string_lossy().into_owned()))
                .collect();
            let memory_budget = available.iter().any(|e| e == "VK_EXT_memory_budget");
            let mut dev_exts = vec![ash::khr::swapchain::NAME.as_ptr()];
            if memory_budget {
                dev_exts.push(ash::ext::memory_budget::NAME.as_ptr());
            }
            let families = instance.get_physical_device_queue_family_properties(physical);
            let timestamps = families.get(queue_family as usize).is_some_and(|f| f.timestamp_valid_bits > 0) && props.limits.timestamp_period > 0.0;
            let device = instance
                .create_device(
                    physical,
                    &vk::DeviceCreateInfo::default()
                        .queue_create_infos(&queue_info)
                        .enabled_extension_names(&dev_exts)
                        .push_next(&mut features2),
                    None,
                )
                .map_err(vkerr("create device"))?;
            let queue = device.get_device_queue(queue_family, 0);
            let swapchain_fn = ash::khr::swapchain::Device::new(&instance, &device);

            let allocator = Allocator::new(&AllocatorCreateDesc {
                instance: instance.clone(),
                device: device.clone(),
                physical_device: physical,
                debug_settings: Default::default(),
                buffer_device_address: false,
                allocation_sizes: Default::default(),
            })
            .map_err(|e| format!("allocator: {e}"))?;

            let upload_pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(queue_family)
                        .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                    None,
                )
                .map_err(vkerr("command pool"))?;

            Ok(Context {
                entry,
                instance,
                debug,
                surface_fn,
                surface,
                physical,
                device,
                swapchain_fn,
                queue,
                queue_family,
                memory_budget,
                timestamps,
                allocator: ManuallyDrop::new(allocator),
                upload_pool,
                limits: props.limits,
                wireframe_supported: supported.fill_mode_non_solid == vk::TRUE,
                anisotropy: (supported.sampler_anisotropy == vk::TRUE).then_some(props.limits.max_sampler_anisotropy.min(8.0)),
                device_name,
            })
        }
    }

    /// Record and submit a one-off command buffer, waiting for completion.
    pub fn immediate(&self, f: impl FnOnce(vk::CommandBuffer)) {
        unsafe {
            let cb = self
                .device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.upload_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .expect("allocate upload command buffer")[0];
            self.device
                .begin_command_buffer(cb, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))
                .unwrap();
            f(cb);
            self.device.end_command_buffer(cb).unwrap();
            let cbs = [cb];
            self.device
                .queue_submit(self.queue, &[vk::SubmitInfo::default().command_buffers(&cbs)], vk::Fence::null())
                .unwrap();
            self.device.queue_wait_idle(self.queue).unwrap();
            self.device.free_command_buffers(self.upload_pool, &cbs);
        }
    }

    pub fn create_buffer(&mut self, size: u64, usage: vk::BufferUsageFlags, location: MemoryLocation, name: &str) -> Buffer {
        unsafe {
            let buffer = self
                .device
                .create_buffer(&vk::BufferCreateInfo::default().size(size.max(4)).usage(usage), None)
                .expect("create buffer");
            let req = self.device.get_buffer_memory_requirements(buffer);
            let alloc = self
                .allocator
                .allocate(&AllocationCreateDesc {
                    name,
                    requirements: req,
                    location,
                    linear: true,
                    allocation_scheme: AllocationScheme::GpuAllocatorManaged,
                })
                .expect("allocate buffer memory");
            self.device.bind_buffer_memory(buffer, alloc.memory(), alloc.offset()).unwrap();
            Buffer { buffer, alloc: Some(alloc), size }
        }
    }

    /// Device-local buffer filled through a staging copy.
    pub fn create_buffer_init(&mut self, data: &[u8], usage: vk::BufferUsageFlags, name: &str) -> Buffer {
        let dst = self.create_buffer(data.len() as u64, usage | vk::BufferUsageFlags::TRANSFER_DST, MemoryLocation::GpuOnly, name);
        let mut staging = self.create_buffer(data.len() as u64, vk::BufferUsageFlags::TRANSFER_SRC, MemoryLocation::CpuToGpu, "staging");
        staging.write(0, data);
        let (src_b, dst_b, size) = (staging.buffer, dst.buffer, data.len() as u64);
        self.immediate(|cb| unsafe {
            self.device.cmd_copy_buffer(cb, src_b, dst_b, &[vk::BufferCopy::default().size(size)]);
        });
        self.destroy_buffer(staging);
        dst
    }

    pub fn destroy_buffer(&mut self, mut b: Buffer) {
        unsafe { self.device.destroy_buffer(b.buffer, None) };
        if let Some(a) = b.alloc.take() {
            let _ = self.allocator.free(a);
        }
    }

    pub fn create_image(
        &mut self,
        extent: vk::Extent2D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
        mip_levels: u32,
        mutable_srgb: bool,
        name: &str,
    ) -> Image {
        unsafe {
            let list_formats = [format, srgb_of(format)];
            let mut fmt_list = vk::ImageFormatListCreateInfo::default().view_formats(&list_formats);
            let mut info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 })
                .mip_levels(mip_levels)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            if mutable_srgb {
                info = info.flags(vk::ImageCreateFlags::MUTABLE_FORMAT).push_next(&mut fmt_list);
            }
            let image = self.device.create_image(&info, None).expect("create image");
            let req = self.device.get_image_memory_requirements(image);
            let alloc = self
                .allocator
                .allocate(&AllocationCreateDesc {
                    name,
                    requirements: req,
                    location: MemoryLocation::GpuOnly,
                    linear: false,
                    allocation_scheme: AllocationScheme::GpuAllocatorManaged,
                })
                .expect("allocate image memory");
            self.device.bind_image_memory(image, alloc.memory(), alloc.offset()).unwrap();
            let aspect = if is_depth(format) { vk::ImageAspectFlags::DEPTH } else { vk::ImageAspectFlags::COLOR };
            let make_view = |f: vk::Format| {
                self.device
                    .create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(f)
                            .subresource_range(range(aspect, mip_levels)),
                        None,
                    )
                    .expect("create image view")
            };
            let view = make_view(format);
            let srgb_view = if mutable_srgb { Some(make_view(srgb_of(format))) } else { None };
            Image { image, view, srgb_view, alloc: Some(alloc), extent, format, mip_levels }
        }
    }

    pub fn destroy_image(&mut self, mut img: Image) {
        unsafe {
            self.device.destroy_image_view(img.view, None);
            if let Some(v) = img.srgb_view {
                self.device.destroy_image_view(v, None);
            }
            self.device.destroy_image(img.image, None);
        }
        if let Some(a) = img.alloc.take() {
            let _ = self.allocator.free(a);
        }
    }

    /// Upload RGBA8 pixels into a sampled image with a full mip chain.
    pub fn create_texture_rgba8(&mut self, width: u32, height: u32, pixels: &[u8], mips: bool, name: &str) -> Image {
        let levels = if mips { 32 - width.max(height).max(1).leading_zeros() } else { 1 };
        let extent = vk::Extent2D { width, height };
        let img = self.create_image(
            extent,
            vk::Format::R8G8B8A8_UNORM,
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC,
            levels,
            true,
            name,
        );
        let mut staging = self.create_buffer(pixels.len() as u64, vk::BufferUsageFlags::TRANSFER_SRC, MemoryLocation::CpuToGpu, "tex staging");
        staging.write(0, pixels);
        let (image, sbuf) = (img.image, staging.buffer);
        let device = self.device.clone();
        self.immediate(|cb| unsafe {
            barrier(&device, cb, image, vk::ImageAspectFlags::COLOR, 0, levels, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
            device.cmd_copy_buffer_to_image(
                cb,
                sbuf,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy::default()
                    .image_subresource(layers(0))
                    .image_extent(vk::Extent3D { width, height, depth: 1 })],
            );
            let (mut w, mut h) = (width as i32, height as i32);
            for level in 1..levels {
                barrier(&device, cb, image, vk::ImageAspectFlags::COLOR, level - 1, 1, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
                let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
                device.cmd_blit_image(
                    cb,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[vk::ImageBlit::default()
                        .src_subresource(layers(level - 1))
                        .src_offsets([vk::Offset3D::default(), vk::Offset3D { x: w, y: h, z: 1 }])
                        .dst_subresource(layers(level))
                        .dst_offsets([vk::Offset3D::default(), vk::Offset3D { x: nw, y: nh, z: 1 }])],
                    vk::Filter::LINEAR,
                );
                barrier(&device, cb, image, vk::ImageAspectFlags::COLOR, level - 1, 1, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                w = nw;
                h = nh;
            }
            barrier(&device, cb, image, vk::ImageAspectFlags::COLOR, levels - 1, 1, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        });
        self.destroy_buffer(staging);
        img
    }

    /// Overwrite a region of an existing RGBA8 texture (egui font atlas updates).
    pub fn update_texture_region(&mut self, img: &Image, x: u32, y: u32, w: u32, h: u32, pixels: &[u8]) {
        let mut staging = self.create_buffer(pixels.len() as u64, vk::BufferUsageFlags::TRANSFER_SRC, MemoryLocation::CpuToGpu, "tex staging");
        staging.write(0, pixels);
        let (image, sbuf) = (img.image, staging.buffer);
        let device = self.device.clone();
        unsafe { device.queue_wait_idle(self.queue).ok() };
        self.immediate(|cb| unsafe {
            barrier(&device, cb, image, vk::ImageAspectFlags::COLOR, 0, 1, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
            device.cmd_copy_buffer_to_image(
                cb,
                sbuf,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy::default()
                    .image_subresource(layers(0))
                    .image_offset(vk::Offset3D { x: x as i32, y: y as i32, z: 0 })
                    .image_extent(vk::Extent3D { width: w, height: h, depth: 1 })],
            );
            barrier(&device, cb, image, vk::ImageAspectFlags::COLOR, 0, 1, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        });
        self.destroy_buffer(staging);
    }

    pub fn shader_module(&self, spv: &[u8]) -> vk::ShaderModule {
        let words: Vec<u32> = spv.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        unsafe { self.device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None).expect("shader module") }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_command_pool(self.upload_pool, None);
            ManuallyDrop::drop(&mut self.allocator);
            self.device.destroy_device(None);
            self.surface_fn.destroy_surface(self.surface, None);
            if let Some((loader, m)) = self.debug.take() {
                loader.destroy_debug_utils_messenger(m, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

pub struct Buffer {
    pub buffer: vk::Buffer,
    pub alloc: Option<Allocation>,
    pub size: u64,
}

impl Buffer {
    /// Write into a host-visible buffer.
    pub fn write(&mut self, offset: u64, data: &[u8]) {
        let alloc = self.alloc.as_mut().expect("buffer freed");
        let slice = alloc.mapped_slice_mut().expect("buffer is not host visible");
        slice[offset as usize..offset as usize + data.len()].copy_from_slice(data);
    }
}

#[allow(dead_code)]
pub struct Image {
    pub image: vk::Image,
    pub view: vk::ImageView,
    pub srgb_view: Option<vk::ImageView>,
    pub alloc: Option<Allocation>,
    pub extent: vk::Extent2D,
    pub format: vk::Format,
    pub mip_levels: u32,
}

pub fn is_depth(f: vk::Format) -> bool {
    matches!(f, vk::Format::D32_SFLOAT | vk::Format::D24_UNORM_S8_UINT | vk::Format::D16_UNORM)
}

fn srgb_of(f: vk::Format) -> vk::Format {
    match f {
        vk::Format::R8G8B8A8_UNORM => vk::Format::R8G8B8A8_SRGB,
        vk::Format::B8G8R8A8_UNORM => vk::Format::B8G8R8A8_SRGB,
        f => f,
    }
}

pub fn range(aspect: vk::ImageAspectFlags, levels: u32) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default().aspect_mask(aspect).base_mip_level(0).level_count(levels).base_array_layer(0).layer_count(1)
}

fn layers(level: u32) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).mip_level(level).base_array_layer(0).layer_count(1)
}

/// Coarse image layout transition (ALL_COMMANDS scope — simple and correct, not maximally fast).
#[allow(clippy::too_many_arguments)]
pub fn barrier(
    device: &ash::Device,
    cb: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    base_level: u32,
    levels: u32,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
) {
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
                .aspect_mask(aspect)
                .base_mip_level(base_level)
                .level_count(levels)
                .base_array_layer(0)
                .layer_count(1),
        );
    let barriers = [b];
    unsafe { device.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().image_memory_barriers(&barriers)) };
}

impl Context {
    /// (bytes used, bytes budget) of device-local memory, all processes included when the
    /// driver reports it (VK_EXT_memory_budget); otherwise our own allocations and the heap size.
    pub fn vram(&self) -> (u64, u64) {
        unsafe {
            let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
            let mut props2 = vk::PhysicalDeviceMemoryProperties2::default();
            if self.memory_budget {
                props2 = props2.push_next(&mut budget);
            }
            self.instance.get_physical_device_memory_properties2(self.physical, &mut props2);
            let mp = props2.memory_properties;
            let mut used = 0;
            let mut total = 0;
            for i in 0..mp.memory_heap_count as usize {
                if mp.memory_heaps[i].flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL) {
                    total += if self.memory_budget { budget.heap_budget[i] } else { mp.memory_heaps[i].size };
                    used += budget.heap_usage[i];
                }
            }
            if !self.memory_budget {
                used = self.allocator.generate_report().total_reserved_bytes;
            }
            (used, total)
        }
    }

    /// Bytes allocated by this engine (gpu-allocator).
    pub fn engine_vram(&self) -> u64 {
        self.allocator.generate_report().total_reserved_bytes
    }
}

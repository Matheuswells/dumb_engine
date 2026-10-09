//! GPU-side caches of models, textures and materials, refreshed when asset versions change.

use crate::vk::{Buffer, Context, Image};
use crate::Garbage;
use ash::vk;
use dumb_asset::{AlphaMode, AssetDatabase, MaterialData};
use dumb_core::AssetId;
use std::collections::HashMap;

pub struct GpuPrimitive {
    pub vertex: Buffer,
    pub index: Buffer,
    /// (first index, index count) per LOD level inside `index` (level 0 = full detail).
    pub lods: Vec<(u32, u32)>,
}

pub struct GpuModel {
    pub version: u64,
    pub meshes: Vec<Vec<GpuPrimitive>>,
}

pub struct GpuTexture {
    pub version: u64,
    pub image: Image,
}

pub struct GpuMaterial {
    /// (material version, texture versions) when the set was written.
    pub key: (u64, [u64; 6]),
    pub data: MaterialData,
    pub set: vk::DescriptorSet,
    pub frame_used: u64,
}

pub struct GpuAssets {
    pub models: HashMap<AssetId, GpuModel>,
    pub textures: HashMap<AssetId, GpuTexture>,
    pub materials: HashMap<AssetId, GpuMaterial>,
    pub white: Image,
    pub flat_normal: Image,
    pub material_layout: vk::DescriptorSetLayout,
    pub pool: vk::DescriptorPool,
    pub sampler: vk::Sampler,
    /// Bytes that may still be uploaded this frame (see `begin_frame`).
    upload_left: u64,
    upload_full: u64,
    /// Uploads pushed to a later frame by the budget (this frame).
    pub deferred: u32,
}

/// GPU upload budget per frame. Big imports spread over frames instead of stalling one.
pub const UPLOAD_BUDGET: u64 = 48 << 20;

impl GpuAssets {
    pub fn new(ctx: &mut Context) -> Self {
        let white = ctx.create_texture_rgba8(1, 1, &[255, 255, 255, 255], false, "white");
        let flat_normal = ctx.create_texture_rgba8(1, 1, &[128, 128, 255, 255], false, "flat normal");
        unsafe {
            let mut bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..6)
                .map(|i| {
                    vk::DescriptorSetLayoutBinding::default()
                        .binding(i)
                        .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                        .descriptor_count(1)
                        .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                })
                .collect();
            bindings.push(
                vk::DescriptorSetLayoutBinding::default()
                    .binding(6)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            );
            let material_layout = ctx
                .device
                .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)
                .unwrap();
            let sizes = [
                vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLED_IMAGE, descriptor_count: 6 * 4096 },
                vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLER, descriptor_count: 4096 },
            ];
            let pool = ctx
                .device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(4096)
                        .pool_sizes(&sizes)
                        .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET),
                    None,
                )
                .unwrap();
            let mut sinfo = vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::REPEAT)
                .address_mode_v(vk::SamplerAddressMode::REPEAT)
                .address_mode_w(vk::SamplerAddressMode::REPEAT)
                .max_lod(vk::LOD_CLAMP_NONE);
            if let Some(a) = ctx.anisotropy {
                sinfo = sinfo.anisotropy_enable(true).max_anisotropy(a);
            }
            let sampler = ctx.device.create_sampler(&sinfo, None).unwrap();
            GpuAssets {
                models: HashMap::new(),
                textures: HashMap::new(),
                materials: HashMap::new(),
                white,
                flat_normal,
                material_layout,
                pool,
                sampler,
                upload_left: UPLOAD_BUDGET,
                upload_full: UPLOAD_BUDGET,
                deferred: 0,
            }
        }
    }

    /// Reset the per-frame upload budget.
    pub fn begin_frame(&mut self, budget: u64) {
        self.upload_left = budget;
        self.upload_full = budget;
        self.deferred = 0;
    }

    /// Take `bytes` from the budget. The first upload of a frame always fits, so one asset
    /// bigger than the budget still loads.
    fn take_budget(&mut self, bytes: u64) -> bool {
        if bytes > self.upload_left && self.upload_left < self.upload_full {
            self.deferred += 1;
            return false;
        }
        self.upload_left = self.upload_left.saturating_sub(bytes);
        true
    }

    /// Make sure the model is on the GPU and current. Returns false if not loaded yet.
    pub fn ensure_model(&mut self, ctx: &mut Context, db: &mut AssetDatabase, id: AssetId, garbage: &mut Vec<Garbage>) -> bool {
        let version = db.version(id);
        if let Some(m) = self.models.get(&id) {
            if m.version == version {
                return true;
            }
        }
        let Some(model) = db.model(id) else {
            return self.models.contains_key(&id);
        };
        let bytes: u64 = model.meshes.iter().flat_map(|m| &m.primitives).map(|p| (std::mem::size_of_val(p.vertices.as_slice()) + 4 * (p.indices.len() + p.lods.iter().map(Vec::len).sum::<usize>())) as u64).sum();
        if !self.take_budget(bytes) {
            // Keep drawing the previous version (if any) until there is room.
            return self.models.contains_key(&id);
        }
        let mut meshes = Vec::with_capacity(model.meshes.len());
        for mesh in &model.meshes {
            let mut prims = Vec::with_capacity(mesh.primitives.len());
            for p in &mesh.primitives {
                let vbytes = unsafe {
                    std::slice::from_raw_parts(p.vertices.as_ptr() as *const u8, std::mem::size_of_val(p.vertices.as_slice()))
                };
                let vertex = ctx.create_buffer_init(vbytes, vk::BufferUsageFlags::VERTEX_BUFFER, "vertices");
                // All LOD levels share one index buffer.
                let mut all: Vec<u32> = p.indices.clone();
                let mut lods = vec![(0u32, p.indices.len() as u32)];
                for l in &p.lods {
                    lods.push((all.len() as u32, l.len() as u32));
                    all.extend_from_slice(l);
                }
                let index = ctx.create_buffer_init(bytemuck::cast_slice(&all), vk::BufferUsageFlags::INDEX_BUFFER, "indices");
                prims.push(GpuPrimitive { vertex, index, lods });
            }
            meshes.push(prims);
        }
        if let Some(old) = self.models.insert(id, GpuModel { version, meshes }) {
            garbage.push(Garbage::Model(old));
        }
        true
    }

    fn ensure_texture(&mut self, ctx: &mut Context, db: &mut AssetDatabase, id: AssetId, garbage: &mut Vec<Garbage>) -> Option<u64> {
        if id.is_none() {
            return Some(0);
        }
        let version = db.version(id);
        if let Some(t) = self.textures.get(&id) {
            if t.version == version {
                return Some(version);
            }
        }
        let data = db.texture(id)?;
        if data.width == 0 || data.height == 0 {
            return None;
        }
        // RGBA8 plus mips.
        if !self.take_budget(data.width as u64 * data.height as u64 * 16 / 3) {
            return self.textures.get(&id).map(|t| t.version);
        }
        let image = ctx.create_texture_rgba8(data.width, data.height, &data.rgba8, true, &data.name);
        if let Some(old) = self.textures.insert(id, GpuTexture { version, image }) {
            garbage.push(Garbage::Image(old.image));
        }
        Some(version)
    }

    /// Resolve a material to a descriptor set + factors, uploading textures as needed.
    pub fn ensure_material(
        &mut self,
        ctx: &mut Context,
        db: &mut AssetDatabase,
        id: AssetId,
        frame: u64,
        garbage: &mut Vec<Garbage>,
    ) -> &GpuMaterial {
        let mat_version = if id.is_none() { 0 } else { db.version(id) };
        // Fast path: unchanged material whose textures are unchanged.
        let current = self.materials.get(&id).map(|m| m.key);
        let data = match self.materials.get(&id) {
            Some(m) if m.key.0 == mat_version => m.data.clone(),
            _ => {
                if id.is_none() {
                    MaterialData::default()
                } else {
                    db.material(id).unwrap_or_default()
                }
            }
        };
        let texs = data.textures();
        let mut tex_versions = [0u64; 6];
        for (i, t) in texs.iter().enumerate() {
            tex_versions[i] = self.ensure_texture(ctx, db, *t, garbage).unwrap_or(u64::MAX);
        }
        let key = (mat_version, tex_versions);
        if current != Some(key) {
            let views: Vec<vk::ImageView> = texs
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let srgb = i == 0 || i == 4;
                    match self.textures.get(t).filter(|_| tex_versions[i] != u64::MAX) {
                        Some(gt) if srgb => gt.image.srgb_view.unwrap_or(gt.image.view),
                        Some(gt) => gt.image.view,
                        None if i == 1 => self.flat_normal.view,
                        None => self.white.view,
                    }
                })
                .collect();
            let set = match self.materials.get(&id) {
                Some(m) => m.set,
                None => unsafe {
                    let layouts = [self.material_layout];
                    ctx.device
                        .allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.pool).set_layouts(&layouts))
                        .expect("material descriptor pool exhausted")[0]
                },
            };
            // The set may be in use by an in-flight frame; wait before rewriting it.
            if self.materials.contains_key(&id) {
                unsafe { ctx.device.queue_wait_idle(ctx.queue).ok() };
            }
            let infos: Vec<[vk::DescriptorImageInfo; 1]> = views
                .iter()
                .map(|v| [vk::DescriptorImageInfo::default().image_view(*v).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)])
                .collect();
            let sampler_info = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
            let mut writes: Vec<vk::WriteDescriptorSet> = infos
                .iter()
                .enumerate()
                .map(|(i, info)| {
                    vk::WriteDescriptorSet::default()
                        .dst_set(set)
                        .dst_binding(i as u32)
                        .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                        .image_info(info)
                })
                .collect();
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(6)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .image_info(&sampler_info),
            );
            unsafe { ctx.device.update_descriptor_sets(&writes, &[]) };
            self.materials.insert(id, GpuMaterial { key, data, set, frame_used: frame });
        }
        let m = self.materials.get_mut(&id).unwrap();
        m.frame_used = frame;
        m
    }

    pub fn destroy(&mut self, ctx: &mut Context) {
        for (_, m) in self.models.drain() {
            for p in m.meshes.into_iter().flatten() {
                ctx.destroy_buffer(p.vertex);
                ctx.destroy_buffer(p.index);
            }
        }
        for (_, t) in self.textures.drain() {
            ctx.destroy_image(t.image);
        }
        let white = std::mem::replace(&mut self.white, dummy_image());
        let flat = std::mem::replace(&mut self.flat_normal, dummy_image());
        ctx.destroy_image(white);
        ctx.destroy_image(flat);
        unsafe {
            ctx.device.destroy_descriptor_pool(self.pool, None);
            ctx.device.destroy_descriptor_set_layout(self.material_layout, None);
            ctx.device.destroy_sampler(self.sampler, None);
        }
    }
}

fn dummy_image() -> Image {
    Image {
        image: vk::Image::null(),
        view: vk::ImageView::null(),
        srgb_view: None,
        alloc: None,
        extent: vk::Extent2D::default(),
        format: vk::Format::UNDEFINED,
        mip_levels: 1,
    }
}

pub fn material_flags(m: &MaterialData) -> u32 {
    let mut f = 0;
    use crate::types::*;
    if m.alpha_mode == AlphaMode::Mask {
        f |= FLAG_ALPHA_MASK;
    }
    if m.unlit {
        f |= FLAG_UNLIT;
    }
    if m.normal_format == dumb_asset::NormalFormat::DirectX {
        f |= FLAG_NORMAL_DX;
    }
    match m.roughness_source {
        dumb_asset::RoughnessSource::Packed => {}
        dumb_asset::RoughnessSource::RoughnessMap => f |= FLAG_ROUGHNESS_MAP,
        dumb_asset::RoughnessSource::SmoothnessMap => f |= FLAG_SMOOTHNESS_MAP,
    }
    f
}

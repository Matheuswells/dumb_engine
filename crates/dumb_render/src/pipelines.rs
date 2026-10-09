use crate::vk::Context;
use ash::vk;

pub const COLOR_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;
pub const DEPTH_FORMAT: vk::Format = vk::Format::D32_SFLOAT;

static MESH_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/mesh.spv"));
static LINE_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/line.spv"));
static EGUI_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/egui.spv"));
static SKY_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sky.spv"));

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MeshVariant {
    pub double_sided: bool,
    pub wireframe: bool,
    pub blend: bool,
}

pub struct Pipelines {
    pub view_layout: vk::DescriptorSetLayout,
    pub egui_layout: vk::DescriptorSetLayout,
    pub scene_layout: vk::PipelineLayout,
    pub egui_pipeline_layout: vk::PipelineLayout,
    pub mesh: Vec<(MeshVariant, vk::Pipeline)>,
    pub line: vk::Pipeline,
    pub line_overlay: vk::Pipeline,
    pub sky: vk::Pipeline,
    /// Depth-only pass into a shadow cascade.
    pub shadow: vk::Pipeline,
    pub egui: vk::Pipeline,
    pub egui_format: vk::Format,
}

struct Desc<'a> {
    module: vk::ShaderModule,
    layout: vk::PipelineLayout,
    bindings: &'a [vk::VertexInputBindingDescription],
    attributes: &'a [vk::VertexInputAttributeDescription],
    topology: vk::PrimitiveTopology,
    cull: vk::CullModeFlags,
    polygon: vk::PolygonMode,
    depth_test: bool,
    depth_write: bool,
    blend: bool,
    /// `None` = depth-only (shadow maps).
    color_format: Option<vk::Format>,
    depth_format: Option<vk::Format>,
    vs: &'a std::ffi::CStr,
    fs: &'a std::ffi::CStr,
    depth_bias: bool,
}

fn build(ctx: &Context, d: &Desc) -> vk::Pipeline {
    let stages = [
        vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(d.module).name(d.vs),
        vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(d.module).name(d.fs),
    ];
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(d.bindings)
        .vertex_attribute_descriptions(d.attributes);
    let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(d.topology);
    let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
    let rs = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(d.polygon)
        .cull_mode(d.cull)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let rs = if d.depth_bias { rs.depth_bias_enable(true).depth_bias_constant_factor(1.5).depth_bias_slope_factor(2.0) } else { rs };
    let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let ds = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(d.depth_test)
        .depth_write_enable(d.depth_write)
        .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
    let mut att = vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA);
    if d.blend {
        att = att
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_DST_ALPHA)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD);
    }
    let atts = [att];
    let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(if d.color_format.is_some() { &atts } else { &[] });
    let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);
    let color_formats: Vec<vk::Format> = d.color_format.into_iter().collect();
    let mut rendering = vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&color_formats);
    if let Some(df) = d.depth_format {
        rendering = rendering.depth_attachment_format(df);
    }
    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&ia)
        .viewport_state(&vp)
        .rasterization_state(&rs)
        .multisample_state(&ms)
        .depth_stencil_state(&ds)
        .color_blend_state(&cb)
        .dynamic_state(&dynamic)
        .layout(d.layout)
        .push_next(&mut rendering);
    unsafe {
        ctx.device
            .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
            .map_err(|(_, e)| e)
            .expect("create pipeline")[0]
    }
}

fn attr(location: u32, format: vk::Format, offset: u32) -> vk::VertexInputAttributeDescription {
    vk::VertexInputAttributeDescription { location, binding: 0, format, offset }
}

impl Pipelines {
    pub fn new(ctx: &Context, material_layout: vk::DescriptorSetLayout, swapchain_format: vk::Format) -> Self {
        unsafe {
            let view_bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(2)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(3)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(4)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(5)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(6)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            ];
            let view_layout = ctx
                .device
                .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&view_bindings), None)
                .unwrap();
            let egui_bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            ];
            let egui_layout = ctx
                .device
                .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&egui_bindings), None)
                .unwrap();

            let scene_sets = [view_layout, material_layout];
            let scene_push = [vk::PushConstantRange {
                stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                offset: 0,
                size: 128,
            }];
            let scene_layout = ctx
                .device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&scene_sets).push_constant_ranges(&scene_push),
                    None,
                )
                .unwrap();
            let egui_sets = [egui_layout];
            let egui_push = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::VERTEX, offset: 0, size: 8 }];
            let egui_pipeline_layout = ctx
                .device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&egui_sets).push_constant_ranges(&egui_push),
                    None,
                )
                .unwrap();

            let mut p = Pipelines {
                view_layout,
                egui_layout,
                scene_layout,
                egui_pipeline_layout,
                mesh: Vec::new(),
                line: vk::Pipeline::null(),
                line_overlay: vk::Pipeline::null(),
                sky: vk::Pipeline::null(),
                shadow: vk::Pipeline::null(),
                egui: vk::Pipeline::null(),
                egui_format: swapchain_format,
            };
            p.build_all(ctx);
            p
        }
    }

    fn build_all(&mut self, ctx: &Context) {
        let mesh_mod = ctx.shader_module(MESH_SPV);
        let line_mod = ctx.shader_module(LINE_SPV);
        let egui_mod = ctx.shader_module(EGUI_SPV);
        let sky_mod = ctx.shader_module(SKY_SPV);
        self.sky = build(
            ctx,
            &Desc {
                module: sky_mod,
                layout: self.scene_layout,
                bindings: &[],
                attributes: &[],
                topology: vk::PrimitiveTopology::TRIANGLE_LIST,
                cull: vk::CullModeFlags::NONE,
                polygon: vk::PolygonMode::FILL,
                depth_test: false,
                depth_write: false,
                blend: false,
                color_format: Some(COLOR_FORMAT),
                depth_format: Some(DEPTH_FORMAT),
                vs: c"vs_main",
                fs: c"fs_main",
                depth_bias: false,
            },
        );

        let mesh_bindings = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<dumb_asset::Vertex>() as u32,
            input_rate: vk::VertexInputRate::VERTEX,
        }];
        let mesh_attrs = [
            attr(0, vk::Format::R32G32B32_SFLOAT, 0),
            attr(1, vk::Format::R32G32B32_SFLOAT, 12),
            attr(2, vk::Format::R32G32_SFLOAT, 24),
            attr(3, vk::Format::R32G32B32A32_SFLOAT, 32),
            attr(4, vk::Format::R16G16B16A16_UINT, 48),
            attr(5, vk::Format::R32G32B32A32_SFLOAT, 56),
        ];
        for double_sided in [false, true] {
            for wireframe in [false, true] {
                for blend in [false, true] {
                    if wireframe && !ctx.wireframe_supported {
                        continue;
                    }
                    let v = MeshVariant { double_sided, wireframe, blend };
                    let pipe = build(
                        ctx,
                        &Desc {
                            module: mesh_mod,
                            layout: self.scene_layout,
                            bindings: &mesh_bindings,
                            attributes: &mesh_attrs,
                            topology: vk::PrimitiveTopology::TRIANGLE_LIST,
                            cull: if double_sided || wireframe { vk::CullModeFlags::NONE } else { vk::CullModeFlags::BACK },
                            polygon: if wireframe { vk::PolygonMode::LINE } else { vk::PolygonMode::FILL },
                            depth_test: true,
                            depth_write: !blend,
                            blend,
                            color_format: Some(COLOR_FORMAT),
                            depth_format: Some(DEPTH_FORMAT),
                            vs: c"vs_main",
                            fs: c"fs_main",
                            depth_bias: false,
                        },
                    );
                    self.mesh.push((v, pipe));
                }
            }
        }

        self.shadow = build(
            ctx,
            &Desc {
                module: mesh_mod,
                layout: self.scene_layout,
                bindings: &mesh_bindings,
                attributes: &mesh_attrs,
                topology: vk::PrimitiveTopology::TRIANGLE_LIST,
                // Both faces cast: thin and open meshes still shadow.
                cull: vk::CullModeFlags::NONE,
                polygon: vk::PolygonMode::FILL,
                depth_test: true,
                depth_write: true,
                blend: false,
                color_format: None,
                depth_format: Some(crate::shadows::SHADOW_FORMAT),
                vs: c"vs_shadow",
                fs: c"fs_shadow",
                depth_bias: true,
            },
        );

        let line_bindings = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<crate::LineVertex>() as u32,
            input_rate: vk::VertexInputRate::VERTEX,
        }];
        let line_attrs = [attr(0, vk::Format::R32G32B32_SFLOAT, 0), attr(1, vk::Format::R32G32B32A32_SFLOAT, 12)];
        for overlay in [false, true] {
            let pipe = build(
                ctx,
                &Desc {
                    module: line_mod,
                    layout: self.scene_layout,
                    bindings: &line_bindings,
                    attributes: &line_attrs,
                    topology: vk::PrimitiveTopology::LINE_LIST,
                    cull: vk::CullModeFlags::NONE,
                    polygon: vk::PolygonMode::FILL,
                    depth_test: !overlay,
                    depth_write: false,
                    blend: true,
                    color_format: Some(COLOR_FORMAT),
                    depth_format: Some(DEPTH_FORMAT),
                    vs: c"vs_main",
                    fs: c"fs_main",
                    depth_bias: false,
                },
            );
            if overlay {
                self.line_overlay = pipe;
            } else {
                self.line = pipe;
            }
        }

        let egui_bindings = [vk::VertexInputBindingDescription { binding: 0, stride: 20, input_rate: vk::VertexInputRate::VERTEX }];
        let egui_attrs = [
            attr(0, vk::Format::R32G32_SFLOAT, 0),
            attr(1, vk::Format::R32G32_SFLOAT, 8),
            attr(2, vk::Format::R8G8B8A8_UNORM, 16),
        ];
        self.egui = build(
            ctx,
            &Desc {
                module: egui_mod,
                layout: self.egui_pipeline_layout,
                bindings: &egui_bindings,
                attributes: &egui_attrs,
                topology: vk::PrimitiveTopology::TRIANGLE_LIST,
                cull: vk::CullModeFlags::NONE,
                polygon: vk::PolygonMode::FILL,
                depth_test: false,
                depth_write: false,
                blend: true,
                color_format: Some(self.egui_format),
                depth_format: None,
                vs: c"vs_main",
                fs: c"fs_main",
                depth_bias: false,
            },
        );

        unsafe {
            ctx.device.destroy_shader_module(mesh_mod, None);
            ctx.device.destroy_shader_module(line_mod, None);
            ctx.device.destroy_shader_module(egui_mod, None);
            ctx.device.destroy_shader_module(sky_mod, None);
        }
    }

    pub fn mesh_pipeline(&self, v: MeshVariant) -> vk::Pipeline {
        let v = if v.wireframe && !self.mesh.iter().any(|(m, _)| m.wireframe) { MeshVariant { wireframe: false, ..v } } else { v };
        self.mesh.iter().find(|(m, _)| *m == v).map(|(_, p)| *p).unwrap_or(self.mesh[0].1)
    }

    pub fn destroy(&mut self, ctx: &Context) {
        unsafe {
            for (_, p) in self.mesh.drain(..) {
                ctx.device.destroy_pipeline(p, None);
            }
            ctx.device.destroy_pipeline(self.line, None);
            ctx.device.destroy_pipeline(self.line_overlay, None);
            ctx.device.destroy_pipeline(self.sky, None);
            ctx.device.destroy_pipeline(self.shadow, None);
            ctx.device.destroy_pipeline(self.egui, None);
            ctx.device.destroy_pipeline_layout(self.scene_layout, None);
            ctx.device.destroy_pipeline_layout(self.egui_pipeline_layout, None);
            ctx.device.destroy_descriptor_set_layout(self.view_layout, None);
            ctx.device.destroy_descriptor_set_layout(self.egui_layout, None);
        }
    }
}

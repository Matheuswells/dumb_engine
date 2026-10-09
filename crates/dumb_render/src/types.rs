use dumb_core::{AssetId, Color, Mat4, Vec3};

/// Handle to an offscreen render target (scene viewport, model viewer, material preview...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RenderTargetId(pub u32);

#[derive(Clone, Copy, Debug)]
pub struct PointLightData {
    pub position: Vec3,
    pub range: f32,
    pub color: Color,
    pub intensity: f32,
}

#[derive(Clone, Debug)]
pub struct Lighting {
    /// Direction the sun light travels.
    pub sun_dir: Vec3,
    pub sun_color: Color,
    pub sun_intensity: f32,
    pub ambient_sky: Color,
    pub ambient_ground: Color,
    pub points: Vec<PointLightData>,
    pub exposure: f32,
}

impl Default for Lighting {
    fn default() -> Self {
        Lighting {
            sun_dir: Vec3::new(-0.4, -1.0, -0.3).normalize(),
            sun_color: Color::rgb(1.0, 0.96, 0.9),
            sun_intensity: 3.0,
            ambient_sky: Color::rgb(0.35, 0.4, 0.5),
            ambient_ground: Color::rgb(0.12, 0.1, 0.09),
            points: Vec::new(),
            exposure: 1.0,
        }
    }
}

pub const FLAG_SKINNED: u32 = 1;
pub const FLAG_SELECTED: u32 = 2;
pub const FLAG_ALPHA_MASK: u32 = 4;
pub const FLAG_UNLIT: u32 = 8;
pub const FLAG_NORMAL_DX: u32 = 16;
pub const FLAG_ROUGHNESS_MAP: u32 = 32;
pub const FLAG_SMOOTHNESS_MAP: u32 = 64;

/// One primitive to draw.
#[derive(Clone, Copy, Debug)]
pub struct DrawItem {
    pub model: AssetId,
    pub mesh: u32,
    pub primitive: u32,
    /// Resolved material (override or the primitive's own). `NONE` = default material.
    pub material: AssetId,
    pub transform: Mat4,
    pub tint: Color,
    /// Offset into `RenderView::joints` when skinned.
    pub joint_offset: Option<u32>,
    pub selected: bool,
    /// LOD level (clamped to the levels the primitive has).
    pub lod: u8,
    /// Rendered into the sun's shadow map.
    pub cast_shadows: bool,
    /// World-space bounding sphere (center xyz, radius w) for shadow culling. Radius 0 = unknown.
    pub sphere: dumb_core::Vec4,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

/// Everything needed to render one camera into one target.
#[derive(Clone, Debug)]
pub struct RenderView {
    pub target: RenderTargetId,
    pub view: Mat4,
    pub proj: Mat4,
    pub camera_pos: Vec3,
    pub clear_color: Color,
    pub lighting: Lighting,
    pub draws: Vec<DrawItem>,
    pub joints: Vec<Mat4>,
    /// Depth-tested lines.
    pub lines: Vec<LineVertex>,
    /// Lines drawn on top of everything.
    pub overlay_lines: Vec<LineVertex>,
    pub wireframe: bool,
    pub time: f32,
    /// Draw the skybox behind the scene (and use it for ambient light and reflections).
    /// When false the view is cleared to `clear_color`.
    pub sky: bool,
    /// Force a LOD level for every model (model viewer preview). `None` = pick by screen size.
    pub lod_override: Option<u8>,
    /// Multiplies projected sizes before LOD selection (>1 keeps detail longer).
    pub lod_bias: f32,
    /// Sun shadows (cascaded shadow maps) for this view.
    pub shadows: bool,
}

impl RenderView {
    pub fn new(target: RenderTargetId) -> Self {
        RenderView {
            target,
            view: Mat4::IDENTITY,
            proj: Mat4::IDENTITY,
            camera_pos: Vec3::ZERO,
            clear_color: Color::rgb(0.08, 0.09, 0.11),
            lighting: Lighting::default(),
            draws: Vec::new(),
            joints: Vec::new(),
            lines: Vec::new(),
            overlay_lines: Vec::new(),
            wireframe: false,
            time: 0.0,
            sky: true,
            lod_override: None,
            lod_bias: 1.0,
            shadows: true,
        }
    }

    pub fn view_proj(&self) -> Mat4 {
        self.proj * self.view
    }

    pub fn line(&mut self, a: Vec3, b: Vec3, c: Color) {
        let color = c.to_array();
        self.lines.push(LineVertex { pos: a.to_array(), color });
        self.lines.push(LineVertex { pos: b.to_array(), color });
    }

    pub fn overlay_line(&mut self, a: Vec3, b: Vec3, c: Color) {
        let color = c.to_array();
        self.overlay_lines.push(LineVertex { pos: a.to_array(), color });
        self.overlay_lines.push(LineVertex { pos: b.to_array(), color });
    }

    pub fn aabb_lines(&mut self, b: &dumb_core::Aabb, m: &Mat4, c: Color) {
        let k = b.corners().map(|p| m.transform_point3(p));
        for (i, j) in [(0, 1), (1, 2), (2, 3), (3, 0), (4, 5), (5, 6), (6, 7), (7, 4), (0, 4), (1, 5), (2, 6), (3, 7)] {
            self.line(k[i], k[j], c);
        }
    }

    /// Ground grid on the XZ plane.
    pub fn grid(&mut self, half: i32, step: f32) {
        let ext = half as f32 * step;
        for i in -half..=half {
            let p = i as f32 * step;
            let (cx, cz) = if i == 0 {
                (Color::rgba(0.2, 0.35, 0.9, 0.9), Color::rgba(0.9, 0.25, 0.25, 0.9))
            } else if i % 10 == 0 {
                (Color::rgba(0.5, 0.5, 0.5, 0.55), Color::rgba(0.5, 0.5, 0.5, 0.55))
            } else {
                (Color::rgba(0.4, 0.4, 0.4, 0.22), Color::rgba(0.4, 0.4, 0.4, 0.22))
            };
            self.line(Vec3::new(p, 0.0, -ext), Vec3::new(p, 0.0, ext), cx);
            self.line(Vec3::new(-ext, 0.0, p), Vec3::new(ext, 0.0, p), cz);
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    pub views: u32,
    pub draw_calls: u32,
    /// Objects drawn (one draw call can draw many instances).
    pub instances: u32,
    pub triangles: u64,
    pub culled: u32,
    pub lines: u32,
    pub gpu_models: u32,
    pub gpu_textures: u32,
    pub gpu_materials: u32,
    /// Uploads postponed to later frames by the per-frame upload budget.
    pub deferred_uploads: u32,
    /// Instanced draws into shadow cascades.
    pub shadow_draws: u32,
    /// GPU time of the last measured frame (ms, from timestamps; 0 if unsupported).
    pub gpu_ms: f32,
}

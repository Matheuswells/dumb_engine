use dumb_core::{AssetId, Color, Vec2};
use dumb_derive::Editor;

/// Normal map convention: OpenGL (Y+, Blender/glTF) or DirectX (Y-, flips green).
#[derive(Editor, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NormalFormat {
    #[default]
    OpenGL,
    DirectX,
}

/// Where roughness (and metalness) come from.
#[derive(Editor, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RoughnessSource {
    /// glTF packed texture: G = roughness, B = metallic.
    #[default]
    Packed,
    /// Grayscale roughness map (red channel); metallic from the slider.
    RoughnessMap,
    /// Grayscale smoothness/gloss map (inverted roughness); metallic from the slider.
    SmoothnessMap,
}

#[derive(Editor, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlphaMode {
    #[default]
    Opaque,
    Mask,
    Blend,
}

/// PBR metallic-roughness material. Stored in `.mat` files as a reflected value tree, so the
/// material viewer edits it with the same inspector as components.
#[derive(Editor, Clone, Debug, PartialEq)]
pub struct MaterialData {
    #[editor(color)]
    pub albedo: Color,
    #[editor(asset = "texture")]
    pub albedo_texture: AssetId,
    #[editor(asset = "texture")]
    pub normal_texture: AssetId,
    #[editor(range = 0.0..=2.0)]
    pub normal_scale: f32,
    #[editor(range = 0.0..=1.0)]
    pub metallic: f32,
    #[editor(range = 0.0..=1.0)]
    pub roughness: f32,
    /// glTF layout: G = roughness, B = metallic.
    #[editor(asset = "texture")]
    pub metallic_roughness_texture: AssetId,
    /// Separate metallic map (red channel), used when the roughness source is not "packed".
    #[editor(asset = "texture")]
    pub metallic_texture: AssetId,
    #[editor(asset = "texture")]
    pub ao_texture: AssetId,
    #[editor(range = 0.0..=1.0)]
    pub ao_strength: f32,
    #[editor(color)]
    pub emission: Color,
    #[editor(range = 0.0..=50.0)]
    pub emission_strength: f32,
    #[editor(asset = "texture")]
    pub emission_texture: AssetId,
    pub alpha_mode: AlphaMode,
    #[editor(range = 0.0..=1.0)]
    pub alpha_cutoff: f32,
    pub double_sided: bool,
    pub normal_format: NormalFormat,
    pub roughness_source: RoughnessSource,
    /// Texture repeat count.
    #[editor(speed = 0.05)]
    pub uv_tiling: Vec2,
    #[editor(speed = 0.01)]
    pub uv_offset: Vec2,
    /// Ignore lighting: show albedo (+ emission) as is.
    pub unlit: bool,
}

impl Default for MaterialData {
    fn default() -> Self {
        MaterialData {
            albedo: Color::rgb(0.8, 0.8, 0.8),
            albedo_texture: AssetId::NONE,
            normal_texture: AssetId::NONE,
            normal_scale: 1.0,
            metallic: 0.0,
            roughness: 0.6,
            metallic_roughness_texture: AssetId::NONE,
            metallic_texture: AssetId::NONE,
            ao_texture: AssetId::NONE,
            ao_strength: 1.0,
            emission: Color::BLACK,
            emission_strength: 1.0,
            emission_texture: AssetId::NONE,
            alpha_mode: AlphaMode::Opaque,
            alpha_cutoff: 0.5,
            double_sided: false,
            normal_format: NormalFormat::OpenGL,
            roughness_source: RoughnessSource::Packed,
            uv_tiling: Vec2::ONE,
            uv_offset: Vec2::ZERO,
            unlit: false,
        }
    }
}

impl MaterialData {
    /// Texture slots in shader binding order.
    pub fn textures(&self) -> [AssetId; 6] {
        [
            self.albedo_texture,
            self.normal_texture,
            self.metallic_roughness_texture,
            self.ao_texture,
            self.emission_texture,
            self.metallic_texture,
        ]
    }

    pub fn to_ron(&self) -> String {
        let v = dumb_reflect::to_value(self);
        ron::ser::to_string_pretty(&v, ron::ser::PrettyConfig::default()).unwrap_or_default()
    }

    pub fn from_ron(s: &str) -> Result<Self, String> {
        let v: dumb_reflect::Value = ron::from_str(s).map_err(|e| e.to_string())?;
        let mut m = MaterialData::default();
        dumb_reflect::apply(&mut m, &v);
        Ok(m)
    }
}

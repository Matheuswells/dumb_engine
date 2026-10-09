/// Decoded RGBA8 texture.
#[derive(Clone, Debug, Default)]
pub struct TextureData {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

impl TextureData {
    pub fn solid(name: &str, rgba: [u8; 4]) -> Self {
        TextureData { name: name.into(), width: 1, height: 1, rgba8: rgba.to_vec() }
    }

    /// Decode an in-memory image (PNG, JPEG, ...).
    pub fn decode(name: &str, bytes: &[u8]) -> Result<Self, String> {
        let img = image::load_from_memory(bytes).map_err(|e| format!("{name}: {e}"))?;
        let rgba = img.to_rgba8();
        Ok(TextureData { name: name.into(), width: rgba.width(), height: rgba.height(), rgba8: rgba.into_raw() })
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let img = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let rgba = img.to_rgba8();
        Ok(TextureData {
            name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            width: rgba.width(),
            height: rgba.height(),
            rgba8: rgba.into_raw(),
        })
    }

    /// Convert decoded glTF image data into RGBA8.
    pub fn from_gltf(name: String, img: &gltf::image::Data) -> Self {
        use gltf::image::Format;
        let px = (img.width * img.height) as usize;
        let mut out = Vec::with_capacity(px * 4);
        let p = &img.pixels;
        let u16_at = |i: usize| (u16::from_le_bytes([p[i * 2], p[i * 2 + 1]]) >> 8) as u8;
        let f32_at = |i: usize| {
            let f = f32::from_le_bytes([p[i * 4], p[i * 4 + 1], p[i * 4 + 2], p[i * 4 + 3]]);
            (f.clamp(0.0, 1.0) * 255.0) as u8
        };
        for i in 0..px {
            let rgba = match img.format {
                Format::R8 => [p[i], p[i], p[i], 255],
                Format::R8G8 => [p[i * 2], p[i * 2 + 1], 0, 255],
                Format::R8G8B8 => [p[i * 3], p[i * 3 + 1], p[i * 3 + 2], 255],
                Format::R8G8B8A8 => [p[i * 4], p[i * 4 + 1], p[i * 4 + 2], p[i * 4 + 3]],
                Format::R16 => {
                    let v = u16_at(i);
                    [v, v, v, 255]
                }
                Format::R16G16 => [u16_at(i * 2), u16_at(i * 2 + 1), 0, 255],
                Format::R16G16B16 => [u16_at(i * 3), u16_at(i * 3 + 1), u16_at(i * 3 + 2), 255],
                Format::R16G16B16A16 => [u16_at(i * 4), u16_at(i * 4 + 1), u16_at(i * 4 + 2), u16_at(i * 4 + 3)],
                Format::R32G32B32FLOAT => [f32_at(i * 3), f32_at(i * 3 + 1), f32_at(i * 3 + 2), 255],
                Format::R32G32B32A32FLOAT => [f32_at(i * 4), f32_at(i * 4 + 1), f32_at(i * 4 + 2), f32_at(i * 4 + 3)],
            };
            out.extend_from_slice(&rgba);
        }
        TextureData { name, width: img.width, height: img.height, rgba8: out }
    }
}

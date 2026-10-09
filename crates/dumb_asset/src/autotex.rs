//! Guess a material's textures from texture files in the project, by name.
//!
//! Texture packs name their maps `<base>_<channel>`: `crate_BaseColor.png`, `crate_Normal_DX.png`,
//! `crate_ARM.png`... A texture is a candidate when it shares name words with the material or
//! model (`military`, `crate`); its channel comes from the remaining words.

use crate::database::{AssetDatabase, AssetKind};
use crate::material::{MaterialData, NormalFormat, RoughnessSource};
use dumb_core::AssetId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Albedo,
    NormalGl,
    NormalDx,
    Normal,
    Roughness,
    Smoothness,
    Metallic,
    Ao,
    /// R = AO, G = roughness, B = metallic (ARM / ORM).
    Packed,
    Emission,
}

/// Words of a name: `military_crate_BaseColor-2k` -> [military, crate, base, color, 2k].
pub fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let boundary = c.is_uppercase() && i > 0 && (chars[i - 1].is_lowercase() || (chars[i - 1].is_uppercase() && chars.get(i + 1).is_some_and(|n| n.is_lowercase())));
        if boundary && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Words that describe a channel, resolution or variant rather than the object.
fn is_descriptor(w: &str) -> bool {
    const D: &[&str] = &[
        "base", "color", "colour", "albedo", "diffuse", "diff", "col", "basecolor", "normal", "nor", "nrm", "norm", "gl", "dx", "opengl",
        "directx", "rough", "roughness", "rgh", "gloss", "glossiness", "smooth", "smoothness", "metal", "metallic", "metalness", "mtl", "ao",
        "occlusion", "ambient", "ambientocclusion", "mixed", "arm", "orm", "emissive", "emission", "emit", "glow", "height", "displacement", "disp",
        "bump", "map", "tex", "texture", "png", "jpg", "1k", "2k", "4k", "8k", "lod0", "lod1", "lod2", "lod",
    ];
    D.contains(&w) || w.chars().all(|c| c.is_ascii_digit())
}

/// Which channel a texture name describes.
pub fn channel_of(name: &str) -> Option<Channel> {
    let w = words(name);
    let has = |s: &[&str]| w.iter().any(|x| s.contains(&x.as_str()));
    let joined = w.join("");
    if has(&["arm", "orm"]) {
        return Some(Channel::Packed);
    }
    if has(&["normal", "nor", "nrm", "norm"]) || joined.contains("normalmap") {
        return Some(if has(&["dx", "directx"]) {
            Channel::NormalDx
        } else if has(&["gl", "opengl"]) {
            Channel::NormalGl
        } else {
            Channel::Normal
        });
    }
    if has(&["emissive", "emission", "emit", "glow"]) {
        return Some(Channel::Emission);
    }
    if has(&["ao", "occlusion", "ambientocclusion"]) {
        return Some(Channel::Ao);
    }
    if has(&["rough", "roughness", "rgh"]) {
        return Some(Channel::Roughness);
    }
    if has(&["gloss", "glossiness", "smoothness"]) {
        return Some(Channel::Smoothness);
    }
    if has(&["metal", "metallic", "metalness", "mtl"]) {
        return Some(Channel::Metallic);
    }
    if has(&["albedo", "diffuse", "diff", "basecolor", "col"]) || (has(&["base"]) && has(&["color", "colour"])) || has(&["color", "colour"]) {
        return Some(Channel::Albedo);
    }
    None
}

/// Result of a guess: the material and a human-readable report of what was assigned.
pub struct Guess {
    pub material: MaterialData,
    pub found: Vec<(Channel, AssetId, String)>,
}

/// Find textures for a material named `material_name` on a model named `model_name`.
/// `base` is the starting material (keeps colors and factors from the import).
pub fn guess_material(db: &AssetDatabase, model_name: &str, material_name: &str, base: &MaterialData) -> Guess {
    let object_words = |n: &str| words(n).into_iter().filter(|w| !is_descriptor(w) && w.len() > 1).collect::<Vec<_>>();
    let mut wanted: Vec<String> = object_words(material_name);
    for w in object_words(model_name) {
        if !wanted.contains(&w) {
            wanted.push(w);
        }
    }

    // Best candidate per channel by number of shared words.
    let mut best: Vec<(Channel, AssetId, usize, String)> = Vec::new();
    for e in db.entries().filter(|e| e.kind == AssetKind::Texture) {
        let stem = e.stem();
        let Some(ch) = channel_of(stem) else { continue };
        let tex_words = object_words(stem);
        let score = tex_words.iter().filter(|w| wanted.contains(w)).count();
        if score == 0 {
            continue;
        }
        match best.iter_mut().find(|b| b.0 == ch) {
            Some(b) if b.2 >= score => {}
            Some(b) => *b = (ch, e.id, score, e.path.clone()),
            None => best.push((ch, e.id, score, e.path.clone())),
        }
    }

    let mut m = base.clone();
    let get = |c: Channel| best.iter().find(|b| b.0 == c).map(|b| b.1);
    if let Some(t) = get(Channel::Albedo) {
        m.albedo_texture = t;
        // Imported factors often tint white; a found albedo map should show as-is.
        m.albedo = dumb_core::Color::rgba(1.0, 1.0, 1.0, base.albedo.a);
    }
    // Prefer the OpenGL normal map when a pack ships both.
    if let Some(t) = get(Channel::NormalGl).or(get(Channel::Normal)) {
        m.normal_texture = t;
        m.normal_format = NormalFormat::OpenGL;
    } else if let Some(t) = get(Channel::NormalDx) {
        m.normal_texture = t;
        m.normal_format = NormalFormat::DirectX;
    }
    if let Some(t) = get(Channel::Packed) {
        m.metallic_roughness_texture = t;
        m.ao_texture = t;
        m.roughness_source = RoughnessSource::Packed;
        m.roughness = 1.0;
        m.metallic = 1.0;
    } else {
        if let Some(t) = get(Channel::Roughness) {
            m.metallic_roughness_texture = t;
            m.roughness_source = RoughnessSource::RoughnessMap;
            m.roughness = 1.0;
        } else if let Some(t) = get(Channel::Smoothness) {
            m.metallic_roughness_texture = t;
            m.roughness_source = RoughnessSource::SmoothnessMap;
            m.roughness = 1.0;
        }
        if let Some(t) = get(Channel::Metallic) {
            m.metallic_texture = t;
            m.metallic = 1.0;
            if m.roughness_source == RoughnessSource::Packed {
                // A lone metallic map still needs the "separate maps" path.
                m.roughness_source = RoughnessSource::RoughnessMap;
            }
        }
        if let Some(t) = get(Channel::Ao) {
            m.ao_texture = t;
        }
    }
    if let Some(t) = get(Channel::Emission) {
        m.emission_texture = t;
        m.emission = dumb_core::Color::WHITE;
    }
    let found = best.into_iter().map(|(c, id, _, p)| (c, id, p)).collect();
    Guess { material: m, found }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_words() {
        assert_eq!(words("military_crate_BaseColor-2k"), ["military", "crate", "base", "color", "2k"]);
        assert_eq!(words("Rock01_NormalDX"), ["rock01", "normal", "dx"]);
    }

    #[test]
    fn classifies_common_names() {
        assert_eq!(channel_of("military_crate_Base_color"), Some(Channel::Albedo));
        assert_eq!(channel_of("military_crate_Normal_DX"), Some(Channel::NormalDx));
        assert_eq!(channel_of("military_crate_Normal_GL"), Some(Channel::NormalGl));
        assert_eq!(channel_of("military_crate_Roughness"), Some(Channel::Roughness));
        assert_eq!(channel_of("military_crate_Metallic"), Some(Channel::Metallic));
        assert_eq!(channel_of("military_crate_Mixed_AO"), Some(Channel::Ao));
        assert_eq!(channel_of("rock_arm_2k"), Some(Channel::Packed));
        assert_eq!(channel_of("ground_tiles_01_color_2k"), Some(Channel::Albedo));
        assert_eq!(channel_of("ground_tiles_01_height_2k"), None);
        assert_eq!(channel_of("lamp_emissive"), Some(Channel::Emission));
        assert_eq!(channel_of("wood_diff"), Some(Channel::Albedo));
    }

    #[test]
    fn guesses_a_full_material_from_a_project() {
        let dir = std::env::temp_dir().join(format!("dumb_autotex_{}", std::process::id()));
        let tex = dir.join("Assets/Textures/crate_textures");
        std::fs::create_dir_all(&tex).unwrap();
        for n in ["Base_color", "Metallic", "Mixed_AO", "Normal_DX", "Normal_GL", "Roughness"] {
            std::fs::write(tex.join(format!("military_crate_{n}.png")), b"x").unwrap();
        }
        std::fs::write(tex.join("barrel_Base_color.png"), b"x").unwrap(); // unrelated
        let db = AssetDatabase::open(&dir).unwrap();
        let g = guess_material(&db, "military_crate_01_lod0", "Material", &MaterialData::default());
        let name = |id: AssetId| db.entry(id).map(|e| e.stem().to_string()).unwrap_or_default();
        assert_eq!(name(g.material.albedo_texture), "military_crate_Base_color");
        assert_eq!(name(g.material.normal_texture), "military_crate_Normal_GL");
        assert_eq!(g.material.normal_format, NormalFormat::OpenGL);
        assert_eq!(name(g.material.metallic_roughness_texture), "military_crate_Roughness");
        assert_eq!(g.material.roughness_source, RoughnessSource::RoughnessMap);
        assert_eq!(name(g.material.metallic_texture), "military_crate_Metallic");
        assert_eq!(name(g.material.ao_texture), "military_crate_Mixed_AO");
        let _ = std::fs::remove_dir_all(dir);
    }
}

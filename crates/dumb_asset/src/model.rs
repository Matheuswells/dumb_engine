//! CPU-side imported model data: meshes, materials, textures, skeletons, animations.

use crate::material::MaterialData;
use crate::texture::TextureData;
use dumb_core::{Aabb, AssetId, Mat4, Quat, Vec3, Vec4};

/// Unified vertex used by every mesh. Skinning attributes are zero for static meshes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub tangent: [f32; 4],
    pub joints: [u16; 4],
    pub weights: [f32; 4],
}

#[derive(Clone, Debug, Default)]
pub struct Primitive {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// Index into `ModelData::materials`.
    pub material: Option<usize>,
    pub aabb: Aabb,
    /// Generated LOD index buffers (level 1, 2, ...) over the same vertices.
    pub lods: Vec<Vec<u32>>,
}

impl Primitive {
    /// Index list for a LOD level (0 = full detail); clamps to the last level.
    pub fn lod_indices(&self, level: usize) -> &[u32] {
        if level == 0 || self.lods.is_empty() {
            &self.indices
        } else {
            &self.lods[(level - 1).min(self.lods.len() - 1)]
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MeshData {
    pub name: String,
    pub primitives: Vec<Primitive>,
}

impl MeshData {
    pub fn vertex_count(&self) -> usize {
        self.primitives.iter().map(|p| p.vertices.len()).sum()
    }
    pub fn triangle_count(&self) -> usize {
        self.primitives.iter().map(|p| p.indices.len() / 3).sum()
    }
    pub fn aabb(&self) -> Aabb {
        self.primitives.iter().fold(Aabb::EMPTY, |a, p| a.union(&p.aabb))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trs {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Trs {
    fn default() -> Self {
        Trs { translation: Vec3::ZERO, rotation: Quat::IDENTITY, scale: Vec3::ONE }
    }
}

impl Trs {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub name: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub local: Trs,
    pub mesh: Option<usize>,
    pub skin: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Skin {
    pub name: String,
    /// Node indices of the joints.
    pub joints: Vec<usize>,
    pub inverse_bind: Vec<Mat4>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelTarget {
    Translation,
    Rotation,
    Scale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    Step,
    Linear,
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub node: usize,
    pub target: ChannelTarget,
    pub interpolation: Interpolation,
    pub times: Vec<f32>,
    /// xyz for translation/scale, xyzw for rotation.
    pub values: Vec<Vec4>,
}

impl Channel {
    pub fn sample(&self, t: f32) -> Vec4 {
        let n = self.times.len();
        if n == 0 {
            return Vec4::ZERO;
        }
        if t <= self.times[0] || n == 1 {
            return self.values[0];
        }
        if t >= self.times[n - 1] {
            return self.values[n - 1];
        }
        let i = self.times.partition_point(|&x| x <= t).saturating_sub(1).min(n - 2);
        if self.interpolation == Interpolation::Step {
            return self.values[i];
        }
        let (t0, t1) = (self.times[i], self.times[i + 1]);
        let f = ((t - t0) / (t1 - t0).max(1e-6)).clamp(0.0, 1.0);
        let (a, b) = (self.values[i], self.values[i + 1]);
        match self.target {
            ChannelTarget::Rotation => {
                let qa = Quat::from_vec4(a);
                let mut qb = Quat::from_vec4(b);
                if qa.dot(qb) < 0.0 {
                    qb = -qb;
                }
                Vec4::from(qa.slerp(qb, f))
            }
            _ => a.lerp(b, f),
        }
    }
}

/// A named marker on an animation timeline (footsteps, hit frames, ...).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnimationEvent {
    pub time: f32,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct AnimationClip {
    pub name: String,
    pub duration: f32,
    pub channels: Vec<Channel>,
}

impl AnimationClip {
    /// Write sampled TRS values into `pose` (one entry per node).
    pub fn sample_into(&self, t: f32, pose: &mut [Trs]) {
        for c in &self.channels {
            let Some(p) = pose.get_mut(c.node) else { continue };
            let v = c.sample(t);
            match c.target {
                ChannelTarget::Translation => p.translation = v.truncate(),
                ChannelTarget::Rotation => p.rotation = Quat::from_vec4(v).normalize(),
                ChannelTarget::Scale => p.scale = v.truncate(),
            }
        }
    }
}

/// Groups of nodes named like `Rock_LOD0`, `Rock_LOD1`, ...
#[derive(Clone, Debug, Default)]
pub struct LodGroup {
    pub name: String,
    /// (level, node) sorted by level.
    pub levels: Vec<(u32, usize)>,
}

#[derive(Clone, Debug, Default)]
pub struct ModelData {
    pub nodes: Vec<Node>,
    /// Root nodes of the default scene.
    pub roots: Vec<usize>,
    pub meshes: Vec<MeshData>,
    pub materials: Vec<MaterialData>,
    pub material_names: Vec<String>,
    pub textures: Vec<TextureData>,
    pub skins: Vec<Skin>,
    pub animations: Vec<AnimationClip>,
    /// Nodes treated as collision shapes (`UCX_`, `UBX_`, `USP_`, `COL_` prefixes or `-col` suffix).
    pub collision_nodes: Vec<usize>,
    pub lods: Vec<LodGroup>,
    pub source_format: String,
    /// Nodes hidden by import settings.
    pub hidden_nodes: Vec<usize>,
    /// Screen-height fraction above which each LOD level is used (LOD0 first).
    pub lod_screen_sizes: Vec<f32>,
    /// Below this screen size the model is not drawn (0 = never culled).
    pub cull_screen_size: f32,
    /// Material slot -> project material, from import settings (replaces the embedded material).
    pub material_remap: Vec<(usize, AssetId)>,
    /// Cached `aabb()` (set after import processing; empty = compute on demand).
    pub bounds_cache: Option<Aabb>,
}

impl ModelData {
    pub fn rest_pose(&self) -> Vec<Trs> {
        self.nodes.iter().map(|n| n.local).collect()
    }

    /// Global matrices for a pose, in model space.
    pub fn global_matrices(&self, pose: &[Trs]) -> Vec<Mat4> {
        let mut out = vec![Mat4::IDENTITY; self.nodes.len()];
        let mut stack: Vec<(usize, Mat4)> = self.roots.iter().map(|r| (*r, Mat4::IDENTITY)).collect();
        // Nodes not reachable from the scene roots still get a matrix.
        for (i, n) in self.nodes.iter().enumerate() {
            if n.parent.is_none() && !self.roots.contains(&i) {
                stack.push((i, Mat4::IDENTITY));
            }
        }
        while let Some((i, parent)) = stack.pop() {
            let m = parent * pose[i].matrix();
            out[i] = m;
            for c in &self.nodes[i].children {
                stack.push((*c, m));
            }
        }
        out
    }

    /// Skinning matrices for skin `skin` given node global matrices.
    pub fn joint_matrices(&self, skin: usize, globals: &[Mat4], mesh_node_global: Mat4) -> Vec<Mat4> {
        let s = &self.skins[skin];
        let inv_mesh = mesh_node_global.inverse();
        s.joints
            .iter()
            .zip(&s.inverse_bind)
            .map(|(j, ib)| inv_mesh * globals[*j] * *ib)
            .collect()
    }

    /// Sample clip `a`, optionally blended with clip `b` by `w`.
    pub fn sample_pose(&self, a: Option<(usize, f32)>, b: Option<(usize, f32, f32)>) -> Vec<Trs> {
        let mut pose = self.rest_pose();
        if let Some((ci, t)) = a {
            if let Some(c) = self.animations.get(ci) {
                c.sample_into(t, &mut pose);
            }
        }
        if let Some((ci, t, w)) = b {
            if let Some(c) = self.animations.get(ci) {
                if w > 0.0 {
                    let mut other = self.rest_pose();
                    c.sample_into(t, &mut other);
                    for (p, o) in pose.iter_mut().zip(&other) {
                        p.translation = p.translation.lerp(o.translation, w);
                        p.rotation = p.rotation.slerp(o.rotation, w);
                        p.scale = p.scale.lerp(o.scale, w);
                    }
                }
            }
        }
        pose
    }

    pub fn find_clip(&self, name: &str) -> Option<usize> {
        if name.is_empty() {
            return if self.animations.is_empty() { None } else { Some(0) };
        }
        self.animations.iter().position(|a| a.name == name)
    }

    /// Model-space bounds of all meshes in the rest pose.
    pub fn aabb(&self) -> Aabb {
        let globals = self.global_matrices(&self.rest_pose());
        let mut b = Aabb::EMPTY;
        for (i, n) in self.nodes.iter().enumerate() {
            if let Some(m) = n.mesh {
                let mb = self.meshes[m].aabb();
                if mb.is_valid() {
                    b = b.union(&mb.transformed(&globals[i]));
                }
            }
        }
        if !b.is_valid() {
            b = Aabb { min: Vec3::splat(-0.5), max: Vec3::splat(0.5) };
        }
        b
    }

    pub fn vertex_count(&self) -> usize {
        self.meshes.iter().map(|m| m.vertex_count()).sum()
    }

    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|m| m.triangle_count()).sum()
    }

    /// Detect collision nodes and LOD groups from naming conventions.
    pub fn classify_nodes(&mut self) {
        self.collision_nodes.clear();
        self.lods.clear();
        for (i, n) in self.nodes.iter().enumerate() {
            let up = n.name.to_ascii_uppercase();
            if ["UCX_", "UBX_", "USP_", "UCP_", "COL_"].iter().any(|p| up.starts_with(p)) || up.ends_with("-COL") {
                self.collision_nodes.push(i);
            }
            if let Some(pos) = up.rfind("_LOD") {
                if let Ok(level) = up[pos + 4..].parse::<u32>() {
                    let base = n.name[..pos].to_string();
                    match self.lods.iter_mut().find(|g| g.name == base) {
                        Some(g) => g.levels.push((level, i)),
                        None => self.lods.push(LodGroup { name: base, levels: vec![(level, i)] }),
                    }
                }
            }
        }
        for g in &mut self.lods {
            g.levels.sort();
        }
    }

    /// Whether a node should be drawn (collision proxies and non-zero LODs are hidden).
    pub fn node_visible_by_default(&self, node: usize) -> bool {
        if self.collision_nodes.contains(&node) || self.hidden_nodes.contains(&node) {
            return false;
        }
        !self.lods.iter().any(|g| g.levels.iter().any(|(lvl, n)| *n == node && *lvl != 0))
    }
}

impl ModelData {
    /// Number of detail levels: generated levels and/or `_LODn` node groups.
    pub fn lod_count(&self) -> usize {
        let generated = self.meshes.iter().flat_map(|m| &m.primitives).map(|p| p.lods.len() + 1).max().unwrap_or(1);
        let named = self.lods.iter().map(|g| g.levels.iter().map(|l| l.0 as usize + 1).max().unwrap_or(1)).max().unwrap_or(1);
        generated.max(named)
    }

    /// Triangles drawn at a LOD level.
    pub fn triangle_count_at(&self, level: usize) -> usize {
        let mut n = 0;
        for (ni, node) in self.nodes.iter().enumerate() {
            let Some(mi) = node.mesh else { continue };
            if !self.node_visible_at_lod(ni, level) {
                continue;
            }
            n += self.meshes[mi].primitives.iter().map(|p| p.lod_indices(level).len() / 3).sum::<usize>();
        }
        n
    }

    /// Visibility of a node when rendering a given LOD level (named `_LODn` groups switch nodes).
    pub fn node_visible_at_lod(&self, node: usize, level: usize) -> bool {
        if self.collision_nodes.contains(&node) || self.hidden_nodes.contains(&node) {
            return false;
        }
        for g in &self.lods {
            if g.levels.iter().any(|(_, n)| *n == node) {
                let max = g.levels.iter().map(|(l, _)| *l as usize).max().unwrap_or(0);
                let want = level.min(max);
                return g.levels.iter().any(|(l, n)| *n == node && *l as usize == want);
            }
        }
        true
    }

    /// Asset id of material slot `slot`: the remapped project material or the embedded sub-asset.
    pub fn material_id(&self, model_id: AssetId, slot: usize) -> AssetId {
        self.material_remap.iter().find(|(s, _)| *s == slot).map_or_else(|| model_id.sub("material", slot), |(_, id)| *id)
    }

    /// Model-space bounds, cached after import.
    pub fn bounds(&self) -> Aabb {
        self.bounds_cache.unwrap_or_else(|| self.aabb())
    }

    /// Which LOD level to use for a given projected size (fraction of screen height),
    /// or `None` when it's small enough to be culled.
    pub fn select_lod(&self, screen_size: f32) -> Option<usize> {
        if screen_size < self.cull_screen_size {
            return None;
        }
        if self.lod_screen_sizes.is_empty() {
            return Some(0);
        }
        let count = self.lod_count();
        for (i, s) in self.lod_screen_sizes.iter().enumerate() {
            if screen_size >= *s {
                return Some(i.min(count - 1));
            }
        }
        Some(count - 1)
    }
}

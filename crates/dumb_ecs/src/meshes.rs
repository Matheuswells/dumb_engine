//! Meshes made by scripts at runtime, with no file: procedural terrain, generated buildings,
//! anything built in code.
//!
//! ```ignore
//! let id = AssetId::from_name("my_game/terrain/chunk_12");
//! ctx.meshes.set(id, mesh);                       // uploaded before the next frame is drawn
//! world.insert(e, MeshRenderer { model: id, ..Default::default() });
//! ctx.meshes.remove(id);                          // frees it (CPU and GPU)
//! ```
//!
//! A mesh can be built on any thread (it's plain data); hand it to `ctx.meshes` from a system.
//! Setting the same id again replaces the mesh in place.

use dumb_core::AssetId;

/// One mesh: shared vertex arrays and one or more parts, each drawn with its own material.
#[derive(Clone, Debug, Default)]
pub struct RuntimeMesh {
    pub positions: Vec<[f32; 3]>,
    /// One per position (unit length).
    pub normals: Vec<[f32; 3]>,
    /// One per position, or empty (all zero).
    pub uvs: Vec<[f32; 2]>,
    /// One per position (xyz + handedness sign), or empty (derived from the triangles and UVs).
    pub tangents: Vec<[f32; 4]>,
    pub parts: Vec<MeshPart>,
}

/// Triangles of a [`RuntimeMesh`] that share a material.
#[derive(Clone, Debug, Default)]
pub struct MeshPart {
    /// Three vertex indices per triangle, counter-clockwise seen from the front.
    pub indices: Vec<u32>,
    /// NONE = the default material (a `MeshRenderer` material override still applies).
    pub material: AssetId,
}

impl RuntimeMesh {
    pub fn triangles(&self) -> usize {
        self.parts.iter().map(|p| p.indices.len() / 3).sum()
    }
}

/// Mesh changes scripts asked for this frame (`ctx.meshes`); the host applies them after
/// the scripts run.
#[derive(Default, Debug)]
pub struct MeshUploads {
    pub set: Vec<(AssetId, RuntimeMesh)>,
    pub remove: Vec<AssetId>,
}

impl MeshUploads {
    /// Create or replace the mesh `id`. Use it as a model anywhere (`MeshRenderer::model`).
    pub fn set(&mut self, id: AssetId, mesh: RuntimeMesh) {
        self.remove.retain(|r| *r != id);
        self.set.retain(|(s, _)| *s != id);
        self.set.push((id, mesh));
    }

    /// Free the mesh `id`.
    pub fn remove(&mut self, id: AssetId) {
        self.set.retain(|(s, _)| *s != id);
        self.remove.push(id);
    }
}

//! Non-destructive model processing applied after every import (from `ImportSettings`):
//! transform/pivot, normals, node visibility, collision tagging and LOD generation.

use crate::database::{ImportSettings, NormalsMode, Pivot};
use crate::import_gltf::compute_tangents;
use crate::model::{ModelData, Primitive, Vertex};
use dumb_core::{Aabb, EulerRot, Mat4, Quat, Vec3};
use std::collections::HashMap;

pub fn post_process(m: &mut ModelData, s: &ImportSettings) {
    apply_transform(m, s);
    match s.normals {
        NormalsMode::Import => {}
        NormalsMode::Smooth => m.meshes.iter_mut().flat_map(|mesh| &mut mesh.primitives).for_each(smooth_normals),
        NormalsMode::Flat => m.meshes.iter_mut().flat_map(|mesh| &mut mesh.primitives).for_each(flat_normals),
    }
    m.hidden_nodes = (0..m.nodes.len()).filter(|i| s.hidden_nodes.contains(&m.nodes[*i].name)).collect();
    for (i, n) in m.nodes.iter().enumerate() {
        if s.collision_nodes.contains(&n.name) && !m.collision_nodes.contains(&i) {
            m.collision_nodes.push(i);
        }
    }
    if s.lod.generate {
        generate_lods(m, &s.lod.ratios, s.lod.max_error);
    }
    m.lod_screen_sizes = s.lod.screen_sizes.clone();
    m.cull_screen_size = s.lod.cull_screen_size;
    m.material_remap = s.material_remap.clone();
    m.bounds_cache = Some(m.aabb());
}

/// Rotation and pivot are applied to the root nodes, so skinning stays valid
/// (mesh and joints move together).
fn apply_transform(m: &mut ModelData, s: &ImportSettings) {
    let [rx, ry, rz] = s.rotation;
    let rot = Quat::from_euler(EulerRot::XYZ, rx.to_radians(), ry.to_radians(), rz.to_radians());
    if rot.angle_between(Quat::IDENTITY) < 1e-6 && s.pivot == Pivot::Keep {
        return;
    }
    let rm = Mat4::from_quat(rot);
    let roots = m.roots.clone();
    for r in &roots {
        let local = rm * m.nodes[*r].local.matrix();
        let (sc, ro, t) = local.to_scale_rotation_translation();
        m.nodes[*r].local = crate::model::Trs { translation: t, rotation: ro, scale: sc };
    }
    if s.pivot != Pivot::Keep {
        let b = m.aabb();
        let offset = match s.pivot {
            Pivot::Center => -b.center(),
            Pivot::BottomCenter => -Vec3::new(b.center().x, b.min.y, b.center().z),
            Pivot::Keep => Vec3::ZERO,
        };
        for r in &roots {
            m.nodes[*r].local.translation += offset;
        }
    }
}

/// Smooth normals: vertices at the same position (split by UV seams) share a normal.
fn smooth_normals(p: &mut Primitive) {
    let key = |v: &Vertex| {
        let q = |x: f32| (x * 10000.0).round() as i64;
        (q(v.position[0]), q(v.position[1]), q(v.position[2]))
    };
    let mut acc: HashMap<(i64, i64, i64), Vec3> = HashMap::new();
    for t in p.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| p.vertices[i as usize]);
        let n = (Vec3::from(b.position) - Vec3::from(a.position)).cross(Vec3::from(c.position) - Vec3::from(a.position));
        for v in [a, b, c] {
            *acc.entry(key(&v)).or_insert(Vec3::ZERO) += n;
        }
    }
    for v in &mut p.vertices {
        v.normal = acc.get(&key(v)).copied().unwrap_or(Vec3::Y).normalize_or(Vec3::Y).to_array();
    }
    compute_tangents(&mut p.vertices, &p.indices);
}

/// Faceted shading: every triangle gets its own vertices and face normal.
fn flat_normals(p: &mut Primitive) {
    let mut verts = Vec::with_capacity(p.indices.len());
    for t in p.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| p.vertices[i as usize]);
        let n = (Vec3::from(b.position) - Vec3::from(a.position)).cross(Vec3::from(c.position) - Vec3::from(a.position)).normalize_or(Vec3::Y);
        for mut v in [a, b, c] {
            v.normal = n.to_array();
            verts.push(v);
        }
    }
    p.indices = (0..verts.len() as u32).collect();
    p.vertices = verts;
    p.lods.clear();
    compute_tangents(&mut p.vertices, &p.indices);
    p.aabb = Aabb::from_points(p.vertices.iter().map(|v| Vec3::from(v.position)));
}

/// Simplify every primitive into extra index buffers (vertices are shared between levels).
pub fn generate_lods(m: &mut ModelData, ratios: &[f32], max_error: f32) {
    for p in m.meshes.iter_mut().flat_map(|mesh| &mut mesh.primitives) {
        p.lods = simplify_levels(p, ratios, max_error);
    }
}

fn simplify_levels(p: &Primitive, ratios: &[f32], max_error: f32) -> Vec<Vec<u32>> {
    let bytes = unsafe { std::slice::from_raw_parts(p.vertices.as_ptr() as *const u8, std::mem::size_of_val(p.vertices.as_slice())) };
    let Ok(adapter) = meshopt::VertexDataAdapter::new(bytes, std::mem::size_of::<Vertex>(), 0) else { return Vec::new() };
    let mut levels: Vec<Vec<u32>> = Vec::new();
    let mut prev_len = p.indices.len();
    for &ratio in ratios {
        let target = ((p.indices.len() as f32 * ratio.clamp(0.001, 1.0)) as usize / 3 * 3).max(3);
        let mut lod = meshopt::simplify(&p.indices, &adapter, target, max_error, meshopt::SimplifyOptions::None, None);
        // UV seams and hard edges can stop the regular simplifier; fall back to the sloppy one.
        if lod.len() > target * 3 / 2 && lod.len() as f32 > prev_len as f32 * 0.9 {
            let sloppy = meshopt::simplify_sloppy(&p.indices, &adapter, target, max_error.max(0.02) * 2.0, None);
            if !sloppy.is_empty() && sloppy.len() < lod.len() {
                lod = sloppy;
            }
        }
        if lod.len() < 3 || lod.len() >= prev_len {
            // Can't go further without destroying the mesh; reuse the previous level.
            lod = levels.last().cloned().unwrap_or_else(|| p.indices.clone());
        }
        prev_len = lod.len();
        levels.push(lod);
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{single_mesh_model, sphere};

    #[test]
    fn lods_reduce_triangles() {
        let mut m = single_mesh_model("s", sphere(64, 32));
        let full = m.triangle_count_at(0);
        generate_lods(&mut m, &[0.5, 0.25, 0.1], 0.05);
        let l1 = m.triangle_count_at(1);
        let l3 = m.triangle_count_at(3);
        assert_eq!(m.lod_count(), 4);
        assert!(l1 < full && l1 as f32 > full as f32 * 0.3, "lod1 {l1} vs {full}");
        assert!(l3 < l1 && l3 as f32 <= full as f32 * 0.2, "lod3 {l3} vs {full}");
    }

    #[test]
    fn pivot_and_rotation() {
        let mut m = single_mesh_model("s", sphere(16, 8));
        let s = ImportSettings { rotation: [90.0, 0.0, 0.0], pivot: Pivot::BottomCenter, ..Default::default() };
        post_process(&mut m, &s);
        let b = m.aabb();
        assert!(b.min.y.abs() < 1e-3 && b.center().x.abs() < 1e-3, "{b:?}");
    }

    #[test]
    fn lod_selection_by_screen_size() {
        let mut m = single_mesh_model("s", sphere(32, 16));
        generate_lods(&mut m, &[0.5, 0.25], 0.05);
        m.lod_screen_sizes = vec![0.3, 0.1, 0.0];
        m.cull_screen_size = 0.01;
        assert_eq!(m.select_lod(0.5), Some(0));
        assert_eq!(m.select_lod(0.2), Some(1));
        assert_eq!(m.select_lod(0.05), Some(2));
        assert_eq!(m.select_lod(0.005), None);
    }
}

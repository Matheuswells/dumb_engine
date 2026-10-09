//! CPU ray picking against model geometry.

use dumb_asset::AssetDatabase;
use dumb_core::{Entity, Ray, Vec3};
use dumb_ecs::{MeshRenderer, World};

fn ray_triangle(r: &Ray, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let p = r.dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = 1.0 / det;
    let s = r.origin - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = r.dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

/// Closest entity hit by `ray` (world space). Returns (entity, distance).
pub fn pick(world: &World, db: &AssetDatabase, ray: &Ray) -> Option<(Entity, f32)> {
    // Broad phase: model bounds in model space.
    let mut candidates: Vec<(f32, Entity)> = Vec::new();
    for (e, mr) in world.query_ref::<(Entity, &MeshRenderer)>() {
        if !mr.visible {
            continue;
        }
        let Some(model) = db.model_loaded(mr.model) else { continue };
        let m = world.global_matrix(e);
        let inv = m.inverse();
        let local = Ray { origin: inv.transform_point3(ray.origin), dir: inv.transform_vector3(ray.dir) };
        let len = local.dir.length();
        if len < 1e-12 {
            continue;
        }
        let local = Ray { origin: local.origin, dir: local.dir / len };
        if let Some(t) = local.intersect_aabb(&model.aabb()) {
            let world_t = (m.transform_point3(local.origin + local.dir * t) - ray.origin).length();
            candidates.push((world_t, e));
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Narrow phase: triangles of the closest few candidates.
    let mut best: Option<(Entity, f32)> = None;
    for (bound_t, e) in candidates.into_iter().take(32) {
        if best.is_some_and(|b| b.1 < bound_t) {
            break;
        }
        let Some(mr) = world.get::<MeshRenderer>(e) else { continue };
        let Some(model) = db.model_loaded(mr.model) else { continue };
        let m = world.global_matrix(e);
        let globals = model.global_matrices(&model.rest_pose());
        for (ni, node) in model.nodes.iter().enumerate() {
            let Some(mi) = node.mesh else { continue };
            if !model.node_visible_by_default(ni) {
                continue;
            }
            let nm = m * globals[ni];
            let inv = nm.inverse();
            let lr = Ray { origin: inv.transform_point3(ray.origin), dir: inv.transform_vector3(ray.dir) };
            for p in &model.meshes[mi].primitives {
                if lr.intersect_aabb(&p.aabb).is_none() {
                    continue;
                }
                for tri in p.indices.chunks_exact(3) {
                    let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| Vec3::from(p.vertices[i as usize].position));
                    if let Some(t) = ray_triangle(&lr, a, b, c) {
                        let wt = (nm.transform_point3(lr.origin + lr.dir * t) - ray.origin).length();
                        if best.is_none_or(|bb| wt < bb.1) {
                            best = Some((e, wt));
                        }
                    }
                }
            }
        }
    }
    best
}

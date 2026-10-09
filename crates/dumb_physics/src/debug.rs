//! Collider wireframes for the editor, drawn from component data (works in edit mode too).

use dumb_asset::AssetDatabase;
use dumb_core::{Color, Entity, Mat4, Vec3};
use dumb_ecs::{CharacterController, Collider, ColliderShape, MeshRenderer, RigidBody, World};
use std::f32::consts::TAU;

const STATIC: Color = Color::rgba(0.3, 0.85, 0.4, 0.8);
const DYNAMIC: Color = Color::rgba(0.35, 0.65, 1.0, 0.9);
const TRIGGER: Color = Color::rgba(1.0, 0.8, 0.2, 0.8);
const CHARACTER: Color = Color::rgba(0.9, 0.4, 1.0, 0.9);

fn circle(m: &Mat4, center: Vec3, u: Vec3, w: Vec3, r: f32, c: Color, out: &mut dyn FnMut(Vec3, Vec3, Color)) {
    let n = 32;
    for i in 0..n {
        let a = i as f32 / n as f32 * TAU;
        let b = (i + 1) as f32 / n as f32 * TAU;
        out(m.transform_point3(center + (u * a.cos() + w * a.sin()) * r), m.transform_point3(center + (u * b.cos() + w * b.sin()) * r), c);
    }
}

fn boxed(m: &Mat4, center: Vec3, half: Vec3, c: Color, out: &mut dyn FnMut(Vec3, Vec3, Color)) {
    let k: Vec<Vec3> = (0..8)
        .map(|i| {
            let s = Vec3::new(if i & 1 == 0 { -1.0 } else { 1.0 }, if i & 2 == 0 { -1.0 } else { 1.0 }, if i & 4 == 0 { -1.0 } else { 1.0 });
            m.transform_point3(center + half * s)
        })
        .collect();
    for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
        out(k[a], k[b], c);
    }
}

/// Vertical capsule or cylinder with the given half height (of the straight part) and radius.
fn upright(m: &Mat4, center: Vec3, half: f32, r: f32, capsule: bool, c: Color, out: &mut dyn FnMut(Vec3, Vec3, Color)) {
    let (top, bot) = (center + Vec3::Y * half, center - Vec3::Y * half);
    circle(m, top, Vec3::X, Vec3::Z, r, c, out);
    circle(m, bot, Vec3::X, Vec3::Z, r, c, out);
    for d in [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z] {
        out(m.transform_point3(top + d * r), m.transform_point3(bot + d * r), c);
    }
    if capsule {
        // Half circles over the caps.
        for (cap, sign) in [(top, 1.0f32), (bot, -1.0)] {
            for u in [Vec3::X, Vec3::Z] {
                let n = 16;
                for i in 0..n {
                    let a = i as f32 / n as f32 * std::f32::consts::PI;
                    let b = (i + 1) as f32 / n as f32 * std::f32::consts::PI;
                    let p = |t: f32| cap + (u * t.cos() + Vec3::Y * sign * t.sin()) * r;
                    out(m.transform_point3(p(a)), m.transform_point3(p(b)), c);
                }
            }
        }
    }
}

/// Emit wireframe lines for every collider and character controller in the world.
/// Uses the same sizing rules as the simulation (auto-fit, entity scale).
pub fn collider_debug_lines(world: &World, db: &AssetDatabase, out: &mut dyn FnMut(Vec3, Vec3, Color)) {
    for (e, cc) in world.query_ref::<(Entity, &CharacterController)>() {
        let (_, rot, pos) = world.global_matrix(e).to_scale_rotation_translation();
        let m = Mat4::from_rotation_translation(rot, pos);
        let r = cc.radius.max(0.01);
        upright(&m, Vec3::Y * cc.height * 0.5, (cc.height * 0.5 - r).max(0.01), r, true, CHARACTER, out);
    }
    let mut entities: Vec<(Entity, Option<Collider>)> = world.query_ref::<(Entity, &Collider)>().map(|(e, c)| (e, Some(c.clone()))).collect();
    for (e, _) in world.query_ref::<(Entity, &RigidBody)>() {
        if world.get::<Collider>(e).is_none() && world.get::<CharacterController>(e).is_none() {
            entities.push((e, None));
        }
    }
    for (e, c) in entities {
        if world.has::<CharacterController>(e) {
            continue;
        }
        let c = c.unwrap_or(Collider { auto_fit: true, ..Default::default() });
        let (scale, rot, pos) = world.global_matrix(e).to_scale_rotation_translation();
        let m = Mat4::from_rotation_translation(rot, pos);
        let scale = scale.abs().max(Vec3::splat(1e-4));
        let dynamic = world.get::<RigidBody>(e).is_some_and(|b| b.body_type != dumb_ecs::BodyType::Static);
        let color = if c.is_trigger {
            TRIGGER
        } else if dynamic {
            DYNAMIC
        } else {
            STATIC
        };
        let bounds = world.get::<MeshRenderer>(e).and_then(|mr| db.model_loaded(mr.model)).map(|md| md.aabb());
        let (size, offset) = match (c.auto_fit, bounds) {
            (true, Some(b)) => ((b.max - b.min).max(Vec3::splat(0.02)), b.center()),
            _ => (c.size, c.offset),
        };
        let size = (size * scale).max(Vec3::splat(0.01));
        let center = offset * scale;
        match c.shape {
            ColliderShape::Sphere => {
                let r = size.max_element() * 0.5;
                circle(&m, center, Vec3::X, Vec3::Y, r, color, out);
                circle(&m, center, Vec3::X, Vec3::Z, r, color, out);
                circle(&m, center, Vec3::Y, Vec3::Z, r, color, out);
            }
            ColliderShape::Capsule => {
                let r = size.x.max(size.z) * 0.5;
                upright(&m, center, (size.y * 0.5 - r).max(0.01), r, true, color, out);
            }
            ColliderShape::Cylinder => upright(&m, center, size.y * 0.5, size.x.max(size.z) * 0.5, false, color, out),
            // Hulls and meshes are shown by their bounds.
            _ => boxed(&m, center, size * 0.5, color, out),
        }
    }
}

//! Turn ECS worlds and models into draw lists.

use crate::types::*;
use dumb_asset::model::Trs;
use dumb_asset::{AssetDatabase, ModelData};
use dumb_core::{AssetId, Color, Entity, Frustum, Mat4, Vec3};
use dumb_ecs::{Animator, Camera, Light, LightKind, MeshRenderer, World};

#[derive(Default)]
pub struct ExtractOptions<'a> {
    pub selected: &'a [Entity],
    pub frustum_cull: bool,
    /// Draw model bounding boxes.
    pub show_bounds: bool,
}

/// Add every visible `MeshRenderer` in the world to `view`, plus lights.
/// Call `world.update_transforms()` first.
pub fn extract_world(world: &World, db: &mut AssetDatabase, view: &mut RenderView, opts: &ExtractOptions) -> u32 {
    let frustum = Frustum::from_view_proj(&view.view_proj());
    let mut culled = 0;
    let mut sun_found = false;
    let mut points = Vec::new();

    for (e, light) in world.query_ref::<(Entity, &Light)>() {
        let m = world.global_matrix(e);
        match light.kind {
            LightKind::Directional if !sun_found => {
                sun_found = true;
                view.lighting.sun_dir = m.transform_vector3(-Vec3::Z).normalize_or(Vec3::NEG_Y);
                view.lighting.sun_color = light.color;
                view.lighting.sun_intensity = light.intensity;
            }
            LightKind::Point => points.push(PointLightData {
                position: m.w_axis.truncate(),
                range: light.range,
                color: light.color,
                intensity: light.intensity,
            }),
            _ => {}
        }
    }
    if !points.is_empty() {
        let cam = view.camera_pos;
        points.sort_by(|a, b| (a.position - cam).length_squared().total_cmp(&(b.position - cam).length_squared()));
        points.truncate(8);
        view.lighting.points = points;
    }

    for (e, mr, anim) in world.query_ref::<(Entity, &MeshRenderer, Option<&Animator>)>() {
        if !mr.visible || mr.model.is_none() {
            continue;
        }
        let Some(model) = db.model(mr.model) else { continue };
        let selected = opts.selected.iter().any(|s| *s == e || world.is_ancestor(*s, e));
        let world_m = world.global_matrix(e);
        let pose = anim.and_then(|a| {
            let clip = model.find_clip(&a.clip)?;
            let blend = model.find_clip(&a.blend_clip).filter(|_| !a.blend_clip.is_empty()).map(|b| (b, a.time, a.blend));
            Some(model.sample_pose(Some((clip, a.time)), blend))
        });
        let first = view.draws.len();
        culled += push_model(view, &model, mr.model, mr.material, world_m, mr.tint, pose.as_deref(), selected, opts.frustum_cull.then_some(&frustum));
        if !mr.cast_shadows {
            for d in &mut view.draws[first..] {
                d.cast_shadows = false;
            }
        }
        if opts.show_bounds {
            view.aabb_lines(&model.aabb(), &world_m, Color::rgba(0.3, 0.9, 0.4, 0.8));
        }
    }
    culled
}

/// Add all meshes of a model. Returns how many primitives were frustum-culled.
#[allow(clippy::too_many_arguments)]
pub fn push_model(
    view: &mut RenderView,
    model: &ModelData,
    model_id: AssetId,
    material_override: AssetId,
    world_m: Mat4,
    tint: Color,
    pose: Option<&[Trs]>,
    selected: bool,
    frustum: Option<&Frustum>,
) -> u32 {
    // LOD: projected bounding-sphere height as a fraction of the screen.
    let lod = match view.lod_override {
        Some(l) => l as usize,
        None if model.lod_count() > 1 || model.cull_screen_size > 0.0 => {
            match model.select_lod(screen_size(view, &model.bounds(), &world_m)) {
                Some(l) => l,
                None => return 1,
            }
        }
        None => 0,
    };
    let mut culled = 0;
    let single = model.nodes.len() == 1 && pose.is_none();
    let globals = if single {
        vec![model.nodes[0].local.matrix()]
    } else {
        let rest;
        let p = match pose {
            Some(p) => p,
            None => {
                rest = model.rest_pose();
                &rest
            }
        };
        model.global_matrices(p)
    };

    for (ni, node) in model.nodes.iter().enumerate() {
        let Some(mesh_i) = node.mesh else { continue };
        if !model.node_visible_at_lod(ni, lod) {
            continue;
        }
        let mesh = &model.meshes[mesh_i];
        let node_m = world_m * globals[ni];
        let joint_offset = node.skin.filter(|s| *s < model.skins.len()).map(|s| {
            let off = view.joints.len() as u32;
            view.joints.extend(model.joint_matrices(s, &globals, globals[ni]));
            off
        });
        for (pi, prim) in mesh.primitives.iter().enumerate() {
            if let (Some(f), None) = (frustum, joint_offset) {
                if prim.aabb.is_valid() && !f.intersects_aabb(&prim.aabb.transformed(&node_m)) {
                    culled += 1;
                    continue;
                }
            }
            let material = if !material_override.is_none() {
                material_override
            } else {
                prim.material.map_or(AssetId::NONE, |m| model.material_id(model_id, m))
            };
            view.draws.push(DrawItem {
                model: model_id,
                mesh: mesh_i as u32,
                primitive: pi as u32,
                material,
                transform: node_m,
                tint,
                joint_offset,
                selected,
                lod: lod.min(255) as u8,
                cast_shadows: true,
                sphere: {
                    let b = prim.aabb.transformed(&node_m);
                    if b.is_valid() { b.center().extend((b.max - b.min).length() * 0.5) } else { dumb_core::Vec4::ZERO }
                },
            });
        }
    }
    culled
}

/// Draw a model's skeleton as overlay lines.
pub fn skeleton_lines(view: &mut RenderView, model: &ModelData, globals: &[Mat4], world_m: Mat4, highlight: Option<usize>) {
    for skin in &model.skins {
        for &j in &skin.joints {
            let p = world_m.transform_point3(globals[j].w_axis.truncate());
            if let Some(parent) = model.nodes[j].parent {
                let pp = world_m.transform_point3(globals[parent].w_axis.truncate());
                let c = if highlight == Some(j) { Color::YELLOW } else { Color::rgba(0.3, 0.8, 1.0, 1.0) };
                view.overlay_line(pp, p, c);
            }
            let s = 0.015 * (world_m.x_axis.truncate().length());
            view.overlay_line(p - Vec3::X * s, p + Vec3::X * s, Color::rgba(1.0, 1.0, 1.0, 0.7));
            view.overlay_line(p - Vec3::Y * s, p + Vec3::Y * s, Color::rgba(1.0, 1.0, 1.0, 0.7));
        }
    }
}

/// Camera matrices for an entity with `Camera` + transform.
/// Returns (view, projection, position, clear colour, draw skybox).
pub fn camera_view(world: &World, e: Entity, aspect: f32) -> Option<(Mat4, Mat4, Vec3, Color, bool)> {
    let cam = world.get::<Camera>(e)?;
    let m = world.global_matrix(e);
    let (_, rot, pos) = m.to_scale_rotation_translation();
    let view = Mat4::from_rotation_translation(rot, pos).inverse();
    Some((view, cam.projection_matrix(aspect), pos, cam.clear_color, cam.background == dumb_ecs::CameraBackground::Skybox))
}

/// First camera marked primary (or any camera).
pub fn find_primary_camera(world: &World) -> Option<Entity> {
    let mut any = None;
    for (e, c) in world.query_ref::<(Entity, &Camera)>() {
        if c.primary {
            return Some(e);
        }
        any.get_or_insert(e);
    }
    any
}

/// Projected height of a bounding box's sphere as a fraction of the screen (1 = fills it),
/// scaled by `view.lod_bias`. Used for LOD selection and small-object culling.
pub fn screen_size(view: &RenderView, bounds: &dumb_core::Aabb, world_m: &Mat4) -> f32 {
    let scale = world_m.x_axis.truncate().length().max(world_m.y_axis.truncate().length()).max(world_m.z_axis.truncate().length());
    let center = world_m.transform_point3(bounds.center());
    let radius = (bounds.max - bounds.min).length() * 0.5 * scale;
    let dist = (center - view.camera_pos).length().max(1e-3);
    if dist <= radius {
        return 1.0;
    }
    radius * view.proj.y_axis.y.abs() / dist * view.lod_bias
}

//! Physics simulation for Dumb Engine, backed by Rapier.
//!
//! The ECS components (`RigidBody`, `Collider`, `CharacterController` in `dumb_ecs`) are the
//! source of truth for configuration; [`Physics`] mirrors them into a Rapier world and writes the
//! simulated poses and velocities back every fixed step.
//!
//! Sync rules, per entity:
//! * a body is (re)built when its configuration, collider or world scale changes,
//! * dynamic/static bodies are teleported when their `Transform` was changed by someone else
//!   (editor gizmo, script),
//! * kinematic bodies follow their `Transform`,
//! * edits to `RigidBody::linear_velocity` / `angular_velocity` are pushed to the simulation,
//!   impulses and forces queued on the component are applied and cleared.

mod debug;

pub use debug::collider_debug_lines;

use dumb_asset::{AssetDatabase, ModelData};
use dumb_core::physics::{CollisionEvent, PhysicsQuery, RayHit};
use dumb_core::{Aabb, Entity, Mat4, Quat, Vec3};
use dumb_ecs::{BodyType, CharacterController, Collider, ColliderShape, MeshRenderer, RigidBody, Transform, World};
use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::prelude as rp;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

// ---------------------------------------------------------------- math conversion (glam 0.30 <-> rapier's glam)

fn v(a: Vec3) -> rp::Vector {
    rp::Vector::new(a.x, a.y, a.z)
}

fn uv(a: rp::Vector) -> Vec3 {
    Vec3::new(a.x, a.y, a.z)
}

fn q(r: Quat) -> rp::Rotation {
    rp::Rotation::from_xyzw(r.x, r.y, r.z, r.w)
}

fn uq(r: rp::Rotation) -> Quat {
    Quat::from_xyzw(r.x, r.y, r.z, r.w)
}

fn pose(t: Vec3, r: Quat) -> rp::Pose {
    rp::Pose::from_parts(v(t), q(r))
}

fn entity_data(e: Entity) -> u128 {
    e.to_bits() as u128
}

fn data_entity(d: u128) -> Entity {
    Entity::from_bits(d as u64)
}

/// What a link was built from; a difference means the body must be rebuilt.
#[derive(Clone)]
struct BuildKey {
    body: Option<RigidBody>,
    collider: Option<Collider>,
    character: Option<CharacterController>,
    scale: Vec3,
    model_ready: bool,
}

impl BuildKey {
    fn same(&self, o: &BuildKey) -> bool {
        let body = match (&self.body, &o.body) {
            (Some(a), Some(b)) => a.config_eq(b),
            (None, None) => true,
            _ => false,
        };
        let ch = match (&self.character, &o.character) {
            (Some(a), Some(b)) => a.config_eq(b),
            (None, None) => true,
            _ => false,
        };
        body && ch && self.collider == o.collider && (self.scale - o.scale).abs().max_element() < 1e-4 && self.model_ready == o.model_ready
    }
}

struct Link {
    body: rp::RigidBodyHandle,
    colliders: Vec<rp::ColliderHandle>,
    key: BuildKey,
    /// World pose written after the last step (to detect outside teleports).
    last_pos: Vec3,
    last_rot: Quat,
    last_linvel: Vec3,
    last_angvel: Vec3,
    kind: LinkKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkKind {
    Dynamic,
    Static,
    Kinematic,
    Character,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PhysicsStats {
    pub bodies: usize,
    pub colliders: usize,
    pub steps_last_frame: u32,
    pub step_ms: f32,
}

/// A running physics simulation for one `World`.
pub struct Physics {
    world: rp::PhysicsWorld,
    links: HashMap<Entity, Link>,
    accumulator: f32,
    /// Fixed simulation step in seconds.
    pub fixed_dt: f32,
    /// At most this many steps per frame (avoids the spiral of death after a hitch).
    pub max_steps: u32,
    events: Vec<CollisionEvent>,
    collision_rx: Receiver<rp::CollisionEvent>,
    collector: rp::ChannelEventCollector,
    pub stats: PhysicsStats,
}

impl Physics {
    pub fn new(gravity: Vec3) -> Self {
        let (ctx, crx) = channel();
        let (ftx, _frx) = channel();
        let (stx, _srx) = channel();
        let mut world = rp::PhysicsWorld::new();
        world.gravity = v(gravity);
        Physics {
            world,
            links: HashMap::new(),
            accumulator: 0.0,
            fixed_dt: 1.0 / 60.0,
            max_steps: 4,
            events: Vec::new(),
            collision_rx: crx,
            collector: rp::ChannelEventCollector::new(ctx, ftx, stx),
            stats: PhysicsStats::default(),
        }
    }

    pub fn set_gravity(&mut self, g: Vec3) {
        self.world.gravity = v(g);
    }

    /// Advance the simulation by `dt` seconds of game time (in fixed steps).
    /// Call `world.update_transforms()` before; call it again afterwards.
    pub fn step(&mut self, world: &mut World, db: &AssetDatabase, dt: f32) {
        let t0 = Instant::now();
        self.events.clear();
        self.accumulator = (self.accumulator + dt).min(self.fixed_dt * self.max_steps as f32);
        let mut steps = 0;
        self.sync_structure(world, db);
        while self.accumulator >= self.fixed_dt {
            self.accumulator -= self.fixed_dt;
            self.world.integration_parameters.dt = self.fixed_dt;
            self.push_state(world, steps == 0);
            self.move_characters(world);
            self.world.step_with_events(&(), &self.collector);
            self.pull_state(world);
            steps += 1;
        }
        while let Ok(ev) = self.collision_rx.try_recv() {
            let (h1, h2, started, flags) = match ev {
                rp::CollisionEvent::Started(a, b, f) => (a, b, true, f),
                rp::CollisionEvent::Stopped(a, b, f) => (a, b, false, f),
            };
            let (Some(c1), Some(c2)) = (self.world.colliders.get(h1), self.world.colliders.get(h2)) else { continue };
            self.events.push(CollisionEvent {
                a: data_entity(c1.user_data),
                b: data_entity(c2.user_data),
                started,
                trigger: flags.contains(rp::CollisionEventFlags::SENSOR),
            });
        }
        self.stats = PhysicsStats {
            bodies: self.world.bodies.len(),
            colliders: self.world.colliders.len(),
            steps_last_frame: steps,
            step_ms: t0.elapsed().as_secs_f32() * 1000.0,
        };
    }

    // ------------------------------------------------------------ structure sync

    fn sync_structure(&mut self, world: &World, db: &AssetDatabase) {
        let mut wanted: HashSet<Entity> = HashSet::new();
        for (e, _) in world.query_ref::<(Entity, &RigidBody)>() {
            wanted.insert(e);
        }
        for (e, _) in world.query_ref::<(Entity, &Collider)>() {
            wanted.insert(e);
        }
        for (e, _) in world.query_ref::<(Entity, &CharacterController)>() {
            wanted.insert(e);
        }

        let gone: Vec<Entity> = self.links.keys().filter(|e| !wanted.contains(e)).copied().collect();
        for e in gone {
            self.remove(e);
        }

        for e in wanted {
            let (scale, _, _) = world.global_matrix(e).to_scale_rotation_translation();
            let model = world.get::<MeshRenderer>(e).and_then(|mr| db.model_loaded(mr.model));
            let key = BuildKey {
                body: world.get::<RigidBody>(e).cloned(),
                collider: world.get::<Collider>(e).cloned(),
                character: world.get::<CharacterController>(e).cloned(),
                scale,
                model_ready: model.is_some(),
            };
            if self.links.get(&e).is_some_and(|l| l.key.same(&key)) {
                continue;
            }
            self.remove(e);
            self.build(e, world, key, model.as_deref());
        }
    }

    fn remove(&mut self, e: Entity) {
        if let Some(link) = self.links.remove(&e) {
            self.world.remove_body_with_colliders(link.body, true);
        }
    }

    fn build(&mut self, e: Entity, world: &World, key: BuildKey, model: Option<&ModelData>) {
        let m = world.global_matrix(e);
        let (scale, rot, pos) = m.to_scale_rotation_translation();
        let kind = if key.character.is_some() {
            LinkKind::Character
        } else {
            match key.body.as_ref().map(|b| b.body_type) {
                Some(BodyType::Dynamic) => LinkKind::Dynamic,
                Some(BodyType::Kinematic) => LinkKind::Kinematic,
                Some(BodyType::Static) | None => LinkKind::Static,
            }
        };
        let mut builder = match kind {
            LinkKind::Dynamic => rp::RigidBodyBuilder::dynamic(),
            LinkKind::Static => rp::RigidBodyBuilder::fixed(),
            LinkKind::Kinematic | LinkKind::Character => rp::RigidBodyBuilder::kinematic_position_based(),
        };
        builder = builder.pose(pose(pos, rot)).user_data(entity_data(e));
        if let Some(b) = &key.body {
            builder = builder
                .gravity_scale(b.gravity_scale)
                .linear_damping(b.linear_damping)
                .angular_damping(b.angular_damping)
                .ccd_enabled(b.ccd)
                .enabled_rotations(!b.lock_rotation_x, !b.lock_rotation_y, !b.lock_rotation_z)
                .linvel(v(b.linear_velocity))
                .angvel(v(b.angular_velocity));
        }
        let body = self.world.insert_body(builder);

        let mut colliders = Vec::new();
        let shapes = self.collider_shapes(&key, scale, model, kind);
        let total_shapes = shapes.len().max(1) as f32;
        for (shape, offset) in shapes {
            let c = key.collider.clone().unwrap_or_default();
            let mut cb = rp::ColliderBuilder::new(shape)
                .translation(v(offset))
                .friction(c.friction)
                .restitution(c.restitution)
                .sensor(c.is_trigger)
                .active_events(rp::ActiveEvents::COLLISION_EVENTS)
                .user_data(entity_data(e));
            match key.body.as_ref().filter(|b| b.mass > 0.0) {
                Some(b) => cb = cb.mass(b.mass / total_shapes),
                None => cb = cb.density(c.density.max(0.001)),
            }
            colliders.push(self.world.insert_collider(cb, Some(body)));
        }

        let (linvel, angvel) = key.body.as_ref().map_or((Vec3::ZERO, Vec3::ZERO), |b| (b.linear_velocity, b.angular_velocity));
        self.links.insert(e, Link { body, colliders, key, last_pos: pos, last_rot: rot, last_linvel: linvel, last_angvel: angvel, kind });
    }

    /// Shapes (with local offsets) for a link, in the entity's scaled space.
    fn collider_shapes(&self, key: &BuildKey, scale: Vec3, model: Option<&ModelData>, kind: LinkKind) -> Vec<(rp::SharedShape, Vec3)> {
        let scale = scale.abs().max(Vec3::splat(1e-4));
        if let Some(cc) = &key.character {
            let r = cc.radius.max(0.01);
            let half = (cc.height * 0.5 - r).max(0.01);
            return vec![(rp::SharedShape::capsule_y(half, r), Vec3::new(0.0, cc.height * 0.5, 0.0))];
        }
        let c = match &key.collider {
            Some(c) => c.clone(),
            // A RigidBody without a Collider gets a box around its model.
            None => Collider { auto_fit: true, ..Default::default() },
        };
        let bounds = model.map(|m| m.aabb());
        let (size, offset) = match (c.auto_fit, bounds) {
            (true, Some(b)) => ((b.max - b.min).max(Vec3::splat(0.02)), b.center()),
            _ => (c.size, c.offset),
        };
        let size = (size * scale).max(Vec3::splat(0.01));
        let offset = offset * scale;
        let hull_points = |m: &ModelData, nodes: &[usize]| -> Vec<rp::Vector> {
            let globals = m.global_matrices(&m.rest_pose());
            let mut pts = Vec::new();
            for &ni in nodes {
                let Some(mi) = m.nodes[ni].mesh else { continue };
                let nm = Mat4::from_scale(scale) * globals[ni];
                for p in &m.meshes[mi].primitives {
                    pts.extend(p.vertices.iter().map(|vx| v(nm.transform_point3(Vec3::from(vx.position)))));
                }
            }
            pts
        };
        let visible_nodes = |m: &ModelData| -> Vec<usize> { (0..m.nodes.len()).filter(|n| m.node_visible_by_default(*n)).collect() };

        let shape = match c.shape {
            ColliderShape::Box => rp::SharedShape::cuboid(size.x * 0.5, size.y * 0.5, size.z * 0.5),
            ColliderShape::Sphere => rp::SharedShape::ball(size.max_element() * 0.5),
            ColliderShape::Capsule => {
                let r = size.x.max(size.z) * 0.5;
                rp::SharedShape::capsule_y((size.y * 0.5 - r).max(0.01), r)
            }
            ColliderShape::Cylinder => rp::SharedShape::cylinder(size.y * 0.5, size.x.max(size.z) * 0.5),
            ColliderShape::ConvexHull => {
                match model.and_then(|m| rp::SharedShape::convex_hull(&hull_points(m, &visible_nodes(m)))) {
                    Some(s) => return vec![(s, Vec3::ZERO)],
                    None => rp::SharedShape::cuboid(size.x * 0.5, size.y * 0.5, size.z * 0.5),
                }
            }
            ColliderShape::Mesh => {
                if kind == LinkKind::Dynamic {
                    log::warn!("Mesh colliders can't be dynamic; using a convex hull instead");
                    if let Some(s) = model.and_then(|m| rp::SharedShape::convex_hull(&hull_points(m, &visible_nodes(m)))) {
                        return vec![(s, Vec3::ZERO)];
                    }
                }
                match model.and_then(|m| trimesh(m, scale)) {
                    Some(s) => return vec![(s, Vec3::ZERO)],
                    None => rp::SharedShape::cuboid(size.x * 0.5, size.y * 0.5, size.z * 0.5),
                }
            }
            ColliderShape::ModelCollision => {
                if let Some(m) = model {
                    let hulls: Vec<_> = m
                        .collision_nodes
                        .iter()
                        .filter_map(|n| rp::SharedShape::convex_hull(&hull_points(m, &[*n])))
                        .map(|s| (s, Vec3::ZERO))
                        .collect();
                    if !hulls.is_empty() {
                        return hulls;
                    }
                    log::warn!("model has no collision proxies (UCX_/UBX_ meshes); using its bounds");
                }
                rp::SharedShape::cuboid(size.x * 0.5, size.y * 0.5, size.z * 0.5)
            }
        };
        vec![(shape, offset)]
    }

    // ------------------------------------------------------------ per-step state

    /// ECS -> simulation: teleports, kinematic targets, velocity edits, impulses, forces.
    fn push_state(&mut self, world: &mut World, first_substep: bool) {
        for (e, link) in self.links.iter_mut() {
            let Some(rb) = self.world.bodies.get_mut(link.body) else { continue };
            let (_, rot, pos) = world.global_matrix(*e).to_scale_rotation_translation();
            match link.kind {
                LinkKind::Kinematic => rb.set_next_kinematic_position(pose(pos, rot)),
                LinkKind::Character => {}
                LinkKind::Static | LinkKind::Dynamic => {
                    if first_substep && ((pos - link.last_pos).length_squared() > 1e-8 || rot.angle_between(link.last_rot) > 1e-4) {
                        rb.set_position(pose(pos, rot), true);
                        link.last_pos = pos;
                        link.last_rot = rot;
                    }
                }
            }
            if link.kind == LinkKind::Dynamic {
                rb.reset_forces(false);
                if let Some(c) = world.get_mut::<RigidBody>(*e) {
                    if first_substep {
                        if c.linear_velocity != link.last_linvel {
                            rb.set_linvel(v(c.linear_velocity), true);
                        }
                        if c.angular_velocity != link.last_angvel {
                            rb.set_angvel(v(c.angular_velocity), true);
                        }
                        if c.impulse != Vec3::ZERO {
                            rb.apply_impulse(v(c.impulse), true);
                            c.impulse = Vec3::ZERO;
                        }
                        if c.torque_impulse != Vec3::ZERO {
                            rb.apply_torque_impulse(v(c.torque_impulse), true);
                            c.torque_impulse = Vec3::ZERO;
                        }
                    }
                    if c.force != Vec3::ZERO {
                        rb.add_force(v(c.force), true);
                    }
                }
            }
        }
    }

    /// Character controllers: gravity, jumping and collide-and-slide movement.
    fn move_characters(&mut self, world: &mut World) {
        let dt = self.fixed_dt;
        let gravity = uv(self.world.gravity);
        let chars: Vec<(Entity, rp::RigidBodyHandle, rp::ColliderHandle)> = self
            .links
            .iter()
            .filter(|(_, l)| l.kind == LinkKind::Character && !l.colliders.is_empty())
            .map(|(e, l)| (*e, l.body, l.colliders[0]))
            .collect();
        for (e, body, col) in chars {
            let Some(cc) = world.get_mut::<CharacterController>(e) else { continue };
            let mut vel = cc.velocity;
            vel.x = cc.desired_velocity.x;
            vel.z = cc.desired_velocity.z;
            if cc.grounded && cc.jump_request > 0.0 {
                vel.y = cc.jump_request;
            } else {
                vel.y += gravity.y * cc.gravity_scale * dt;
            }
            cc.jump_request = 0.0;
            let controller = KinematicCharacterController {
                up: rp::Vector::Y,
                offset: CharacterLength::Absolute(0.02),
                slide: true,
                autostep: (cc.step_height > 0.0).then_some(CharacterAutostep {
                    max_height: CharacterLength::Absolute(cc.step_height),
                    min_width: CharacterLength::Absolute(cc.radius * 0.5),
                    include_dynamic_bodies: true,
                }),
                max_slope_climb_angle: cc.max_slope_degrees.to_radians(),
                min_slope_slide_angle: cc.max_slope_degrees.to_radians(),
                snap_to_ground: (cc.snap_to_ground > 0.0).then_some(CharacterLength::Absolute(cc.snap_to_ground)),
                ..Default::default()
            };
            let Some(collider) = self.world.colliders.get(col) else { continue };
            let shape = collider.shared_shape().clone();
            let shape_pos = *collider.position();
            let movement = {
                let qp = self.world.query_pipeline_with_filter(rp::QueryFilter::default().exclude_rigid_body(body).exclude_sensors());
                controller.move_shape(dt, &qp, &*shape, &shape_pos, v(vel * dt), |_| {})
            };
            if movement.grounded && vel.y < 0.0 {
                vel.y = 0.0;
            }
            cc.grounded = movement.grounded;
            cc.velocity = vel;
            if let Some(rb) = self.world.bodies.get_mut(body) {
                let next = rb.translation() + movement.translation;
                let rot = *rb.rotation();
                rb.set_next_kinematic_position(rp::Pose::from_parts(next, rot));
            }
        }
    }

    /// Simulation -> ECS: poses of dynamic bodies and characters, velocities.
    fn pull_state(&mut self, world: &mut World) {
        for (e, link) in self.links.iter_mut() {
            if !matches!(link.kind, LinkKind::Dynamic | LinkKind::Character) {
                continue;
            }
            let Some(rb) = self.world.bodies.get(link.body) else { continue };
            let pos = uv(rb.translation());
            // Characters keep their own facing (scripts rotate them); only position comes back.
            let rot = if link.kind == LinkKind::Character { world.global_matrix(*e).to_scale_rotation_translation().1 } else { uq(*rb.rotation()) };
            let parent = world.parent(*e).map(|p| world.global_matrix(p));
            if let Some(t) = world.get_mut::<Transform>(*e) {
                let world_m = Mat4::from_scale_rotation_translation(t.scale, rot, pos);
                let local = match parent {
                    Some(pm) => pm.inverse() * world_m,
                    None => world_m,
                };
                let (_, lr, lt) = local.to_scale_rotation_translation();
                t.translation = lt;
                t.rotation = lr;
            }
            link.last_pos = pos;
            link.last_rot = rot;
            if link.kind == LinkKind::Dynamic {
                let (lv, av) = (uv(rb.linvel()), uv(rb.angvel()));
                if let Some(c) = world.get_mut::<RigidBody>(*e) {
                    c.linear_velocity = lv;
                    c.angular_velocity = av;
                    c.force = Vec3::ZERO;
                }
                link.last_linvel = lv;
                link.last_angvel = av;
            }
        }
        // Children of moved bodies need fresh world matrices for the next substep.
        world.update_transforms();
    }

    // ------------------------------------------------------------ queries

    pub fn body_count(&self) -> usize {
        self.links.len()
    }

    /// Entity's current physics bounds, if it has a body.
    pub fn entity_aabb(&self, e: Entity) -> Option<Aabb> {
        let link = self.links.get(&e)?;
        let mut out = Aabb::EMPTY;
        for h in &link.colliders {
            let a = self.world.colliders.get(*h)?.compute_aabb();
            out = out.union(&Aabb { min: uv(a.mins), max: uv(a.maxs) });
        }
        Some(out)
    }
}

fn trimesh(m: &ModelData, scale: Vec3) -> Option<rp::SharedShape> {
    let globals = m.global_matrices(&m.rest_pose());
    let mut verts = Vec::new();
    let mut idx = Vec::new();
    for (ni, node) in m.nodes.iter().enumerate() {
        let Some(mi) = node.mesh else { continue };
        if !m.node_visible_by_default(ni) {
            continue;
        }
        let nm = Mat4::from_scale(scale) * globals[ni];
        for p in &m.meshes[mi].primitives {
            let base = verts.len() as u32;
            verts.extend(p.vertices.iter().map(|vx| v(nm.transform_point3(Vec3::from(vx.position)))));
            idx.extend(p.indices.chunks_exact(3).map(|t| [base + t[0], base + t[1], base + t[2]]));
        }
    }
    if idx.is_empty() {
        return None;
    }
    rp::SharedShape::trimesh(verts, idx).ok()
}

impl PhysicsQuery for Physics {
    fn raycast(&self, origin: Vec3, dir: Vec3, max_distance: f32, exclude: Option<Entity>) -> Option<RayHit> {
        let len = dir.length();
        if len < 1e-9 {
            return None;
        }
        let ray = rp::Ray::new(v(origin), v(dir / len));
        let mut filter = rp::QueryFilter::default().exclude_sensors();
        if let Some(body) = exclude.and_then(|e| self.links.get(&e)).map(|l| l.body) {
            filter = filter.exclude_rigid_body(body);
        }
        let (h, hit) = self.world.cast_ray_and_get_normal(&ray, max_distance, true, filter)?;
        let c = self.world.colliders.get(h)?;
        let d = hit.time_of_impact;
        Some(RayHit { entity: data_entity(c.user_data), point: origin + dir / len * d, normal: uv(hit.normal), distance: d })
    }

    fn overlap_sphere(&self, center: Vec3, radius: f32) -> Vec<Entity> {
        let ball = rp::Ball::new(radius);
        let mut out: Vec<Entity> = self
            .world
            .intersect_shape(rp::Pose::from_translation(v(center)), &ball, rp::QueryFilter::default())
            .map(|(_, c)| data_entity(c.user_data))
            .collect();
        out.dedup();
        out
    }

    fn collision_events(&self) -> &[CollisionEvent] {
        &self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> AssetDatabase {
        let dir = std::env::temp_dir().join(format!("dumb_phys_test_{}", std::process::id()));
        AssetDatabase::open(&dir).unwrap()
    }

    #[test]
    fn ball_falls_and_rests_on_ground() {
        let db = db();
        let mut w = World::new();
        let ground = w.spawn_named("ground");
        w.insert(ground, Collider::cuboid(Vec3::new(20.0, 1.0, 20.0)));
        w.get_mut::<Transform>(ground).unwrap().translation = Vec3::new(0.0, -0.5, 0.0);
        let ball = w.spawn_named("ball");
        w.get_mut::<Transform>(ball).unwrap().translation = Vec3::new(0.0, 5.0, 0.0);
        w.insert(ball, RigidBody::dynamic());
        w.insert(ball, Collider::sphere(1.0));
        w.update_transforms();

        let mut p = Physics::new(Vec3::new(0.0, -9.81, 0.0));
        for _ in 0..240 {
            p.step(&mut w, &db, 1.0 / 60.0);
        }
        let y = w.get::<Transform>(ball).unwrap().translation.y;
        assert!((y - 0.5).abs() < 0.05, "ball should rest on the ground, y = {y}");
        let hit = p.raycast(Vec3::new(0.0, 10.0, 0.0), Vec3::NEG_Y, 100.0, None).unwrap();
        assert_eq!(hit.entity, ball);
        assert!((hit.point.y - 1.0).abs() < 0.05);
        assert_eq!(p.overlap_sphere(Vec3::new(0.0, 0.5, 0.0), 0.2), vec![ball]);
    }

    #[test]
    fn impulses_teleports_and_events() {
        let db = db();
        let mut w = World::new();
        let ground = w.spawn_named("ground");
        w.insert(ground, Collider::cuboid(Vec3::new(50.0, 1.0, 50.0)));
        w.get_mut::<Transform>(ground).unwrap().translation = Vec3::new(0.0, -0.5, 0.0);
        let cube = w.spawn_named("cube");
        w.insert(cube, RigidBody { gravity_scale: 0.0, ..RigidBody::dynamic() });
        w.insert(cube, Collider::cuboid(Vec3::ONE));
        w.get_mut::<Transform>(cube).unwrap().translation = Vec3::new(0.0, 3.0, 0.0);
        w.update_transforms();
        let mut p = Physics::new(Vec3::new(0.0, -9.81, 0.0));
        p.step(&mut w, &db, 1.0 / 60.0);

        // Impulse sideways: no gravity, so it slides along +X.
        w.get_mut::<RigidBody>(cube).unwrap().apply_impulse(Vec3::new(1.0, 0.0, 0.0));
        for _ in 0..60 {
            p.step(&mut w, &db, 1.0 / 60.0);
        }
        let t = w.get::<Transform>(cube).unwrap().translation;
        assert!(t.x > 0.5 && (t.y - 3.0).abs() < 1e-3, "{t:?}");
        assert!(w.get::<RigidBody>(cube).unwrap().linear_velocity.x > 0.5);

        // Teleport through the Transform (like the gizmo), then drop it onto the ground.
        w.get_mut::<Transform>(cube).unwrap().translation = Vec3::new(10.0, 2.0, 0.0);
        let rb = w.get_mut::<RigidBody>(cube).unwrap();
        rb.linear_velocity = Vec3::ZERO;
        rb.gravity_scale = 1.0;
        w.update_transforms();
        let mut started = false;
        for _ in 0..120 {
            p.step(&mut w, &db, 1.0 / 60.0);
            started |= p.collision_events().iter().any(|ev| ev.started && ev.other(cube) == Some(ground));
        }
        let t = w.get::<Transform>(cube).unwrap().translation;
        assert!((t.x - 10.0).abs() < 0.2 && (t.y - 0.5).abs() < 0.05, "{t:?}");
        assert!(started, "expected a collision event with the ground");
    }

    #[test]
    fn character_walks_and_stays_grounded() {
        let db = db();
        let mut w = World::new();
        let ground = w.spawn_named("ground");
        w.insert(ground, Collider::cuboid(Vec3::new(50.0, 1.0, 50.0)));
        w.get_mut::<Transform>(ground).unwrap().translation = Vec3::new(0.0, -0.5, 0.0);
        let wall = w.spawn_named("wall");
        w.insert(wall, Collider::cuboid(Vec3::new(1.0, 4.0, 10.0)));
        w.get_mut::<Transform>(wall).unwrap().translation = Vec3::new(5.0, 2.0, 0.0);
        let hero = w.spawn_named("hero");
        w.get_mut::<Transform>(hero).unwrap().translation = Vec3::new(0.0, 1.0, 0.0);
        w.insert(hero, CharacterController::default());
        w.update_transforms();
        let mut p = Physics::new(Vec3::new(0.0, -9.81, 0.0));
        for _ in 0..180 {
            w.get_mut::<CharacterController>(hero).unwrap().move_velocity(Vec3::new(3.0, 0.0, 0.0));
            p.step(&mut w, &db, 1.0 / 60.0);
        }
        let t = w.get::<Transform>(hero).unwrap().translation;
        let cc = w.get::<CharacterController>(hero).unwrap();
        assert!(cc.grounded, "should be grounded");
        assert!(t.y.abs() < 0.1, "feet on the ground, y = {}", t.y);
        // The wall's near face is at x = 4.5; the capsule radius keeps it ~0.35 away.
        assert!(t.x > 3.5 && t.x < 4.5, "blocked by the wall, x = {}", t.x);
    }
}

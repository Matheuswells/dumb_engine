//! Physics components. They are plain reflected data: the editor edits them, scenes save them,
//! scripts read and write them. The simulation backend (`dumb_physics`) syncs them every step.

use crate::World;
use dumb_core::Vec3;
use dumb_derive::{Component, Editor};

#[derive(Editor, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BodyType {
    /// Simulated: gravity, forces and collisions move it.
    #[default]
    Dynamic,
    /// Never moves (floors, walls). A `Collider` without a `RigidBody` is static too.
    Static,
    /// Moved by its `Transform` (scripts, animation); pushes dynamic bodies.
    Kinematic,
}

/// Makes an entity part of the physics simulation.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct RigidBody {
    pub body_type: BodyType,
    /// Total mass in kg. 0 = computed from the collider's density.
    #[editor(range = 0.0..=1000.0)]
    pub mass: f32,
    #[editor(range = -2.0..=4.0)]
    pub gravity_scale: f32,
    #[editor(range = 0.0..=10.0)]
    pub linear_damping: f32,
    #[editor(range = 0.0..=10.0)]
    pub angular_damping: f32,
    pub lock_rotation_x: bool,
    pub lock_rotation_y: bool,
    pub lock_rotation_z: bool,
    /// Continuous collision detection for fast objects.
    pub ccd: bool,
    /// Live velocity (m/s). Editing it while playing changes the body's velocity.
    #[editor(speed = 0.1)]
    pub linear_velocity: Vec3,
    /// Live angular velocity (rad/s).
    #[editor(speed = 0.1)]
    pub angular_velocity: Vec3,
    /// Impulse applied on the next step (use [`RigidBody::apply_impulse`]).
    #[editor(hidden)]
    pub impulse: Vec3,
    /// Force applied during the next step (use [`RigidBody::add_force`]).
    #[editor(hidden)]
    pub force: Vec3,
    #[editor(hidden)]
    pub torque_impulse: Vec3,
}

impl Default for RigidBody {
    fn default() -> Self {
        RigidBody {
            body_type: BodyType::Dynamic,
            mass: 0.0,
            gravity_scale: 1.0,
            linear_damping: 0.0,
            angular_damping: 0.05,
            lock_rotation_x: false,
            lock_rotation_y: false,
            lock_rotation_z: false,
            ccd: false,
            linear_velocity: Vec3::ZERO,
            angular_velocity: Vec3::ZERO,
            impulse: Vec3::ZERO,
            force: Vec3::ZERO,
            torque_impulse: Vec3::ZERO,
        }
    }
}

impl RigidBody {
    pub fn dynamic() -> Self {
        Self::default()
    }

    pub fn fixed() -> Self {
        RigidBody { body_type: BodyType::Static, ..Default::default() }
    }

    pub fn kinematic() -> Self {
        RigidBody { body_type: BodyType::Kinematic, ..Default::default() }
    }

    /// Instant change in momentum (N·s), applied at the next physics step.
    pub fn apply_impulse(&mut self, impulse: Vec3) {
        self.impulse += impulse;
    }

    /// Continuous force (N) for the next step. Call every frame for a sustained push.
    pub fn add_force(&mut self, force: Vec3) {
        self.force += force;
    }

    pub fn apply_torque_impulse(&mut self, torque: Vec3) {
        self.torque_impulse += torque;
    }

    /// The settings that define the body (everything except live state).
    pub fn config_eq(&self, o: &RigidBody) -> bool {
        self.body_type == o.body_type
            && self.mass == o.mass
            && self.gravity_scale == o.gravity_scale
            && self.linear_damping == o.linear_damping
            && self.angular_damping == o.angular_damping
            && self.lock_rotation_x == o.lock_rotation_x
            && self.lock_rotation_y == o.lock_rotation_y
            && self.lock_rotation_z == o.lock_rotation_z
            && self.ccd == o.ccd
    }
}

#[derive(Editor, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColliderShape {
    #[default]
    Box,
    Sphere,
    /// Vertical capsule.
    Capsule,
    /// Vertical cylinder.
    Cylinder,
    /// Convex hull of the entity's model (works for dynamic bodies).
    ConvexHull,
    /// Exact triangle mesh of the entity's model (static/kinematic only).
    Mesh,
    /// The model's imported collision proxies (UCX_/UBX_/USP_ meshes from Blender).
    ModelCollision,
}

/// Collision shape. Without a `RigidBody` the entity is static scenery.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct Collider {
    pub shape: ColliderShape,
    /// Full size before the entity's scale. Box: x/y/z. Sphere: x = diameter.
    /// Capsule/Cylinder: x = diameter, y = total height.
    #[editor(speed = 0.02)]
    pub size: Vec3,
    #[editor(speed = 0.02)]
    pub offset: Vec3,
    /// Fit size and offset to the `MeshRenderer` bounds when the simulation starts.
    pub auto_fit: bool,
    #[editor(range = 0.0..=2.0)]
    pub friction: f32,
    #[editor(range = 0.0..=1.0)]
    pub restitution: f32,
    /// kg/m³-ish relative density (used when the body's mass is 0).
    #[editor(range = 0.01..=100.0)]
    pub density: f32,
    /// Detects overlaps (collision events) without physical response.
    pub is_trigger: bool,
}

impl Default for Collider {
    fn default() -> Self {
        Collider {
            shape: ColliderShape::Box,
            size: Vec3::ONE,
            offset: Vec3::ZERO,
            auto_fit: true,
            friction: 0.6,
            restitution: 0.1,
            density: 1.0,
            is_trigger: false,
        }
    }
}

impl Collider {
    pub fn cuboid(size: Vec3) -> Self {
        Collider { shape: ColliderShape::Box, size, auto_fit: false, ..Default::default() }
    }

    pub fn sphere(diameter: f32) -> Self {
        Collider { shape: ColliderShape::Sphere, size: Vec3::splat(diameter), auto_fit: false, ..Default::default() }
    }

    pub fn capsule(diameter: f32, height: f32) -> Self {
        Collider { shape: ColliderShape::Capsule, size: Vec3::new(diameter, height, diameter), auto_fit: false, ..Default::default() }
    }

    /// Shape fitted to the entity's model bounds.
    pub fn fitted(shape: ColliderShape) -> Self {
        Collider { shape, auto_fit: true, ..Default::default() }
    }
}

/// Kinematic character movement: slides along walls, climbs steps and slopes, snaps to ground.
/// Scripts call [`CharacterController::move_velocity`] / [`CharacterController::jump`] each frame.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct CharacterController {
    /// Capsule height; the feet are at the entity's origin.
    #[editor(range = 0.2..=5.0)]
    pub height: f32,
    #[editor(range = 0.05..=2.0)]
    pub radius: f32,
    #[editor(range = 0.0..=1.0)]
    pub step_height: f32,
    #[editor(range = 0.0..=89.0)]
    pub max_slope_degrees: f32,
    #[editor(range = 0.0..=1.0)]
    pub snap_to_ground: f32,
    #[editor(range = 0.0..=4.0)]
    pub gravity_scale: f32,
    /// Horizontal velocity requested for the next step (m/s).
    #[editor(hidden)]
    pub desired_velocity: Vec3,
    /// Jump speed requested for the next step (consumed when grounded).
    #[editor(hidden)]
    pub jump_request: f32,
    #[editor(readonly)]
    pub grounded: bool,
    #[editor(readonly)]
    pub velocity: Vec3,
}

impl Default for CharacterController {
    fn default() -> Self {
        CharacterController {
            height: 1.8,
            radius: 0.35,
            step_height: 0.3,
            max_slope_degrees: 45.0,
            snap_to_ground: 0.2,
            gravity_scale: 1.0,
            desired_velocity: Vec3::ZERO,
            jump_request: 0.0,
            grounded: false,
            velocity: Vec3::ZERO,
        }
    }
}

impl CharacterController {
    /// Move horizontally at this velocity (m/s) during the next step.
    pub fn move_velocity(&mut self, v: Vec3) {
        self.desired_velocity = Vec3::new(v.x, 0.0, v.z);
    }

    /// Jump with this upward speed if grounded.
    pub fn jump(&mut self, speed: f32) {
        self.jump_request = speed;
    }

    pub fn config_eq(&self, o: &CharacterController) -> bool {
        self.height == o.height
            && self.radius == o.radius
            && self.step_height == o.step_height
            && self.max_slope_degrees == o.max_slope_degrees
            && self.snap_to_ground == o.snap_to_ground
            && self.gravity_scale == o.gravity_scale
    }
}

pub(crate) fn register_physics(w: &mut World) {
    w.register_type::<RigidBody>();
    w.register_type::<Collider>();
    w.register_type::<CharacterController>();
}

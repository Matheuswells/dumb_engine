//! Physics query types shared between the engine and script libraries.
//!
//! Scripts reach the physics backend through the [`PhysicsQuery`] trait object, so script
//! libraries never link the physics engine itself.

use crate::{Entity, Vec3};

/// Result of a ray cast.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub entity: Entity,
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

/// Two colliders started or stopped touching during the last physics step.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollisionEvent {
    pub a: Entity,
    pub b: Entity,
    /// `true` when contact started, `false` when it ended.
    pub started: bool,
    /// One of the colliders is a trigger (no physical response).
    pub trigger: bool,
}

impl CollisionEvent {
    /// The other entity if `e` is part of this event.
    pub fn other(&self, e: Entity) -> Option<Entity> {
        if self.a == e {
            Some(self.b)
        } else if self.b == e {
            Some(self.a)
        } else {
            None
        }
    }
}

/// Scene queries available to scripts while the game runs.
pub trait PhysicsQuery {
    /// First collider hit by a ray. `dir` does not need to be normalized.
    fn raycast(&self, origin: Vec3, dir: Vec3, max_distance: f32, exclude: Option<Entity>) -> Option<RayHit>;
    /// Entities whose colliders overlap a sphere.
    fn overlap_sphere(&self, center: Vec3, radius: f32) -> Vec<Entity>;
    /// Collision and trigger events from the last frame's physics steps.
    fn collision_events(&self) -> &[CollisionEvent];
}

/// Used when no physics world is running (edit mode, tools).
pub struct NoPhysics;

impl PhysicsQuery for NoPhysics {
    fn raycast(&self, _: Vec3, _: Vec3, _: f32, _: Option<Entity>) -> Option<RayHit> {
        None
    }
    fn overlap_sphere(&self, _: Vec3, _: f32) -> Vec<Entity> {
        Vec::new()
    }
    fn collision_events(&self) -> &[CollisionEvent] {
        &[]
    }
}

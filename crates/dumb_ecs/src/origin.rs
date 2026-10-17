//! Floating origin: keeps the action near (0, 0, 0) so `f32` transforms stay precise in
//! huge worlds.
//!
//! When the focus (the camera) drifts farther than a threshold from the origin, every root
//! entity is moved back by a grid-snapped amount and the shift is added to the [`WorldOrigin`]
//! resource, which holds the true position of the local origin in `f64`. Local `Transform`s
//! are what the simulation and renderer see; `WorldOrigin::to_world` / `to_local` convert.

use crate::components::{Parent, Transform};
use crate::World;
use dumb_core::{Entity, Vec3};
use dumb_derive::{Component, Editor};

/// Where the local origin sits in true world space (meters, `f64`). A resource: it shows
/// in the inspector and saves with the scene.
#[derive(Component, Editor, Clone, Copy, Debug, Default, PartialEq)]
pub struct WorldOrigin {
    #[editor(readonly)]
    pub x: f64,
    #[editor(readonly)]
    pub y: f64,
    #[editor(readonly)]
    pub z: f64,
}

impl WorldOrigin {
    /// True world position of a local position.
    pub fn to_world(&self, local: Vec3) -> [f64; 3] {
        [self.x + local.x as f64, self.y + local.y as f64, self.z + local.z as f64]
    }

    /// Local position of a true world position (precise as long as it is near the origin).
    pub fn to_local(&self, world: [f64; 3]) -> Vec3 {
        Vec3::new((world[0] - self.x) as f32, (world[1] - self.y) as f32, (world[2] - self.z) as f32)
    }
}

/// Settings of the floating origin (from `project.ron`).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct OriginSettings {
    /// Rebase when the focus is farther than this from the local origin (meters); 0 = off.
    pub threshold: f32,
    /// Shifts are multiples of this (meters), so repeated rebases never accumulate error.
    pub snap: f32,
}

impl Default for OriginSettings {
    fn default() -> Self {
        OriginSettings { threshold: 2000.0, snap: 1000.0 }
    }
}

/// Move the local origin if `focus` is too far from it. Returns the shift that was applied
/// to the local space (new local = old local − shift), or `None` when nothing moved.
/// Callers must shift anything else that stores local positions (physics) and refresh
/// the world matrices.
pub fn rebase(world: &mut World, focus: Vec3, settings: &OriginSettings) -> Option<Vec3> {
    if settings.threshold <= 0.0 || focus.abs().max_element() <= settings.threshold {
        return None;
    }
    let snap = settings.snap.max(1.0);
    let shift = (focus / snap).round() * snap;
    if shift == Vec3::ZERO {
        return None;
    }
    let roots: Vec<Entity> = world.query_ref::<(Entity, &Transform)>().map(|(e, _)| e).filter(|e| !world.has::<Parent>(*e)).collect();
    for e in roots {
        if let Some(t) = world.get_mut::<Transform>(e) {
            t.translation -= shift;
        }
    }
    let origin = world.resource_or_default::<WorldOrigin>();
    origin.x += shift.x as f64;
    origin.y += shift.y as f64;
    origin.z += shift.z as f64;
    Some(shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebases_roots_only_and_tracks_offset() {
        let mut w = World::new();
        let root = w.spawn();
        w.insert(root, Transform::from_translation(Vec3::new(5000.0, 1.0, -3000.0)));
        let child = w.spawn();
        w.insert(child, Transform::from_translation(Vec3::new(1.0, 2.0, 3.0)));
        w.insert(child, Parent { entity: root });
        let s = OriginSettings { threshold: 2000.0, snap: 1000.0 };
        let shift = rebase(&mut w, Vec3::new(5000.0, 1.0, -3000.0), &s).unwrap();
        assert_eq!(shift, Vec3::new(5000.0, 0.0, -3000.0));
        assert_eq!(w.get::<Transform>(root).unwrap().translation, Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(w.get::<Transform>(child).unwrap().translation, Vec3::new(1.0, 2.0, 3.0));
        let o = *w.resource::<WorldOrigin>().unwrap();
        assert_eq!(o.to_world(Vec3::ZERO), [5000.0, 0.0, -3000.0]);
        assert_eq!(o.to_local([5000.0, 1.0, -3000.0]), Vec3::new(0.0, 1.0, 0.0));
        // Near the origin again: nothing to do.
        assert!(rebase(&mut w, Vec3::ZERO, &s).is_none());
    }

    #[test]
    fn disabled_when_threshold_is_zero() {
        let mut w = World::new();
        let s = OriginSettings { threshold: 0.0, snap: 1000.0 };
        assert!(rebase(&mut w, Vec3::splat(1e6), &s).is_none());
    }

    #[test]
    fn keeps_precision_far_away() {
        let mut w = World::new();
        let e = w.spawn();
        w.insert(e, Transform::from_translation(Vec3::new(100_000.25, 0.0, 0.0)));
        let s = OriginSettings::default();
        rebase(&mut w, Vec3::new(100_000.25, 0.0, 0.0), &s);
        assert_eq!(w.get::<Transform>(e).unwrap().translation.x, 0.25);
    }
}

//! Building blocks for loading while playing: a streamed world cell component and
//! time-sliced scene instantiation.

use crate::components::Parent;
use crate::scene::SceneData;
use crate::World;
use dumb_core::{AssetId, Entity};
use dumb_derive::{Component, Editor};
use dumb_reflect::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A piece of the world stored in its own scene, loaded when the camera comes near and
/// unloaded when it moves away. Its entities are spawned as children of this entity, so the
/// cell's transform places them.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct StreamingCell {
    #[editor(asset = "scene")]
    pub scene: AssetId,
    /// Start loading when the camera is closer than this (meters).
    #[editor(range = 1.0..=10000.0)]
    pub load_distance: f32,
    /// Unload when farther than this. Keep it larger than `load_distance` to avoid flicker.
    #[editor(range = 1.0..=20000.0)]
    pub unload_distance: f32,
    /// Current state, for display: 0 unloaded, 1 loading, 2 loaded.
    #[editor(readonly)]
    pub state: u32,
    /// Entities currently spawned from this cell.
    #[editor(readonly)]
    pub entities: u32,
}

impl Default for StreamingCell {
    fn default() -> Self {
        StreamingCell { scene: AssetId::NONE, load_distance: 150.0, unload_distance: 200.0, state: 0, entities: 0 }
    }
}

/// Instantiates scene data a slice at a time so a big load doesn't stall a frame.
pub struct IncrementalSpawn {
    data: Arc<SceneData>,
    map: HashMap<u64, u64>,
    /// Entities in data order (all ids are reserved up front).
    pub spawned: Vec<Entity>,
    next: usize,
    parent: Option<Entity>,
}

impl IncrementalSpawn {
    /// Reserve every entity id now (cheap); components are inserted by `step`.
    pub fn new(data: Arc<SceneData>, world: &mut World, parent: Option<Entity>) -> Self {
        let mut map = HashMap::with_capacity(data.entities.len());
        let spawned: Vec<Entity> = data
            .entities
            .iter()
            .map(|d| {
                let e = world.spawn();
                map.insert(d.id, e.to_bits());
                e
            })
            .collect();
        IncrementalSpawn { data, map, spawned, next: 0, parent }
    }

    pub fn total(&self) -> usize {
        self.data.entities.len()
    }

    pub fn done(&self) -> usize {
        self.next
    }

    pub fn is_finished(&self) -> bool {
        self.next >= self.data.entities.len()
    }

    /// Insert components until `budget` is used up. Returns how many entities were finished.
    pub fn step(&mut self, world: &mut World, budget: Duration) -> usize {
        let t0 = Instant::now();
        let parent_name = <Parent as dumb_reflect::Typed>::TYPE_NAME;
        let start = self.next;
        while self.next < self.data.entities.len() {
            let d = &self.data.entities[self.next];
            let e = self.spawned[self.next];
            let mut has_parent = false;
            for (name, v) in &d.components {
                let mut v = v.clone();
                v.visit_mut(&mut |x| {
                    if let Value::Entity(bits) = x {
                        if let Some(n) = self.map.get(bits) {
                            *bits = *n;
                        }
                    }
                });
                has_parent |= name == parent_name;
                world.insert_by_name(e, name, &v);
            }
            // Top-level entities hang under the cell.
            if let (Some(p), false) = (self.parent, has_parent) {
                world.insert(e, Parent { entity: p });
            }
            self.next += 1;
            // Check the clock every few entities (Instant::now is not free).
            if (self.next - start) % 16 == 0 && t0.elapsed() >= budget {
                break;
            }
        }
        self.next - start
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Name, Transform};

    #[test]
    fn spawns_in_slices_with_remapped_parents() {
        let mut src = World::new();
        let root = src.spawn_named("root");
        for i in 0..200 {
            let c = src.spawn_named(&format!("c{i}"));
            src.insert(c, Parent { entity: root });
        }
        let data = Arc::new(SceneData::capture(&src));

        let mut w = World::new();
        let cell = w.spawn_named("cell");
        let mut job = IncrementalSpawn::new(data, &mut w, Some(cell));
        let mut steps = 0;
        while !job.is_finished() {
            job.step(&mut w, Duration::ZERO); // smallest slices
            steps += 1;
        }
        assert!(steps > 1, "work was split");
        let new_root = job.spawned[0];
        assert_eq!(w.get::<Name>(new_root).unwrap().name, "root");
        assert_eq!(w.get::<Parent>(new_root).unwrap().entity, cell, "top level goes under the cell");
        let kids = w.children(new_root);
        assert_eq!(kids.len(), 200, "children point at the new root");
        assert!(w.get::<Transform>(kids[0]).is_some());
    }
}

//! Scene and prefab serialization built on reflection.

use crate::components::{Parent, PrefabInstance};
use crate::World;
use dumb_core::{AssetId, Entity};
use dumb_reflect::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EntityData {
    /// Entity bits at capture time; references inside components point at these.
    pub id: u64,
    pub components: Vec<(String, Value)>,
}

/// A serialized set of entities: a whole scene, a prefab, or a play-mode snapshot.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SceneData {
    pub entities: Vec<EntityData>,
}

impl SceneData {
    fn capture_entity(world: &World, e: Entity, skip_parent: bool) -> EntityData {
        let mut components = Vec::new();
        for id in world.components_of(e) {
            let desc = world.descriptor(id).unwrap();
            if skip_parent && desc.name == <Parent as dumb_reflect::Typed>::TYPE_NAME {
                continue;
            }
            let r = world.get_reflect(e, id).unwrap();
            components.push((desc.name.clone(), dumb_reflect::to_value(r)));
        }
        for (name, v) in world.missing_components(e) {
            components.push((name.clone(), v.clone()));
        }
        EntityData { id: e.to_bits(), components }
    }

    /// Capture every entity in the world.
    pub fn capture(world: &World) -> Self {
        SceneData { entities: world.entities().map(|e| Self::capture_entity(world, e, false)).collect() }
    }

    /// Capture `root` and all of its descendants. The root's parent link is dropped.
    pub fn capture_subtree(world: &World, root: Entity) -> Self {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            out.push(Self::capture_entity(world, e, e == root));
            let mut ch = world.children(e);
            ch.reverse();
            stack.extend(ch);
        }
        SceneData { entities: out }
    }

    /// Spawn fresh entities for everything in this data, remapping entity references.
    /// Returns the spawned entities in data order (index 0 is the root for prefabs).
    pub fn instantiate(&self, world: &mut World) -> Vec<Entity> {
        let mut map = HashMap::new();
        let spawned: Vec<Entity> = self
            .entities
            .iter()
            .map(|d| {
                let e = world.spawn();
                map.insert(d.id, e.to_bits());
                e
            })
            .collect();
        for (d, e) in self.entities.iter().zip(&spawned) {
            for (name, v) in &d.components {
                let mut v = v.clone();
                v.visit_mut(&mut |x| {
                    if let Value::Entity(bits) = x {
                        if let Some(n) = map.get(bits) {
                            *bits = *n;
                        }
                    }
                });
                world.insert_by_name(*e, name, &v);
            }
        }
        world.update_transforms();
        spawned
    }

    /// Replace the world's contents with this data, keeping the exact entity ids.
    /// Used for play-mode stop and undo so editor selections stay valid.
    pub fn restore_exact(&self, world: &mut World) {
        world.clear();
        for d in &self.entities {
            world.spawn_at(Entity::from_bits(d.id));
        }
        for d in &self.entities {
            let e = Entity::from_bits(d.id);
            for (name, v) in &d.components {
                world.insert_by_name(e, name, v);
            }
        }
        world.update_transforms();
    }

    /// Instantiate as a prefab: the root gets a `PrefabInstance` link.
    pub fn instantiate_prefab(&self, world: &mut World, prefab: AssetId) -> Option<Entity> {
        let spawned = self.instantiate(world);
        let root = *spawned.first()?;
        world.insert(root, PrefabInstance { prefab });
        Some(root)
    }

    /// Asset ids referenced anywhere in the data (for dependency tracking).
    pub fn asset_refs(&self) -> Vec<AssetId> {
        let mut out = Vec::new();
        for d in &self.entities {
            for (_, v) in &d.components {
                v.visit(&mut |x| {
                    if let Value::Asset(a) = x {
                        if !a.is_none() && !out.contains(a) {
                            out.push(*a);
                        }
                    }
                });
            }
        }
        out
    }

    pub fn to_ron(&self) -> Result<String, ron::Error> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default().depth_limit(8))
    }

    pub fn from_ron(s: &str) -> Result<Self, ron::error::SpannedError> {
        ron::from_str(s)
    }
}

impl World {
    /// Make a specific entity id alive (for exact restores).
    pub fn spawn_at(&mut self, e: Entity) {
        self.spawn_at_impl(e);
    }
}

//! Dumb Engine ECS.
//!
//! Sparse-set storage with type-erased columns. Component types are identified by their
//! stable type *name*, so components defined in hot-reloaded script libraries work exactly
//! like engine components, and their data survives a reload (see [`World::unregister`]).

extern crate self as dumb_ecs;

mod blob;
pub mod ai;
pub mod commands;
pub mod components;
pub mod hud;
pub mod physics;
mod query;
pub mod scene;
pub mod streaming;
mod world;

pub use ai::{AiAgent, AiSettings, AiStats};
pub use commands::Commands;
pub use hud::{Anchor, Hud, HudCmd, HudPos, UiBar, UiButton, UiImage, UiPanel, UiText, UiWeb, UiWorldLabel};
pub use components::*;
pub use physics::{BodyType, CharacterController, Collider, ColliderShape, RigidBody};
pub use dumb_derive::{Component, Editor};
pub use query::{QueryData, QueryIter, ReadOnlyQueryData, With, Without};
pub use scene::{EntityData, SceneData};
pub use streaming::{IncrementalSpawn, StreamingCell};
pub use world::World;

use dumb_reflect::{Reflect, Typed};
use std::alloc::Layout;
use std::any::TypeId;

/// A component type. Implement with `#[derive(Component, Editor, Default)]`.
pub trait Component: Typed {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentId(pub u32);

/// Everything the world needs to store a component type without knowing it statically.
#[derive(Clone)]
pub struct ComponentDescriptor {
    pub name: String,
    pub type_id: TypeId,
    pub layout: Layout,
    pub drop: unsafe fn(*mut u8),
    pub reflect: unsafe fn(*mut u8) -> *mut dyn Reflect,
    pub construct_default: unsafe fn(*mut u8),
    /// Which script library registered this type (`None` for engine types).
    pub source: Option<String>,
}

impl ComponentDescriptor {
    pub fn of<T: Component>() -> Self {
        unsafe fn drop_t<T>(p: *mut u8) {
            std::ptr::drop_in_place(p as *mut T)
        }
        unsafe fn reflect_t<T: Reflect>(p: *mut u8) -> *mut dyn Reflect {
            p as *mut T as *mut dyn Reflect
        }
        unsafe fn default_t<T: Default>(p: *mut u8) {
            (p as *mut T).write(T::default())
        }
        ComponentDescriptor {
            name: T::TYPE_NAME.to_string(),
            type_id: TypeId::of::<T>(),
            layout: Layout::new::<T>(),
            drop: drop_t::<T>,
            reflect: reflect_t::<T>,
            construct_default: default_t::<T>,
            source: None,
        }
    }

    /// Short name for display (`game_scripts::Player` -> `Player`).
    pub fn short_name(&self) -> &str {
        self.name.rsplit("::").next().unwrap_or(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumb_core::Vec3;

    #[derive(Component, Editor, Default, Debug, PartialEq)]
    struct Health {
        #[editor(range = 0.0..=100.0)]
        hp: f32,
        name: String,
    }

    #[test]
    fn insert_query_remove() {
        let mut w = World::new();
        let a = w.spawn_named("a");
        let b = w.spawn_named("b");
        w.insert(a, Health { hp: 10.0, name: "x".into() });
        for (t, h) in w.query::<(&mut Transform, &Health)>() {
            t.translation = Vec3::splat(h.hp);
        }
        assert_eq!(w.get::<Transform>(a).unwrap().translation, Vec3::splat(10.0));
        assert_eq!(w.get::<Transform>(b).unwrap().translation, Vec3::ZERO);
        w.despawn(a);
        assert!(w.get::<Health>(a).is_none());
        assert_eq!(w.query::<&Name>().count(), 1);
    }

    #[test]
    fn scene_roundtrip_and_unregister() {
        let mut w = World::new();
        let p = w.spawn_named("parent");
        let c = w.spawn_named("child");
        w.insert(c, Health { hp: 42.0, name: "hero".into() });
        w.set_parent(c, Some(p));
        let data = SceneData::capture(&w);
        let ron = data.to_ron().unwrap();
        let mut w2 = World::new();
        w2.register_type::<Health>();
        let spawned = SceneData::from_ron(&ron).unwrap().instantiate(&mut w2);
        let c2 = spawned[1];
        assert_eq!(w2.get::<Health>(c2).unwrap().hp, 42.0);
        assert_eq!(w2.parent(c2), Some(spawned[0]));

        // Unregister keeps the data as "missing" and restores it on re-register.
        w2.unregister(Health::TYPE_NAME);
        assert_eq!(w2.missing_components(c2).len(), 1);
        w2.register_type::<Health>();
        assert_eq!(w2.get::<Health>(c2).unwrap().name, "hero");
    }

    #[test]
    fn restore_exact_keeps_ids() {
        let mut w = World::new();
        let _a = w.spawn_named("a");
        let b = w.spawn_named("b");
        let snap = SceneData::capture(&w);
        w.despawn(b);
        let x = w.spawn_named("x");
        snap.restore_exact(&mut w);
        assert!(w.is_alive(b));
        assert!(!w.is_alive(x) || x == b);
        assert_eq!(w.get::<Name>(b).unwrap().name, "b");
    }
}

#[cfg(test)]
mod rename_tests {
    use super::*;

    mod moved {
        use dumb_derive::{Component, Editor};
        #[derive(Component, Editor, Default, Debug)]
        pub struct Health {
            pub hp: f32,
        }
    }

    #[test]
    fn data_follows_type_to_new_module() {
        let mut w = World::new();
        let e = w.spawn();
        // Saved under the old path `dumb_ecs::rename_tests::Health`.
        let v = dumb_reflect::Value::Struct(vec![("hp".into(), dumb_reflect::Value::Float(7.0))]);
        w.insert_by_name(e, "dumb_ecs::rename_tests::Health", &v);
        assert_eq!(w.missing_components(e).len(), 1);
        w.register_type::<moved::Health>();
        assert_eq!(w.get::<moved::Health>(e).unwrap().hp, 7.0);
        assert!(w.missing_components(e).is_empty());
    }
}

#[cfg(test)]
mod par_tests {
    use super::*;
    use dumb_core::Vec3;

    #[test]
    fn par_for_each_visits_every_entity_once() {
        let mut w = World::new();
        let mut es = Vec::new();
        for i in 0..10_000 {
            let e = w.spawn_named("e");
            if i % 3 != 0 {
                w.insert(e, MeshRenderer::default());
            }
            es.push(e);
        }
        w.par_for_each::<(&mut Transform, &MeshRenderer), _>(|(t, _)| t.translation += Vec3::X);
        for (i, e) in es.iter().enumerate() {
            let x = w.get::<Transform>(*e).unwrap().translation.x;
            assert_eq!(x, if i % 3 != 0 { 1.0 } else { 0.0 });
        }
        let count = std::sync::atomic::AtomicUsize::new(0);
        w.par_for_each_ref::<&Transform, _>(|_| {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        assert_eq!(count.into_inner(), 10_000);
    }
}

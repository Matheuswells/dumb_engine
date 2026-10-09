//! Deferred structural changes, typed events and singleton resources.

use crate::components::Name;
use crate::{Component, World};
use dumb_core::Entity;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Mutex;

type Command = Box<dyn FnOnce(&mut World) + Send>;

/// Structural changes recorded while iterating (even from `par_for_each` threads) and applied
/// afterwards with [`Commands::apply`].
///
/// ```ignore
/// let cmds = Commands::default();
/// ctx.world.par_for_each::<(Entity, &Health), _>(|(e, h)| if h.hp <= 0.0 { cmds.despawn(e) });
/// cmds.apply(ctx.world);
/// ```
#[derive(Default)]
pub struct Commands {
    queue: Mutex<Vec<Command>>,
}

impl Commands {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue any change to the world.
    pub fn add(&self, f: impl FnOnce(&mut World) + Send + 'static) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).push(Box::new(f));
    }

    pub fn despawn(&self, e: Entity) {
        self.add(move |w| w.despawn_recursive(e));
    }

    pub fn insert<T: Component + Send>(&self, e: Entity, c: T) {
        self.add(move |w| {
            if w.is_alive(e) {
                w.insert(e, c);
            }
        });
    }

    pub fn remove<T: Component>(&self, e: Entity) {
        self.add(move |w| {
            w.remove::<T>(e);
        });
    }

    /// Spawn an entity and let `init` add its components.
    pub fn spawn(&self, init: impl FnOnce(&mut World, Entity) + Send + 'static) {
        self.add(move |w| {
            let e = w.spawn();
            init(w, e);
        });
    }

    pub fn len(&self) -> usize {
        self.queue.lock().map(|q| q.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Run every queued command, in the order they were added.
    pub fn apply(&self, world: &mut World) {
        let cmds = std::mem::take(&mut *self.queue.lock().unwrap_or_else(|e| e.into_inner()));
        for c in cmds {
            c(world);
        }
    }
}

/// Typed event queues, double-buffered per frame.
#[derive(Default)]
pub struct Events {
    queues: HashMap<&'static str, (Vec<Box<dyn Any + Send + Sync>>, Vec<Box<dyn Any + Send + Sync>>)>,
}

impl World {
    /// Send an event. Readers get it next frame from [`World::events`].
    pub fn send_event<E: Any + Send + Sync>(&mut self, e: E) {
        self.events.queues.entry(std::any::type_name::<E>()).or_default().1.push(Box::new(e));
    }

    /// Events of type `E` sent during the previous frame. Every reader sees each event exactly
    /// once, whatever order systems run in.
    pub fn events<E: Any + Send + Sync>(&self) -> impl Iterator<Item = &E> {
        self.events.queues.get(std::any::type_name::<E>()).into_iter().flat_map(|(prev, _)| prev.iter().filter_map(|b| b.downcast_ref::<E>()))
    }

    /// Events of type `E` sent so far this frame (for same-frame reactions).
    pub fn events_this_frame<E: Any + Send + Sync>(&self) -> impl Iterator<Item = &E> {
        self.events.queues.get(std::any::type_name::<E>()).into_iter().flat_map(|(_, cur)| cur.iter().filter_map(|b| b.downcast_ref::<E>()))
    }

    /// End of frame: this frame's events become readable, last frame's are dropped.
    pub fn update_events(&mut self) {
        for (prev, cur) in self.events.queues.values_mut() {
            *prev = std::mem::take(cur);
        }
    }

    /// Drop all events (before unloading the script library that defined their types).
    pub fn clear_events(&mut self) {
        self.events.queues.clear();
    }

    /// A singleton component ("resource"): the first entity that has `T`.
    pub fn resource<T: Component>(&self) -> Option<&T> {
        self.query_ref::<&T>().next()
    }

    pub fn resource_mut<T: Component>(&mut self) -> Option<&mut T> {
        self.query::<&mut T>().next()
    }

    /// Set a resource, creating its entity (named `Resource: <type>`) the first time. Being a
    /// component, it shows in the inspector, saves with the scene and survives hot reload.
    pub fn insert_resource<T: Component>(&mut self, value: T) -> Entity {
        if let Some(e) = self.query_ref::<(Entity, &T)>().map(|(e, _)| e).next() {
            self.insert(e, value);
            return e;
        }
        let e = self.spawn();
        let short = std::any::type_name::<T>().rsplit("::").next().unwrap_or("?");
        self.insert(e, Name { name: format!("Resource: {short}") });
        self.insert(e, value);
        e
    }

    /// The resource, inserting `T::default()` when missing.
    pub fn resource_or_default<T: Component + Default>(&mut self) -> &mut T {
        if self.resource::<T>().is_none() {
            self.insert_resource(T::default());
        }
        self.resource_mut::<T>().expect("just inserted")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Transform;
    use dumb_derive::{Component, Editor};

    #[derive(Component, Editor, Default, Debug, Clone)]
    struct Score {
        points: u32,
    }

    struct Hit(u32);

    #[test]
    fn commands_from_parallel_queries() {
        let mut w = World::new();
        for i in 0..1000 {
            let e = w.spawn_named("x");
            w.get_mut::<Transform>(e).unwrap().translation.x = i as f32;
        }
        let cmds = Commands::new();
        w.par_for_each::<(Entity, &Transform), _>(|(e, t)| {
            if t.translation.x >= 500.0 {
                cmds.despawn(e);
            }
        });
        assert_eq!(cmds.len(), 500);
        cmds.apply(&mut w);
        assert_eq!(w.entity_count(), 500);
    }

    #[test]
    fn events_arrive_next_frame_once() {
        let mut w = World::new();
        w.send_event(Hit(3));
        w.send_event(Hit(4));
        assert_eq!(w.events::<Hit>().count(), 0, "not before the frame ends");
        assert_eq!(w.events_this_frame::<Hit>().map(|h| h.0).sum::<u32>(), 7);
        w.update_events();
        assert_eq!(w.events::<Hit>().map(|h| h.0).collect::<Vec<_>>(), [3, 4]);
        w.update_events();
        assert_eq!(w.events::<Hit>().count(), 0, "dropped after one frame");
    }

    #[test]
    fn resources_are_singleton_components() {
        let mut w = World::new();
        assert!(w.resource::<Score>().is_none());
        w.resource_or_default::<Score>().points += 5;
        w.resource_or_default::<Score>().points += 1;
        assert_eq!(w.resource::<Score>().unwrap().points, 6);
        let e = w.insert_resource(Score { points: 1 });
        assert_eq!(w.get::<Name>(e).unwrap().name, "Resource: Score");
        assert_eq!(w.query_ref::<&Score>().count(), 1);
    }
}

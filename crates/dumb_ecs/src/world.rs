use crate::blob::BlobVec;
use crate::components::{Name, Parent, Transform};
use crate::{Component, ComponentDescriptor, ComponentId};
use dumb_core::{Entity, Mat4};
use dumb_reflect::{Reflect, Value};
use std::any::TypeId;
use std::collections::HashMap;

const NO_SLOT: u32 = u32::MAX;

/// Sparse set storage for one component type.
#[doc(hidden)]
pub struct Column {
    pub data: BlobVec,
    pub entities: Vec<Entity>,
    /// entity index -> dense index
    pub sparse: Vec<u32>,
}

impl Column {
    fn new(desc: &ComponentDescriptor) -> Self {
        Column { data: BlobVec::new(desc.layout, desc.drop), entities: Vec::new(), sparse: Vec::new() }
    }

    #[inline]
    pub fn dense_index(&self, e: Entity) -> Option<usize> {
        let i = *self.sparse.get(e.index as usize)?;
        if i == NO_SLOT || self.entities[i as usize] != e {
            return None;
        }
        Some(i as usize)
    }

    #[inline]
    pub fn get_ptr(&self, e: Entity) -> Option<*mut u8> {
        self.dense_index(e).map(|i| self.data.get_ptr(i))
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Insert or replace. `write` initializes the slot.
    unsafe fn insert_with(&mut self, e: Entity, drop: unsafe fn(*mut u8), write: impl FnOnce(*mut u8)) {
        if let Some(i) = self.dense_index(e) {
            let p = self.data.get_ptr(i);
            drop(p);
            write(p);
            return;
        }
        let idx = e.index as usize;
        if self.sparse.len() <= idx {
            self.sparse.resize(idx + 1, NO_SLOT);
        }
        self.sparse[idx] = self.entities.len() as u32;
        self.entities.push(e);
        self.data.push_with(write);
    }

    fn remove(&mut self, e: Entity) -> bool {
        let Some(i) = self.dense_index(e) else { return false };
        self.data.swap_remove_drop(i);
        self.entities.swap_remove(i);
        if i < self.entities.len() {
            let moved = self.entities[i];
            self.sparse[moved.index as usize] = i as u32;
        }
        self.sparse[e.index as usize] = NO_SLOT;
        true
    }
}

#[derive(Default)]
struct Entities {
    generations: Vec<u32>,
    alive: Vec<bool>,
    free: Vec<u32>,
    count: usize,
}

/// The ECS world: entities, their components and derived transform data.
pub struct World {
    entities: Entities,
    pub(crate) descriptors: Vec<Option<ComponentDescriptor>>,
    by_name: HashMap<String, ComponentId>,
    by_type: HashMap<TypeId, ComponentId>,
    pub(crate) columns: Vec<Option<Column>>,
    /// Components whose type is not currently registered (e.g. a script library that
    /// is unloaded or failed to build). Kept so nothing is lost on save or hot reload.
    missing: HashMap<Entity, Vec<(String, Value)>>,
    /// World matrices, indexed by entity index. Updated by [`World::update_transforms`].
    global: Vec<Mat4>,
    /// Bumped on every structural change; lets caches (hierarchy view) know to rebuild.
    pub structure_version: u64,
    pub(crate) events: crate::commands::Events,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// A new world with the engine's built-in components registered.
    pub fn new() -> Self {
        let mut w = World {
            entities: Entities::default(),
            descriptors: Vec::new(),
            by_name: HashMap::new(),
            by_type: HashMap::new(),
            columns: Vec::new(),
            missing: HashMap::new(),
            events: Default::default(),
            global: Vec::new(),
            structure_version: 0,
        };
        crate::components::register_builtin(&mut w);
        w
    }

    // ---------------------------------------------------------------- registry

    /// Register a component type. Re-registering a name replaces the descriptor (used by
    /// script hot reload after the old library's components were extracted).
    pub fn register(&mut self, desc: ComponentDescriptor) -> ComponentId {
        if let Some(&id) = self.by_name.get(&desc.name) {
            if self.descriptors[id.0 as usize].is_none() {
                self.by_type.insert(desc.type_id, id);
                self.columns[id.0 as usize] = Some(Column::new(&desc));
                self.descriptors[id.0 as usize] = Some(desc);
                self.resolve_missing(id);
            } else {
                self.by_type.insert(desc.type_id, id);
            }
            return id;
        }
        let id = ComponentId(self.descriptors.len() as u32);
        self.by_name.insert(desc.name.clone(), id);
        self.by_type.insert(desc.type_id, id);
        self.columns.push(Some(Column::new(&desc)));
        self.descriptors.push(Some(desc));
        self.resolve_missing(id);
        id
    }

    pub fn register_type<T: Component>(&mut self) -> ComponentId {
        self.register(ComponentDescriptor::of::<T>())
    }

    /// Unregister a component type. Every instance is converted into a "missing"
    /// value so it can be restored when the type is registered again.
    pub fn unregister(&mut self, name: &str) {
        let Some(&id) = self.by_name.get(name) else { return };
        let Some(col) = self.columns[id.0 as usize].take() else { return };
        let desc = self.descriptors[id.0 as usize].take().unwrap();
        for (i, e) in col.entities.iter().enumerate() {
            let r = unsafe { &*(desc.reflect)(col.data.get_ptr(i)) };
            let v = dumb_reflect::to_value(r);
            self.missing.entry(*e).or_default().push((desc.name.clone(), v));
        }
        self.by_type.retain(|_, v| *v != id);
        drop(col);
        self.structure_version += 1;
    }

    fn resolve_missing(&mut self, id: ComponentId) {
        let name = self.descriptors[id.0 as usize].as_ref().unwrap().name.clone();
        let mut restore = Vec::new();
        for (e, list) in self.missing.iter_mut() {
            let pos = list
                .iter()
                .position(|(n, _)| *n == name)
                .or_else(|| list.iter().position(|(n, _)| short_name(n) == short_name(&name) && crate_name(n) == crate_name(&name)));
            if let Some(pos) = pos {
                restore.push((*e, list.remove(pos).1));
            }
        }
        self.missing.retain(|_, l| !l.is_empty());
        for (e, v) in restore {
            if self.is_alive(e) {
                self.insert_value(e, id, Some(&v));
            }
        }
    }

    /// Look a type up by its full name, falling back to a unique match on the short name so
    /// data survives a type moving between modules (`game::Player` -> `game::player::Player`).
    pub fn component_id(&self, name: &str) -> Option<ComponentId> {
        if let Some(&id) = self.by_name.get(name) {
            if self.descriptors[id.0 as usize].is_some() {
                return Some(id);
            }
        }
        let short = short_name(name);
        let mut found = None;
        for (i, d) in self.descriptors.iter().enumerate() {
            if d.as_ref().is_some_and(|d| short_name(&d.name) == short && crate_name(&d.name) == crate_name(name)) {
                if found.is_some() {
                    return None; // ambiguous
                }
                found = Some(ComponentId(i as u32));
            }
        }
        found
    }

    #[inline]
    pub fn component_id_of<T: Component>(&self) -> Option<ComponentId> {
        if let Some(id) = self.by_type.get(&TypeId::of::<T>()) {
            return Some(*id);
        }
        // Same type compiled into another binary (script library): resolve by name.
        self.component_id(T::TYPE_NAME)
    }

    pub fn descriptor(&self, id: ComponentId) -> Option<&ComponentDescriptor> {
        self.descriptors.get(id.0 as usize)?.as_ref()
    }

    /// All registered component types.
    pub fn component_types(&self) -> impl Iterator<Item = (ComponentId, &ComponentDescriptor)> {
        self.descriptors
            .iter()
            .enumerate()
            .filter_map(|(i, d)| d.as_ref().map(|d| (ComponentId(i as u32), d)))
    }

    // ---------------------------------------------------------------- entities

    pub fn spawn(&mut self) -> Entity {
        self.structure_version += 1;
        self.entities.count += 1;
        if let Some(index) = self.entities.free.pop() {
            self.entities.alive[index as usize] = true;
            return Entity { index, generation: self.entities.generations[index as usize] };
        }
        let index = self.entities.generations.len() as u32;
        self.entities.generations.push(1);
        self.entities.alive.push(true);
        Entity { index, generation: 1 }
    }

    pub(crate) fn spawn_at_impl(&mut self, e: Entity) {
        let i = e.index as usize;
        if self.entities.generations.len() <= i {
            let old = self.entities.generations.len();
            self.entities.generations.resize(i + 1, 1);
            self.entities.alive.resize(i + 1, false);
            self.entities.free.extend((old..i).map(|x| x as u32));
        } else {
            if self.entities.alive[i] {
                return;
            }
            self.entities.free.retain(|&f| f as usize != i);
        }
        self.entities.free.retain(|&f| f as usize != i);
        self.entities.generations[i] = e.generation;
        self.entities.alive[i] = true;
        self.entities.count += 1;
        self.structure_version += 1;
    }

    /// Spawn with a name and a default transform.
    pub fn spawn_named(&mut self, name: &str) -> Entity {
        let e = self.spawn();
        self.insert(e, Name::new(name));
        self.insert(e, Transform::default());
        e
    }

    #[inline]
    pub fn is_alive(&self, e: Entity) -> bool {
        let i = e.index as usize;
        i < self.entities.alive.len() && self.entities.alive[i] && self.entities.generations[i] == e.generation
    }

    pub fn entity_count(&self) -> usize {
        self.entities.count
    }

    /// Despawn an entity and all of its descendants.
    pub fn despawn_recursive(&mut self, e: Entity) {
        for c in self.children(e) {
            self.despawn_recursive(c);
        }
        self.despawn(e);
    }

    pub fn despawn(&mut self, e: Entity) {
        if !self.is_alive(e) {
            return;
        }
        for col in self.columns.iter_mut().flatten() {
            col.remove(e);
        }
        self.missing.remove(&e);
        let i = e.index as usize;
        self.entities.alive[i] = false;
        self.entities.generations[i] = self.entities.generations[i].wrapping_add(1).max(1);
        self.entities.free.push(e.index);
        self.entities.count -= 1;
        self.structure_version += 1;
    }

    /// Every live entity, in index order.
    pub fn entities(&self) -> impl Iterator<Item = Entity> + '_ {
        self.entities.alive.iter().enumerate().filter(|(_, a)| **a).map(|(i, _)| Entity {
            index: i as u32,
            generation: self.entities.generations[i],
        })
    }

    pub fn clear(&mut self) {
        let all: Vec<_> = self.entities().collect();
        for e in all {
            self.despawn(e);
        }
        self.missing.clear();
    }

    // ---------------------------------------------------------------- components (typed)

    pub fn insert<T: Component>(&mut self, e: Entity, value: T) {
        assert!(self.is_alive(e), "insert on dead entity {e:?}");
        let id = match self.component_id_of::<T>() {
            Some(id) => id,
            None => self.register_type::<T>(),
        };
        let drop = self.descriptors[id.0 as usize].as_ref().unwrap().drop;
        let col = self.columns[id.0 as usize].as_mut().unwrap();
        let mut value = Some(value);
        unsafe {
            col.insert_with(e, drop, |p| (p as *mut T).write(value.take().unwrap()));
        }
        self.structure_version += 1;
    }

    pub fn remove<T: Component>(&mut self, e: Entity) -> bool {
        match self.component_id_of::<T>() {
            Some(id) => self.remove_id(e, id),
            None => false,
        }
    }

    #[inline]
    pub fn get<T: Component>(&self, e: Entity) -> Option<&T> {
        let id = self.component_id_of::<T>()?;
        let p = self.columns[id.0 as usize].as_ref()?.get_ptr(e)?;
        Some(unsafe { &*(p as *const T) })
    }

    #[inline]
    pub fn get_mut<T: Component>(&mut self, e: Entity) -> Option<&mut T> {
        let id = self.component_id_of::<T>()?;
        let p = self.columns[id.0 as usize].as_ref()?.get_ptr(e)?;
        Some(unsafe { &mut *(p as *mut T) })
    }

    pub fn has<T: Component>(&self, e: Entity) -> bool {
        self.get::<T>(e).is_some()
    }

    /// Number of entities that have component `T`.
    pub fn count<T: Component>(&self) -> usize {
        self.component_id_of::<T>()
            .and_then(|id| self.columns[id.0 as usize].as_ref())
            .map_or(0, |c| c.len())
    }

    // ---------------------------------------------------------------- components (reflected)

    /// Insert a default-constructed component by id, then apply an optional value tree.
    pub fn insert_value(&mut self, e: Entity, id: ComponentId, value: Option<&Value>) {
        let Some(desc) = self.descriptors[id.0 as usize].as_ref() else { return };
        let (drop, ctor, reflect) = (desc.drop, desc.construct_default, desc.reflect);
        let col = self.columns[id.0 as usize].as_mut().unwrap();
        unsafe {
            col.insert_with(e, drop, |p| ctor(p));
            if let Some(v) = value {
                let p = col.get_ptr(e).unwrap();
                dumb_reflect::apply(&mut *reflect(p), v);
            }
        }
        self.structure_version += 1;
    }

    /// Insert by type name. If the type is unknown the value is kept as a missing component.
    pub fn insert_by_name(&mut self, e: Entity, name: &str, value: &Value) {
        match self.component_id(name) {
            Some(id) => self.insert_value(e, id, Some(value)),
            None => self.missing.entry(e).or_default().push((name.to_string(), value.clone())),
        }
    }

    pub fn remove_id(&mut self, e: Entity, id: ComponentId) -> bool {
        let removed = self.columns[id.0 as usize].as_mut().is_some_and(|c| c.remove(e));
        if removed {
            self.structure_version += 1;
        }
        removed
    }

    pub fn has_id(&self, e: Entity, id: ComponentId) -> bool {
        self.columns.get(id.0 as usize).and_then(|c| c.as_ref()).is_some_and(|c| c.dense_index(e).is_some())
    }

    pub fn get_reflect(&self, e: Entity, id: ComponentId) -> Option<&dyn Reflect> {
        let desc = self.descriptors.get(id.0 as usize)?.as_ref()?;
        let p = self.columns[id.0 as usize].as_ref()?.get_ptr(e)?;
        Some(unsafe { &*(desc.reflect)(p) })
    }

    pub fn get_reflect_mut(&mut self, e: Entity, id: ComponentId) -> Option<&mut dyn Reflect> {
        let desc = self.descriptors.get(id.0 as usize)?.as_ref()?;
        let p = self.columns[id.0 as usize].as_ref()?.get_ptr(e)?;
        Some(unsafe { &mut *(desc.reflect)(p) })
    }

    /// Ids of every component on `e`, in registration order.
    pub fn components_of(&self, e: Entity) -> Vec<ComponentId> {
        self.columns
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.as_ref().filter(|c| c.dense_index(e).is_some()).map(|_| ComponentId(i as u32)))
            .collect()
    }

    /// Drop a stored component whose type is not loaded.
    pub fn remove_missing(&mut self, e: Entity, name: &str) {
        if let Some(list) = self.missing.get_mut(&e) {
            list.retain(|(n, _)| n != name);
        }
        self.structure_version += 1;
    }

    /// Components on `e` whose type is not loaded.
    pub fn missing_components(&self, e: Entity) -> &[(String, Value)] {
        self.missing.get(&e).map_or(&[], |v| v.as_slice())
    }

    /// Entities that have the given component id.
    pub fn entities_with(&self, id: ComponentId) -> &[Entity] {
        self.columns.get(id.0 as usize).and_then(|c| c.as_ref()).map_or(&[], |c| &c.entities)
    }

    // ---------------------------------------------------------------- hierarchy

    pub fn parent(&self, e: Entity) -> Option<Entity> {
        self.get::<Parent>(e).map(|p| p.entity).filter(|p| self.is_alive(*p))
    }

    /// Children in index order. O(n) — fine for tools; hot paths should cache.
    pub fn children(&self, e: Entity) -> Vec<Entity> {
        let Some(id) = self.component_id_of::<Parent>() else { return Vec::new() };
        let col = self.columns[id.0 as usize].as_ref().unwrap();
        let mut out: Vec<Entity> = col
            .entities
            .iter()
            .enumerate()
            .filter(|(i, _)| unsafe { (*(col.data.get_ptr(*i) as *const Parent)).entity } == e)
            .map(|(_, c)| *c)
            .collect();
        out.sort_by_key(|c| c.index);
        out
    }

    pub fn roots(&self) -> Vec<Entity> {
        self.entities().filter(|e| self.parent(*e).is_none()).collect()
    }

    pub fn is_ancestor(&self, ancestor: Entity, mut e: Entity) -> bool {
        while let Some(p) = self.parent(e) {
            if p == ancestor {
                return true;
            }
            e = p;
        }
        false
    }

    /// Reparent `child`, keeping its world transform. `None` makes it a root.
    pub fn set_parent(&mut self, child: Entity, parent: Option<Entity>) {
        if let Some(p) = parent {
            if p == child || self.is_ancestor(child, p) {
                return;
            }
        }
        self.update_transforms();
        let world_m = self.global_matrix(child);
        match parent {
            Some(p) => {
                let parent_m = self.global_matrix(p);
                self.insert(child, Parent { entity: p });
                if let Some(t) = self.get_mut::<Transform>(child) {
                    *t = Transform::from_matrix(parent_m.inverse() * world_m);
                }
            }
            None => {
                self.remove::<Parent>(child);
                if let Some(t) = self.get_mut::<Transform>(child) {
                    *t = Transform::from_matrix(world_m);
                }
            }
        }
        self.update_transforms();
    }

    // ---------------------------------------------------------------- transforms

    /// World matrix computed by the last [`World::update_transforms`].
    #[inline]
    pub fn global_matrix(&self, e: Entity) -> Mat4 {
        self.global.get(e.index as usize).copied().unwrap_or(Mat4::IDENTITY)
    }

    /// Propagate local transforms down the hierarchy into world matrices.
    pub fn update_transforms(&mut self) {
        let n = self.entities.generations.len();
        self.global.clear();
        self.global.resize(n, Mat4::IDENTITY);
        // 0 = not computed, 1 = in progress, 2 = done
        let mut state = vec![0u8; n];
        let all: Vec<Entity> = self.entities().collect();
        for e in all {
            self.compute_global(e, &mut state);
        }
    }

    fn compute_global(&mut self, e: Entity, state: &mut [u8]) -> Mat4 {
        let i = e.index as usize;
        match state[i] {
            2 => return self.global[i],
            1 => return Mat4::IDENTITY, // cycle guard
            _ => {}
        }
        state[i] = 1;
        let local = self.get::<Transform>(e).map_or(Mat4::IDENTITY, |t| t.matrix());
        let m = match self.parent(e) {
            Some(p) => self.compute_global(p, state) * local,
            None => local,
        };
        self.global[i] = m;
        state[i] = 2;
        m
    }

    pub(crate) fn column_ptr(&self, id: ComponentId) -> *const Column {
        match self.columns.get(id.0 as usize) {
            Some(Some(c)) => c as *const Column,
            _ => std::ptr::null(),
        }
    }
}

/// `crate::module::Type` -> `Type`.
fn short_name(n: &str) -> &str {
    n.rsplit("::").next().unwrap_or(n)
}

/// `crate::module::Type` -> `crate`.
fn crate_name(n: &str) -> &str {
    n.split("::").next().unwrap_or(n)
}

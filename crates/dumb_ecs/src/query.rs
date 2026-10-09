use crate::world::{Column, World};
use crate::{Component, ComponentId};
use dumb_core::Entity;
use std::marker::PhantomData;

/// Something that can be fetched for an entity in a query: `&T`, `&mut T`,
/// `Option<&T>`, `Entity`, `With<T>`, `Without<T>` and tuples of those.
///
/// # Safety
/// Implementors must report every component they access in `init` so aliasing
/// `&mut` access is detected.
pub unsafe trait QueryData {
    type Item<'w>;
    type State: Copy;

    /// Resolve component columns. `None` means the query can never match.
    fn init(world: &World, access: &mut Vec<(ComponentId, bool)>) -> Option<Self::State>;
    /// Columns that every matching entity must be in (used to pick the driving column).
    fn required(state: &Self::State, out: &mut Vec<*const Column>);
    /// # Safety
    /// The column pointers in `state` must be valid for `'w`.
    unsafe fn fetch<'w>(state: &Self::State, e: Entity) -> Option<Self::Item<'w>>;
}

/// Marker for queries that only read.
pub unsafe trait ReadOnlyQueryData: QueryData {}

#[derive(Clone, Copy)]
pub struct ColumnRef(*const Column);

unsafe impl<T: Component> QueryData for &T {
    type Item<'w> = &'w T;
    type State = ColumnRef;

    fn init(world: &World, access: &mut Vec<(ComponentId, bool)>) -> Option<ColumnRef> {
        let id = world.component_id_of::<T>()?;
        access.push((id, false));
        Some(ColumnRef(world.column_ptr(id)))
    }
    fn required(state: &ColumnRef, out: &mut Vec<*const Column>) {
        out.push(state.0);
    }
    unsafe fn fetch<'w>(state: &ColumnRef, e: Entity) -> Option<&'w T> {
        (*state.0).get_ptr(e).map(|p| &*(p as *const T))
    }
}
unsafe impl<T: Component> ReadOnlyQueryData for &T {}

unsafe impl<T: Component> QueryData for &mut T {
    type Item<'w> = &'w mut T;
    type State = ColumnRef;

    fn init(world: &World, access: &mut Vec<(ComponentId, bool)>) -> Option<ColumnRef> {
        let id = world.component_id_of::<T>()?;
        access.push((id, true));
        Some(ColumnRef(world.column_ptr(id)))
    }
    fn required(state: &ColumnRef, out: &mut Vec<*const Column>) {
        out.push(state.0);
    }
    unsafe fn fetch<'w>(state: &ColumnRef, e: Entity) -> Option<&'w mut T> {
        (*state.0).get_ptr(e).map(|p| &mut *(p as *mut T))
    }
}

unsafe impl<T: Component> QueryData for Option<&T> {
    type Item<'w> = Option<&'w T>;
    type State = ColumnRef;

    fn init(world: &World, access: &mut Vec<(ComponentId, bool)>) -> Option<ColumnRef> {
        Some(match world.component_id_of::<T>() {
            Some(id) => {
                access.push((id, false));
                ColumnRef(world.column_ptr(id))
            }
            None => ColumnRef(std::ptr::null()),
        })
    }
    fn required(_: &ColumnRef, _: &mut Vec<*const Column>) {}
    unsafe fn fetch<'w>(state: &ColumnRef, e: Entity) -> Option<Option<&'w T>> {
        if state.0.is_null() {
            return Some(None);
        }
        Some((*state.0).get_ptr(e).map(|p| &*(p as *const T)))
    }
}
unsafe impl<T: Component> ReadOnlyQueryData for Option<&T> {}

unsafe impl<T: Component> QueryData for Option<&mut T> {
    type Item<'w> = Option<&'w mut T>;
    type State = ColumnRef;

    fn init(world: &World, access: &mut Vec<(ComponentId, bool)>) -> Option<ColumnRef> {
        Some(match world.component_id_of::<T>() {
            Some(id) => {
                access.push((id, true));
                ColumnRef(world.column_ptr(id))
            }
            None => ColumnRef(std::ptr::null()),
        })
    }
    fn required(_: &ColumnRef, _: &mut Vec<*const Column>) {}
    unsafe fn fetch<'w>(state: &ColumnRef, e: Entity) -> Option<Option<&'w mut T>> {
        if state.0.is_null() {
            return Some(None);
        }
        Some((*state.0).get_ptr(e).map(|p| &mut *(p as *mut T)))
    }
}

unsafe impl QueryData for Entity {
    type Item<'w> = Entity;
    type State = ();
    fn init(_: &World, _: &mut Vec<(ComponentId, bool)>) -> Option<()> {
        Some(())
    }
    fn required(_: &(), _: &mut Vec<*const Column>) {}
    unsafe fn fetch<'w>(_: &(), e: Entity) -> Option<Self::Item<'w>> {
        Some(e)
    }
}
unsafe impl ReadOnlyQueryData for Entity {}

/// Filter: entity must have `T` (not fetched).
pub struct With<T>(PhantomData<T>);
/// Filter: entity must not have `T`.
pub struct Without<T>(PhantomData<T>);

unsafe impl<T: Component> QueryData for With<T> {
    type Item<'w> = ();
    type State = ColumnRef;
    fn init(world: &World, _: &mut Vec<(ComponentId, bool)>) -> Option<ColumnRef> {
        Some(ColumnRef(world.column_ptr(world.component_id_of::<T>()?)))
    }
    fn required(state: &ColumnRef, out: &mut Vec<*const Column>) {
        out.push(state.0);
    }
    unsafe fn fetch<'w>(state: &ColumnRef, e: Entity) -> Option<Self::Item<'w>> {
        (*state.0).dense_index(e).map(|_| ())
    }
}
unsafe impl<T: Component> ReadOnlyQueryData for With<T> {}

unsafe impl<T: Component> QueryData for Without<T> {
    type Item<'w> = ();
    type State = ColumnRef;
    fn init(world: &World, _: &mut Vec<(ComponentId, bool)>) -> Option<ColumnRef> {
        Some(match world.component_id_of::<T>() {
            Some(id) => ColumnRef(world.column_ptr(id)),
            None => ColumnRef(std::ptr::null()),
        })
    }
    fn required(_: &ColumnRef, _: &mut Vec<*const Column>) {}
    unsafe fn fetch<'w>(state: &ColumnRef, e: Entity) -> Option<Self::Item<'w>> {
        if state.0.is_null() || (*state.0).dense_index(e).is_none() {
            Some(())
        } else {
            None
        }
    }
}
unsafe impl<T: Component> ReadOnlyQueryData for Without<T> {}

macro_rules! tuple_query {
    ($($name:ident),+) => {
        #[allow(non_snake_case)]
        unsafe impl<$($name: QueryData),+> QueryData for ($($name,)+) {
            type Item<'w> = ($($name::Item<'w>,)+);
            type State = ($($name::State,)+);

            fn init(world: &World, access: &mut Vec<(ComponentId, bool)>) -> Option<Self::State> {
                Some(($($name::init(world, access)?,)+))
            }
            fn required(state: &Self::State, out: &mut Vec<*const Column>) {
                let ($($name,)+) = state;
                $($name::required($name, out);)+
            }
            unsafe fn fetch<'w>(state: &Self::State, e: Entity) -> Option<Self::Item<'w>> {
                let ($($name,)+) = state;
                Some(($($name::fetch($name, e)?,)+))
            }
        }
        unsafe impl<$($name: ReadOnlyQueryData),+> ReadOnlyQueryData for ($($name,)+) {}
    };
}

tuple_query!(A);
tuple_query!(A, B);
tuple_query!(A, B, C);
tuple_query!(A, B, C, D);
tuple_query!(A, B, C, D, E);
tuple_query!(A, B, C, D, E, F);
tuple_query!(A, B, C, D, E, F, G);
tuple_query!(A, B, C, D, E, F, G, H);

/// Iterator over the entities matching `Q`.
pub struct QueryIter<'w, Q: QueryData> {
    state: Option<Q::State>,
    entities: Vec<Entity>,
    driver: Option<&'w [Entity]>,
    pos: usize,
    _marker: PhantomData<&'w World>,
}

impl<'w, Q: QueryData> QueryIter<'w, Q> {
    fn new(world: &'w World) -> Self {
        let mut access = Vec::new();
        let state = Q::init(world, &mut access);
        for (i, (a, am)) in access.iter().enumerate() {
            for (b, bm) in &access[i + 1..] {
                assert!(!(a == b && (*am || *bm)), "query accesses a component mutably twice");
            }
        }
        let mut driver = None;
        let mut entities = Vec::new();
        if let Some(s) = &state {
            let mut req = Vec::new();
            Q::required(s, &mut req);
            let smallest = req.iter().copied().min_by_key(|c| unsafe { (**c).len() });
            match smallest {
                Some(c) => driver = Some(unsafe { (*c).entities.as_slice() }),
                None => entities = world.entities().collect(),
            }
        }
        QueryIter { state, entities, driver, pos: 0, _marker: PhantomData }
    }
}

impl<'w, Q: QueryData> Iterator for QueryIter<'w, Q> {
    type Item = Q::Item<'w>;

    fn next(&mut self) -> Option<Self::Item> {
        let state = self.state.as_ref()?;
        let list = self.driver.unwrap_or(&self.entities);
        while self.pos < list.len() {
            let e = list[self.pos];
            self.pos += 1;
            if let Some(item) = unsafe { Q::fetch(state, e) } {
                return Some(item);
            }
        }
        None
    }
}

impl World {
    /// Iterate entities matching `Q`, with mutable access.
    ///
    /// ```ignore
    /// for (t, p) in world.query::<(&mut Transform, &Player)>() { ... }
    /// ```
    pub fn query<Q: QueryData>(&mut self) -> QueryIter<'_, Q> {
        QueryIter::new(self)
    }

    /// Read-only query through a shared reference.
    pub fn query_ref<Q: ReadOnlyQueryData>(&self) -> QueryIter<'_, Q> {
        QueryIter::new(self)
    }

    /// Fetch query data for a single entity.
    pub fn query_one<Q: QueryData>(&mut self, e: Entity) -> Option<Q::Item<'_>> {
        let mut access = Vec::new();
        let state = Q::init(self, &mut access)?;
        unsafe { Q::fetch(&state, e) }
    }
}

/// Query state shared between worker threads. The raw column pointers are only read, and
/// every entity is visited by exactly one thread, so mutable items never alias.
struct SharedState<S>(S);
unsafe impl<S> Sync for SharedState<S> {}
unsafe impl<S> Send for SharedState<S> {}

/// Entities per parallel task: big enough to amortize scheduling, small enough to balance.
const PAR_CHUNK: usize = 256;

impl World {
    /// Run `f` for every entity matching `Q`, in parallel on the job pool (rayon).
    /// Use it for heavy per-entity work (AI, simulation, procedural animation); `f` must not
    /// touch other entities' components.
    ///
    /// ```ignore
    /// world.par_for_each::<(&mut Transform, &Velocity), _>(|(t, v)| t.translation += v.0 * dt);
    /// ```
    pub fn par_for_each<Q, F>(&mut self, f: F)
    where
        Q: QueryData,
        F: Fn(Q::Item<'_>) + Send + Sync,
        for<'w> Q::Item<'w>: Send,
    {
        par_run::<Q, F>(self, f)
    }

    /// Read-only parallel iteration through a shared reference.
    pub fn par_for_each_ref<Q, F>(&self, f: F)
    where
        Q: ReadOnlyQueryData,
        F: Fn(Q::Item<'_>) + Send + Sync,
        for<'w> Q::Item<'w>: Send,
    {
        par_run::<Q, F>(self, f)
    }
}

fn par_run<Q, F>(world: &World, f: F)
where
    Q: QueryData,
    F: Fn(Q::Item<'_>) + Send + Sync,
    for<'w> Q::Item<'w>: Send,
{
    use rayon::prelude::*;
    let it = QueryIter::<Q>::new(world);
    let Some(state) = it.state else { return };
    let list: &[Entity] = it.driver.unwrap_or(&it.entities);
    let shared = SharedState(state);
    let shared = &shared;
    let f = &f;
    list.par_chunks(PAR_CHUNK).for_each(move |chunk| {
        for &e in chunk {
            if let Some(item) = unsafe { Q::fetch(&shared.0, e) } {
                f(item);
            }
        }
    });
}

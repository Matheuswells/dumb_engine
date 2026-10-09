//! Runtime reflection for Dumb Engine.
//!
//! Anything that implements [`Reflect`] can be inspected and edited by the editor,
//! serialized into scenes/prefabs, and carried across a script hot-reload, without the
//! engine knowing the concrete type at compile time.
//!
//! Dispatch never relies on `TypeId`: types compiled into a dynamically loaded script
//! library get different `TypeId`s than the host, so the shape of a value is described
//! by the [`ReflectRef`]/[`ReflectMut`] enums instead.

mod impls;
mod value;

pub use value::Value;

use dumb_core::{AssetId, Color, Entity, Quat, Vec2, Vec3, Vec4};
use std::any::Any;

/// Editor hints attached to a field with `#[editor(...)]`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FieldAttrs {
    /// Slider range.
    pub range: Option<(f64, f64)>,
    /// Drag speed for numeric fields.
    pub speed: Option<f64>,
    /// Shown but not editable.
    pub readonly: bool,
    /// Not shown in the inspector (still serialized).
    pub hidden: bool,
    /// Hover text.
    pub tooltip: Option<&'static str>,
    /// For `AssetId` fields: which asset kind the picker should offer ("model", "material", ...).
    pub asset_kind: Option<&'static str>,
    /// Edit Vec3/Vec4/Color as a color.
    pub color: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct FieldInfo {
    pub name: &'static str,
    pub attrs: FieldAttrs,
}

/// A reflected struct with named fields.
pub trait Struct {
    fn fields(&self) -> &'static [FieldInfo];
    fn field(&self, index: usize) -> &dyn Reflect;
    fn field_mut(&mut self, index: usize) -> &mut dyn Reflect;

    fn field_index(&self, name: &str) -> Option<usize> {
        self.fields().iter().position(|f| f.name == name)
    }
}

/// A reflected fieldless enum.
pub trait Enum {
    fn variants(&self) -> &'static [&'static str];
    fn variant_index(&self) -> usize;
    fn set_variant_index(&mut self, index: usize);
}

/// A reflected growable list (`Vec<T>`).
pub trait List {
    fn len(&self) -> usize;
    fn get(&self, index: usize) -> &dyn Reflect;
    fn get_mut(&mut self, index: usize) -> &mut dyn Reflect;
    fn push_default(&mut self);
    fn remove(&mut self, index: usize);
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub enum ReflectRef<'a> {
    Struct(&'a dyn Struct),
    Enum(&'a dyn Enum),
    List(&'a dyn List),
    Bool(&'a bool),
    I32(&'a i32),
    U32(&'a u32),
    I64(&'a i64),
    U64(&'a u64),
    F32(&'a f32),
    F64(&'a f64),
    String(&'a String),
    Vec2(&'a Vec2),
    Vec3(&'a Vec3),
    Vec4(&'a Vec4),
    Quat(&'a Quat),
    Color(&'a Color),
    Asset(&'a AssetId),
    Entity(&'a Entity),
    /// A value that can only be shown, not edited (anything else).
    Opaque(String),
}

pub enum ReflectMut<'a> {
    Struct(&'a mut dyn Struct),
    Enum(&'a mut dyn Enum),
    List(&'a mut dyn List),
    Bool(&'a mut bool),
    I32(&'a mut i32),
    U32(&'a mut u32),
    I64(&'a mut i64),
    U64(&'a mut u64),
    F32(&'a mut f32),
    F64(&'a mut f64),
    String(&'a mut String),
    Vec2(&'a mut Vec2),
    Vec3(&'a mut Vec3),
    Vec4(&'a mut Vec4),
    Quat(&'a mut Quat),
    Color(&'a mut Color),
    Asset(&'a mut AssetId),
    Entity(&'a mut Entity),
    Opaque,
}

/// The core reflection trait. Implemented by `#[derive(Editor)]` for user types.
pub trait Reflect: Any + Send + Sync {
    /// Fully qualified, stable type name (e.g. `game_scripts::Player`).
    fn type_name(&self) -> &'static str;
    fn reflect_ref(&self) -> ReflectRef<'_>;
    fn reflect_mut(&mut self) -> ReflectMut<'_>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Static type information for types that can be constructed by the engine.
pub trait Typed: Reflect + Default {
    const TYPE_NAME: &'static str;
}

/// Convert any reflected value into a neutral [`Value`] tree.
pub fn to_value(r: &dyn Reflect) -> Value {
    match r.reflect_ref() {
        ReflectRef::Struct(s) => Value::Struct(
            s.fields()
                .iter()
                .enumerate()
                .map(|(i, f)| (f.name.to_string(), to_value(s.field(i))))
                .collect(),
        ),
        ReflectRef::Enum(e) => Value::Enum(e.variants()[e.variant_index()].to_string()),
        ReflectRef::List(l) => Value::List((0..l.len()).map(|i| to_value(l.get(i))).collect()),
        ReflectRef::Bool(v) => Value::Bool(*v),
        ReflectRef::I32(v) => Value::Int(*v as i64),
        ReflectRef::U32(v) => Value::Int(*v as i64),
        ReflectRef::I64(v) => Value::Int(*v),
        ReflectRef::U64(v) => Value::Int(*v as i64),
        ReflectRef::F32(v) => Value::Float(*v as f64),
        ReflectRef::F64(v) => Value::Float(*v),
        ReflectRef::String(v) => Value::String(v.clone()),
        ReflectRef::Vec2(v) => Value::Vec(v.to_array().to_vec()),
        ReflectRef::Vec3(v) => Value::Vec(v.to_array().to_vec()),
        ReflectRef::Vec4(v) => Value::Vec(v.to_array().to_vec()),
        ReflectRef::Quat(v) => Value::Vec(v.to_array().to_vec()),
        ReflectRef::Color(v) => Value::Vec(v.to_array().to_vec()),
        ReflectRef::Asset(v) => Value::Asset(*v),
        ReflectRef::Entity(v) => Value::Entity(v.to_bits()),
        ReflectRef::Opaque(_) => Value::None,
    }
}

/// Apply a [`Value`] tree onto a reflected value. Unknown fields are ignored and missing
/// fields keep their current value, so data survives adding/removing/reordering fields.
pub fn apply(r: &mut dyn Reflect, v: &Value) {
    match (r.reflect_mut(), v) {
        (ReflectMut::Struct(s), Value::Struct(fields)) => {
            for (name, fv) in fields {
                if let Some(i) = s.field_index(name) {
                    apply(s.field_mut(i), fv);
                }
            }
        }
        (ReflectMut::Enum(e), Value::Enum(name)) => {
            if let Some(i) = e.variants().iter().position(|n| n == name) {
                e.set_variant_index(i);
            }
        }
        (ReflectMut::List(l), Value::List(items)) => {
            while l.len() > items.len() {
                l.remove(l.len() - 1);
            }
            while l.len() < items.len() {
                l.push_default();
            }
            for (i, item) in items.iter().enumerate() {
                apply(l.get_mut(i), item);
            }
        }
        (ReflectMut::Bool(t), Value::Bool(v)) => *t = *v,
        (ReflectMut::I32(t), v) => {
            if let Some(n) = v.as_f64() {
                *t = n as i32
            }
        }
        (ReflectMut::U32(t), v) => {
            if let Some(n) = v.as_f64() {
                *t = n as u32
            }
        }
        (ReflectMut::I64(t), v) => {
            if let Some(n) = v.as_f64() {
                *t = n as i64
            }
        }
        (ReflectMut::U64(t), v) => {
            if let Some(n) = v.as_f64() {
                *t = n as u64
            }
        }
        (ReflectMut::F32(t), v) => {
            if let Some(n) = v.as_f64() {
                *t = n as f32
            }
        }
        (ReflectMut::F64(t), v) => {
            if let Some(n) = v.as_f64() {
                *t = n
            }
        }
        (ReflectMut::String(t), Value::String(v)) => *t = v.clone(),
        (ReflectMut::Vec2(t), Value::Vec(v)) if v.len() >= 2 => *t = Vec2::new(v[0], v[1]),
        (ReflectMut::Vec3(t), Value::Vec(v)) if v.len() >= 3 => *t = Vec3::new(v[0], v[1], v[2]),
        (ReflectMut::Vec4(t), Value::Vec(v)) if v.len() >= 4 => *t = Vec4::new(v[0], v[1], v[2], v[3]),
        (ReflectMut::Quat(t), Value::Vec(v)) if v.len() >= 4 => {
            *t = Quat::from_xyzw(v[0], v[1], v[2], v[3]).normalize()
        }
        (ReflectMut::Color(t), Value::Vec(v)) if v.len() >= 4 => *t = Color::rgba(v[0], v[1], v[2], v[3]),
        (ReflectMut::Asset(t), Value::Asset(v)) => *t = *v,
        (ReflectMut::Entity(t), Value::Entity(v)) => *t = Entity::from_bits(*v),
        _ => {}
    }
}

/// Walk a value along a dotted path (`"stats.health"`) and return the field.
pub fn path_mut<'a>(r: &'a mut dyn Reflect, path: &str) -> Option<&'a mut dyn Reflect> {
    let mut cur = r;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        cur = match cur.reflect_mut() {
            ReflectMut::Struct(s) => {
                let i = s.field_index(seg)?;
                s.field_mut(i)
            }
            ReflectMut::List(l) => {
                let i: usize = seg.parse().ok()?;
                if i >= l.len() {
                    return None;
                }
                l.get_mut(i)
            }
            _ => return None,
        };
    }
    Some(cur)
}

/// Implement `Reflect` for a plain value type that maps onto a `ReflectRef` variant.
#[macro_export]
macro_rules! impl_reflect_value {
    ($ty:ty, $variant:ident, $name:expr) => {
        impl $crate::Reflect for $ty {
            fn type_name(&self) -> &'static str {
                $name
            }
            fn reflect_ref(&self) -> $crate::ReflectRef<'_> {
                $crate::ReflectRef::$variant(self)
            }
            fn reflect_mut(&mut self) -> $crate::ReflectMut<'_> {
                $crate::ReflectMut::$variant(self)
            }
            fn as_any(&self) -> &dyn ::std::any::Any {
                self
            }
            fn as_any_mut(&mut self) -> &mut dyn ::std::any::Any {
                self
            }
        }
        impl $crate::Typed for $ty {
            const TYPE_NAME: &'static str = $name;
        }
    };
}

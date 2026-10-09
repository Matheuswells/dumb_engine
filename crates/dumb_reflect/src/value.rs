use dumb_core::AssetId;
use serde::{Deserialize, Serialize};

/// A type-erased, serializable value tree. This is what scenes, prefabs, undo snapshots
/// and script hot-reload state are made of.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub enum Value {
    #[default]
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    /// Vectors, quaternions and colors.
    Vec(Vec<f32>),
    Asset(AssetId),
    Entity(u64),
    Enum(String),
    List(Vec<Value>),
    Struct(Vec<(String, Value)>),
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::Bool(b) => Some(*b as i32 as f64),
            _ => None,
        }
    }

    pub fn field(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Struct(fields) => fields.iter().find(|(n, _)| n == name).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Visit every value in the tree mutably (used to remap entity references).
    pub fn visit_mut(&mut self, f: &mut impl FnMut(&mut Value)) {
        f(self);
        match self {
            Value::List(items) => items.iter_mut().for_each(|v| v.visit_mut(f)),
            Value::Struct(fields) => fields.iter_mut().for_each(|(_, v)| v.visit_mut(f)),
            _ => {}
        }
    }

    /// Visit every value in the tree.
    pub fn visit(&self, f: &mut impl FnMut(&Value)) {
        f(self);
        match self {
            Value::List(items) => items.iter().for_each(|v| v.visit(f)),
            Value::Struct(fields) => fields.iter().for_each(|(_, v)| v.visit(f)),
            _ => {}
        }
    }
}

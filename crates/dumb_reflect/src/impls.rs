use crate::{impl_reflect_value, List, Reflect, ReflectMut, ReflectRef, Typed};
use dumb_core::{AssetId, Color, Entity, Quat, Vec2, Vec3, Vec4};
use std::any::Any;

impl_reflect_value!(bool, Bool, "bool");
impl_reflect_value!(i32, I32, "i32");
impl_reflect_value!(u32, U32, "u32");
impl_reflect_value!(i64, I64, "i64");
impl_reflect_value!(u64, U64, "u64");
impl_reflect_value!(f32, F32, "f32");
impl_reflect_value!(f64, F64, "f64");
impl_reflect_value!(String, String, "String");
impl_reflect_value!(Vec2, Vec2, "Vec2");
impl_reflect_value!(Vec3, Vec3, "Vec3");
impl_reflect_value!(Vec4, Vec4, "Vec4");
impl_reflect_value!(Quat, Quat, "Quat");
impl_reflect_value!(Color, Color, "Color");
impl_reflect_value!(AssetId, Asset, "AssetId");
impl_reflect_value!(Entity, Entity, "Entity");

impl<T: Typed> List for Vec<T> {
    fn len(&self) -> usize {
        Vec::len(self)
    }
    fn get(&self, index: usize) -> &dyn Reflect {
        &self[index]
    }
    fn get_mut(&mut self, index: usize) -> &mut dyn Reflect {
        &mut self[index]
    }
    fn push_default(&mut self) {
        self.push(T::default());
    }
    fn remove(&mut self, index: usize) {
        Vec::remove(self, index);
    }
}

impl<T: Typed> Reflect for Vec<T> {
    fn type_name(&self) -> &'static str {
        "Vec"
    }
    fn reflect_ref(&self) -> ReflectRef<'_> {
        ReflectRef::List(self)
    }
    fn reflect_mut(&mut self) -> ReflectMut<'_> {
        ReflectMut::List(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl<T: Typed> Typed for Vec<T> {
    const TYPE_NAME: &'static str = "Vec";
}

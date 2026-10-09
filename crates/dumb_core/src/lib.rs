//! Core types shared by every Dumb Engine crate: math, ids, colors, time and input.
//!
//! Everything in here is plain data with a stable `#[repr(C)]` layout where it crosses
//! the script-plugin boundary, so host and dynamically loaded game code agree on it.

pub use glam;
pub use glam::{Affine3A, EulerRot, Mat3, Mat4, Quat, Vec2, Vec3, Vec4};

mod input;
pub mod physics;
pub mod profiler;
mod time;

pub use input::{Input, Key, MouseButton};
pub use time::Time;

use serde::{Deserialize, Serialize};

/// Stable identifier of an asset. Lives in the asset's `.meta` sidecar file, so it
/// survives renames and moves.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct AssetId(pub uuid::Uuid);

impl AssetId {
    pub const NONE: AssetId = AssetId(uuid::Uuid::nil());

    pub fn new() -> Self {
        AssetId(uuid::Uuid::new_v4())
    }

    pub fn is_none(&self) -> bool {
        self.0.is_nil()
    }

    /// Deterministic id for a sub-asset (mesh #3 inside a model, etc.).
    pub fn sub(&self, kind: &str, index: usize) -> AssetId {
        let name = format!("{}/{}/{}", self.0, kind, index);
        AssetId(uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()))
    }
}

impl std::fmt::Debug for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AssetId({})", self.0)
    }
}

impl std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Handle to an entity in a `World`. The generation guards against stale handles.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Entity {
    pub index: u32,
    pub generation: u32,
}

impl Entity {
    pub const NONE: Entity = Entity { index: u32::MAX, generation: 0 };

    pub fn is_none(&self) -> bool {
        self.index == u32::MAX
    }

    pub fn to_bits(self) -> u64 {
        (self.generation as u64) << 32 | self.index as u64
    }

    pub fn from_bits(bits: u64) -> Self {
        Entity { index: bits as u32, generation: (bits >> 32) as u32 }
    }
}

impl Default for Entity {
    fn default() -> Self {
        Entity::NONE
    }
}

impl std::fmt::Debug for Entity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_none() {
            write!(f, "Entity(none)")
        } else {
            write!(f, "Entity({}v{})", self.index, self.generation)
        }
    }
}

/// Linear RGBA color.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Color = Color::rgb(1.0, 1.0, 1.0);
    pub const BLACK: Color = Color::rgb(0.0, 0.0, 0.0);
    pub const RED: Color = Color::rgb(1.0, 0.2, 0.2);
    pub const GREEN: Color = Color::rgb(0.2, 1.0, 0.2);
    pub const BLUE: Color = Color::rgb(0.2, 0.4, 1.0);
    pub const YELLOW: Color = Color::rgb(1.0, 0.9, 0.2);

    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Color { r, g, b, a: 1.0 }
    }

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Color { r, g, b, a }
    }

    pub fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub fn to_vec4(self) -> Vec4 {
        Vec4::new(self.r, self.g, self.b, self.a)
    }
}

impl Default for Color {
    fn default() -> Self {
        Color::WHITE
    }
}

/// Axis-aligned bounding box.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Default for Aabb {
    fn default() -> Self {
        Aabb::EMPTY
    }
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb { min: Vec3::splat(f32::MAX), max: Vec3::splat(f32::MIN) };

    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Self {
        let mut b = Aabb::EMPTY;
        for p in points {
            b.extend(p);
        }
        b
    }

    pub fn extend(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&self, o: &Aabb) -> Aabb {
        Aabb { min: self.min.min(o.min), max: self.max.max(o.max) }
    }

    pub fn is_valid(&self) -> bool {
        self.min.cmple(self.max).all()
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn extents(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    pub fn corners(&self) -> [Vec3; 8] {
        let (a, b) = (self.min, self.max);
        [
            Vec3::new(a.x, a.y, a.z),
            Vec3::new(b.x, a.y, a.z),
            Vec3::new(b.x, b.y, a.z),
            Vec3::new(a.x, b.y, a.z),
            Vec3::new(a.x, a.y, b.z),
            Vec3::new(b.x, a.y, b.z),
            Vec3::new(b.x, b.y, b.z),
            Vec3::new(a.x, b.y, b.z),
        ]
    }

    /// Bounding box of this box after an affine transform.
    pub fn transformed(&self, m: &Mat4) -> Aabb {
        Aabb::from_points(self.corners().iter().map(|c| m.transform_point3(*c)))
    }
}

/// View frustum as six inward-facing planes (xyz = normal, w = distance).
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    pub planes: [Vec4; 6],
}

impl Frustum {
    /// Extract planes from a view-projection matrix with Vulkan's 0..1 depth range.
    pub fn from_view_proj(m: &Mat4) -> Self {
        let r0 = m.row(0);
        let r1 = m.row(1);
        let r2 = m.row(2);
        let r3 = m.row(3);
        let mut planes = [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r2, r3 - r2];
        for p in &mut planes {
            let len = p.truncate().length();
            *p /= len;
        }
        Frustum { planes }
    }

    pub fn intersects_aabb(&self, b: &Aabb) -> bool {
        let c = b.center();
        let e = b.extents();
        for p in &self.planes {
            let n = p.truncate();
            let r = e.dot(n.abs());
            if n.dot(c) + p.w < -r {
                return false;
            }
        }
        true
    }
}

/// A ray in world space, used for picking.
#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3,
}

impl Ray {
    /// Slab test. Returns distance along the ray on hit.
    pub fn intersect_aabb(&self, b: &Aabb) -> Option<f32> {
        let inv = self.dir.recip();
        let t1 = (b.min - self.origin) * inv;
        let t2 = (b.max - self.origin) * inv;
        let tmin = t1.min(t2).max_element();
        let tmax = t1.max(t2).min_element();
        if tmax >= tmin.max(0.0) {
            Some(tmin.max(0.0))
        } else {
            None
        }
    }

    pub fn intersect_plane(&self, point: Vec3, normal: Vec3) -> Option<f32> {
        let d = normal.dot(self.dir);
        if d.abs() < 1e-6 {
            return None;
        }
        let t = (point - self.origin).dot(normal) / d;
        (t >= 0.0).then_some(t)
    }
}

/// Engine ABI tag. Script plugins built against a different engine/compiler are refused.
pub const ENGINE_ABI: &str = concat!("dumb-engine/", env!("CARGO_PKG_VERSION"), "/abi-7");

/// Fixed ids of built-in assets (procedural primitives, default material).
pub mod builtin {
    use super::AssetId;

    const fn id(n: u128) -> AssetId {
        AssetId(uuid::Uuid::from_u128(0xd0d0_0000_0000_0000_0000_0000_0000_0000 | n))
    }

    pub const CUBE: AssetId = id(1);
    pub const SPHERE: AssetId = id(2);
    pub const PLANE: AssetId = id(3);
    pub const CYLINDER: AssetId = id(4);
    pub const DEFAULT_MATERIAL: AssetId = id(100);

    pub fn is_builtin(a: AssetId) -> bool {
        a.0.as_u128() >> 64 == 0xd0d0_0000_0000_0000
    }
}

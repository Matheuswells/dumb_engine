//! Built-in components.

use crate::World;
use dumb_core::{AssetId, Color, Entity, EulerRot, Mat4, Quat, Vec3};
use dumb_derive::{Component, Editor};

/// Display name shown in the hierarchy.
#[derive(Component, Editor, Clone, Debug, Default)]
pub struct Name {
    pub name: String,
}

impl Name {
    pub fn new(s: &str) -> Self {
        Name { name: s.to_string() }
    }
}

/// Local transform relative to the parent.
#[repr(C)]
#[derive(Component, Editor, Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    #[editor(speed = 0.05)]
    pub translation: Vec3,
    pub rotation: Quat,
    #[editor(speed = 0.01)]
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Transform { translation: Vec3::ZERO, rotation: Quat::IDENTITY, scale: Vec3::ONE }
    }
}

impl Transform {
    pub fn from_translation(t: Vec3) -> Self {
        Transform { translation: t, ..Default::default() }
    }

    pub fn from_matrix(m: Mat4) -> Self {
        let (scale, rotation, translation) = m.to_scale_rotation_translation();
        Transform { translation, rotation, scale }
    }

    pub fn with_scale(mut self, s: Vec3) -> Self {
        self.scale = s;
        self
    }

    pub fn with_rotation(mut self, r: Quat) -> Self {
        self.rotation = r;
        self
    }

    #[inline]
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    pub fn forward(&self) -> Vec3 {
        self.rotation * -Vec3::Z
    }

    pub fn right(&self) -> Vec3 {
        self.rotation * Vec3::X
    }

    /// Euler angles in degrees (YXZ order, matches the inspector).
    pub fn euler_degrees(&self) -> Vec3 {
        let (y, x, z) = self.rotation.to_euler(EulerRot::YXZ);
        Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees())
    }

    pub fn set_euler_degrees(&mut self, e: Vec3) {
        self.rotation = Quat::from_euler(EulerRot::YXZ, e.y.to_radians(), e.x.to_radians(), e.z.to_radians());
    }

    pub fn look_at(&mut self, target: Vec3, up: Vec3) {
        let m = Mat4::look_at_rh(self.translation, target, up).inverse();
        self.rotation = Quat::from_mat4(&m);
    }
}

/// Parent link. Children are derived from these.
#[derive(Component, Editor, Clone, Copy, Debug, Default)]
pub struct Parent {
    #[editor(readonly)]
    pub entity: Entity,
}

/// Renders a model asset (or a built-in primitive).
#[derive(Component, Editor, Clone, Debug)]
pub struct MeshRenderer {
    #[editor(asset = "model")]
    pub model: AssetId,
    /// Overrides every material of the model when set.
    #[editor(asset = "material")]
    pub material: AssetId,
    #[editor(color)]
    pub tint: Color,
    pub visible: bool,
    pub cast_shadows: bool,
}

impl Default for MeshRenderer {
    fn default() -> Self {
        MeshRenderer {
            model: AssetId::NONE,
            material: AssetId::NONE,
            tint: Color::WHITE,
            visible: true,
            cast_shadows: true,
        }
    }
}

#[derive(Editor, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Projection {
    #[default]
    Perspective,
    Orthographic,
}

#[derive(Editor, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CameraBackground {
    /// The engine skybox (also lights the scene and shows in reflections).
    #[default]
    Skybox,
    /// Fill with `clear_color`.
    SolidColor,
}

#[derive(Component, Editor, Clone, Debug)]
pub struct Camera {
    pub projection: Projection,
    pub background: CameraBackground,
    #[editor(range = 10.0..=150.0)]
    pub fov_degrees: f32,
    #[editor(speed = 0.1)]
    pub ortho_size: f32,
    #[editor(speed = 0.01)]
    pub near: f32,
    pub far: f32,
    /// The camera used when the game runs.
    pub primary: bool,
    #[editor(color)]
    pub clear_color: Color,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            projection: Projection::Perspective,
            fov_degrees: 60.0,
            ortho_size: 10.0,
            near: 0.05,
            far: 5000.0,
            primary: true,
            clear_color: Color::rgb(0.08, 0.09, 0.11),
            background: CameraBackground::Skybox,
        }
    }
}

impl Camera {
    /// Vulkan-style projection (0..1 depth, Y down in clip space).
    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        let mut p = match self.projection {
            Projection::Perspective => {
                Mat4::perspective_rh(self.fov_degrees.to_radians(), aspect.max(1e-4), self.near, self.far)
            }
            Projection::Orthographic => {
                let h = self.ortho_size * 0.5;
                let w = h * aspect;
                Mat4::orthographic_rh(-w, w, -h, h, self.near, self.far)
            }
        };
        p.y_axis.y *= -1.0;
        p
    }
}

#[derive(Editor, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightKind {
    #[default]
    Directional,
    Point,
}

#[derive(Component, Editor, Clone, Debug)]
pub struct Light {
    pub kind: LightKind,
    #[editor(color)]
    pub color: Color,
    #[editor(range = 0.0..=50.0)]
    pub intensity: f32,
    /// Point light falloff distance.
    #[editor(range = 0.1..=200.0)]
    pub range: f32,
}

impl Default for Light {
    fn default() -> Self {
        Light { kind: LightKind::Directional, color: Color::WHITE, intensity: 3.0, range: 10.0 }
    }
}

/// Plays animations from the model on this entity's `MeshRenderer`.
#[derive(Component, Editor, Clone, Debug)]
pub struct Animator {
    /// Clip name in the model.
    pub clip: String,
    #[editor(range = 0.0..=4.0)]
    pub speed: f32,
    pub playing: bool,
    pub looping: bool,
    #[editor(speed = 0.01)]
    pub time: f32,
    /// Optional second clip blended on top of `clip`.
    pub blend_clip: String,
    #[editor(range = 0.0..=1.0)]
    pub blend: f32,
}

impl Default for Animator {
    fn default() -> Self {
        Animator {
            clip: String::new(),
            speed: 1.0,
            playing: true,
            looping: true,
            time: 0.0,
            blend_clip: String::new(),
            blend: 0.0,
        }
    }
}

/// Marks the root of an instantiated prefab.
#[derive(Component, Editor, Clone, Debug, Default)]
pub struct PrefabInstance {
    #[editor(asset = "prefab", readonly)]
    pub prefab: AssetId,
}

pub(crate) fn register_builtin(w: &mut World) {
    w.register_type::<Name>();
    w.register_type::<Transform>();
    w.register_type::<Parent>();
    w.register_type::<MeshRenderer>();
    w.register_type::<Camera>();
    w.register_type::<Light>();
    w.register_type::<Animator>();
    w.register_type::<PrefabInstance>();
    w.register_type::<crate::ai::AiAgent>();
    w.register_type::<crate::streaming::StreamingCell>();
    w.register_type::<crate::hud::UiText>();
    w.register_type::<crate::hud::UiBar>();
    w.register_type::<crate::hud::UiImage>();
    w.register_type::<crate::hud::UiPanel>();
    w.register_type::<crate::hud::UiButton>();
    w.register_type::<crate::hud::UiWorldLabel>();
    w.register_type::<crate::hud::UiWeb>();
    crate::physics::register_physics(w);
}

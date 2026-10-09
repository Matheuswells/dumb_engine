//! Editor fly/orbit camera.

use dumb_core::{Aabb, Mat4, Quat, Ray, Vec2, Vec3};

#[derive(Clone, Debug)]
pub struct EditorCamera {
    pub pivot: Vec3,
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub fov_degrees: f32,
    pub speed: f32,
    pub near: f32,
    pub far: f32,
    /// Mouse look/orbit sensitivity multiplier.
    pub sensitivity: f32,
    pub invert_y: bool,
}

impl Default for EditorCamera {
    fn default() -> Self {
        EditorCamera {
            pivot: Vec3::new(0.0, 0.5, 0.0),
            distance: 9.0,
            yaw: 35f32.to_radians(),
            pitch: -25f32.to_radians(),
            fov_degrees: 60.0,
            speed: 8.0,
            near: 0.05,
            far: 20000.0,
            sensitivity: 1.0,
            invert_y: false,
        }
    }
}

impl EditorCamera {
    pub fn rotation(&self) -> Quat {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(self.pitch)
    }

    pub fn forward(&self) -> Vec3 {
        self.rotation() * -Vec3::Z
    }

    pub fn position(&self) -> Vec3 {
        self.pivot - self.forward() * self.distance
    }

    pub fn view(&self) -> Mat4 {
        Mat4::from_rotation_translation(self.rotation(), self.position()).inverse()
    }

    pub fn proj(&self, aspect: f32) -> Mat4 {
        let mut p = Mat4::perspective_rh(self.fov_degrees.to_radians(), aspect.max(1e-3), self.near, self.far);
        p.y_axis.y *= -1.0;
        p
    }

    /// Look around in place (RMB drag): pivot moves with the camera.
    pub fn look(&mut self, delta: Vec2) {
        let pos = self.position();
        let (k, y) = (0.005 * self.sensitivity, if self.invert_y { -delta.y } else { delta.y });
        self.yaw -= delta.x * k;
        self.pitch = (self.pitch - y * k).clamp(-1.55, 1.55);
        self.pivot = pos + self.forward() * self.distance;
    }

    /// Orbit around the pivot (Alt+LMB).
    pub fn orbit(&mut self, delta: Vec2) {
        let (k, y) = (0.006 * self.sensitivity, if self.invert_y { -delta.y } else { delta.y });
        self.yaw -= delta.x * k;
        self.pitch = (self.pitch - y * k).clamp(-1.55, 1.55);
    }

    pub fn pan(&mut self, delta: Vec2, viewport_h: f32) {
        let r = self.rotation();
        let scale = self.distance * (self.fov_degrees.to_radians() * 0.5).tan() * 2.0 / viewport_h.max(1.0);
        self.pivot += (r * Vec3::X) * (-delta.x * scale) + (r * Vec3::Y) * (delta.y * scale);
    }

    pub fn zoom(&mut self, scroll: f32) {
        self.distance = (self.distance * (1.0 - scroll * 0.1)).clamp(0.05, 50000.0);
    }

    /// WASD/QE fly movement (while RMB is held).
    pub fn fly(&mut self, dir: Vec3, dt: f32, fast: bool) {
        let r = self.rotation();
        let speed = self.speed * if fast { 4.0 } else { 1.0 };
        self.pivot += (r * Vec3::new(dir.x, 0.0, dir.z) + Vec3::Y * dir.y) * speed * dt;
    }

    /// Frame a bounding box.
    pub fn focus(&mut self, b: &Aabb) {
        self.pivot = b.center();
        let radius = b.extents().length().max(0.1);
        self.distance = radius / (self.fov_degrees.to_radians() * 0.5).sin() * 1.1;
    }

    /// Ray through a point in the viewport (`uv` in 0..1, y down).
    pub fn ray(&self, uv: Vec2, aspect: f32) -> Ray {
        screen_ray(self.proj(aspect) * self.view(), uv)
    }
}

pub fn screen_ray(view_proj: Mat4, uv: Vec2) -> Ray {
    let inv = view_proj.inverse();
    let ndc = Vec2::new(uv.x * 2.0 - 1.0, uv.y * 2.0 - 1.0);
    let near = inv.project_point3(ndc.extend(0.0));
    let far = inv.project_point3(ndc.extend(1.0));
    Ray { origin: near, dir: (far - near).normalize() }
}

/// World point to viewport-local pixel position. `None` when behind the camera.
pub fn project(view_proj: Mat4, p: Vec3, size: Vec2) -> Option<Vec2> {
    let c = view_proj * p.extend(1.0);
    if c.w <= 1e-5 {
        return None;
    }
    let ndc = c.truncate() / c.w;
    Some(Vec2::new((ndc.x + 1.0) * 0.5 * size.x, (ndc.y + 1.0) * 0.5 * size.y))
}

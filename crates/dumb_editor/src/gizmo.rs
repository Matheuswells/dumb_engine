//! Translate / rotate / scale gizmos drawn with the egui painter over a viewport.

use crate::camera::{project, screen_ray};
use dumb_core::{Mat4, Quat, Ray, Vec2, Vec3};
use egui::{Color32, Pos2, Stroke};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GizmoMode {
    Translate,
    Rotate,
    Scale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GizmoSpace {
    World,
    Local,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Handle {
    Axis(usize),
    Plane(usize),
    Uniform,
}

#[derive(Clone, Debug)]
struct Drag {
    handle: Handle,
    start_world: Mat4,
    start_param: f32,
    start_point: Vec3,
    axes: [Vec3; 3],
}

pub struct Gizmo {
    pub mode: GizmoMode,
    pub space: GizmoSpace,
    pub snap: bool,
    pub translate_snap: f32,
    pub rotate_snap_deg: f32,
    pub scale_snap: f32,
    drag: Option<Drag>,
    hover: Option<Handle>,
}

impl Default for Gizmo {
    fn default() -> Self {
        Gizmo {
            mode: GizmoMode::Translate,
            space: GizmoSpace::World,
            snap: false,
            translate_snap: 0.5,
            rotate_snap_deg: 15.0,
            scale_snap: 0.1,
            drag: None,
            hover: None,
        }
    }
}

const COLORS: [Color32; 3] = [Color32::from_rgb(235, 70, 70), Color32::from_rgb(110, 210, 60), Color32::from_rgb(70, 130, 245)];
const HOVER: Color32 = Color32::from_rgb(255, 220, 60);

/// Viewport description passed to the gizmo.
pub struct GizmoView {
    pub rect: egui::Rect,
    pub view_proj: Mat4,
    pub camera_pos: Vec3,
}

impl GizmoView {
    fn to_screen(&self, p: Vec3) -> Option<Pos2> {
        let size = Vec2::new(self.rect.width(), self.rect.height());
        project(self.view_proj, p, size).map(|v| self.rect.min + egui::vec2(v.x, v.y))
    }

    fn ray(&self, pos: Pos2) -> Ray {
        let uv = Vec2::new((pos.x - self.rect.min.x) / self.rect.width(), (pos.y - self.rect.min.y) / self.rect.height());
        screen_ray(self.view_proj, uv)
    }
}

fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

/// Parameter along `axis` (through `origin`) of the closest point to `ray`.
fn axis_param(ray: &Ray, origin: Vec3, axis: Vec3) -> f32 {
    let w0 = origin - ray.origin;
    let a = axis.dot(axis);
    let b = axis.dot(ray.dir);
    let c = ray.dir.dot(ray.dir);
    let d = axis.dot(w0);
    let e = ray.dir.dot(w0);
    let denom = a * c - b * b;
    if denom.abs() < 1e-6 {
        return 0.0;
    }
    (b * e - c * d) / denom
}

fn snap(v: f32, step: f32) -> f32 {
    if step > 0.0 {
        (v / step).round() * step
    } else {
        v
    }
}

impl Gizmo {
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn is_hovered(&self) -> bool {
        self.hover.is_some()
    }

    /// Draw and interact. `world` is the selected entity's world matrix.
    /// Returns the new world matrix while dragging, and whether a drag started this frame.
    pub fn update(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        view: &GizmoView,
        world: Mat4,
        snap_override: bool,
    ) -> (Option<Mat4>, bool) {
        let painter = ui.painter_at(view.rect);
        let (scale, rot, pos) = world.to_scale_rotation_translation();
        let axes = match self.space {
            GizmoSpace::World => [Vec3::X, Vec3::Y, Vec3::Z],
            GizmoSpace::Local => [rot * Vec3::X, rot * Vec3::Y, rot * Vec3::Z],
        };
        let axes = if self.mode == GizmoMode::Scale { [rot * Vec3::X, rot * Vec3::Y, rot * Vec3::Z] } else { axes };
        let size = (view.camera_pos - pos).length() * 0.14;
        let Some(center) = view.to_screen(pos) else { return (None, false) };
        let mouse = ui.input(|i| i.pointer.hover_pos());
        let snapping = self.snap ^ snap_override;

        // ---- hit testing (only when not dragging)
        if self.drag.is_none() {
            self.hover = None;
            if let Some(m) = mouse.filter(|m| view.rect.contains(*m)) {
                let mut best = (12.0f32, None);
                match self.mode {
                    GizmoMode::Translate | GizmoMode::Scale => {
                        for (i, a) in axes.iter().enumerate() {
                            if let Some(end) = view.to_screen(pos + *a * size) {
                                let d = dist_to_segment(m, center, end);
                                if d < best.0 {
                                    best = (d, Some(Handle::Axis(i)));
                                }
                            }
                        }
                        if self.mode == GizmoMode::Translate {
                            for i in 0..3 {
                                let (u, v) = (axes[(i + 1) % 3], axes[(i + 2) % 3]);
                                let q = [0.25, 0.45].map(|s| s * size);
                                let pts: Vec<Pos2> = [(q[0], q[0]), (q[1], q[0]), (q[1], q[1]), (q[0], q[1])]
                                    .iter()
                                    .filter_map(|(a, b)| view.to_screen(pos + u * *a + v * *b))
                                    .collect();
                                if pts.len() == 4 && point_in_quad(m, &pts) {
                                    best = (0.0, Some(Handle::Plane(i)));
                                }
                            }
                        } else if (m - center).length() < 10.0 {
                            best = (0.0, Some(Handle::Uniform));
                        }
                    }
                    GizmoMode::Rotate => {
                        for (i, a) in axes.iter().enumerate() {
                            let circle = circle_points(view, pos, *a, size);
                            for w in circle.windows(2) {
                                let d = dist_to_segment(m, w[0], w[1]);
                                if d < best.0 {
                                    best = (d, Some(Handle::Axis(i)));
                                }
                            }
                        }
                    }
                }
                self.hover = best.1;
            }
        }

        // ---- start drag
        let mut started = false;
        if self.drag.is_none() && response.drag_started_by(egui::PointerButton::Primary) {
            if let (Some(h), Some(m)) = (self.hover, mouse) {
                let ray = view.ray(m);
                let (start_param, start_point) = match (self.mode, h) {
                    (GizmoMode::Rotate, Handle::Axis(i)) => {
                        let p = ray.intersect_plane(pos, axes[i]).map(|t| ray.origin + ray.dir * t).unwrap_or(pos);
                        (0.0, p)
                    }
                    (_, Handle::Axis(i)) => (axis_param(&ray, pos, axes[i]), pos),
                    (_, Handle::Plane(i)) => {
                        let p = ray.intersect_plane(pos, axes[i]).map(|t| ray.origin + ray.dir * t).unwrap_or(pos);
                        (0.0, p)
                    }
                    (_, Handle::Uniform) => (m.y, pos),
                };
                self.drag = Some(Drag { handle: h, start_world: world, start_param, start_point, axes });
                started = true;
            }
        }

        // ---- drag
        let mut result = None;
        if let Some(drag) = &self.drag {
            if !ui.input(|i| i.pointer.primary_down()) {
                self.drag = None;
            } else if let Some(m) = mouse {
                let ray = view.ray(m);
                let (s0, r0, p0) = drag.start_world.to_scale_rotation_translation();
                let new = match (self.mode, drag.handle) {
                    (GizmoMode::Translate, Handle::Axis(i)) => {
                        let mut d = axis_param(&ray, p0, drag.axes[i]) - drag.start_param;
                        if snapping {
                            d = snap(d, self.translate_snap);
                        }
                        Mat4::from_scale_rotation_translation(s0, r0, p0 + drag.axes[i] * d)
                    }
                    (GizmoMode::Translate, Handle::Plane(i)) => {
                        let hit = ray.intersect_plane(p0, drag.axes[i]).map(|t| ray.origin + ray.dir * t).unwrap_or(drag.start_point);
                        let mut d = hit - drag.start_point;
                        if snapping {
                            d = Vec3::new(snap(d.x, self.translate_snap), snap(d.y, self.translate_snap), snap(d.z, self.translate_snap));
                        }
                        Mat4::from_scale_rotation_translation(s0, r0, p0 + d)
                    }
                    (GizmoMode::Rotate, Handle::Axis(i)) => {
                        let axis = drag.axes[i];
                        let hit = ray.intersect_plane(p0, axis).map(|t| ray.origin + ray.dir * t).unwrap_or(drag.start_point);
                        let a = (drag.start_point - p0).normalize_or_zero();
                        let b = (hit - p0).normalize_or_zero();
                        let mut angle = a.cross(b).dot(axis).atan2(a.dot(b));
                        if snapping {
                            angle = snap(angle.to_degrees(), self.rotate_snap_deg).to_radians();
                        }
                        Mat4::from_scale_rotation_translation(s0, (Quat::from_axis_angle(axis, angle) * r0).normalize(), p0)
                    }
                    (GizmoMode::Scale, Handle::Axis(i)) => {
                        let d = axis_param(&ray, p0, drag.axes[i]) - drag.start_param;
                        let mut f = (1.0 + d / size).max(0.01);
                        if snapping {
                            f = snap(f, self.scale_snap).max(self.scale_snap);
                        }
                        let mut s = s0;
                        s[i] *= f;
                        Mat4::from_scale_rotation_translation(s, r0, p0)
                    }
                    (GizmoMode::Scale, Handle::Uniform) => {
                        let mut f = (1.0 + (drag.start_param - m.y) / 100.0).max(0.01);
                        if snapping {
                            f = snap(f, self.scale_snap).max(self.scale_snap);
                        }
                        Mat4::from_scale_rotation_translation(s0 * f, r0, p0)
                    }
                    _ => drag.start_world,
                };
                result = Some(new);
            }
        }

        // ---- draw
        let active = self.drag.as_ref().map(|d| d.handle).or(self.hover);
        let col = |h: Handle, i: usize| if active == Some(h) { HOVER } else { COLORS[i] };
        let draw_axes = if let Some(d) = &self.drag { d.axes } else { axes };
        let _ = scale;
        match self.mode {
            GizmoMode::Translate | GizmoMode::Scale => {
                for (i, a) in draw_axes.iter().enumerate() {
                    let Some(end) = view.to_screen(pos + *a * size) else { continue };
                    let c = col(Handle::Axis(i), i);
                    painter.line_segment([center, end], Stroke::new(3.0, c));
                    if self.mode == GizmoMode::Translate {
                        let dir = (end - center).normalized();
                        let perp = egui::vec2(-dir.y, dir.x);
                        painter.add(egui::Shape::convex_polygon(
                            vec![end + dir * 12.0, end + perp * 5.0, end - perp * 5.0],
                            c,
                            Stroke::NONE,
                        ));
                    } else {
                        painter.rect_filled(egui::Rect::from_center_size(end, egui::vec2(9.0, 9.0)), 1.0, c);
                    }
                }
                if self.mode == GizmoMode::Translate {
                    for i in 0..3 {
                        let (u, v) = (draw_axes[(i + 1) % 3], draw_axes[(i + 2) % 3]);
                        let q = [0.25, 0.45].map(|s| s * size);
                        let pts: Vec<Pos2> = [(q[0], q[0]), (q[1], q[0]), (q[1], q[1]), (q[0], q[1])]
                            .iter()
                            .filter_map(|(a, b)| view.to_screen(pos + u * *a + v * *b))
                            .collect();
                        if pts.len() == 4 {
                            let c = col(Handle::Plane(i), i).gamma_multiply(0.45);
                            painter.add(egui::Shape::convex_polygon(pts, c, Stroke::new(1.0, COLORS[i])));
                        }
                    }
                } else {
                    let c = if active == Some(Handle::Uniform) { HOVER } else { Color32::from_gray(220) };
                    painter.circle_filled(center, 6.0, c);
                }
            }
            GizmoMode::Rotate => {
                for (i, a) in draw_axes.iter().enumerate() {
                    let pts = circle_points(view, pos, *a, size);
                    painter.add(egui::Shape::line(pts, Stroke::new(2.5, col(Handle::Axis(i), i))));
                }
                painter.circle_stroke(center, 4.0, Stroke::new(1.0, Color32::WHITE));
            }
        }
        (result, started)
    }
}

fn circle_points(view: &GizmoView, center: Vec3, axis: Vec3, radius: f32) -> Vec<Pos2> {
    let u = axis.any_orthonormal_vector();
    let v = axis.cross(u);
    (0..=64)
        .filter_map(|i| {
            let t = i as f32 / 64.0 * std::f32::consts::TAU;
            view.to_screen(center + (u * t.cos() + v * t.sin()) * radius)
        })
        .collect()
}

fn point_in_quad(p: Pos2, q: &[Pos2]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let a = q[i];
        let b = q[(i + 1) % 4];
        let c = (b - a).x * (p - a).y - (b - a).y * (p - a).x;
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    true
}

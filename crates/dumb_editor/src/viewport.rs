//! Reusable 3D preview widget (model / material / animation viewers).

use crate::camera::EditorCamera;
use dumb_core::Vec2;
use dumb_render::{RenderTargetId, Renderer};

pub struct PreviewViewport {
    pub target: RenderTargetId,
    pub camera: EditorCamera,
    pub rect: egui::Rect,
    pub auto_rotate: bool,
}

impl PreviewViewport {
    pub fn new(renderer: &mut Renderer) -> Self {
        PreviewViewport {
            target: renderer.create_target(512, 512),
            camera: EditorCamera { distance: 3.0, pivot: dumb_core::Vec3::ZERO, ..Default::default() },
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(512.0, 512.0)),
            auto_rotate: false,
        }
    }

    pub fn aspect(&self) -> f32 {
        self.rect.width().max(1.0) / self.rect.height().max(1.0)
    }

    /// Show the rendered image and handle orbit (LMB), pan (MMB / Shift+LMB) and zoom (wheel).
    pub fn show(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer, size: egui::Vec2) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(size.max(egui::vec2(32.0, 32.0)), egui::Sense::click_and_drag());
        self.rect = rect;
        let ppp = ui.ctx().pixels_per_point();
        renderer.resize_target(self.target, (rect.width() * ppp) as u32, (rect.height() * ppp) as u32);
        if let Some(tex) = renderer.target_texture(self.target) {
            ui.painter().image(tex, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        let d = resp.drag_delta();
        let delta = Vec2::new(d.x, d.y);
        let shift = ui.input(|i| i.modifiers.shift);
        if resp.dragged_by(egui::PointerButton::Primary) && !shift {
            self.camera.orbit(delta);
        }
        if resp.dragged_by(egui::PointerButton::Middle) || (resp.dragged_by(egui::PointerButton::Primary) && shift) {
            self.camera.pan(delta, rect.height());
        }
        if resp.dragged_by(egui::PointerButton::Secondary) {
            self.camera.look(delta);
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.camera.zoom(scroll / 50.0);
            }
        }
        if self.auto_rotate && !resp.dragged() {
            self.camera.yaw += ui.input(|i| i.stable_dt) * 0.4;
            ui.ctx().request_repaint();
        }
        resp
    }
}

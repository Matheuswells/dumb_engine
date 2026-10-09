//! Startup splash screen, in the spirit of Blender's: artwork with the version, quick actions
//! on the left, recent projects on the right. Click outside it (or press Esc) to dismiss.

use crate::project_manager::recent_projects;
use egui::{epaint::Vertex, Align2, Color32, FontId, Mesh, Pos2, Rect, Shape, Stroke, Vec2};
use std::path::PathBuf;

const WIDTH: f32 = 640.0;
const ART_HEIGHT: f32 = 300.0;
const REPO: &str = "https://github.com/Matheuswells/dumb_engine";

/// What the user picked on the splash.
pub enum SplashAction {
    NewProject,
    OpenDialog,
    Open(PathBuf),
}

#[derive(Default)]
pub struct Splash {
    pub open: bool,
    /// (name, folder) of recent projects, read once when the splash opens.
    recent: Vec<(String, PathBuf)>,
}

impl Splash {
    pub fn show(&mut self) {
        self.open = true;
        self.recent = recent_projects().into_iter().take(8).map(|p| (dumb_runtime::Project::open(&p).settings.name, p)).collect();
    }

    /// `show_at_startup` is the preference the checkbox edits.
    pub fn ui(&mut self, ctx: &egui::Context, show_at_startup: &mut bool) -> Option<SplashAction> {
        if !self.open {
            return None;
        }
        let mut action = None;
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(0.0).corner_radius(8.0);
        let modal = egui::Modal::new(egui::Id::new("splash")).frame(frame).show(ctx, |ui| {
            ui.set_width(WIDTH);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(WIDTH, ART_HEIGHT), egui::Sense::hover());
            paint_art(ui.painter(), rect);

            egui::Frame::NONE.inner_margin(egui::Margin { left: 18, right: 18, top: 12, bottom: 12 }).show(ui, |ui| {
                ui.columns(2, |cols| {
                    let ui = &mut cols[0];
                    ui.label(egui::RichText::new("Get started").strong());
                    ui.add_space(4.0);
                    if link(ui, "➕  New Project…") {
                        action = Some(SplashAction::NewProject);
                    }
                    if link(ui, "📂  Open Project…") {
                        action = Some(SplashAction::OpenDialog);
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Learn").strong());
                    ui.add_space(4.0);
                    ui.hyperlink_to("📖  Documentation", format!("{REPO}#readme"));
                    ui.hyperlink_to("🔌  Connect an AI agent (MCP)", format!("{REPO}/blob/main/docs/MCP.md"));
                    ui.hyperlink_to("🎁  Release notes", format!("{REPO}/releases"));

                    let ui = &mut cols[1];
                    ui.label(egui::RichText::new("Recent Projects").strong());
                    ui.add_space(4.0);
                    if self.recent.is_empty() {
                        ui.weak("Nothing yet. Make something dumb!");
                    }
                    for (name, path) in &self.recent {
                        let r = ui.add(egui::Button::new(format!("🎮  {name}")).frame(false)).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(path.display().to_string());
                        if r.clicked() {
                            action = Some(SplashAction::Open(path.clone()));
                        }
                    }
                });
                ui.add_space(6.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.checkbox(show_at_startup, "Show at startup");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak("Click outside to start creating");
                    });
                });
            });
        });
        if modal.should_close() || action.is_some() {
            self.open = false;
        }
        action
    }
}

/// A borderless button that reads like a link, Blender-style.
fn link(ui: &mut egui::Ui, text: &str) -> bool {
    ui.add(egui::Button::new(text).frame(false)).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// Vertical gradient through `stops` (position 0..1 within `rect`, color).
fn gradient(rect: Rect, stops: &[(f32, Color32)]) -> Shape {
    let mut mesh = Mesh::default();
    for (i, (t, c)) in stops.iter().enumerate() {
        let y = rect.top() + rect.height() * t;
        mesh.vertices.push(Vertex { pos: Pos2::new(rect.left(), y), uv: egui::epaint::WHITE_UV, color: *c });
        mesh.vertices.push(Vertex { pos: Pos2::new(rect.right(), y), uv: egui::epaint::WHITE_UV, color: *c });
        if i > 0 {
            let b = (i as u32) * 2;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b + 1, b);
        }
    }
    Shape::mesh(mesh)
}

/// An isometric crate: top, left and right faces.
fn crate_box(p: &egui::Painter, center: Pos2, s: f32, base: Color32) {
    let h = s * 0.5;
    let top = [center + Vec2::new(0.0, -s), center + Vec2::new(s, -h), center, center + Vec2::new(-s, -h)];
    let left = [center + Vec2::new(-s, -h), center, center + Vec2::new(0.0, s), center + Vec2::new(-s, h)];
    let right = [center, center + Vec2::new(s, -h), center + Vec2::new(s, h), center + Vec2::new(0.0, s)];
    let shade = |f: f32| Color32::from_rgb((base.r() as f32 * f) as u8, (base.g() as f32 * f) as u8, (base.b() as f32 * f) as u8);
    let edge = Stroke::new(1.0, Color32::from_rgba_unmultiplied(60, 30, 20, 90));
    p.add(Shape::convex_polygon(top.to_vec(), shade(1.08), edge));
    p.add(Shape::convex_polygon(left.to_vec(), shade(0.82), edge));
    p.add(Shape::convex_polygon(right.to_vec(), shade(0.62), edge));
}

fn paint_art(p: &egui::Painter, rect: Rect) {
    let p = p.with_clip_rect(rect);
    let horizon = rect.top() + rect.height() * 0.62;

    // Sunset sky, like the engine's built-in panorama.
    let sky = Rect::from_min_max(rect.min, Pos2::new(rect.right(), horizon));
    p.add(gradient(
        sky,
        &[(0.0, Color32::from_rgb(34, 28, 74)), (0.45, Color32::from_rgb(140, 72, 120)), (0.8, Color32::from_rgb(240, 140, 96)), (1.0, Color32::from_rgb(255, 196, 130))],
    ));
    // Sun with a soft glow.
    let sun = Pos2::new(rect.left() + rect.width() * 0.68, horizon - 8.0);
    for i in (1..=6).rev() {
        let a = (60 / i) as u8;
        p.circle_filled(sun, 20.0 + i as f32 * 12.0, Color32::from_rgba_unmultiplied(255, 210, 150, a));
    }
    p.circle_filled(sun, 22.0, Color32::from_rgb(255, 236, 200));
    // A few stars up top.
    for (i, (x, y)) in [(0.08, 0.10), (0.21, 0.22), (0.37, 0.07), (0.55, 0.15), (0.83, 0.09), (0.93, 0.25), (0.12, 0.33)].iter().enumerate() {
        let r = if i % 3 == 0 { 1.6 } else { 1.0 };
        p.circle_filled(Pos2::new(rect.left() + rect.width() * x, rect.top() + rect.height() * y), r, Color32::from_rgba_unmultiplied(255, 255, 255, 170));
    }

    // Ground with a perspective grid fading into the horizon.
    let ground = Rect::from_min_max(Pos2::new(rect.left(), horizon), rect.max);
    p.add(gradient(ground, &[(0.0, Color32::from_rgb(70, 46, 70)), (1.0, Color32::from_rgb(22, 20, 34))]));
    let vanish = Pos2::new(rect.center().x, horizon);
    let grid = |t: f32| Color32::from_rgba_unmultiplied(255, 170, 120, (25.0 + 70.0 * t) as u8);
    for i in -12..=12 {
        let x = rect.center().x + i as f32 * 70.0;
        p.line_segment([vanish, Pos2::new(x, rect.bottom())], Stroke::new(1.0, grid(0.6)));
    }
    let mut d = 1.0f32;
    while d < 40.0 {
        let y = horizon + (rect.bottom() - horizon) / d;
        p.line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], Stroke::new(1.0, grid(1.0 / d.sqrt())));
        d *= 1.55;
    }
    p.line_segment([Pos2::new(rect.left(), horizon), Pos2::new(rect.right(), horizon)], Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 220, 170, 160)));

    // The demo's crate pyramid ("supports colored rectangles") and a gold orb.
    let s = 22.0;
    let base = Pos2::new(rect.left() + rect.width() * 0.74, rect.bottom() - 52.0);
    let wood = Color32::from_rgb(222, 178, 128);
    for row in 0..4 {
        let n = 4 - row;
        for i in 0..n {
            let x = base.x + (i as f32 - (n - 1) as f32 / 2.0) * s * 2.0;
            crate_box(&p, Pos2::new(x, base.y - row as f32 * s * 1.5), s, wood);
        }
    }
    let orb = Pos2::new(base.x - 150.0, rect.bottom() - 46.0);
    p.add(Shape::ellipse_filled(orb + Vec2::new(14.0, 26.0), Vec2::new(30.0, 7.0), Color32::from_black_alpha(90)));
    for i in 0..14 {
        let t = i as f32 / 13.0;
        let c = lerp_color(Color32::from_rgb(150, 98, 20), Color32::from_rgb(255, 238, 170), t);
        p.circle_filled(orb + Vec2::new(-8.0, -8.0) * t, 28.0 * (1.0 - t * 0.85), c);
    }
    // A red ball, because every engine demo needs one.
    let ball = Pos2::new(orb.x + 62.0, rect.bottom() - 36.0);
    p.add(Shape::ellipse_filled(ball + Vec2::new(8.0, 16.0), Vec2::new(18.0, 5.0), Color32::from_black_alpha(90)));
    for i in 0..10 {
        let t = i as f32 / 9.0;
        p.circle_filled(ball + Vec2::new(-5.0, -5.0) * t, 17.0 * (1.0 - t * 0.8), lerp_color(Color32::from_rgb(120, 10, 16), Color32::from_rgb(255, 120, 110), t));
    }

    // Title, tagline and version.
    let shadow = Color32::from_black_alpha(120);
    let title_pos = Pos2::new(rect.left() + 26.0, rect.top() + 34.0);
    let title = FontId::proportional(44.0);
    p.text(title_pos + Vec2::new(2.0, 3.0), Align2::LEFT_TOP, "Dumb Engine", title.clone(), shadow);
    p.text(title_pos, Align2::LEFT_TOP, "Dumb Engine", title, Color32::WHITE);
    let tag = FontId::proportional(15.0);
    let tag_pos = title_pos + Vec2::new(2.0, 56.0);
    p.text(tag_pos + Vec2::new(1.0, 1.0), Align2::LEFT_TOP, "Minimal brainpower. Maximum enthusiasm.", tag.clone(), shadow);
    p.text(tag_pos, Align2::LEFT_TOP, "Minimal brainpower. Maximum enthusiasm.", tag, Color32::from_rgb(255, 226, 200));
    p.text(rect.right_top() + Vec2::new(-16.0, 14.0), Align2::RIGHT_TOP, concat!("v", env!("CARGO_PKG_VERSION")), FontId::proportional(16.0), Color32::from_rgba_unmultiplied(255, 255, 255, 220));
    p.text(rect.left_bottom() + Vec2::new(16.0, -12.0), Align2::LEFT_BOTTOM, "Rust · Vulkan · 100% artisanal rectangles", FontId::proportional(11.0), Color32::from_rgba_unmultiplied(255, 255, 255, 150));
}

//! Draws a `Hud` (scripts' commands + HUD components) with egui over the game view.

use dumb_asset::AssetDatabase;
use dumb_core::{AssetId, Color, Mat4, Vec2, Vec3};
use dumb_ecs::{Anchor, Hud, HudCmd, HudPos};
use dumb_render::Renderer;
use std::collections::HashMap;
use std::time::Duration;

/// egui texture ids used for HUD images.
const TEX_BASE: u64 = 50_000_000;

struct HudImage {
    version: u64,
    size: egui::Vec2,
    /// One texture per frame; GIFs have several with their delays.
    frames: Vec<(egui::TextureId, Duration)>,
    total: Duration,
}

/// A web panel the host should show this frame (id, url, rect in points).
#[derive(Clone, Debug, PartialEq)]
pub struct WebRequest {
    pub id: String,
    pub url: String,
    pub rect: egui::Rect,
}

#[derive(Default)]
pub struct HudView {
    images: HashMap<AssetId, HudImage>,
    next_tex: u64,
}

fn c32(c: Color) -> egui::Color32 {
    egui::Rgba::from_rgba_unmultiplied(c.r, c.g, c.b, c.a).into()
}

fn align(a: Anchor) -> egui::Align2 {
    match a {
        Anchor::TopLeft => egui::Align2::LEFT_TOP,
        Anchor::TopCenter => egui::Align2::CENTER_TOP,
        Anchor::TopRight => egui::Align2::RIGHT_TOP,
        Anchor::CenterLeft => egui::Align2::LEFT_CENTER,
        Anchor::Center => egui::Align2::CENTER_CENTER,
        Anchor::CenterRight => egui::Align2::RIGHT_CENTER,
        Anchor::BottomLeft => egui::Align2::LEFT_BOTTOM,
        Anchor::BottomCenter => egui::Align2::CENTER_BOTTOM,
        Anchor::BottomRight => egui::Align2::RIGHT_BOTTOM,
    }
}

/// Screen point of an anchored position.
fn point(screen: egui::Rect, p: HudPos) -> egui::Pos2 {
    let f = p.anchor.fraction();
    egui::pos2(screen.left() + screen.width() * f.x + p.offset.x, screen.top() + screen.height() * f.y + p.offset.y)
}

/// Rect of an element of `size` placed at an anchored position (aligned like its anchor).
pub fn element_rect(screen: egui::Rect, p: HudPos, size: Vec2) -> egui::Rect {
    align(p.anchor).anchor_size(point(screen, p), egui::vec2(size.x, size.y))
}

impl HudView {
    /// Draw the HUD into `screen`. Returns the web panels to show. Button clicks go into
    /// `hud.clicked` for the next frame.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        screen: egui::Rect,
        hud: &mut Hud,
        view_proj: Mat4,
        camera_pos: Vec3,
        db: &mut AssetDatabase,
        renderer: &mut Renderer,
        time: f64,
        interactive: bool,
    ) -> Vec<WebRequest> {
        let painter = ui.painter_at(screen);
        let mut clicked = Vec::new();
        let mut webs = Vec::new();
        let mut over_ui = false;
        for (k, cmd) in hud.commands.iter().enumerate() {
            match cmd {
                HudCmd::Panel { pos, size, color, rounding } => {
                    painter.rect_filled(element_rect(screen, *pos, *size), *rounding, c32(*color));
                }
                HudCmd::Text { pos, text, size, color } => {
                    let font = egui::FontId::proportional(*size);
                    let p = point(screen, *pos);
                    // Soft shadow keeps text readable over any background.
                    painter.text(p + egui::vec2(1.0, 1.5), align(pos.anchor), text, font.clone(), egui::Color32::from_black_alpha(160));
                    painter.text(p, align(pos.anchor), text, font, c32(*color));
                }
                HudCmd::Bar { pos, size, value, fg, bg, label } => {
                    let r = element_rect(screen, *pos, *size);
                    let round = (size.y * 0.35).min(8.0);
                    painter.rect_filled(r, round, c32(*bg));
                    let fill = egui::Rect::from_min_size(r.min, egui::vec2(r.width() * value.clamp(0.0, 1.0), r.height()));
                    painter.rect_filled(fill.shrink(1.0), round, c32(*fg));
                    painter.rect_stroke(r, round, egui::Stroke::new(1.0, egui::Color32::from_white_alpha(40)), egui::StrokeKind::Inside);
                    if !label.is_empty() {
                        painter.text(r.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional((size.y * 0.7).max(9.0)), egui::Color32::WHITE);
                    }
                }
                HudCmd::Image { pos, size, image, tint } => {
                    if let Some((tex, natural)) = self.image(*image, db, renderer, time) {
                        let s = if size.x <= 0.0 || size.y <= 0.0 { Vec2::new(natural.x, natural.y) } else { *size };
                        let r = element_rect(screen, *pos, s);
                        painter.image(tex, r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), c32(*tint));
                    }
                }
                HudCmd::Button { id, pos, size, label } => {
                    let r = element_rect(screen, *pos, *size);
                    let resp = ui.interact(r, ui.id().with(("hud_button", id, k)), if interactive { egui::Sense::click() } else { egui::Sense::hover() });
                    let hovered = interactive && resp.hovered();
                    over_ui |= hovered;
                    let bg = if resp.is_pointer_button_down_on() {
                        egui::Color32::from_rgb(40, 90, 170)
                    } else if hovered {
                        egui::Color32::from_rgb(60, 120, 210)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(30, 34, 42, 220)
                    };
                    painter.rect_filled(r, 6.0, bg);
                    painter.rect_stroke(r, 6.0, egui::Stroke::new(1.0, egui::Color32::from_white_alpha(if hovered { 120 } else { 50 })), egui::StrokeKind::Inside);
                    painter.text(r.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional((size.y * 0.42).clamp(10.0, 28.0)), egui::Color32::WHITE);
                    if resp.clicked() {
                        clicked.push(id.clone());
                    }
                }
                HudCmd::WorldText { world, text, size, color, max_distance } => {
                    if *max_distance > 0.0 && world.distance(camera_pos) > *max_distance {
                        continue;
                    }
                    let clip = view_proj * world.extend(1.0);
                    if clip.w <= 0.01 {
                        continue;
                    }
                    let ndc = clip.truncate() / clip.w;
                    if ndc.x.abs() > 1.1 || ndc.y.abs() > 1.1 {
                        continue;
                    }
                    let p = egui::pos2(screen.left() + (ndc.x * 0.5 + 0.5) * screen.width(), screen.top() + (0.5 - ndc.y * 0.5) * screen.height());
                    let font = egui::FontId::proportional(*size);
                    painter.text(p + egui::vec2(1.0, 1.0), egui::Align2::CENTER_BOTTOM, text, font.clone(), egui::Color32::from_black_alpha(170));
                    painter.text(p, egui::Align2::CENTER_BOTTOM, text, font, c32(*color));
                }
                HudCmd::Web { id, pos, size, url } => {
                    let r = element_rect(screen, *pos, *size);
                    over_ui |= ui.rect_contains_pointer(r);
                    webs.push(WebRequest { id: id.clone(), url: url.clone(), rect: r });
                }
                HudCmd::Crosshair { size, color } => {
                    let c = screen.center();
                    let s = *size * 0.5;
                    let st = egui::Stroke::new(2.0, c32(*color));
                    painter.line_segment([c - egui::vec2(s, 0.0), c - egui::vec2(s * 0.3, 0.0)], st);
                    painter.line_segment([c + egui::vec2(s * 0.3, 0.0), c + egui::vec2(s, 0.0)], st);
                    painter.line_segment([c - egui::vec2(0.0, s), c - egui::vec2(0.0, s * 0.3)], st);
                    painter.line_segment([c + egui::vec2(0.0, s * 0.3), c + egui::vec2(0.0, s)], st);
                }
            }
        }
        hud.clicked = clicked;
        hud.pointer_over_ui = over_ui;
        if self.images.values().any(|i| i.frames.len() > 1) {
            ui.ctx().request_repaint();
        }
        webs
    }

    /// Texture of an image asset at time `t` (GIFs animate), loading it on first use.
    fn image(&mut self, id: AssetId, db: &mut AssetDatabase, renderer: &mut Renderer, t: f64) -> Option<(egui::TextureId, egui::Vec2)> {
        let version = db.version(id);
        if self.images.get(&id).is_none_or(|i| i.version != version) {
            let frames = load_frames(id, db)?;
            if let Some(old) = self.images.remove(&id) {
                for (tex, _) in old.frames {
                    renderer.free_egui_image(tex);
                }
            }
            let size = egui::vec2(frames[0].1 as f32, frames[0].2 as f32);
            let mut out = Vec::new();
            let mut total = Duration::ZERO;
            for (rgba, w, h, delay) in frames {
                self.next_tex += 1;
                let tex = egui::TextureId::User(TEX_BASE + self.next_tex);
                renderer.register_egui_image(tex, w, h, &rgba);
                total += delay;
                out.push((tex, delay));
            }
            self.images.insert(id, HudImage { version, size, frames: out, total });
        }
        let img = self.images.get(&id)?;
        if img.frames.len() == 1 || img.total.is_zero() {
            return Some((img.frames[0].0, img.size));
        }
        let mut at = Duration::from_secs_f64(t % img.total.as_secs_f64());
        for (tex, d) in &img.frames {
            if at < *d {
                return Some((*tex, img.size));
            }
            at -= *d;
        }
        Some((img.frames[0].0, img.size))
    }

    /// Free every HUD texture (stopping play mode).
    pub fn clear(&mut self, renderer: &mut Renderer) {
        for (_, img) in self.images.drain() {
            for (tex, _) in img.frames {
                renderer.free_egui_image(tex);
            }
        }
    }
}

type Frame = (Vec<u8>, u32, u32, Duration);

/// Decode an image asset: all frames of a GIF, otherwise the texture's single frame.
fn load_frames(id: AssetId, db: &mut AssetDatabase) -> Option<Vec<Frame>> {
    let path = db.abs_path(id);
    if let Some(p) = path.as_ref().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif"))) {
        if let Some(frames) = decode_gif(p) {
            return Some(frames);
        }
    }
    let t = db.texture(id)?;
    Some(vec![(t.rgba8.clone(), t.width, t.height, Duration::ZERO)])
}

/// All frames of an animated GIF (up to 300) with their delays.
pub fn decode_gif(path: &std::path::Path) -> Option<Vec<Frame>> {
    use image::AnimationDecoder;
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let decoder = image::codecs::gif::GifDecoder::new(file).ok()?;
    let mut out = Vec::new();
    for f in decoder.into_frames().take(300) {
        let f = f.ok()?;
        let (n, d) = f.delay().numer_denom_ms();
        let ms = if d == 0 { 100 } else { (n / d).max(20) };
        let buf = f.into_buffer();
        let (w, h) = buf.dimensions();
        out.push((buf.into_raw(), w, h, Duration::from_millis(ms as u64)));
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchored_rects() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let r = element_rect(screen, HudPos::bottom_right(-10.0, -10.0), Vec2::new(100.0, 20.0));
        assert_eq!(r, egui::Rect::from_min_max(egui::pos2(690.0, 570.0), egui::pos2(790.0, 590.0)));
        let c = element_rect(screen, HudPos::center(0.0, 0.0), Vec2::new(100.0, 40.0));
        assert_eq!(c.center(), egui::pos2(400.0, 300.0));
    }

    #[test]
    fn gif_frames_and_delays() {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame, RgbaImage};
        let dir = std::env::temp_dir().join(format!("dumb_gif_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.gif");
        {
            let mut enc = GifEncoder::new(std::fs::File::create(&path).unwrap());
            for i in 0..3u8 {
                let img = RgbaImage::from_pixel(4, 4, image::Rgba([i * 80, 0, 0, 255]));
                enc.encode_frame(Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(50, 1))).unwrap();
            }
        }
        let f = decode_gif(&path).unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[1].3, Duration::from_millis(50));
        assert_eq!((f[0].1, f[0].2), (4, 4));
        let _ = std::fs::remove_dir_all(dir);
    }
}

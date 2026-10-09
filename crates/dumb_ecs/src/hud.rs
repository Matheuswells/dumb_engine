//! Game HUD: screen-space UI for games.
//!
//! Two ways to build it, both drawn by the runtime on top of the game view:
//! - **Components** (`UiText`, `UiBar`, `UiImage`, `UiPanel`, `UiButton`, `UiWorldLabel`): add them
//!   to entities in the editor, edit them live, they save with the scene.
//! - **Scripts**: `ctx.hud` is an immediate-mode command list rebuilt every frame:
//!
//! ```ignore
//! ctx.hud.text(HudPos::top_left(20.0, 20.0), format!("Score {}", score), 24.0, Color::WHITE);
//! ctx.hud.bar(HudPos::bottom_center(0.0, -40.0), Vec2::new(300.0, 18.0), hp / max_hp, Color::RED);
//! if ctx.hud.button("restart", HudPos::center(0.0, 0.0), Vec2::new(160.0, 40.0), "Restart") { ... }
//! ctx.hud.world_text(head_pos, "Guard", Color::YELLOW);
//! ```
//!
//! Positions are in logical pixels relative to an anchor on the game view.

use crate::World;
use dumb_core::{AssetId, Color, Entity, Vec2, Vec3};
use dumb_derive::{Component, Editor};

#[derive(Editor, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Anchor {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Anchor {
    /// (x, y) fraction of the screen this anchor sits at.
    pub fn fraction(self) -> Vec2 {
        match self {
            Anchor::TopLeft => Vec2::new(0.0, 0.0),
            Anchor::TopCenter => Vec2::new(0.5, 0.0),
            Anchor::TopRight => Vec2::new(1.0, 0.0),
            Anchor::CenterLeft => Vec2::new(0.0, 0.5),
            Anchor::Center => Vec2::new(0.5, 0.5),
            Anchor::CenterRight => Vec2::new(1.0, 0.5),
            Anchor::BottomLeft => Vec2::new(0.0, 1.0),
            Anchor::BottomCenter => Vec2::new(0.5, 1.0),
            Anchor::BottomRight => Vec2::new(1.0, 1.0),
        }
    }
}

/// A point on the screen: an anchor plus an offset in pixels. Elements are aligned to their
/// anchor (a TopRight element grows to the left, a Center one is centered).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HudPos {
    pub anchor: Anchor,
    pub offset: Vec2,
}

macro_rules! hud_pos_ctor {
    ($($name:ident => $a:ident),*) => {
        impl HudPos {
            $(pub fn $name(x: f32, y: f32) -> Self { HudPos { anchor: Anchor::$a, offset: Vec2::new(x, y) } })*
        }
    };
}
hud_pos_ctor!(top_left => TopLeft, top_center => TopCenter, top_right => TopRight, center_left => CenterLeft, center => Center,
    center_right => CenterRight, bottom_left => BottomLeft, bottom_center => BottomCenter, bottom_right => BottomRight);

#[derive(Clone, Debug, PartialEq)]
pub enum HudCmd {
    Text { pos: HudPos, text: String, size: f32, color: Color },
    Bar { pos: HudPos, size: Vec2, value: f32, fg: Color, bg: Color, label: String },
    Image { pos: HudPos, size: Vec2, image: AssetId, tint: Color },
    Panel { pos: HudPos, size: Vec2, color: Color, rounding: f32 },
    Button { id: String, pos: HudPos, size: Vec2, label: String },
    /// Text over a point in the 3D world (projected with the game camera).
    WorldText { world: Vec3, text: String, size: f32, color: Color, max_distance: f32 },
    /// A web page (HTML/CSS/JS, https, video) in a rectangle of the screen.
    Web { id: String, pos: HudPos, size: Vec2, url: String },
    Crosshair { size: f32, color: Color },
}

/// The HUD command list scripts write into each frame.
#[derive(Default, Debug)]
pub struct Hud {
    pub commands: Vec<HudCmd>,
    /// Buttons clicked during the previous frame (filled by the runtime).
    pub clicked: Vec<String>,
    /// Pointer over any HUD button/web panel (games can ignore world clicks then).
    pub pointer_over_ui: bool,
    /// Messages pages sent with `window.ipc.postMessage(text)` since last frame: (web id, text).
    pub web_messages: Vec<(String, String)>,
    /// JavaScript to run in web panels this frame: (web id, code). Use [`Hud::web_eval`].
    pub web_eval: Vec<(String, String)>,
}

impl Hud {
    /// Start a new frame: drop last frame's commands (immediate mode).
    pub fn begin_frame(&mut self) {
        self.commands.clear();
    }

    pub fn text(&mut self, pos: HudPos, text: impl Into<String>, size: f32, color: Color) {
        self.commands.push(HudCmd::Text { pos, text: text.into(), size, color });
    }

    /// A progress/health bar; `value` is 0..1.
    pub fn bar(&mut self, pos: HudPos, size: Vec2, value: f32, color: Color) {
        self.commands.push(HudCmd::Bar { pos, size, value: value.clamp(0.0, 1.0), fg: color, bg: Color::rgba(0.0, 0.0, 0.0, 0.55), label: String::new() });
    }

    pub fn bar_labeled(&mut self, pos: HudPos, size: Vec2, value: f32, color: Color, label: impl Into<String>) {
        self.commands.push(HudCmd::Bar { pos, size, value: value.clamp(0.0, 1.0), fg: color, bg: Color::rgba(0.0, 0.0, 0.0, 0.55), label: label.into() });
    }

    /// A texture asset (PNG, JPG, animated GIF...). Size 0 = the image's own size.
    pub fn image(&mut self, pos: HudPos, size: Vec2, image: AssetId) {
        self.commands.push(HudCmd::Image { pos, size, image, tint: Color::WHITE });
    }

    pub fn panel(&mut self, pos: HudPos, size: Vec2, color: Color) {
        self.commands.push(HudCmd::Panel { pos, size, color, rounding: 6.0 });
    }

    /// A button. Returns true if it was clicked (during the previous frame).
    pub fn button(&mut self, id: &str, pos: HudPos, size: Vec2, label: impl Into<String>) -> bool {
        self.commands.push(HudCmd::Button { id: id.to_string(), pos, size, label: label.into() });
        self.clicked.iter().any(|c| c == id)
    }

    pub fn world_text(&mut self, world: Vec3, text: impl Into<String>, color: Color) {
        self.commands.push(HudCmd::WorldText { world, text: text.into(), size: 14.0, color, max_distance: 0.0 });
    }

    /// Show a web page (HTML/CSS/JS, https, video, GIF) in a screen rectangle while this is
    /// called every frame; it closes when the call stops.
    pub fn web(&mut self, id: &str, pos: HudPos, size: Vec2, url: impl Into<String>) {
        self.commands.push(HudCmd::Web { id: id.to_string(), pos, size, url: url.into() });
    }

    /// Run JavaScript in a web panel (e.g. `updateScore(12)`), to push game state to the page.
    pub fn web_eval(&mut self, id: &str, js: impl Into<String>) {
        self.web_eval.push((id.to_string(), js.into()));
    }

    /// Messages a page posted with `window.ipc.postMessage(...)` (since last frame).
    pub fn web_received<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.web_messages.iter().filter(move |(i, _)| i == id).map(|(_, m)| m.as_str())
    }

    pub fn crosshair(&mut self, size: f32, color: Color) {
        self.commands.push(HudCmd::Crosshair { size, color });
    }
}

// ---------------------------------------------------------------------------------------------
// Components (no-code HUD).

/// Text on the game screen.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct UiText {
    pub text: String,
    pub anchor: Anchor,
    pub offset: Vec2,
    #[editor(range = 6.0..=128.0)]
    pub size: f32,
    #[editor(color)]
    pub color: Color,
    pub visible: bool,
}

impl Default for UiText {
    fn default() -> Self {
        UiText { text: "Text".into(), anchor: Anchor::TopLeft, offset: Vec2::new(20.0, 20.0), size: 22.0, color: Color::WHITE, visible: true }
    }
}

/// A bar (health, stamina, loading...). Scripts set `value`.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct UiBar {
    #[editor(range = 0.0..=1.0)]
    pub value: f32,
    pub anchor: Anchor,
    pub offset: Vec2,
    pub size: Vec2,
    #[editor(color)]
    pub color: Color,
    #[editor(color)]
    pub background: Color,
    pub label: String,
    pub visible: bool,
}

impl Default for UiBar {
    fn default() -> Self {
        UiBar {
            value: 0.75,
            anchor: Anchor::BottomLeft,
            offset: Vec2::new(20.0, -20.0),
            size: Vec2::new(260.0, 18.0),
            color: Color::rgb(0.85, 0.2, 0.2),
            background: Color::rgba(0.0, 0.0, 0.0, 0.55),
            label: String::new(),
            visible: true,
        }
    }
}

/// An image (PNG, JPG, animated GIF) on the screen.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct UiImage {
    #[editor(asset = "texture")]
    pub image: AssetId,
    pub anchor: Anchor,
    pub offset: Vec2,
    /// 0 = the image's own size.
    pub size: Vec2,
    #[editor(color)]
    pub tint: Color,
    pub visible: bool,
}

impl Default for UiImage {
    fn default() -> Self {
        UiImage { image: AssetId::NONE, anchor: Anchor::TopRight, offset: Vec2::new(-20.0, 20.0), size: Vec2::new(128.0, 128.0), tint: Color::WHITE, visible: true }
    }
}

/// A colored rectangle (backgrounds for HUD groups).
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct UiPanel {
    pub anchor: Anchor,
    pub offset: Vec2,
    pub size: Vec2,
    #[editor(color)]
    pub color: Color,
    #[editor(range = 0.0..=40.0)]
    pub rounding: f32,
    pub visible: bool,
}

impl Default for UiPanel {
    fn default() -> Self {
        UiPanel { anchor: Anchor::TopLeft, offset: Vec2::new(10.0, 10.0), size: Vec2::new(300.0, 120.0), color: Color::rgba(0.05, 0.06, 0.08, 0.7), rounding: 8.0, visible: true }
    }
}

/// A clickable button. `clicks` counts presses; scripts can watch it.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct UiButton {
    pub label: String,
    pub anchor: Anchor,
    pub offset: Vec2,
    pub size: Vec2,
    #[editor(readonly)]
    pub clicks: u32,
    pub visible: bool,
}

impl Default for UiButton {
    fn default() -> Self {
        UiButton { label: "Button".into(), anchor: Anchor::Center, offset: Vec2::ZERO, size: Vec2::new(160.0, 42.0), clicks: 0, visible: true }
    }
}

/// Text floating over this entity in the world (names, health above heads).
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct UiWorldLabel {
    pub text: String,
    /// Offset from the entity's position (meters).
    pub offset: Vec3,
    #[editor(color)]
    pub color: Color,
    #[editor(range = 6.0..=64.0)]
    pub size: f32,
    /// Hide when farther than this from the camera (0 = always).
    #[editor(range = 0.0..=1000.0)]
    pub max_distance: f32,
}

impl Default for UiWorldLabel {
    fn default() -> Self {
        UiWorldLabel { text: "Label".into(), offset: Vec3::new(0.0, 2.2, 0.0), color: Color::WHITE, size: 14.0, max_distance: 60.0 }
    }
}

/// A web page shown on the game screen (HTML/CSS/JS, https, video, GIF).
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct UiWeb {
    /// https://..., file path in the project (`Assets/UI/menu.html`) or inline `<html>`.
    pub url: String,
    pub anchor: Anchor,
    pub offset: Vec2,
    pub size: Vec2,
    pub visible: bool,
}

impl Default for UiWeb {
    fn default() -> Self {
        UiWeb { url: "https://example.com".into(), anchor: Anchor::Center, offset: Vec2::ZERO, size: Vec2::new(640.0, 400.0), visible: true }
    }
}

/// Turn the HUD components of a world into commands (appended after the scripts' commands).
pub fn collect_components(world: &World, hud: &mut Hud) {
    let pos = |anchor: Anchor, offset: Vec2| HudPos { anchor, offset };
    for p in world.query_ref::<&UiPanel>().filter(|p| p.visible) {
        hud.commands.push(HudCmd::Panel { pos: pos(p.anchor, p.offset), size: p.size, color: p.color, rounding: p.rounding });
    }
    for i in world.query_ref::<&UiImage>().filter(|i| i.visible && !i.image.is_none()) {
        hud.commands.push(HudCmd::Image { pos: pos(i.anchor, i.offset), size: i.size, image: i.image, tint: i.tint });
    }
    for b in world.query_ref::<&UiBar>().filter(|b| b.visible) {
        hud.commands.push(HudCmd::Bar { pos: pos(b.anchor, b.offset), size: b.size, value: b.value.clamp(0.0, 1.0), fg: b.color, bg: b.background, label: b.label.clone() });
    }
    for t in world.query_ref::<&UiText>().filter(|t| t.visible) {
        hud.commands.push(HudCmd::Text { pos: pos(t.anchor, t.offset), text: t.text.clone(), size: t.size, color: t.color });
    }
    for (e, b) in world.query_ref::<(Entity, &UiButton)>().filter(|(_, b)| b.visible) {
        hud.commands.push(HudCmd::Button { id: format!("entity:{}", e.to_bits()), pos: pos(b.anchor, b.offset), size: b.size, label: b.label.clone() });
    }
    for (e, w) in world.query_ref::<(Entity, &UiWeb)>().filter(|(_, w)| w.visible) {
        hud.commands.push(HudCmd::Web { id: format!("entity:{}", e.to_bits()), pos: pos(w.anchor, w.offset), size: w.size, url: w.url.clone() });
    }
    for (e, l) in world.query_ref::<(Entity, &UiWorldLabel)>() {
        let p = world.global_matrix(e).w_axis.truncate() + l.offset;
        hud.commands.push(HudCmd::WorldText { world: p, text: l.text.clone(), size: l.size, color: l.color, max_distance: l.max_distance });
    }
}

/// Count clicks on `UiButton` components (ids `entity:<bits>`).
pub fn apply_button_clicks(world: &mut World, clicked: &[String]) {
    for id in clicked {
        if let Some(bits) = id.strip_prefix("entity:").and_then(|b| b.parse::<u64>().ok()) {
            if let Some(b) = world.get_mut::<UiButton>(Entity::from_bits(bits)) {
                b.clicks += 1;
            }
        }
    }
}

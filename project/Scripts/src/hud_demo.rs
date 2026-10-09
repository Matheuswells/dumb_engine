//! HUD example: score, a health bar, a button, a crosshair and names over robots.
//! Add `HudDemo` to any entity and press Play. (HUD can also be built without code: add
//! UiText / UiBar / UiImage / UiButton / UiWorldLabel components in the inspector.)

use dumb_script::prelude::*;

#[derive(Component, Editor, Clone, Debug)]
pub struct HudDemo {
    #[editor(range = 0.0..=100.0)]
    pub health: f32,
    #[editor(range = 0.0..=50.0, tooltip = "Health lost per second")]
    pub drain: f32,
    pub heals: u32,
    pub show_names: bool,
    /// Show Assets/UI/hud.html (HTML/CSS/JS, https image and video) as a web panel.
    pub show_web: bool,
}

impl Default for HudDemo {
    fn default() -> Self {
        HudDemo { health: 100.0, drain: 8.0, heals: 0, show_names: true, show_web: true }
    }
}

fn hud_demo(ctx: &mut ScriptContext) {
    let dt = ctx.time.delta;
    let Some(mut d) = ctx.world.query::<&HudDemo>().next().cloned() else { return };
    d.health = (d.health - d.drain * dt).max(0.0);

    let hud = &mut *ctx.hud;
    hud.panel(HudPos::top_left(12.0, 12.0), Vec2::new(250.0, 70.0), Color::rgba(0.05, 0.06, 0.09, 0.65));
    hud.text(HudPos::top_left(24.0, 20.0), "DUMB DEMO", 22.0, Color::rgb(1.0, 0.8, 0.3));
    hud.text(HudPos::top_left(24.0, 50.0), format!("time {:.1}s · heals {}", ctx.time.elapsed, d.heals), 15.0, Color::WHITE);

    let color = if d.health > 50.0 { Color::rgb(0.3, 0.85, 0.4) } else if d.health > 20.0 { Color::rgb(0.95, 0.75, 0.2) } else { Color::rgb(0.95, 0.25, 0.2) };
    hud.bar_labeled(HudPos::bottom_center(0.0, -30.0), Vec2::new(360.0, 22.0), d.health / 100.0, color, format!("HP {:.0}", d.health));
    if hud.button("heal", HudPos::bottom_center(0.0, -64.0), Vec2::new(140.0, 34.0), "Heal") {
        d.health = 100.0;
        d.heals += 1;
        info!("healed ({} times)", d.heals);
    }
    hud.crosshair(22.0, Color::rgba(1.0, 1.0, 1.0, 0.8));

    if d.show_web {
        hud.web("hud_web", HudPos::top_right(-12.0, 48.0), Vec2::new(380.0, 340.0), "UI/hud.html");
        // The page posts "heal" with window.ipc.postMessage; we send health back with JavaScript.
        if hud.web_received("hud_web").any(|m| m == "heal") {
            d.health = 100.0;
            d.heals += 1;
            info!("healed from the web page");
        }
        if (ctx.time.elapsed * 4.0).fract() < (ctx.time.delta as f64 * 4.0) {
            hud.web_eval("hud_web", format!("window.setHealth && setHealth({:.0})", d.health));
        }
    }

    if d.show_names {
        for (name, t) in ctx.world.query::<(&Name, &Transform)>() {
            if name.name.starts_with("Robot") || name.name == "Player" {
                hud.world_text(t.translation + Vec3::Y * 2.3, name.name.clone(), Color::rgb(1.0, 0.95, 0.6));
            }
        }
    }
    if let Some(h) = ctx.world.query::<&mut HudDemo>().next() {
        *h = d;
    }
}

pub fn register(reg: &mut Registry) {
    reg.component::<HudDemo>();
    reg.system("hud_demo::draw", hud_demo);
}

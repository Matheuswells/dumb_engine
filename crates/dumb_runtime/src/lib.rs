//! Game runtime shared by the editor's play mode and the standalone player.

use dumb_asset::AssetDatabase;
use dumb_core::{Input, Key, MouseButton, Time};
use dumb_ecs::{Animator, MeshRenderer, World};
use dumb_script::{ScriptContext, ScriptHost};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub mod export;
pub mod hud_view;
pub mod perf;
pub mod streaming;
pub mod web;

/// `project.ron` at the project root.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProjectSettings {
    pub name: String,
    /// Scene opened by the player and on editor start, relative to `Assets/`.
    #[serde(default)]
    pub startup_scene: String,
    /// Cargo workspace containing the scripts package, relative to the project root.
    #[serde(default = "default_workspace")]
    pub scripts_workspace: String,
    #[serde(default = "default_package")]
    pub scripts_package: String,
    /// Physics gravity (m/s²).
    #[serde(default = "default_gravity")]
    pub gravity: [f32; 3],
    /// Physics steps per second.
    #[serde(default = "default_physics_hz")]
    pub physics_hz: u32,
    /// Most physics steps per frame (prevents slow-motion spirals after hitches).
    #[serde(default = "default_physics_steps")]
    pub physics_max_steps: u32,
    /// Frame cap for the game; 0 = unlimited.
    #[serde(default)]
    pub max_fps: u32,
    #[serde(default = "default_true")]
    pub vsync: bool,
    #[serde(default = "default_window")]
    pub window_size: [u32; 2],
    #[serde(default)]
    pub fullscreen: bool,
    /// Build & Export options (remembered per project).
    #[serde(default)]
    pub build: BuildSettings,
    /// NPC AI scheduling (think budget and distance LOD).
    #[serde(default)]
    pub ai: dumb_ecs::AiSettings,
    /// Main-thread time per frame for spawning/despawning streamed cells (ms).
    #[serde(default = "default_stream_budget")]
    pub stream_budget_ms: f32,
    /// Performance overlay shown in the game at start (F3 cycles it while playing).
    #[serde(default)]
    pub perf_overlay: perf::PerfOverlay,
}

fn default_stream_budget() -> f32 {
    2.0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BuildSettings {
    /// Output folder; relative paths are under the project root. Empty = `Builds/<name>`.
    pub out_dir: String,
    pub release: bool,
    pub include_scripts: bool,
    /// Start the game when the export finishes.
    pub run_after: bool,
}

impl Default for BuildSettings {
    fn default() -> Self {
        BuildSettings { out_dir: String::new(), release: true, include_scripts: true, run_after: false }
    }
}

fn default_physics_hz() -> u32 {
    60
}

fn default_physics_steps() -> u32 {
    4
}

fn default_true() -> bool {
    true
}

fn default_window() -> [u32; 2] {
    [1280, 720]
}

fn default_gravity() -> [f32; 3] {
    [0.0, -9.81, 0.0]
}

fn default_workspace() -> String {
    "Scripts".into()
}

fn default_package() -> String {
    "game_scripts".into()
}

impl Default for ProjectSettings {
    fn default() -> Self {
        ProjectSettings {
            name: "New Project".into(),
            startup_scene: String::new(),
            scripts_workspace: default_workspace(),
            scripts_package: default_package(),
            gravity: default_gravity(),
            physics_hz: default_physics_hz(),
            physics_max_steps: default_physics_steps(),
            max_fps: 0,
            vsync: true,
            window_size: default_window(),
            fullscreen: false,
            build: BuildSettings::default(),
            ai: Default::default(),
            stream_budget_ms: default_stream_budget(),
            perf_overlay: Default::default(),
        }
    }
}

pub struct Project {
    pub root: PathBuf,
    pub settings: ProjectSettings,
}

impl Project {
    pub fn open(root: impl Into<PathBuf>) -> Self {
        let root: PathBuf = root.into();
        let settings = std::fs::read_to_string(root.join("project.ron"))
            .ok()
            .and_then(|s| ron::from_str(&s).ok())
            .unwrap_or_else(|| ProjectSettings {
                name: root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                ..Default::default()
            });
        let mut project = Project { root, settings };
        if !project.is_packaged() {
            project.migrate_scripts_into_project();
            // build.rs is engine-generated: bring old projects up to date.
            let scripts = project.root.join(&project.settings.scripts_workspace);
            if let Ok(true) = dumb_script::project::refresh_generated(&scripts) {
                log::info!("updated the generated build.rs in {}", scripts.display());
            }
        }
        project
    }

    /// Projects made by older builds pointed `scripts_workspace` outside the project (`..`), so
    /// their scripts crate was generated next to the project folder. Move an engine-generated
    /// crate found there into `<project>/Scripts` and point the settings at it.
    fn migrate_scripts_into_project(&mut self) {
        let norm = |p: &Path| strip_unc(&p.canonicalize().unwrap_or(p.to_path_buf()));
        let root = norm(&self.root);
        let ws = norm(&self.root.join(&self.settings.scripts_workspace));
        if ws.starts_with(&root) && ws != root {
            return;
        }
        let target = self.root.join("Scripts");
        let generated = std::fs::read_to_string(ws.join("build.rs")).is_ok_and(|s| s.contains(dumb_script::project::DISCOVERY_MARKER));
        let ours = std::fs::read_to_string(ws.join("Cargo.toml")).is_ok_and(|s| s.contains(&format!("name = \"{}\"", self.settings.scripts_package)));
        if generated && ours && !target.join("Cargo.toml").exists() {
            let _ = std::fs::create_dir_all(&target);
            for name in ["Cargo.toml", "Cargo.lock", "build.rs", ".gitignore", "src", "target"] {
                let (from, to) = (ws.join(name), target.join(name));
                if from.exists() && !to.exists() {
                    if let Err(e) = std::fs::rename(&from, &to) {
                        log::error!("moving {} into the project failed: {e}", from.display());
                    }
                }
            }
            // Re-generate the manifest so engine paths are right from the new location.
            let engine = engine_root();
            if let Err(e) = dumb_script::project::create_scripts_crate(&target, &self.settings.scripts_package, &engine, Some(&engine.join("Cargo.lock"))) {
                log::error!("updating the moved scripts crate failed: {e}");
            }
            log::info!("moved the scripts crate from {} into {}", ws.display(), target.display());
        }
        log::info!("scripts folder set to {}", target.display());
        self.settings.scripts_workspace = "Scripts".into();
        if let Err(e) = self.save() {
            log::error!("could not save project.ron: {e}");
        }
    }

    /// An exported game folder (made by Build & Export) rather than an editable project.
    pub fn is_packaged(&self) -> bool {
        self.root.join(export::MARKER).exists()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let s = ron::ser::to_string_pretty(&self.settings, ron::ser::PrettyConfig::default()).unwrap_or_default();
        std::fs::write(self.root.join("project.ron"), s)
    }

    pub fn script_host(&self) -> ScriptHost {
        let ws = self.root.join(&self.settings.scripts_workspace);
        let ws = ws.canonicalize().unwrap_or(ws);
        let mut host = ScriptHost::new(strip_unc(&ws), &self.settings.scripts_package, self.root.join("Library").join("ScriptCache"));
        // Exported games ship the library next to the executable.
        let packaged = self.root.join(export::library_file(&self.settings.scripts_package));
        if packaged.exists() {
            host.library_path = packaged;
        }
        host
    }
}

/// `canonicalize` on Windows returns `\\?\` paths that cargo dislikes.
pub fn strip_unc(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

/// Advance `Animator` time and wrap/clamp by clip length.
pub fn update_animators(world: &mut World, db: &AssetDatabase, dt: f32) {
    for (anim, mr) in world.query::<(&mut Animator, &MeshRenderer)>() {
        if !anim.playing {
            continue;
        }
        anim.time += dt * anim.speed;
        let Some(model) = db.model_loaded(mr.model) else { continue };
        let Some(ci) = model.find_clip(&anim.clip) else { continue };
        let dur = model.animations[ci].duration.max(1e-3);
        if anim.looping {
            anim.time = anim.time.rem_euclid(dur);
        } else if anim.time > dur {
            anim.time = dur;
            anim.playing = false;
        }
    }
}

/// One simulation step: scripts, animation, transform propagation.
/// One simulation step: scripts, physics, animation, transform propagation.
/// Scripts see the collision events and query state of the previous physics step.
#[derive(Clone, Copy, Debug, Default)]
pub struct SimStats {
    pub ai: dumb_ecs::AiStats,
    pub stream: streaming::StreamStats,
}

/// One simulation frame: world streaming, AI scheduling, scripts, physics, animation.
#[allow(clippy::too_many_arguments)]
pub fn simulate(
    world: &mut World,
    scripts: &mut ScriptHost,
    db: &AssetDatabase,
    time: &Time,
    input: &Input,
    mut physics: Option<&mut dumb_physics::Physics>,
    streamer: Option<&mut streaming::Streamer>,
    settings: &ProjectSettings,
    hud: &mut dumb_ecs::Hud,
) -> SimStats {
    world.update_transforms();
    hud.begin_frame();
    dumb_ecs::hud::apply_button_clicks(world, &hud.clicked);
    let focus = dumb_render::find_primary_camera(world).map(|c| world.global_matrix(c).w_axis.truncate());
    let mut stats = SimStats::default();
    if let Some(s) = streamer {
        let budget = std::time::Duration::from_secs_f32(settings.stream_budget_ms.max(0.1) / 1000.0);
        dumb_core::profile_scope!("streaming");
        stats.stream = s.update(world, db, focus, budget);
        world.update_transforms();
    }
    // Decide which NPCs think this frame, before scripts run.
    stats.ai = {
        dumb_core::profile_scope!("ai orchestrator");
        dumb_ecs::ai::orchestrate(world, time.delta, focus, &settings.ai)
    };
    let lookup = |p: &str| db.id_for_path(p);
    let no_physics = dumb_core::physics::NoPhysics;
    let query: &dyn dumb_core::physics::PhysicsQuery = match &physics {
        Some(p) => &**p,
        None => &no_physics,
    };
    let mut ctx = ScriptContext { world, time, input, assets: &lookup, physics: query, hud: &mut *hud };
    {
        dumb_core::profile_scope!("scripts");
        scripts.run_systems(&mut ctx);
    }
    world.update_transforms();
    if let Some(p) = physics.as_deref_mut() {
        dumb_core::profile_scope!("physics");
        p.step(world, db, time.delta);
    }
    {
        dumb_core::profile_scope!("animation");
        update_animators(world, db, time.delta);
    }
    {
        dumb_core::profile_scope!("transforms");
        world.update_transforms();
    }
    // Events sent this frame become readable next frame.
    world.update_events();
    // HUD components draw after the scripts' own HUD commands.
    dumb_ecs::hud::collect_components(world, hud);
    stats
}

/// Map winit physical keys to engine keys.
pub fn map_key(code: winit::keyboard::KeyCode) -> Option<Key> {
    use winit::keyboard::KeyCode as K;
    Some(match code {
        K::KeyA => Key::A,
        K::KeyB => Key::B,
        K::KeyC => Key::C,
        K::KeyD => Key::D,
        K::KeyE => Key::E,
        K::KeyF => Key::F,
        K::KeyG => Key::G,
        K::KeyH => Key::H,
        K::KeyI => Key::I,
        K::KeyJ => Key::J,
        K::KeyK => Key::K,
        K::KeyL => Key::L,
        K::KeyM => Key::M,
        K::KeyN => Key::N,
        K::KeyO => Key::O,
        K::KeyP => Key::P,
        K::KeyQ => Key::Q,
        K::KeyR => Key::R,
        K::KeyS => Key::S,
        K::KeyT => Key::T,
        K::KeyU => Key::U,
        K::KeyV => Key::V,
        K::KeyW => Key::W,
        K::KeyX => Key::X,
        K::KeyY => Key::Y,
        K::KeyZ => Key::Z,
        K::Digit0 => Key::Num0,
        K::Digit1 => Key::Num1,
        K::Digit2 => Key::Num2,
        K::Digit3 => Key::Num3,
        K::Digit4 => Key::Num4,
        K::Digit5 => Key::Num5,
        K::Digit6 => Key::Num6,
        K::Digit7 => Key::Num7,
        K::Digit8 => Key::Num8,
        K::Digit9 => Key::Num9,
        K::Space => Key::Space,
        K::Enter => Key::Enter,
        K::Escape => Key::Escape,
        K::Tab => Key::Tab,
        K::Backspace => Key::Backspace,
        K::Delete => Key::Delete,
        K::ArrowLeft => Key::Left,
        K::ArrowRight => Key::Right,
        K::ArrowUp => Key::Up,
        K::ArrowDown => Key::Down,
        K::ShiftLeft => Key::LShift,
        K::ShiftRight => Key::RShift,
        K::ControlLeft => Key::LCtrl,
        K::ControlRight => Key::RCtrl,
        K::AltLeft => Key::LAlt,
        K::AltRight => Key::RAlt,
        K::F1 => Key::F1,
        K::F2 => Key::F2,
        K::F3 => Key::F3,
        K::F4 => Key::F4,
        K::F5 => Key::F5,
        K::F6 => Key::F6,
        K::F7 => Key::F7,
        K::F8 => Key::F8,
        K::F9 => Key::F9,
        K::F10 => Key::F10,
        K::F11 => Key::F11,
        K::F12 => Key::F12,
        _ => return None,
    })
}

pub fn map_mouse(b: winit::event::MouseButton) -> Option<MouseButton> {
    match b {
        winit::event::MouseButton::Left => Some(MouseButton::Left),
        winit::event::MouseButton::Right => Some(MouseButton::Right),
        winit::event::MouseButton::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

/// Feed a winit window event into an `Input` snapshot.
pub fn feed_input(input: &mut Input, event: &winit::event::WindowEvent) {
    use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
    use winit::keyboard::PhysicalKey;
    match event {
        WindowEvent::KeyboardInput { event, .. } => {
            if let PhysicalKey::Code(code) = event.physical_key {
                if let Some(k) = map_key(code) {
                    input.set_key(k, event.state == ElementState::Pressed);
                }
            }
        }
        WindowEvent::MouseInput { state, button, .. } => {
            if let Some(b) = map_mouse(*button) {
                input.set_mouse(b, *state == ElementState::Pressed);
            }
        }
        WindowEvent::CursorMoved { position, .. } => {
            let p = dumb_core::Vec2::new(position.x as f32, position.y as f32);
            input.mouse_delta += p - input.mouse_pos;
            input.mouse_pos = p;
        }
        WindowEvent::MouseWheel { delta, .. } => {
            input.scroll += match delta {
                MouseScrollDelta::LineDelta(_, y) => *y,
                MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
            };
        }
        WindowEvent::Focused(false) => input.clear(),
        _ => {}
    }
}

/// Root of the engine source tree (scripts crates depend on its crates by path).
/// `DUMB_ENGINE_ROOT` overrides the location the editor was built from.
pub fn engine_root() -> PathBuf {
    if let Some(p) = std::env::var_os("DUMB_ENGINE_ROOT").map(PathBuf::from).filter(|p| p.join("crates").exists()) {
        return p;
    }
    // Release bundles ship the engine sources next to the executables.
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        if dir.join("crates").join("dumb_script").exists() {
            return strip_unc(&dir.canonicalize().unwrap_or(dir));
        }
    }
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    strip_unc(&p.canonicalize().unwrap_or(p))
}

/// Scripts must be built by the same rustc as the editor (no stable ABI). Release bundles pin
/// it in `rust-toolchain.toml`; export that channel as `RUSTUP_TOOLCHAIN` so every cargo the
/// editor starts (script builds, checks, exports) uses it, wherever the project lives.
/// Call at startup, before any threads exist.
pub fn pin_rust_toolchain() {
    if std::env::var_os("RUSTUP_TOOLCHAIN").is_some() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(engine_root().join("rust-toolchain.toml")) else { return };
    if let Some(channel) = toolchain_channel(&text) {
        log::info!("scripts build with Rust {channel} (rust-toolchain.toml)");
        std::env::set_var("RUSTUP_TOOLCHAIN", channel);
    }
}

fn toolchain_channel(toml: &str) -> Option<String> {
    toml.lines()
        .filter_map(|l| l.trim().strip_prefix("channel"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .map(|v| v.trim().trim_matches('"').to_string())
        .find(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_toolchain_channel() {
        assert_eq!(toolchain_channel("[toolchain]\nchannel = \"1.90.0\"\nprofile = \"minimal\"\n").as_deref(), Some("1.90.0"));
        assert_eq!(toolchain_channel("[toolchain]\n"), None);
    }

    #[test]
    fn scripts_outside_the_project_are_moved_in() {
        let base = std::env::temp_dir().join(format!("dumb_migrate_{}", std::process::id()));
        let proj = base.join("Game");
        std::fs::create_dir_all(proj.join("Assets")).unwrap();
        std::fs::write(proj.join("project.ron"), "(name: \"Game\", scripts_workspace: \"..\", scripts_package: \"game_scripts\")").unwrap();
        // An engine-generated crate next to the project, with a user script.
        dumb_script::project::create_scripts_crate(&base, "game_scripts", &engine_root(), None).unwrap();
        std::fs::write(base.join("src/my_script.rs"), "// mine").unwrap();
        // Unrelated sibling content must stay put.
        std::fs::create_dir_all(base.join("Other")).unwrap();

        let p = Project::open(&proj);
        assert_eq!(p.settings.scripts_workspace, "Scripts");
        assert_eq!(std::fs::read_to_string(proj.join("Scripts/src/my_script.rs")).unwrap(), "// mine");
        assert!(proj.join("Scripts/Cargo.toml").exists() && !base.join("Cargo.toml").exists());
        assert!(base.join("Other").exists());
        assert!(std::fs::read_to_string(proj.join("project.ron")).unwrap().contains("\"Scripts\""));
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// Caps the frame rate. `std::thread::sleep` is coarse on Windows (~1-15 ms), so it sleeps most of
/// the remaining time and spins for the last stretch.
pub struct FrameLimiter {
    next: std::time::Instant,
}

impl Default for FrameLimiter {
    fn default() -> Self {
        FrameLimiter { next: std::time::Instant::now() }
    }
}

impl FrameLimiter {
    /// Wait so frames are at most `max_fps` per second. 0 = unlimited (returns immediately).
    pub fn wait(&mut self, max_fps: u32) {
        let now = std::time::Instant::now();
        if max_fps == 0 {
            self.next = now;
            return;
        }
        let period = std::time::Duration::from_secs_f64(1.0 / max_fps as f64);
        // After a long frame, don't try to catch up.
        if self.next + period < now {
            self.next = now;
        }
        let target = self.next + period;
        loop {
            let now = std::time::Instant::now();
            if now >= target {
                break;
            }
            let left = target - now;
            if left > std::time::Duration::from_millis(2) {
                std::thread::sleep(left - std::time::Duration::from_millis(1));
            } else {
                std::hint::spin_loop();
            }
        }
        self.next = target;
    }
}

impl Project {
    /// Apply the project's physics settings to a simulation.
    pub fn configure_physics(&self, p: &mut dumb_physics::Physics) {
        p.set_gravity(dumb_core::Vec3::from(self.settings.gravity));
        p.fixed_dt = 1.0 / self.settings.physics_hz.clamp(10, 1000) as f32;
        p.max_steps = self.settings.physics_max_steps.clamp(1, 32);
    }
}

#[cfg(test)]
mod limiter_tests {
    use super::FrameLimiter;
    use std::time::Instant;

    #[test]
    fn caps_frame_rate() {
        let mut l = FrameLimiter::default();
        let t0 = Instant::now();
        for _ in 0..30 {
            l.wait(120);
        }
        let s = t0.elapsed().as_secs_f64();
        // 30 frames at 120 fps = 0.25 s.
        assert!((0.23..0.30).contains(&s), "took {s}");
        let t1 = Instant::now();
        for _ in 0..1000 {
            l.wait(0);
        }
        assert!(t1.elapsed().as_millis() < 50, "unlimited must not wait");
    }
}

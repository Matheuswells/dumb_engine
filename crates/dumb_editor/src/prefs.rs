//! Editor preferences (per user, `%APPDATA%/dumb-engine/editor.ron`) and the Preferences window,
//! which also edits the open project's settings (`project.ron`).

use dumb_runtime::Project;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

/// Everything about the editor itself. Persisted per user.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    // Performance
    /// Frame cap while focused; 0 = unlimited.
    pub max_fps: u32,
    /// Frame cap while the window is in the background; 0 = same as focused.
    pub background_fps: u32,
    pub vsync: bool,
    // Interface
    pub ui_scale: f32,
    pub theme: Theme,
    // Viewport
    pub show_grid: bool,
    pub show_bounds: bool,
    pub wireframe: bool,
    pub frustum_cull: bool,
    /// Performance overlay in the scene view (F3 cycles it).
    pub scene_overlay: dumb_runtime::perf::PerfOverlay,
    pub show_icons: bool,
    pub show_colliders: bool,
    pub camera_fov: f32,
    pub camera_speed: f32,
    pub look_sensitivity: f32,
    pub invert_y: bool,
    // Gizmos
    pub snap_translate: f32,
    pub snap_rotate_degrees: f32,
    pub snap_scale: f32,
    // Scripts
    pub scripts_build_on_save: bool,
    pub scripts_auto_reload: bool,
    /// How scripts are opened: empty = built-in code editor, `vscode` = VS Code (or the default
    /// app), anything else is a command.
    /// `{file}` and `{line}` are replaced.
    pub code_editor: String,
    // Play mode
    pub save_before_play: bool,
    pub clear_console_on_play: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        EditorSettings {
            max_fps: 0,
            background_fps: 15,
            vsync: true,
            ui_scale: 1.0,
            theme: Theme::Dark,
            show_grid: true,
            show_bounds: false,
            wireframe: false,
            frustum_cull: true,
            scene_overlay: dumb_runtime::perf::PerfOverlay::Minimal,
            show_icons: true,
            show_colliders: true,
            camera_fov: 60.0,
            camera_speed: 8.0,
            look_sensitivity: 1.0,
            invert_y: false,
            snap_translate: 0.5,
            snap_rotate_degrees: 15.0,
            snap_scale: 0.1,
            scripts_build_on_save: false,
            scripts_auto_reload: true,
            code_editor: String::new(),
            save_before_play: false,
            clear_console_on_play: true,
        }
    }
}

fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("dumb-engine").join("editor.ron")
}

impl EditorSettings {
    pub fn load() -> Self {
        std::fs::read_to_string(path()).ok().and_then(|s| ron::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()) {
            if let Err(e) = std::fs::write(&p, s) {
                log::warn!("could not save editor preferences: {e}");
            }
        }
    }

    /// Frame cap for the current focus state (0 = unlimited).
    pub fn frame_cap(&self, focused: bool) -> u32 {
        if !focused && self.background_fps > 0 {
            if self.max_fps == 0 { self.background_fps } else { self.background_fps.min(self.max_fps) }
        } else {
            self.max_fps
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Editor,
    Project,
}

#[derive(Default)]
pub struct PreferencesWindow {
    pub open: bool,
    tab: Tab,
}

/// What changed in the Preferences window this frame.
#[derive(Default)]
pub struct PrefsChanges {
    pub editor: bool,
    pub project: bool,
}

fn fps_field(ui: &mut egui::Ui, label: &str, fps: &mut u32, allow_same: bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let mut unlimited = *fps == 0;
        let text = if allow_same { "Same as focused" } else { "Unlimited" };
        if ui.checkbox(&mut unlimited, text).changed() {
            *fps = if unlimited { 0 } else { 60 };
            changed = true;
        }
        if !unlimited {
            changed |= ui.add(egui::DragValue::new(fps).range(1..=1000).suffix(" fps")).changed();
            for preset in [30, 60, 120, 144, 240] {
                if ui.small_button(preset.to_string()).clicked() {
                    *fps = preset;
                    changed = true;
                }
            }
        }
    });
    changed
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.strong(title);
    ui.separator();
}

impl PreferencesWindow {
    pub fn ui(&mut self, ctx: &egui::Context, s: &mut EditorSettings, project: &mut Project, scenes: &[String]) -> PrefsChanges {
        let mut ch = PrefsChanges::default();
        if !self.open {
            return ch;
        }
        let mut open = self.open;
        egui::Window::new("⚙ Preferences").open(&mut open).default_size([560.0, 520.0]).collapsible(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Editor, "🖥 Editor");
                ui.selectable_value(&mut self.tab, Tab::Project, format!("📂 Project — {}", project.settings.name));
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                Tab::Editor => ch.editor = editor_tab(ui, s),
                Tab::Project => ch.project = project_tab(ui, project, scenes),
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Restore defaults").clicked() {
                    match self.tab {
                        Tab::Editor => {
                            *s = EditorSettings::default();
                            ch.editor = true;
                        }
                        Tab::Project => {
                            let keep = project.settings.clone();
                            project.settings = dumb_runtime::ProjectSettings {
                                name: keep.name,
                                startup_scene: keep.startup_scene,
                                scripts_workspace: keep.scripts_workspace,
                                scripts_package: keep.scripts_package,
                                ..Default::default()
                            };
                            ch.project = true;
                        }
                    }
                }
                ui.weak(match self.tab {
                    Tab::Editor => "Saved to your user profile; applies to every project.",
                    Tab::Project => "Saved to project.ron; used by the game and the player.",
                });
            });
        });
        self.open = open;
        ch
    }
}

fn editor_tab(ui: &mut egui::Ui, s: &mut EditorSettings) -> bool {
    let mut c = false;
    section(ui, "Performance");
    c |= fps_field(ui, "Max FPS", &mut s.max_fps, false);
    c |= fps_field(ui, "In background", &mut s.background_fps, true);
    c |= ui.checkbox(&mut s.vsync, "VSync (caps to the monitor refresh rate)").changed();
    if s.vsync && s.max_fps == 0 {
        ui.weak("Unlimited FPS needs VSync off to go above the refresh rate.");
    }

    section(ui, "Interface");
    ui.horizontal(|ui| {
        ui.label("UI scale");
        c |= ui.add(egui::Slider::new(&mut s.ui_scale, 0.6..=2.0).step_by(0.05)).changed();
    });
    ui.horizontal(|ui| {
        ui.label("Theme");
        c |= ui.selectable_value(&mut s.theme, Theme::Dark, "Dark").changed();
        c |= ui.selectable_value(&mut s.theme, Theme::Light, "Light").changed();
    });

    section(ui, "Scene view");
    ui.horizontal(|ui| {
        ui.label("Performance overlay");
        for m in [dumb_runtime::perf::PerfOverlay::Off, dumb_runtime::perf::PerfOverlay::Minimal, dumb_runtime::perf::PerfOverlay::Full] {
            c |= ui.radio_value(&mut s.scene_overlay, m, m.label()).changed();
        }
    });
    ui.horizontal_wrapped(|ui| {
        c |= ui.checkbox(&mut s.show_grid, "Grid").changed();
        c |= ui.checkbox(&mut s.show_icons, "Light/camera icons").changed();
        c |= ui.checkbox(&mut s.show_colliders, "Physics colliders").changed();
        c |= ui.checkbox(&mut s.show_bounds, "Bounding boxes").changed();
        c |= ui.checkbox(&mut s.frustum_cull, "Frustum culling").changed();
    });
    egui::Grid::new("camera_prefs").num_columns(2).show(ui, |ui| {
        ui.label("Field of view");
        c |= ui.add(egui::Slider::new(&mut s.camera_fov, 20.0..=120.0).suffix("°")).changed();
        ui.end_row();
        ui.label("Fly speed");
        c |= ui.add(egui::Slider::new(&mut s.camera_speed, 0.5..=200.0).logarithmic(true).suffix(" m/s")).changed();
        ui.end_row();
        ui.label("Look sensitivity");
        c |= ui.add(egui::Slider::new(&mut s.look_sensitivity, 0.1..=4.0)).changed();
        ui.end_row();
        ui.label("Invert Y");
        c |= ui.checkbox(&mut s.invert_y, "").changed();
        ui.end_row();
    });

    section(ui, "Gizmo snapping (hold Ctrl)");
    egui::Grid::new("snap_prefs").num_columns(2).show(ui, |ui| {
        ui.label("Move");
        c |= ui.add(egui::DragValue::new(&mut s.snap_translate).speed(0.05).range(0.001..=100.0).suffix(" m")).changed();
        ui.end_row();
        ui.label("Rotate");
        c |= ui.add(egui::DragValue::new(&mut s.snap_rotate_degrees).speed(1.0).range(1.0..=180.0).suffix("°")).changed();
        ui.end_row();
        ui.label("Scale");
        c |= ui.add(egui::DragValue::new(&mut s.snap_scale).speed(0.01).range(0.001..=10.0)).changed();
        ui.end_row();
    });

    section(ui, "Play mode");
    c |= ui.checkbox(&mut s.save_before_play, "Save scene before entering Play").changed();
    c |= ui.checkbox(&mut s.clear_console_on_play, "Clear console when entering Play").changed();

    section(ui, "Scripts");
    c |= ui.checkbox(&mut s.scripts_build_on_save, "Build on save").changed();
    c |= ui.checkbox(&mut s.scripts_auto_reload, "Reload the library when it changes").changed();
    ui.horizontal(|ui| {
        ui.label("Code editor");
        c |= ui
            .add(egui::TextEdit::singleline(&mut s.code_editor).hint_text("built-in (or: vscode, or a command)").desired_width(260.0))
            .on_hover_text("Empty = the built-in code editor.\n`vscode` = VS Code (or the default app).\nAnything else is a command; {file} and {line} are replaced, e.g.\nnotepad++ -n{line} \"{file}\"\nrustrover --line {line} \"{file}\"")
            .changed();
    });
    c
}

fn project_tab(ui: &mut egui::Ui, project: &mut Project, scenes: &[String]) -> bool {
    let p = &mut project.settings;
    let mut c = false;
    section(ui, "General");
    egui::Grid::new("proj_general").num_columns(2).show(ui, |ui| {
        ui.label("Name");
        c |= ui.text_edit_singleline(&mut p.name).changed();
        ui.end_row();
        ui.label("Startup scene");
        egui::ComboBox::from_id_salt("startup_scene").selected_text(if p.startup_scene.is_empty() { "—" } else { &p.startup_scene }).show_ui(ui, |ui| {
            for s in scenes {
                c |= ui.selectable_value(&mut p.startup_scene, s.clone(), s).changed();
            }
        });
        ui.end_row();
    });

    section(ui, "Game window & frame rate");
    c |= fps_field(ui, "Max FPS", &mut p.max_fps, false);
    c |= ui.checkbox(&mut p.vsync, "VSync").changed();
    ui.horizontal(|ui| {
        ui.label("Window size");
        c |= ui.add(egui::DragValue::new(&mut p.window_size[0]).range(320..=7680).suffix(" px")).changed();
        ui.label("×");
        c |= ui.add(egui::DragValue::new(&mut p.window_size[1]).range(240..=4320).suffix(" px")).changed();
        c |= ui.checkbox(&mut p.fullscreen, "Fullscreen").changed();
    });
    ui.weak("Used by the standalone player (dumb-player). In the editor, the editor's limits apply.");

    section(ui, "Performance overlay (game)");
    ui.horizontal(|ui| {
        for m in [dumb_runtime::perf::PerfOverlay::Off, dumb_runtime::perf::PerfOverlay::Minimal, dumb_runtime::perf::PerfOverlay::Full] {
            c |= ui.radio_value(&mut p.perf_overlay, m, m.label()).changed();
        }
    });
    ui.weak("Shown when the game starts; F3 cycles it while playing (also in exported games).");

    section(ui, "Physics");
    egui::Grid::new("proj_physics").num_columns(2).show(ui, |ui| {
        ui.label("Gravity");
        ui.horizontal(|ui| {
            for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                ui.label(*axis);
                c |= ui.add(egui::DragValue::new(&mut p.gravity[i]).speed(0.1).suffix(" m/s²")).changed();
            }
        });
        ui.end_row();
        ui.label("Simulation rate");
        c |= ui.add(egui::Slider::new(&mut p.physics_hz, 20..=480).suffix(" Hz")).changed();
        ui.end_row();
        ui.label("Max steps per frame");
        c |= ui.add(egui::Slider::new(&mut p.physics_max_steps, 1..=16)).changed();
        ui.end_row();
    });
    ui.weak("Changes apply the next time Play starts.");

    section(ui, "AI (NPC scheduling)");
    egui::Grid::new("proj_ai").num_columns(2).show(ui, |ui| {
        ui.label("Max thinks per frame");
        let mut unlimited = p.ai.max_thinks_per_frame == 0;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!unlimited, |ui| {
                c |= ui.add(egui::Slider::new(&mut p.ai.max_thinks_per_frame, 1..=10_000).logarithmic(true)).changed();
            });
            if ui.checkbox(&mut unlimited, "Unlimited").changed() {
                p.ai.max_thinks_per_frame = if unlimited { 0 } else { 500 };
                c = true;
            }
        });
        ui.end_row();
        for (i, band) in ["Near band", "Mid band", "Far band"].iter().enumerate() {
            ui.label(*band);
            ui.horizontal(|ui| {
                ui.label("up to");
                c |= ui.add(egui::DragValue::new(&mut p.ai.lod_distances[i]).speed(1.0).range(1.0..=100_000.0).suffix(" m")).changed();
                ui.label("think rate ÷");
                c |= ui.add(egui::DragValue::new(&mut p.ai.lod_divisors[i]).speed(0.1).range(1.0..=256.0)).changed();
            });
            ui.end_row();
        }
    });
    ui.weak("Agents beyond the far band sleep. Over-budget agents wait; the most overdue go first.");

    section(ui, "Scripts");
    ui.label(format!("Crate `{}` in `{}`", p.scripts_package, p.scripts_workspace));
    c
}

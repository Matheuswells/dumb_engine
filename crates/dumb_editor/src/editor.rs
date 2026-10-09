//! Editor state and top-level UI.

use crate::asset_browser::{asset_inspector, browser_ui, BrowserState, DragAsset};
use crate::camera::{project, EditorCamera};
use crate::console::{console_ui, ConsoleState, LogBuffer};
use crate::gizmo::{Gizmo, GizmoMode, GizmoSpace, GizmoView};
use crate::hierarchy::{create_menu, entity_label, hierarchy_ui, HierarchyState};
use crate::inspector::{add_component_menu, entity_inspector, ComponentAction, InspectCtx};
use crate::material_viewer::MaterialViewer;
use crate::model_viewer::ModelViewer;
use crate::prefs::{EditorSettings, PreferencesWindow, Theme};
use crate::project_manager::{pick_project_folder, recent_projects, NewProjectForm};
use dumb_asset::{AssetDatabase, AssetEvent, AssetKind};
use dumb_core::{builtin, Aabb, AssetId, Color, Entity, Input, Mat4, Time, Vec2, Vec3};
use dumb_ecs::{Animator, Camera, Light, LightKind, MeshRenderer, Name, Parent, PrefabInstance, SceneData, Transform, World};
use dumb_render::{camera_view, extract_world, find_primary_camera, push_model, ExtractOptions, RenderTargetId, RenderView, Renderer};
use dumb_runtime::{feed_input, simulate, Project};
use dumb_script::{ScriptHost, ScriptStatus};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub enum Action {
    Select(Entity, bool),
    ClearSelection,
    SelectAsset(AssetId),
    OpenAsset(AssetId),
    OpenAnimationViewer(AssetId),
    OpenMaterial(AssetId),
    ExtractMaterial(AssetId, String),
    DeleteSelection,
    DuplicateSelection,
    Reparent(Entity, Option<Entity>),
    Rename(Entity, String),
    CreateEmpty(Option<Entity>),
    CreateLight(LightKind, Option<Entity>),
    CreateCamera(Option<Entity>),
    SpawnAsset { asset: AssetId, at: Option<Vec3>, parent: Option<Entity> },
    AssignMaterial(Entity, AssetId),
    CreatePrefab(Entity),
    ApplyPrefab(Entity),
    Focus(Entity),
    /// Open the New Script dialog.
    NewScript,
}

/// Project-level operations the app performs between frames.
#[derive(Clone, Debug)]
pub enum ProjectRequest {
    Open(PathBuf),
    Close,
    Quit,
}

#[derive(Default)]
pub struct Selection {
    pub entities: Vec<Entity>,
    pub asset: Option<AssetId>,
    /// Inspector shows the asset instead of the entity.
    pub show_asset: bool,
}

impl Selection {
    pub fn primary(&self) -> Option<Entity> {
        self.entities.last().copied()
    }
}

pub struct SceneDoc {
    pub name: String,
    pub asset: Option<AssetId>,
    pub world: World,
    pub dirty: bool,
    pub camera: EditorCamera,
    pub target: RenderTargetId,
    pub selection: Selection,
    pub hierarchy: HierarchyState,
    undo: Vec<SceneData>,
    redo: Vec<SceneData>,
    stable: Option<SceneData>,
    changed: bool,
    editing: bool,
    pub rect: egui::Rect,
}

impl SceneDoc {
    fn new(name: &str, world: World, asset: Option<AssetId>, renderer: &mut Renderer) -> Self {
        let stable = Some(SceneData::capture(&world));
        SceneDoc {
            name: name.into(),
            asset,
            world,
            dirty: false,
            camera: EditorCamera::default(),
            target: renderer.create_target(1280, 720),
            selection: Selection::default(),
            hierarchy: HierarchyState::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            stable,
            changed: false,
            editing: false,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0)),
        }
    }

    pub fn mark_changed(&mut self) {
        self.changed = true;
    }

    fn undo(&mut self) {
        let Some(prev) = self.undo.pop() else { return };
        if let Some(cur) = self.stable.take() {
            self.redo.push(cur);
        }
        prev.restore_exact(&mut self.world);
        self.stable = Some(prev);
        self.selection.entities.retain(|e| self.world.is_alive(*e));
        self.dirty = true;
    }

    fn redo(&mut self) {
        let Some(next) = self.redo.pop() else { return };
        if let Some(cur) = self.stable.take() {
            self.undo.push(cur);
        }
        next.restore_exact(&mut self.world);
        self.stable = Some(next);
        self.selection.entities.retain(|e| self.world.is_alive(*e));
        self.dirty = true;
    }

    /// Commit an edit sequence once a frame passes without changes.
    fn end_frame(&mut self, record: bool, pointer_down: bool) {
        if self.changed || (self.editing && pointer_down) {
            self.dirty = true;
            self.editing = true;
        } else if self.editing {
            self.editing = false;
            if record {
                if let Some(prev) = self.stable.take() {
                    self.undo.push(prev);
                    if self.undo.len() > 64 {
                        self.undo.remove(0);
                    }
                }
                self.redo.clear();
                self.stable = Some(SceneData::capture(&self.world));
            }
        }
        self.changed = false;
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum PlayMode {
    Edit,
    Playing,
    Paused,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum CenterTab {
    Scene,
    Game,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum BottomTab {
    Assets,
    Console,
    Scripts,
}

#[derive(Default)]
struct FrameStats {
    fps: f32,
    frame_ms: f32,
    sim_ms: f32,
    extract_ms: f32,
    culled: u32,
    ai: dumb_ecs::AiStats,
    stream: dumb_runtime::streaming::StreamStats,
}

pub struct Editor {
    pub project: Project,
    pub db: AssetDatabase,
    pub scripts: ScriptHost,
    docs: Vec<SceneDoc>,
    active: usize,
    play: PlayMode,
    play_snapshot: Option<(usize, SceneData)>,
    time: Time,
    step_once: bool,
    input: Input,
    game_focused: bool,
    gizmo: Gizmo,
    browser: BrowserState,
    console: ConsoleState,
    log: LogBuffer,
    model_viewers: Vec<ModelViewer>,
    material_viewers: Vec<MaterialViewer>,
    pub settings: EditorSettings,
    /// Last saved/applied copy, to detect changes from any menu.
    applied_settings: Option<EditorSettings>,
    style_dirty: bool,
    prefs_window: PreferencesWindow,
    build_window: crate::build_window::BuildWindow,
    monitor: dumb_runtime::perf::SystemMonitor,
    profiler_window: crate::profiler_window::ProfilerWindow,
    /// Performance overlay on the game view (starts from the project setting; F3 cycles).
    game_overlay: dumb_runtime::perf::PerfOverlay,
    /// Game HUD (scripts' commands + HUD components) and its renderer.
    pub hud: dumb_ecs::Hud,
    hud_view: dumb_runtime::hud_view::HudView,
    /// Game camera of the last frame (view-projection, position) for world-space HUD labels.
    game_cam: (Mat4, Vec3),
    pub web_browser: crate::web_browser::WebBrowser,
    /// Web views wanted this frame (HUD panels + browser), applied by the main loop.
    pub web: crate::web_browser::WebFrame,
    code_editor: crate::code_editor::CodeEditor,
    center_tab: CenterTab,
    bottom_tab: BottomTab,
    game_target: RenderTargetId,
    game_rect: egui::Rect,
    actions: Vec<Action>,
    new_project: NewProjectForm,
    /// Project-level request for the app (open / close / quit), ready to execute.
    pub request: Option<ProjectRequest>,
    /// Request waiting on the unsaved-changes prompt.
    pending: Option<ProjectRequest>,
    new_script: crate::script_tools::NewScriptDialog,
    scripts_ui: crate::script_tools::ScriptsPanelState,
    add_comp_search: String,
    stats: FrameStats,
    pub window_title: Option<String>,
    next_tex_base: u64,
    status: Option<(String, Instant)>,
    fly_dt: f32,
    pointer_down: bool,
    /// Physics simulation, alive only while playing.
    physics: Option<dumb_physics::Physics>,
    /// World streaming while playing (reset on stop).
    streamer: Option<dumb_runtime::streaming::Streamer>,
}

pub fn apply_style(ctx: &egui::Context, theme: Theme) {
    if theme == Theme::Light {
        ctx.set_visuals(egui::Visuals::light());
        ctx.global_style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(6.0, 4.0);
            s.spacing.button_padding = egui::vec2(6.0, 3.0);
            s.visuals.window_corner_radius = egui::CornerRadius::same(6);
        });
        return;
    }
    ctx.set_visuals(egui::Visuals::dark());
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(6.0, 3.0);
        s.visuals.window_corner_radius = egui::CornerRadius::same(6);
        s.visuals.panel_fill = egui::Color32::from_rgb(30, 31, 34);
        s.visuals.window_fill = egui::Color32::from_rgb(34, 35, 39);
        s.visuals.extreme_bg_color = egui::Color32::from_rgb(20, 21, 24);
        s.visuals.selection.bg_fill = egui::Color32::from_rgb(45, 95, 170);
    });
}

impl Editor {
    pub fn new(project_dir: &Path, renderer: &mut Renderer, log: LogBuffer) -> Self {
        if !project_dir.join("Assets").exists() {
            let name = project_dir.file_name().map_or("Project".into(), |n| n.to_string_lossy().into_owned());
            if let Err(e) = crate::project_manager::create_project(project_dir, &name) {
                log::error!("could not create project: {e}");
            }
        }
        crate::project_manager::remember(project_dir);
        let project = Project::open(project_dir);
        let db = AssetDatabase::open(&project.root).expect("open asset database");
        let scripts = project.script_host();
        let game_target = renderer.create_target(1280, 720);
        let mut ed = Editor {
            project,
            db,
            scripts,
            docs: Vec::new(),
            active: 0,
            play: PlayMode::Edit,
            play_snapshot: None,
            time: Time::new(),
            step_once: false,
            input: Input::default(),
            game_focused: false,
            gizmo: Gizmo::default(),
            browser: BrowserState::default(),
            console: ConsoleState::default(),
            log,
            model_viewers: Vec::new(),
            material_viewers: Vec::new(),
            settings: EditorSettings::load(),
            applied_settings: None,
            style_dirty: true,
            prefs_window: PreferencesWindow::default(),
            build_window: Default::default(),
            monitor: Default::default(),
            profiler_window: Default::default(),
            game_overlay: Default::default(),
            hud: Default::default(),
            hud_view: Default::default(),
            game_cam: (Mat4::IDENTITY, Vec3::ZERO),
            web_browser: Default::default(),
            web: Default::default(),
            code_editor: Default::default(),
            center_tab: CenterTab::Scene,
            bottom_tab: BottomTab::Assets,
            game_target,
            game_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0)),
            actions: Vec::new(),
            new_project: NewProjectForm::default(),
            request: None,
            pending: None,
            new_script: Default::default(),
            scripts_ui: Default::default(),
            add_comp_search: String::new(),
            stats: FrameStats::default(),
            window_title: None,
            next_tex_base: 2_000_000,
            status: None,
            fly_dt: 0.016,
            pointer_down: false,
            physics: None,
            streamer: None,
        };

        // Scripts first, so scene components resolve on load.
        let mut no_worlds: [&mut World; 0] = [];
        if let Err(e) = ed.scripts.load(&mut no_worlds) {
            log::warn!("scripts: {e} — use Scripts ▸ Build");
        }

        let startup = ed.project.settings.startup_scene.clone();
        let opened = (!startup.is_empty()).then(|| ed.db.id_for_path(&startup)).flatten();
        match opened {
            Some(id) => ed.open_scene(id, renderer),
            None => {
                let world = ed.new_world();
                let mut doc = SceneDoc::new("Untitled", world, None, renderer);
                populate_default_scene(&mut doc.world);
                doc.stable = Some(SceneData::capture(&doc.world));
                ed.docs.push(doc);
            }
        }
        ed.open_dev_windows(renderer);
        ed.update_title();
        ed
    }

    /// A world with script components registered.
    fn new_world(&mut self) -> World {
        let mut w = World::new();
        for c in self.scripts.component_descriptors() {
            w.register(c);
        }
        w
    }

    fn doc(&mut self) -> &mut SceneDoc {
        &mut self.docs[self.active]
    }

    fn set_status(&mut self, s: impl Into<String>) {
        let s = s.into();
        log::info!("{s}");
        self.status = Some((s, Instant::now()));
    }

    fn update_title(&mut self) {
        let d = &self.docs[self.active];
        self.window_title = Some(format!(
            "{} — {}{} — Dumb Engine",
            self.project.settings.name,
            d.name,
            if d.dirty { "*" } else { "" }
        ));
    }

    // ------------------------------------------------------------------ events

    pub fn on_window_event(&mut self, event: &winit::event::WindowEvent, _consumed: bool) {
        if self.play == PlayMode::Playing && self.game_focused {
            feed_input(&mut self.input, event);
        }
    }

    pub fn import_dropped(&mut self, path: &Path) {
        let folder = self.browser.folder.clone();
        match self.db.import_external(path, &folder) {
            Ok(id) => self.set_status(format!("Imported {}", self.db.display_name(id))),
            Err(e) => self.set_status(format!("Import failed: {e}")),
        }
    }

    /// Ask to open/close the project or quit. Prompts first when scenes have unsaved changes.
    pub fn request(&mut self, r: ProjectRequest) {
        if let ProjectRequest::Open(dir) = &r {
            let same = |p: &Path| dumb_runtime::strip_unc(&p.canonicalize().unwrap_or(p.to_path_buf()));
            if same(dir) == same(&self.project.root) {
                self.set_status("That project is already open");
                return;
            }
        }
        if self.docs.iter().any(|d| d.dirty) && self.play == PlayMode::Edit {
            self.pending = Some(r);
        } else {
            self.request = Some(r);
        }
    }

    /// Free every GPU resource the editor created (before dropping it).
    pub fn release(&mut self, renderer: &mut Renderer) {
        for d in &self.docs {
            renderer.destroy_target(d.target);
        }
        renderer.destroy_target(self.game_target);
        for v in &self.model_viewers {
            v.release(renderer);
        }
        for v in &self.material_viewers {
            renderer.destroy_target(v.preview.target);
        }
        for t in self.browser.thumbs.thumbs.values() {
            match t.target {
                Some(target) => renderer.destroy_target(target),
                None => renderer.free_egui_image(t.texture),
            }
        }
    }

    pub fn shutdown(&mut self) {
        if self.play != PlayMode::Edit {
            self.stop();
        }
        let mut worlds: Vec<&mut World> = self.docs.iter_mut().map(|d| &mut d.world).collect();
        self.scripts.unload(&mut worlds);
    }

    // ------------------------------------------------------------------ frame

    pub fn pre_frame(&mut self, dt: f32, renderer: &mut Renderer) {
        self.monitor.update(renderer);
        let st = renderer.stats;
        dumb_core::profiler::counter("draw calls", st.draw_calls as f64);
        dumb_core::profiler::counter("objects drawn", st.instances as f64);
        dumb_core::profiler::counter("triangles", st.triangles as f64);
        dumb_core::profiler::counter("shadow draws", st.shadow_draws as f64);
        dumb_core::profiler::counter("deferred uploads", st.deferred_uploads as f64);
        dumb_core::profiler::counter("entities", self.docs[self.active].world.entity_count() as f64);
        self.fly_dt = dt;
        self.gizmo.translate_snap = self.settings.snap_translate;
        self.gizmo.rotate_snap_deg = self.settings.snap_rotate_degrees;
        self.gizmo.scale_snap = self.settings.snap_scale;
        for d in &mut self.docs {
            d.camera.sensitivity = self.settings.look_sensitivity;
            d.camera.invert_y = self.settings.invert_y;
        }
        let frame_ms = dt * 1000.0;
        self.stats.frame_ms = self.stats.frame_ms * 0.9 + frame_ms * 0.1;
        self.stats.fps = 1000.0 / self.stats.frame_ms.max(0.01);

        self.db.update();
        for ev in self.db.drain_events() {
            match ev {
                AssetEvent::Failed(id, e) => self.set_status(format!("Import failed for {}: {}", self.db.display_name(id), e.lines().next().unwrap_or(""))),
                AssetEvent::Reimported(id) => self.set_status(format!("Reimported {}", self.db.display_name(id))),
                _ => {}
            }
        }

        let reloaded = {
            let mut worlds: Vec<&mut World> = self.docs.iter_mut().map(|d| &mut d.world).collect();
            self.scripts.update(&mut worlds)
        };
        if reloaded {
            self.set_status("Scripts reloaded — component state preserved");
        }

        let t0 = Instant::now();
        if self.play == PlayMode::Playing || self.step_once {
            self.time.advance(dt);
            let Editor { docs, active, scripts, db, time, input, physics, streamer, project, stats, hud, .. } = self;
            let sim = simulate(&mut docs[*active].world, scripts, db, time, input, physics.as_mut(), streamer.as_mut(), &project.settings, hud);
            stats.ai = sim.ai;
            stats.stream = sim.stream;
            self.step_once = false;
        } else {
            self.doc().world.update_transforms();
        }
        self.stats.sim_ms = t0.elapsed().as_secs_f32() * 1000.0;

        for v in &mut self.model_viewers {
            let m = self.db.model_loaded(v.id);
            v.tick(dt, m.as_deref());
        }
        if renderer.vsync != self.settings.vsync {
            renderer.set_vsync(self.settings.vsync);
        }
    }

    pub fn post_frame(&mut self) {
        let record = self.play == PlayMode::Edit;
        let was_dirty: Vec<bool> = self.docs.iter().map(|d| d.dirty).collect();
        // A drag (gizmo, slider, drag-value) is one undo step: commit only after release.
        let pointer_down = self.pointer_down || self.gizmo.is_dragging();
        for d in &mut self.docs {
            d.end_frame(record, pointer_down);
        }
        // Toggles flipped in the Scripts panel/menu feed back into the saved preferences.
        if let Some(a) = &self.applied_settings {
            if self.scripts.auto_build != a.scripts_build_on_save {
                self.settings.scripts_build_on_save = self.scripts.auto_build;
            }
            if self.scripts.auto_reload != a.scripts_auto_reload {
                self.settings.scripts_auto_reload = self.scripts.auto_reload;
            }
        }
        // Settings can change from the Preferences window or the View menu: save and apply.
        if self.applied_settings.as_ref() != Some(&self.settings) {
            if self.applied_settings.is_some() {
                self.settings.save();
            }
            self.apply_settings();
            self.applied_settings = Some(self.settings.clone());
        }
        if self.docs.iter().map(|d| d.dirty).collect::<Vec<_>>() != was_dirty {
            self.update_title();
        }
        self.input.end_frame();
    }

    /// Dev/testing hook: `DUMB_OPEN=prefs,console,scripts,material:Materials/Gold.mat,model:Characters/Player/Robot.blend`
    /// opens those windows and tabs at startup (for screenshots and UI checks without input).
    fn open_dev_windows(&mut self, renderer: &mut Renderer) {
        let Ok(list) = std::env::var("DUMB_OPEN") else { return };
        for item in list.split(',').map(str::trim) {
            match item.split_once(':') {
                Some(("file", p)) => self.code_editor.open_file(Path::new(p), None),
                Some(("web", u)) => self.web_browser.open_url(u),
                Some(("material", p)) => {
                    if let Some(id) = self.db.id_for_path(p) {
                        self.material_viewers.push(MaterialViewer::new(id, renderer));
                    }
                }
                Some(("model", p)) => {
                    // `model:path@tab` also selects a model viewer tab (import, materials, lod, nodes).
                    let (p, tab) = p.split_once('@').unwrap_or((p, ""));
                    if let Some(id) = self.db.id_for_path(p) {
                        self.open_asset(id, renderer);
                        if let Some(v) = self.model_viewers.iter_mut().find(|v| v.id == id) {
                            v.select_tab(tab);
                        }
                    }
                }
                _ => match item {
                    "prefs" => self.prefs_window.open = true,
                    "build" => self.build_window.open = true,
                    "profiler" => self.profiler_window.open = true,
                    "browser" => self.web_browser.open = true,
                    "code" => {
                        let dir = self.scripts.crate_dir().join("src");
                        let first = std::fs::read_dir(&dir).ok().and_then(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "rs") && !p.ends_with("lib.rs")).min());
                        match first {
                            Some(p) => self.code_editor.open_file(&p, None),
                            None => self.code_editor.open = true,
                        }
                    }
                    "console" => self.bottom_tab = BottomTab::Console,
                    "scripts" => self.bottom_tab = BottomTab::Scripts,
                    "play" => self.play(),
                    _ => {}
                },
            }
        }
    }

    fn apply_settings(&mut self) {
        let s = &self.settings;
        self.scripts.auto_build = s.scripts_build_on_save;
        self.scripts.auto_reload = s.scripts_auto_reload;
        for d in &mut self.docs {
            d.camera.fov_degrees = s.camera_fov;
            d.camera.speed = s.camera_speed;
        }
        crate::script_tools::set_code_editor(&s.code_editor);
        self.style_dirty = true;
    }

    // ------------------------------------------------------------------ play mode

    fn play(&mut self) {
        match self.play {
            PlayMode::Edit => {
                if self.settings.save_before_play && self.docs[self.active].dirty {
                    self.save_active(false);
                }
                let snap = SceneData::capture(&self.doc().world);
                self.play_snapshot = Some((self.active, snap));
                self.time = Time::new();
                self.input.clear();
                self.play = PlayMode::Playing;
                let mut physics = dumb_physics::Physics::new(Vec3::from(self.project.settings.gravity));
                self.project.configure_physics(&mut physics);
                self.physics = Some(physics);
                self.streamer = Some(Default::default());
                if self.settings.clear_console_on_play {
                    if let Ok(mut b) = self.log.0.lock() {
                        b.clear();
                    }
                }
                self.center_tab = CenterTab::Game;
                self.game_focused = true;
                self.game_overlay = self.project.settings.perf_overlay;
                self.scripts.errors.clear();
                self.set_status("▶ Playing");
            }
            PlayMode::Paused => self.play = PlayMode::Playing,
            PlayMode::Playing => self.play = PlayMode::Paused,
        }
    }

    fn stop(&mut self) {
        if let Some((idx, snap)) = self.play_snapshot.take() {
            if let Some(d) = self.docs.get_mut(idx) {
                snap.restore_exact(&mut d.world);
                d.selection.entities.retain(|e| d.world.is_alive(*e));
                d.changed = false;
                d.editing = false;
            }
        }
        self.play = PlayMode::Edit;
        self.physics = None;
        self.streamer = None;
        self.game_focused = false;
        self.center_tab = CenterTab::Scene;
        self.input.clear();
        self.set_status("■ Stopped — scene restored");
    }

    // ------------------------------------------------------------------ scenes

    pub fn open_scene(&mut self, id: AssetId, renderer: &mut Renderer) {
        if let Some(i) = self.docs.iter().position(|d| d.asset == Some(id)) {
            self.active = i;
            return;
        }
        if self.play != PlayMode::Edit {
            self.stop();
        }
        let Some(text) = self.db.read_asset_text(id) else { return };
        let mut world = self.new_world();
        match SceneData::from_ron(&text) {
            Ok(s) => s.restore_exact(&mut world),
            Err(e) => {
                self.set_status(format!("Scene parse error: {e}"));
                return;
            }
        }
        let name = self.db.entry(id).map_or("Scene".into(), |e| e.stem().to_string());
        let doc = SceneDoc::new(&name, world, Some(id), renderer);
        // Replace a pristine untitled scene instead of piling up tabs.
        if self.docs.len() == 1 && self.docs[0].asset.is_none() && !self.docs[0].dirty {
            let old = std::mem::replace(&mut self.docs[0], doc);
            renderer.destroy_target(old.target);
            self.active = 0;
        } else {
            self.docs.push(doc);
            self.active = self.docs.len() - 1;
        }
        self.update_title();
    }

    fn new_scene(&mut self, renderer: &mut Renderer) {
        let mut world = self.new_world();
        populate_default_scene(&mut world);
        let mut doc = SceneDoc::new("Untitled", world, None, renderer);
        doc.stable = Some(SceneData::capture(&doc.world));
        self.docs.push(doc);
        self.active = self.docs.len() - 1;
        self.update_title();
    }

    /// Save every modified scene (new, never-saved scenes get a file under Scenes/).
    fn save_all_scenes(&mut self) {
        let active = self.active;
        for i in 0..self.docs.len() {
            if self.docs[i].dirty {
                self.active = i;
                self.save_active(false);
            }
        }
        self.active = active;
    }

    fn save_active(&mut self, save_as: bool) {
        if self.play != PlayMode::Edit {
            self.set_status("Stop play mode before saving");
            return;
        }
        let i = self.active;
        let data = SceneData::capture(&self.docs[i].world);
        let text = match data.to_ron() {
            Ok(t) => t,
            Err(e) => {
                self.set_status(format!("Serialize failed: {e}"));
                return;
            }
        };
        let rel = match (self.docs[i].asset.and_then(|a| self.db.entry(a)).map(|e| e.path.clone()), save_as) {
            (Some(p), false) => p,
            _ => {
                let mut n = 0;
                loop {
                    let name = if n == 0 { self.docs[i].name.clone() } else { format!("{} {n}", self.docs[i].name) };
                    let rel = format!("Scenes/{name}.scene");
                    if !self.db.assets_root.join(&rel).exists() {
                        break rel;
                    }
                    n += 1;
                }
            }
        };
        match self.db.write_asset(&rel, &text) {
            Ok(id) => {
                let d = &mut self.docs[i];
                d.asset = Some(id);
                d.name = rel.rsplit('/').next().unwrap_or(&rel).trim_end_matches(".scene").to_string();
                d.dirty = false;
                if self.project.settings.startup_scene.is_empty() {
                    self.project.settings.startup_scene = rel.clone();
                    let _ = self.project.save();
                }
                self.set_status(format!("Saved {rel}"));
                self.update_title();
            }
            Err(e) => self.set_status(format!("Save failed: {e}")),
        }
    }

    // ------------------------------------------------------------------ actions

    fn spawn_asset(&mut self, asset: AssetId, at: Option<Vec3>, parent: Option<Entity>, renderer: &mut Renderer) -> Option<Entity> {
        let kind = if builtin::is_builtin(asset) { Some(AssetKind::Model) } else { self.db.entry(asset).map(|e| e.kind) };
        let pos = at.unwrap_or_else(|| {
            let c = &self.docs[self.active].camera;
            if parent.is_some() {
                Vec3::ZERO
            } else {
                c.pivot
            }
        });
        let e = match kind? {
            AssetKind::Model => {
                let name = self.db.display_name(asset).replace(" (built-in)", "");
                let has_anim = self.db.model(asset).is_some_and(|m| !m.animations.is_empty());
                let w = &mut self.docs[self.active].world;
                let e = w.spawn_named(&name);
                w.get_mut::<Transform>(e).unwrap().translation = pos;
                w.insert(e, MeshRenderer { model: asset, ..Default::default() });
                if has_anim || self.db.entry(asset).is_some_and(|e| e.path.ends_with(".blend")) {
                    w.insert(e, Animator::default());
                }
                e
            }
            AssetKind::Prefab => {
                let text = self.db.read_asset_text(asset)?;
                let data = SceneData::from_ron(&text).ok()?;
                let w = &mut self.docs[self.active].world;
                let e = data.instantiate_prefab(w, asset)?;
                if let Some(t) = w.get_mut::<Transform>(e) {
                    t.translation = pos;
                }
                e
            }
            AssetKind::Scene => {
                self.open_scene(asset, renderer);
                return None;
            }
            AssetKind::Material => return None,
            _ => return None,
        };
        let d = self.doc();
        if let Some(p) = parent {
            d.world.insert(e, Parent { entity: p });
            d.hierarchy.expanded.insert(p);
        }
        d.selection.entities = vec![e];
        d.selection.show_asset = false;
        d.mark_changed();
        Some(e)
    }

    fn entity_bounds(&self, e: Entity) -> Aabb {
        let w = &self.docs[self.active].world;
        let m = w.global_matrix(e);
        if let Some(mr) = w.get::<MeshRenderer>(e) {
            if let Some(model) = self.db.model_loaded(mr.model) {
                return model.aabb().transformed(&m);
            }
        }
        let p = m.w_axis.truncate();
        Aabb { min: p - Vec3::splat(0.5), max: p + Vec3::splat(0.5) }
    }

    fn apply_actions(&mut self, renderer: &mut Renderer) {
        let actions = std::mem::take(&mut self.actions);
        for a in actions {
            match a {
                Action::Select(e, additive) => {
                    let d = self.doc();
                    d.selection.show_asset = false;
                    if additive {
                        if let Some(i) = d.selection.entities.iter().position(|x| *x == e) {
                            d.selection.entities.remove(i);
                        } else {
                            d.selection.entities.push(e);
                        }
                    } else {
                        d.selection.entities = vec![e];
                    }
                }
                Action::ClearSelection => {
                    let d = self.doc();
                    d.selection.entities.clear();
                }
                Action::SelectAsset(id) => {
                    let d = self.doc();
                    d.selection.asset = Some(id);
                    d.selection.show_asset = true;
                }
                Action::OpenAsset(id) => self.open_asset(id, renderer),
                Action::OpenAnimationViewer(id) => {
                    if !self.model_viewers.iter().any(|v| v.id == id && v.animation_mode) {
                        self.next_tex_base += 10_000;
                        self.model_viewers.push(ModelViewer::new(id, renderer, true, self.next_tex_base));
                    }
                }
                Action::OpenMaterial(id) => {
                    if !self.material_viewers.iter().any(|v| v.id == id) {
                        self.material_viewers.push(MaterialViewer::new(id, renderer));
                    }
                }
                Action::ExtractMaterial(id, name) => {
                    if let Some(m) = self.db.material(id) {
                        let name = name.replace(['/', '\\', ':'], "_");
                        let mut rel = format!("Materials/{name}.mat");
                        let mut n = 1;
                        while self.db.assets_root.join(&rel).exists() {
                            rel = format!("Materials/{name} {n}.mat");
                            n += 1;
                        }
                        match self.db.write_asset(&rel, &m.to_ron()) {
                            Ok(new_id) => {
                                self.set_status(format!("Extracted {rel}"));
                                self.material_viewers.push(MaterialViewer::new(new_id, renderer));
                            }
                            Err(e) => self.set_status(format!("Extract failed: {e}")),
                        }
                    }
                }
                Action::DeleteSelection => {
                    let d = self.doc();
                    for e in std::mem::take(&mut d.selection.entities) {
                        d.world.despawn_recursive(e);
                    }
                    d.mark_changed();
                }
                Action::DuplicateSelection => {
                    let d = self.doc();
                    let mut new_sel = Vec::new();
                    let sel = d.selection.entities.clone();
                    let roots: Vec<Entity> = sel.iter().copied().filter(|e| !sel.iter().any(|s| d.world.is_ancestor(*s, *e))).collect();
                    for e in roots {
                        let data = SceneData::capture_subtree(&d.world, e);
                        let spawned = data.instantiate(&mut d.world);
                        if let Some(root) = spawned.first().copied() {
                            if let Some(p) = d.world.parent(e) {
                                d.world.insert(root, Parent { entity: p });
                            }
                            if let Some(n) = d.world.get_mut::<Name>(root) {
                                n.name = format!("{} (copy)", n.name);
                            }
                            new_sel.push(root);
                        }
                    }
                    d.selection.entities = new_sel;
                    d.mark_changed();
                }
                Action::Reparent(child, parent) => {
                    let d = self.doc();
                    d.world.set_parent(child, parent);
                    d.mark_changed();
                }
                Action::Rename(e, name) => {
                    let d = self.doc();
                    if let Some(n) = d.world.get_mut::<Name>(e) {
                        n.name = name;
                    } else {
                        d.world.insert(e, Name { name });
                    }
                    d.mark_changed();
                }
                Action::CreateEmpty(parent) => {
                    let pivot = self.doc().camera.pivot;
                    let d = self.doc();
                    let e = d.world.spawn_named("Empty");
                    match parent {
                        Some(p) => d.world.insert(e, Parent { entity: p }),
                        None => d.world.get_mut::<Transform>(e).unwrap().translation = pivot,
                    }
                    d.selection.entities = vec![e];
                    d.mark_changed();
                }
                Action::CreateLight(kind, parent) => {
                    let pivot = self.doc().camera.pivot;
                    let d = self.doc();
                    let name = if kind == LightKind::Directional { "Directional Light" } else { "Point Light" };
                    let e = d.world.spawn_named(name);
                    let t = d.world.get_mut::<Transform>(e).unwrap();
                    if kind == LightKind::Directional {
                        t.translation = Vec3::new(0.0, 5.0, 0.0);
                        t.set_euler_degrees(Vec3::new(-50.0, -30.0, 0.0));
                    } else {
                        t.translation = pivot + Vec3::Y;
                    }
                    d.world.insert(e, Light { kind, ..Default::default() });
                    if let Some(p) = parent {
                        d.world.insert(e, Parent { entity: p });
                    }
                    d.selection.entities = vec![e];
                    d.mark_changed();
                }
                Action::CreateCamera(parent) => {
                    let cam = self.doc().camera.clone();
                    let d = self.doc();
                    let e = d.world.spawn_named("Camera");
                    let t = d.world.get_mut::<Transform>(e).unwrap();
                    t.translation = cam.position();
                    t.rotation = cam.rotation();
                    let primary = d.world.count::<Camera>() == 0;
                    d.world.insert(e, Camera { primary, ..Default::default() });
                    if let Some(p) = parent {
                        d.world.insert(e, Parent { entity: p });
                    }
                    d.selection.entities = vec![e];
                    d.mark_changed();
                }
                Action::SpawnAsset { asset, at, parent } => {
                    if self.db.entry(asset).is_some_and(|e| e.kind == AssetKind::Material) {
                        if let Some(p) = parent {
                            self.actions.push(Action::AssignMaterial(p, asset));
                        }
                    } else {
                        self.spawn_asset(asset, at, parent, renderer);
                    }
                }
                Action::AssignMaterial(e, mat) => {
                    let d = self.doc();
                    if let Some(mr) = d.world.get_mut::<MeshRenderer>(e) {
                        mr.material = mat;
                        d.mark_changed();
                    }
                }
                Action::CreatePrefab(e) => {
                    let w = &self.docs[self.active].world;
                    let name = entity_label(w, e).replace(['/', '\\', ':'], "_");
                    let data = SceneData::capture_subtree(w, e);
                    let mut rel = format!("Prefabs/{name}.prefab");
                    let mut n = 1;
                    while self.db.assets_root.join(&rel).exists() {
                        rel = format!("Prefabs/{name} {n}.prefab");
                        n += 1;
                    }
                    match data.to_ron().map_err(|e| e.to_string()).and_then(|t| self.db.write_asset(&rel, &t)) {
                        Ok(id) => {
                            let d = self.doc();
                            d.world.insert(e, PrefabInstance { prefab: id });
                            d.mark_changed();
                            self.set_status(format!("Created prefab {rel}"));
                        }
                        Err(err) => self.set_status(format!("Prefab failed: {err}")),
                    }
                }
                Action::ApplyPrefab(root) => self.apply_prefab(root),
                Action::NewScript => self.new_script.show(),
                Action::Focus(e) => {
                    let b = self.entity_bounds(e);
                    let d = self.doc();
                    d.camera.focus(&b);
                    d.hierarchy.reveal_entity(&d.world, e);
                }
            }
        }
    }

    /// Write an instance back to its prefab and refresh every other instance in open scenes.
    fn apply_prefab(&mut self, root: Entity) {
        let Some(pi) = self.docs[self.active].world.get::<PrefabInstance>(root).cloned() else { return };
        let data = SceneData::capture_subtree(&self.docs[self.active].world, root);
        let Some(rel) = self.db.entry(pi.prefab).map(|e| e.path.clone()) else { return };
        let Ok(text) = data.to_ron() else { return };
        if let Err(e) = self.db.write_asset(&rel, &text) {
            self.set_status(format!("Apply failed: {e}"));
            return;
        }
        let mut updated = 0;
        for (di, d) in self.docs.iter_mut().enumerate() {
            let instances: Vec<Entity> = d
                .world
                .query_ref::<(Entity, &PrefabInstance)>()
                .filter(|(e, p)| p.prefab == pi.prefab && !(di == self.active && *e == root))
                .map(|(e, _)| e)
                .collect();
            for inst in instances {
                let parent = d.world.parent(inst);
                let t = d.world.get::<Transform>(inst).copied();
                d.world.despawn_recursive(inst);
                if let Some(n) = data.instantiate_prefab(&mut d.world, pi.prefab) {
                    if let Some(p) = parent {
                        d.world.insert(n, Parent { entity: p });
                    }
                    if let (Some(t), Some(nt)) = (t, d.world.get_mut::<Transform>(n)) {
                        *nt = t;
                    }
                }
                updated += 1;
                d.mark_changed();
            }
        }
        self.set_status(format!("Applied prefab {rel} ({updated} other instances updated)"));
    }

    fn open_asset(&mut self, id: AssetId, renderer: &mut Renderer) {
        let kind = if builtin::is_builtin(id) { Some(AssetKind::Model) } else { self.db.entry(id).map(|e| e.kind) };
        match kind {
            Some(AssetKind::Model) => {
                if let Some(v) = self.model_viewers.iter_mut().find(|v| v.id == id && !v.animation_mode) {
                    v.open = true;
                } else {
                    self.next_tex_base += 10_000;
                    self.model_viewers.push(ModelViewer::new(id, renderer, false, self.next_tex_base));
                }
            }
            Some(AssetKind::Material) => self.actions.push(Action::OpenMaterial(id)),
            Some(AssetKind::Scene) => self.open_scene(id, renderer),
            Some(AssetKind::Prefab) => {
                self.spawn_asset(id, None, None, renderer);
            }
            Some(AssetKind::Script) | Some(AssetKind::Shader) | Some(AssetKind::Other) | Some(AssetKind::Texture) | Some(AssetKind::Audio) => {
                if let Some(p) = self.db.abs_path(id) {
                    #[cfg(windows)]
                    let _ = std::process::Command::new("cmd").args(["/C", "start", "", &p.display().to_string()]).spawn();
                }
            }
            None => {
                if self.db.sub_asset(id).is_some_and(|s| s.kind == AssetKind::Material) {
                    self.actions.push(Action::OpenMaterial(id));
                }
            }
        }
    }

    // ------------------------------------------------------------------ UI

    pub fn ui(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer) {
        let ctx = ui.ctx().clone();
        if self.style_dirty {
            apply_style(&ctx, self.settings.theme);
            ctx.set_zoom_factor(self.settings.ui_scale.clamp(0.5, 3.0));
            self.style_dirty = false;
        }
        self.pointer_down = ctx.input(|i| i.pointer.any_down());
        self.shortcuts(&ctx, renderer);

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| self.menu_bar(ui, renderer));
        });
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, renderer));
        egui::Panel::bottom("status").exact_size(22.0).show(ui, |ui| self.status_bar(ui, renderer));

        egui::Panel::left("hierarchy").resizable(true).default_size(260.0).size_range(160.0..=520.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Hierarchy");
                ui.weak(format!("{} entities", self.docs[self.active].world.entity_count()));
            });
            let Editor { docs, active, actions, .. } = self;
            let d = &mut docs[*active];
            hierarchy_ui(ui, &d.world, &mut d.hierarchy, &d.selection, actions);
        });

        egui::Panel::right("inspector").resizable(true).default_size(340.0).size_range(240.0..=640.0).show(ui, |ui| {
            ui.strong("Inspector");
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.inspector(ui));
        });

        egui::Panel::bottom("bottom").resizable(true).default_size(260.0).size_range(120.0..=700.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.bottom_tab, BottomTab::Assets, "📁 Assets");
                ui.selectable_value(&mut self.bottom_tab, BottomTab::Console, "🖥 Console");
                let scripts_label = match &self.scripts.status {
                    ScriptStatus::Building => "📜 Scripts ⏳".to_string(),
                    ScriptStatus::BuildFailed | ScriptStatus::Error(_) => "📜 Scripts ⚠".to_string(),
                    _ if !self.scripts.errors.is_empty() => "📜 Scripts ⚠".to_string(),
                    _ => "📜 Scripts".to_string(),
                };
                ui.selectable_value(&mut self.bottom_tab, BottomTab::Scripts, scripts_label);
            });
            ui.separator();
            match self.bottom_tab {
                BottomTab::Assets => {
                    let sel = self.docs[self.active].selection.asset;
                    browser_ui(ui, &mut self.db, renderer, &mut self.browser, sel, &mut self.actions);
                }
                BottomTab::Console => console_ui(ui, &self.log, &mut self.console),
                BottomTab::Scripts => self.scripts_panel(ui),
            }
        });

        egui::CentralPanel::no_frame().show(ui, |ui| self.center(ui, renderer));

        // Floating windows.
        let Editor { model_viewers, material_viewers, db, actions, browser, .. } = self;
        for v in model_viewers.iter_mut() {
            v.ui(&ctx, db, renderer, actions, &mut browser.thumbs);
        }
        for v in material_viewers.iter_mut() {
            v.ui(&ctx, db, renderer, actions, &mut browser.thumbs);
        }
        for v in self.model_viewers.iter().filter(|v| !v.open) {
            v.release(renderer);
        }
        self.model_viewers.retain(|v| v.open);
        for v in self.material_viewers.iter().filter(|v| !v.open) {
            renderer.destroy_target(v.preview.target);
        }
        self.material_viewers.retain(|v| v.open);

        let crate_dir = self.scripts.crate_dir().to_path_buf();
        let mut needs_crate = false;
        if let Some(created) = self.new_script.ui(&ctx, &crate_dir, &mut needs_crate) {
            self.on_script_created(created, needs_crate);
        }
        let scenes: Vec<String> = self.db.search("t:scene").into_iter().filter_map(|id| self.db.entry(id).map(|e| e.path.clone())).collect();
        let changes = self.prefs_window.ui(&ctx, &mut self.settings, &mut self.project, &scenes);
        if changes.project {
            if let Err(e) = self.project.save() {
                self.set_status(format!("Could not save project.ron: {e}"));
            }
            self.update_title();
        }
        let script_dir = self.scripts.crate_dir().to_path_buf();
        let logs = if self.code_editor.open && self.code_editor.highlight_logs { self.log.latest_by_location() } else { Vec::new() };
        self.code_editor.ui(&ctx, &script_dir, &self.scripts.messages, logs);
        let dirty_scenes = self.docs.iter().any(|d| d.dirty);
        self.profiler_window.ui(&ctx, &self.monitor, renderer.device_name());
        let html: Vec<String> = self.db.entries().filter(|e| e.path.to_ascii_lowercase().ends_with(".html") || e.path.to_ascii_lowercase().ends_with(".htm")).map(|e| format!("Assets/{}", e.path)).collect();
        self.web_browser.ui(&ctx, &mut self.web, &html);
        let build = self.build_window.ui(&ctx, &mut self.project, &scenes, dirty_scenes);
        if build.save_scenes {
            self.save_all_scenes();
        }
        if build.settings_changed {
            if let Err(e) = self.project.save() {
                self.set_status(format!("Could not save project.ron: {e}"));
            }
            self.update_title();
        }
        if let Some(dir) = self.new_project.ui(&ctx) {
            self.request(ProjectRequest::Open(dir));
        }
        self.quit_dialog(&ctx);
        self.apply_actions(renderer);
    }

    /// Unsaved-changes prompt for a pending open/close/quit.
    fn quit_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.pending else { return };
        let verb = match pending {
            ProjectRequest::Quit => "quit",
            ProjectRequest::Close => "close the project",
            ProjectRequest::Open(_) => "open the other project",
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("quit_modal")).show(ctx, |ui| {
            ui.heading("Unsaved changes");
            ui.label(format!("Save changes before you {verb}?"));
            for d in self.docs.iter().filter(|d| d.dirty) {
                ui.label(format!("• {}", d.name));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Save all").clicked() {
                    choice = Some(true);
                }
                if ui.button("Discard").clicked() {
                    choice = Some(false);
                }
                if ui.button("Cancel").clicked() {
                    self.pending = None;
                }
            });
        });
        if let Some(save) = choice {
            if save {
                let active = self.active;
                for i in 0..self.docs.len() {
                    if self.docs[i].dirty {
                        self.active = i;
                        self.save_active(false);
                    }
                }
                self.active = active;
                if self.docs.iter().any(|d| d.dirty) {
                    // A save failed; keep the prompt so nothing is lost.
                    return;
                }
            }
            self.request = self.pending.take();
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context, renderer: &mut Renderer) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        use egui::{Key, Modifiers};
        // Match modifiers per key event (not end-of-frame state), so fast chords register.
        let chord = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        let rmb = ctx.input(|i| i.pointer.secondary_down());
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        if self.game_focused && self.play == PlayMode::Playing {
            if chord(Modifiers::NONE, Key::Escape) {
                self.game_focused = false;
            }
            if chord(Modifiers::COMMAND, Key::P) {
                self.play();
            }
            if chord(Modifiers::NONE, Key::F3) {
                self.game_overlay = self.game_overlay.next();
            }
            return;
        }
        if chord(Modifiers::NONE, Key::F3) {
            if self.center_tab == CenterTab::Game {
                self.game_overlay = self.game_overlay.next();
            } else {
                self.settings.scene_overlay = self.settings.scene_overlay.next();
            }
        }
        if chord(ctrl_shift, Key::P) {
            self.profiler_window.open = !self.profiler_window.open;
        }
        if chord(ctrl_shift, Key::S) {
            self.save_active(true);
        }
        if chord(Modifiers::COMMAND, Key::S) {
            self.save_active(false);
        }
        if chord(ctrl_shift, Key::Z) || chord(Modifiers::COMMAND, Key::Y) {
            self.doc().redo();
        }
        if chord(Modifiers::COMMAND, Key::Z) {
            self.doc().undo();
        }
        if chord(Modifiers::COMMAND, Key::D) {
            self.actions.push(Action::DuplicateSelection);
        }
        if chord(Modifiers::COMMAND, Key::N) {
            self.new_scene(renderer);
        }
        if chord(Modifiers::COMMAND, Key::B) {
            self.build_window.open = true;
        }
        if chord(Modifiers::COMMAND, Key::E) {
            self.code_editor.open = !self.code_editor.open;
        }
        if chord(Modifiers::COMMAND, Key::P) {
            self.play();
        }
        if chord(Modifiers::NONE, Key::Delete) {
            self.actions.push(Action::DeleteSelection);
        }
        let pressed = |k: Key| chord(Modifiers::NONE, k);
        if !rmb {
            if pressed(egui::Key::W) {
                self.gizmo.mode = GizmoMode::Translate;
            }
            if pressed(egui::Key::E) {
                self.gizmo.mode = GizmoMode::Rotate;
            }
            if pressed(egui::Key::R) {
                self.gizmo.mode = GizmoMode::Scale;
            }
            if pressed(egui::Key::F) {
                if let Some(e) = self.docs[self.active].selection.primary() {
                    self.actions.push(Action::Focus(e));
                }
            }
            if pressed(egui::Key::F2) {
                let d = self.doc();
                if let Some(e) = d.selection.primary() {
                    d.hierarchy.renaming = Some((e, entity_label(&d.world, e)));
                }
            }
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer) {
        ui.menu_button("File", |ui| {
            if ui.button("New Scene  Ctrl+N").clicked() {
                self.new_scene(renderer);
                ui.close();
            }
            if ui.button("Save Scene  Ctrl+S").clicked() {
                self.save_active(false);
                ui.close();
            }
            if ui.button("Save Scene As…  Ctrl+Shift+S").clicked() {
                self.save_active(true);
                ui.close();
            }
            if ui.button("Set as startup scene").clicked() {
                if let Some(p) = self.docs[self.active].asset.and_then(|a| self.db.entry(a)).map(|e| e.path.clone()) {
                    self.project.settings.startup_scene = p;
                    let _ = self.project.save();
                }
                ui.close();
            }
            ui.separator();
            if ui.button("➕ New Project…").clicked() {
                self.new_project.show();
                ui.close();
            }
            if ui.button("📂 Open Project…").clicked() {
                ui.close();
                match pick_project_folder() {
                    Ok(Some(dir)) => self.request(ProjectRequest::Open(dir)),
                    Ok(None) => {}
                    Err(e) => self.set_status(e),
                }
            }
            ui.menu_button("Recent Projects", |ui| {
                let current = dumb_runtime::strip_unc(&self.project.root.canonicalize().unwrap_or(self.project.root.clone()));
                let recent: Vec<PathBuf> = recent_projects().into_iter().filter(|p| *p != current).collect();
                if recent.is_empty() {
                    ui.weak("No other recent projects");
                }
                for p in recent {
                    if ui.button(p.display().to_string()).clicked() {
                        self.request(ProjectRequest::Open(p));
                        ui.close();
                    }
                }
            });
            if ui.button("✖ Close Project").clicked() {
                self.request(ProjectRequest::Close);
                ui.close();
            }
            if ui.button("📦 Build & Export…  Ctrl+B").clicked() {
                self.build_window.open = true;
                ui.close();
            }
            if ui.button("Show project in Explorer").clicked() {
                #[cfg(windows)]
                let _ = std::process::Command::new("explorer").arg(&self.project.root).spawn();
                ui.close();
            }
            ui.separator();
            if ui.button("Exit").clicked() {
                self.request(ProjectRequest::Quit);
                ui.close();
            }
        });
        ui.menu_button("Edit", |ui| {
            if ui.button("⚙ Preferences…").clicked() {
                self.prefs_window.open = true;
                ui.close();
            }
            ui.separator();
            let d = &self.docs[self.active];
            if ui.add_enabled(!d.undo.is_empty(), egui::Button::new(format!("Undo  Ctrl+Z ({})", d.undo.len()))).clicked() {
                self.doc().undo();
                ui.close();
            }
            let d = &self.docs[self.active];
            if ui.add_enabled(!d.redo.is_empty(), egui::Button::new("Redo  Ctrl+Y")).clicked() {
                self.doc().redo();
                ui.close();
            }
            ui.separator();
            if ui.button("Duplicate  Ctrl+D").clicked() {
                self.actions.push(Action::DuplicateSelection);
                ui.close();
            }
            if ui.button("Delete  Del").clicked() {
                self.actions.push(Action::DeleteSelection);
                ui.close();
            }
            if ui.button("Select none").clicked() {
                self.actions.push(Action::ClearSelection);
                ui.close();
            }
        });
        ui.menu_button("GameObject", |ui| create_menu(ui, None, &mut self.actions));
        ui.menu_button("Scripts", |ui| {
            if ui.button("➕ New Script…").clicked() {
                self.new_script.show();
                ui.close();
            }
            if ui.button("📝 Code Editor  Ctrl+E").clicked() {
                self.code_editor.open = true;
                ui.close();
            }
            if ui.button("Open scripts folder in VS Code").clicked() {
                if self.ensure_scripts_crate() {
                    crate::script_tools::open_folder_in_code_editor(self.scripts.crate_dir());
                }
                ui.close();
            }
            if ui.button("📜 Scripts panel").clicked() {
                self.bottom_tab = BottomTab::Scripts;
                ui.close();
            }
            ui.separator();
            if ui.button("🔨 Build scripts").clicked() {
                self.scripts.start_build();
                self.bottom_tab = BottomTab::Scripts;
                ui.close();
            }
            if ui.button("🗑 Clean build").clicked() {
                self.scripts.clean_build();
                self.bottom_tab = BottomTab::Scripts;
                ui.close();
            }
            if ui.button("⟳ Reload library").clicked() {
                let mut worlds: Vec<&mut World> = self.docs.iter_mut().map(|d| &mut d.world).collect();
                if let Err(e) = self.scripts.load(&mut worlds) {
                    log::error!("{e}");
                }
                ui.close();
            }
            if ui.button("Unload").clicked() {
                let mut worlds: Vec<&mut World> = self.docs.iter_mut().map(|d| &mut d.world).collect();
                self.scripts.unload(&mut worlds);
                ui.close();
            }
            ui.separator();
            ui.checkbox(&mut self.scripts.auto_build, "Build on save");
            ui.checkbox(&mut self.scripts.auto_reload, "Auto-reload when the library changes");
        });
        ui.menu_button("View", |ui| {
            ui.checkbox(&mut self.settings.show_grid, "Grid");
            ui.checkbox(&mut self.settings.show_bounds, "Bounding boxes");
            ui.add_enabled(renderer.wireframe_supported(), egui::Checkbox::new(&mut self.settings.wireframe, "Wireframe"));
            ui.checkbox(&mut self.settings.show_icons, "Light / camera icons");
            ui.checkbox(&mut self.settings.show_colliders, "Physics colliders");
            ui.checkbox(&mut self.settings.frustum_cull, "Frustum culling");
            ui.checkbox(&mut self.settings.vsync, "VSync");
            ui.separator();
            ui.menu_button("Performance overlay (F3)", |ui| {
                for m in [dumb_runtime::perf::PerfOverlay::Off, dumb_runtime::perf::PerfOverlay::Minimal, dumb_runtime::perf::PerfOverlay::Full] {
                    ui.radio_value(&mut self.settings.scene_overlay, m, m.label());
                }
            });
            if ui.button("📊 Profiler  Ctrl+Shift+P").clicked() {
                self.profiler_window.open = true;
                ui.close();
            }
            if ui.button("🌐 Web Browser").clicked() {
                self.web_browser.open = true;
                ui.close();
            }
        });
        ui.menu_button("Help", |ui| {
            ui.label("Viewport: RMB+WASD/QE fly · MMB pan · Alt+LMB orbit · wheel zoom · F focus");
            ui.label("Gizmos: W move · E rotate · R scale · hold Ctrl to snap");
            ui.label("Play: Ctrl+P · Esc releases game input");
            ui.label("Drop files onto the window to import them.");
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.gizmo.mode, GizmoMode::Translate, "✋ Move").on_hover_text("W");
            ui.selectable_value(&mut self.gizmo.mode, GizmoMode::Rotate, "⟳ Rotate").on_hover_text("E");
            ui.selectable_value(&mut self.gizmo.mode, GizmoMode::Scale, "↔ Scale").on_hover_text("R");
            ui.separator();
            let space = if self.gizmo.space == GizmoSpace::World { "🌐 World" } else { "📍 Local" };
            if ui.button(space).clicked() {
                self.gizmo.space = if self.gizmo.space == GizmoSpace::World { GizmoSpace::Local } else { GizmoSpace::World };
            }
            ui.toggle_value(&mut self.gizmo.snap, "⌖ Snap");
            if self.gizmo.snap {
                ui.add(egui::DragValue::new(&mut self.settings.snap_translate).speed(0.05).range(0.01..=100.0).prefix("m "));
                ui.add(egui::DragValue::new(&mut self.settings.snap_rotate_degrees).speed(1.0).range(1.0..=90.0).suffix("°"));
            }

            ui.separator();
            ui.add_space((ui.available_width() / 2.0 - 260.0).max(0.0));
            let playing = self.play != PlayMode::Edit;
            let play_label = match self.play {
                PlayMode::Edit => "▶ Play",
                PlayMode::Playing => "⏸ Pause",
                PlayMode::Paused => "▶ Resume",
            };
            let play_btn = egui::Button::new(egui::RichText::new(play_label).strong()).fill(if playing {
                egui::Color32::from_rgb(40, 90, 150)
            } else {
                ui.visuals().widgets.inactive.weak_bg_fill
            });
            if ui.add(play_btn).on_hover_text("Ctrl+P").clicked() {
                self.play();
            }
            if ui.add_enabled(playing, egui::Button::new("⏹ Stop")).clicked() {
                self.stop();
            }
            if ui.add_enabled(self.play == PlayMode::Paused, egui::Button::new("⏭ Step")).clicked() {
                self.step_once = true;
            }
            if playing {
                ui.label(format!("t = {:.1}s", self.time.elapsed));
                ui.add(egui::Slider::new(&mut self.time.time_scale, 0.0..=4.0).text("time scale"));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(renderer.device_name());
                ui.separator();
                match &self.scripts.status {
                    ScriptStatus::Loaded { components, systems } => ui.weak(format!("📜 {components} comps · {systems} systems")),
                    ScriptStatus::Building => ui.weak("📜 building…"),
                    ScriptStatus::BuildFailed => ui.colored_label(egui::Color32::LIGHT_RED, "📜 build failed"),
                    ScriptStatus::Error(e) => ui.colored_label(egui::Color32::LIGHT_RED, "📜 error").on_hover_text(e),
                    ScriptStatus::NotLoaded => ui.weak("📜 no scripts"),
                };
                if ui.add_enabled(!self.scripts.is_building(), egui::Button::new("🔨 Build scripts")).clicked() {
                    self.scripts.start_build();
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, renderer: &Renderer) {
        ui.horizontal(|ui| {
            if let Some((s, t)) = &self.status {
                if t.elapsed().as_secs() < 8 {
                    ui.label(s);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let st = renderer.stats;
                if let Some(p) = &self.physics {
                    ui.weak(format!("physics {} bodies · {:.2} ms", p.stats.bodies, p.stats.step_ms));
                    ui.separator();
                }
                let ss = self.stats.stream;
                if ss.cells > 0 && self.play != PlayMode::Edit {
                    ui.weak(format!("streaming {}/{} cells · {} loading · {:.2} ms", ss.loaded, ss.cells, ss.loading, ss.ms));
                    ui.separator();
                }
                let ai = self.stats.ai;
                if ai.agents > 0 && self.play != PlayMode::Edit {
                    ui.weak(format!("AI {} agents · {} thinking · {} deferred · {} asleep", ai.agents, ai.thinking, ai.deferred, ai.sleeping))
                        .on_hover_text(format!("Agents per distance band (near, mid, far, asleep): {:?}", ai.per_lod));
                    ui.separator();
                }
                ui.weak(format!(
                    "{:.0} fps · {:.2} ms · sim {:.2} ms · {} draws ({} objects) · {:.1}k tris · {} culled · {} entities",
                    self.stats.fps,
                    self.stats.frame_ms,
                    self.stats.sim_ms,
                    st.draw_calls,
                    st.instances,
                    st.triangles as f64 / 1000.0,
                    self.stats.culled,
                    self.docs[self.active].world.entity_count()
                ));
            });
        });
    }

    fn scripts_panel(&mut self, ui: &mut egui::Ui) {
        let trash = self.project.root.join("Library").join("Trash").join("Scripts");
        match crate::script_tools::scripts_panel(ui, &mut self.scripts, &mut self.scripts_ui, &trash) {
            Some(crate::script_tools::PanelAction::NewScript) => self.new_script.show(),
            Some(crate::script_tools::PanelAction::CreateCrate) => {
                if self.ensure_scripts_crate() {
                    self.scripts.start_build();
                }
            }
            Some(crate::script_tools::PanelAction::Reload) => {
                let mut worlds: Vec<&mut World> = self.docs.iter_mut().map(|d| &mut d.world).collect();
                if let Err(e) = self.scripts.load(&mut worlds) {
                    log::error!("{e}");
                }
            }
            Some(crate::script_tools::PanelAction::Unload) => {
                let mut worlds: Vec<&mut World> = self.docs.iter_mut().map(|d| &mut d.world).collect();
                self.scripts.unload(&mut worlds);
            }
            None => {}
        }
    }

    /// Create the project's scripts crate if it doesn't exist. Returns true when it exists afterwards.
    fn ensure_scripts_crate(&mut self) -> bool {
        let dir = self.scripts.crate_dir().to_path_buf();
        if dir.join("Cargo.toml").exists() {
            return true;
        }
        let engine = dumb_runtime::engine_root();
        match dumb_script::project::create_scripts_crate(&dir, &self.scripts.package, &engine, Some(&engine.join("Cargo.lock"))) {
            Ok(()) => {
                self.set_status(format!("Created scripts crate `{}` at {}", self.scripts.package, dir.display()));
                self.scripts_ui.invalidate();
                true
            }
            Err(e) => {
                self.set_status(format!("Could not create scripts crate: {e}"));
                false
            }
        }
    }

    /// A script file was written by the New Script dialog: build it and show it.
    fn on_script_created(&mut self, created: crate::script_tools::CreatedScript, needs_crate: bool) {
        if needs_crate && !self.ensure_scripts_crate() {
            return;
        }
        let name = created.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.set_status(format!("Created script {name} — building…"));
        self.scripts_ui.invalidate();
        self.scripts.mark_sources_seen();
        self.scripts.start_build();
        self.bottom_tab = BottomTab::Scripts;
        if created.open_after {
            crate::script_tools::open_in_code_editor(&created.path, None, Some(self.scripts.crate_dir()));
        }
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let Editor { docs, active, db, actions, add_comp_search, scripts, status, .. } = self;
        let d = &mut docs[*active];
        if d.selection.show_asset {
            if let Some(id) = d.selection.asset {
                asset_inspector(ui, db, id, actions);
                return;
            }
        }
        let Some(e) = d.selection.primary().filter(|e| d.world.is_alive(*e)) else {
            ui.weak("Nothing selected.");
            return;
        };
        if d.selection.entities.len() > 1 {
            ui.weak(format!("{} entities selected (editing the last)", d.selection.entities.len()));
        }
        // Names of entities referenced by this entity's components (for Entity fields).
        let mut names: Vec<(Entity, String)> = Vec::new();
        for id in d.world.components_of(e) {
            if let Some(r) = d.world.get_reflect(e, id) {
                dumb_reflect::to_value(r).visit(&mut |v| {
                    if let dumb_reflect::Value::Entity(bits) = v {
                        let x = Entity::from_bits(*bits);
                        if d.world.is_alive(x) {
                            names.push((x, entity_label(&d.world, x)));
                        }
                    }
                });
            }
        }
        let name_fn = move |x: Entity| names.iter().find(|(n, _)| *n == x).map_or("<missing>".to_string(), |(_, s)| s.clone());
        let mut ictx = InspectCtx { db, entity_name: &name_fn, open_asset: None };
        let (changed, action) = entity_inspector(ui, &mut d.world, e, &mut ictx);
        if let Some(a) = ictx.open_asset {
            actions.push(Action::OpenAsset(a));
        }
        if changed {
            d.mark_changed();
        }
        match action {
            ComponentAction::Remove(id) => {
                d.world.remove_id(e, id);
                d.mark_changed();
            }
            ComponentAction::Reset(id) => {
                d.world.insert_value(e, id, None);
                d.mark_changed();
            }
            ComponentAction::RemoveMissing(name) => {
                d.world.remove_missing(e, &name);
                d.mark_changed();
            }
            ComponentAction::EditScript(type_name) => {
                match dumb_script::project::source_for_type(scripts.crate_dir(), &type_name) {
                    Some(path) => crate::script_tools::open_in_code_editor(&path, None, Some(scripts.crate_dir())),
                    None => {
                        let msg = format!("Source for {type_name} not found in {}", scripts.crate_dir().display());
                        log::warn!("{msg}");
                        *status = Some((msg, Instant::now()));
                    }
                }
            }
            ComponentAction::None => {}
        }
        ui.add_space(8.0);
        if let Some(id) = add_component_menu(ui, &d.world, e, add_comp_search) {
            d.world.insert_value(e, id, None);
            d.mark_changed();
        }
    }

    fn center(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer) {
        // Scene tabs.
        ui.horizontal(|ui| {
            let mut close = None;
            for (i, d) in self.docs.iter().enumerate() {
                let label = format!("🌍 {}{}", d.name, if d.dirty { "*" } else { "" });
                if ui.selectable_label(i == self.active, label).clicked() && self.play == PlayMode::Edit {
                    self.active = i;
                    self.window_title = None;
                }
                if self.docs.len() > 1 && ui.small_button("✖").clicked() && self.play == PlayMode::Edit {
                    close = Some(i);
                }
            }
            if ui.small_button("➕").on_hover_text("New scene").clicked() {
                self.new_scene(renderer);
            }
            if let Some(i) = close {
                let d = self.docs.remove(i);
                renderer.destroy_target(d.target);
                self.active = self.active.min(self.docs.len() - 1);
            }
            ui.separator();
            ui.selectable_value(&mut self.center_tab, CenterTab::Scene, "🎬 Scene");
            ui.selectable_value(&mut self.center_tab, CenterTab::Game, "🎮 Game");
        });
        self.update_title();
        match self.center_tab {
            CenterTab::Scene => self.scene_view(ui, renderer),
            CenterTab::Game => self.game_view(ui, renderer),
        }
    }

    fn game_view(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer) {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, egui::Sense::click());
        self.game_rect = rect;
        let ppp = ui.ctx().pixels_per_point();
        renderer.resize_target(self.game_target, (rect.width() * ppp) as u32, (rect.height() * ppp) as u32);
        if let Some(t) = renderer.target_texture(self.game_target) {
            ui.painter().image(t, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        if find_primary_camera(&self.docs[self.active].world).is_none() {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "No camera in the scene (GameObject ▸ Camera)", egui::FontId::proportional(16.0), egui::Color32::LIGHT_GRAY);
        }
        if resp.clicked() && self.play == PlayMode::Playing {
            self.game_focused = true;
        }
        let border = if self.play == PlayMode::Playing && self.game_focused {
            egui::Color32::from_rgb(60, 140, 255)
        } else if self.play != PlayMode::Edit {
            egui::Color32::from_rgb(200, 160, 40)
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_stroke(rect.shrink(1.0), 0.0, egui::Stroke::new(2.0, border), egui::StrokeKind::Inside);
        if self.play == PlayMode::Playing && !self.game_focused {
            ui.painter().text(rect.center_top() + egui::vec2(0.0, 16.0), egui::Align2::CENTER_TOP, "Click to give input to the game", egui::FontId::proportional(13.0), egui::Color32::WHITE);
        }
        // HUD: live while playing; in edit mode a preview of the HUD components.
        if self.play == PlayMode::Edit {
            self.hud.begin_frame();
            dumb_ecs::hud::collect_components(&self.docs[self.active].world, &mut self.hud);
        }
        let interactive = self.play == PlayMode::Playing && self.game_focused;
        let elapsed = if self.play == PlayMode::Edit { ui.input(|i| i.time) } else { self.time.elapsed };
        let webs = self.hud_view.draw(ui, rect, &mut self.hud, self.game_cam.0, self.game_cam.1, &mut self.db, renderer, elapsed, interactive);
        self.web.requests.extend(webs);
        for (id, js) in self.hud.web_eval.drain(..) {
            self.web.evals.push((id, js));
        }
        let extras = dumb_runtime::perf::OverlayExtras { entities: self.docs[self.active].world.entity_count(), lines: self.sim_lines() };
        dumb_runtime::perf::overlay(ui, rect, self.game_overlay, &self.monitor, &renderer.stats, &extras);
    }

    fn scene_view(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer) {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        let target = self.docs[self.active].target;
        self.docs[self.active].rect = rect;
        renderer.resize_target(target, (rect.width() * ppp) as u32, (rect.height() * ppp) as u32);
        if let Some(t) = renderer.target_texture(target) {
            ui.painter().image(t, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        if self.play != PlayMode::Edit {
            ui.painter().rect_stroke(rect.shrink(1.0), 0.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(200, 160, 40)), egui::StrokeKind::Inside);
        }
        let aspect = rect.width().max(1.0) / rect.height().max(1.0);

        // ---- camera controls
        let (alt, shift) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift));
        let d = resp.drag_delta();
        let delta = Vec2::new(d.x, d.y);
        let dt = self.fly_dt;
        {
            let doc = &mut self.docs[self.active];
            if resp.dragged_by(egui::PointerButton::Secondary) {
                doc.camera.look(delta);
                let k = |key| ui.input(|i| i.key_down(key));
                let mv = Vec3::new(
                    k(egui::Key::D) as i32 as f32 - k(egui::Key::A) as i32 as f32,
                    k(egui::Key::E) as i32 as f32 - k(egui::Key::Q) as i32 as f32,
                    k(egui::Key::S) as i32 as f32 - k(egui::Key::W) as i32 as f32,
                );
                if mv != Vec3::ZERO {
                    doc.camera.fly(mv, dt, shift);
                }
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    doc.camera.speed = (doc.camera.speed * (1.0 + scroll * 0.002)).clamp(0.1, 1000.0);
                }
            } else if resp.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    doc.camera.zoom(scroll / 50.0);
                }
            }
            if resp.dragged_by(egui::PointerButton::Middle) {
                doc.camera.pan(delta, rect.height());
            }
            if resp.dragged_by(egui::PointerButton::Primary) && alt {
                doc.camera.orbit(delta);
            }
        }

        let doc = &self.docs[self.active];
        let view_proj = doc.camera.proj(aspect) * doc.camera.view();
        let cam_pos = doc.camera.position();

        // ---- light / camera icons
        let mut icon_hit = None;
        if self.settings.show_icons {
            let size = Vec2::new(rect.width(), rect.height());
            let painter = ui.painter_at(rect);
            let mouse = ui.input(|i| i.pointer.hover_pos());
            for (e, light, cam) in doc.world.query_ref::<(Entity, Option<&Light>, Option<&Camera>)>() {
                if light.is_none() && cam.is_none() {
                    continue;
                }
                let p = doc.world.global_matrix(e).w_axis.truncate();
                let Some(s) = project(view_proj, p, size) else { continue };
                let pos = rect.min + egui::vec2(s.x, s.y);
                if !rect.contains(pos) {
                    continue;
                }
                let selected = doc.selection.entities.contains(&e);
                painter.circle_filled(pos, 11.0, if selected { egui::Color32::from_rgb(230, 140, 30) } else { egui::Color32::from_black_alpha(150) });
                draw_icon(&painter, pos, light.map(|l| (l.kind, l.color)));
                if mouse.is_some_and(|m| (m - pos).length() < 11.0) {
                    icon_hit = Some(e);
                }
            }
        }

        // ---- gizmo
        let mut gizmo_active = false;
        if let Some(sel) = doc.selection.primary().filter(|e| doc.world.is_alive(*e) && doc.world.has::<Transform>(*e)) {
            let world_m = doc.world.global_matrix(sel);
            let gv = GizmoView { rect, view_proj, camera_pos: cam_pos };
            let ctrl = ui.input(|i| i.modifiers.command);
            let (new, _started) = self.gizmo.update(ui, &resp, &gv, world_m, ctrl);
            gizmo_active = self.gizmo.is_dragging() || self.gizmo.is_hovered();
            if let Some(new_world) = new {
                let doc = &mut self.docs[self.active];
                let parent_m = doc.world.parent(sel).map_or(Mat4::IDENTITY, |p| doc.world.global_matrix(p));
                let local = parent_m.inverse() * new_world;
                if let Some(t) = doc.world.get_mut::<Transform>(sel) {
                    let nt = Transform::from_matrix(local);
                    if nt != *t {
                        *t = nt;
                        doc.mark_changed();
                    }
                }
            }
        }

        // ---- picking
        if resp.clicked_by(egui::PointerButton::Primary) && !gizmo_active && !alt {
            let additive = ui.input(|i| i.modifiers.command || i.modifiers.shift);
            let hit = icon_hit.or_else(|| {
                let pos = resp.interact_pointer_pos()?;
                let uv = Vec2::new((pos.x - rect.min.x) / rect.width(), (pos.y - rect.min.y) / rect.height());
                let ray = self.docs[self.active].camera.ray(uv, aspect);
                crate::picking::pick(&self.docs[self.active].world, &self.db, &ray).map(|(e, _)| e)
            });
            match hit {
                Some(e) => {
                    // Clicking a child of a prefab selects the prefab root first.
                    let w = &self.docs[self.active].world;
                    let mut target = e;
                    let mut cur = e;
                    while let Some(p) = w.parent(cur) {
                        if w.has::<PrefabInstance>(p) {
                            target = p;
                        }
                        cur = p;
                    }
                    let already = self.docs[self.active].selection.entities.contains(&target);
                    let pick = if already && target != e { e } else { target };
                    self.actions.push(Action::Select(pick, additive));
                    let d = self.doc();
                    d.hierarchy.reveal_entity(&d.world, pick);
                }
                None if !additive => self.actions.push(Action::ClearSelection),
                None => {}
            }
        }

        // ---- drop assets into the scene
        if let Some(p) = resp.dnd_release_payload::<DragAsset>() {
            if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                let uv = Vec2::new((pos.x - rect.min.x) / rect.width(), (pos.y - rect.min.y) / rect.height());
                let ray = self.docs[self.active].camera.ray(uv, aspect);
                let hit = crate::picking::pick(&self.docs[self.active].world, &self.db, &ray);
                let is_material = self.db.entry(p.0).is_some_and(|e| e.kind == AssetKind::Material);
                if is_material {
                    if let Some((e, _)) = hit {
                        self.actions.push(Action::AssignMaterial(e, p.0));
                    }
                } else {
                    let at = hit
                        .map(|(_, t)| ray.origin + ray.dir * t)
                        .or_else(|| ray.intersect_plane(Vec3::ZERO, Vec3::Y).map(|t| ray.origin + ray.dir * t))
                        .unwrap_or(self.docs[self.active].camera.pivot);
                    self.actions.push(Action::SpawnAsset { asset: p.0, at: Some(at), parent: None });
                }
            }
        }
        if resp.dnd_hover_payload::<DragAsset>().is_some() {
            ui.painter().rect_stroke(rect.shrink(2.0), 0.0, egui::Stroke::new(2.0, egui::Color32::LIGHT_BLUE), egui::StrokeKind::Inside);
        }

        // ---- overlays
        let mut lines = vec![format!("{} culled · cam speed {:.1}", self.stats.culled, self.docs[self.active].camera.speed)];
        lines.extend(self.sim_lines());
        let extras = dumb_runtime::perf::OverlayExtras { entities: self.docs[self.active].world.entity_count(), lines };
        dumb_runtime::perf::overlay(ui, rect, self.settings.scene_overlay, &self.monitor, &renderer.stats, &extras);
        // Axis indicator.
        let view = self.docs[self.active].camera.view();
        let origin = rect.left_bottom() + egui::vec2(36.0, -36.0);
        for (axis, color, label) in [(Vec3::X, egui::Color32::from_rgb(235, 70, 70), "X"), (Vec3::Y, egui::Color32::from_rgb(110, 210, 60), "Y"), (Vec3::Z, egui::Color32::from_rgb(70, 130, 245), "Z")] {
            let v = view.transform_vector3(axis);
            let end = origin + egui::vec2(v.x, -v.y) * 24.0;
            ui.painter().line_segment([origin, end], egui::Stroke::new(2.0, color));
            ui.painter().text(end, egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(11.0), color);
        }
    }

    // ------------------------------------------------------------------ rendering

    pub fn build_views(&mut self, _renderer: &mut Renderer) -> Vec<RenderView> {
        let mut views = Vec::new();
        let t0 = Instant::now();
        let elapsed = self.time.elapsed as f32;

        // Scene view.
        let active = self.active;
        if self.center_tab == CenterTab::Scene {
            let Editor { docs, db, settings, .. } = self;
            let d = &docs[active];
            let aspect = d.rect.width().max(1.0) / d.rect.height().max(1.0);
            let mut v = RenderView::new(d.target);
            v.view = d.camera.view();
            v.proj = d.camera.proj(aspect);
            v.camera_pos = d.camera.position();
            v.wireframe = settings.wireframe;
            v.time = elapsed;
            if settings.show_grid {
                let step = if d.camera.distance > 200.0 { 10.0 } else { 1.0 };
                v.grid(50, step);
            }
            let culled = extract_world(&d.world, db, &mut v, &ExtractOptions { selected: &d.selection.entities, frustum_cull: settings.frustum_cull, show_bounds: settings.show_bounds });
            self.stats.culled = culled;
            if settings.show_colliders {
                dumb_physics::collider_debug_lines(&d.world, db, &mut |a, b, c| v.line(a, b, c));
            }
            // Selection helpers: bounds, camera frustum, light range.
            for e in &d.selection.entities {
                if !d.world.is_alive(*e) {
                    continue;
                }
                let m = d.world.global_matrix(*e);
                if let Some(mr) = d.world.get::<MeshRenderer>(*e) {
                    if let Some(model) = db.model_loaded(mr.model) {
                        v.aabb_lines(&model.aabb(), &m, Color::rgba(1.0, 0.6, 0.1, 0.6));
                    }
                }
                if let Some(cam) = d.world.get::<Camera>(*e) {
                    let (_, rot, pos) = m.to_scale_rotation_translation();
                    let view = Mat4::from_rotation_translation(rot, pos).inverse();
                    let mut c = cam.clone();
                    c.far = c.far.min(10.0);
                    let inv = (c.projection_matrix(16.0 / 9.0) * view).inverse();
                    let corners: Vec<Vec3> = [(-1.0, -1.0, 0.0), (1.0, -1.0, 0.0), (1.0, 1.0, 0.0), (-1.0, 1.0, 0.0), (-1.0, -1.0, 1.0), (1.0, -1.0, 1.0), (1.0, 1.0, 1.0), (-1.0, 1.0, 1.0)]
                        .iter()
                        .map(|(x, y, z)| inv.project_point3(Vec3::new(*x, *y, *z)))
                        .collect();
                    for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 0), (4, 5), (5, 6), (6, 7), (7, 4), (0, 4), (1, 5), (2, 6), (3, 7)] {
                        v.line(corners[a], corners[b], Color::rgba(0.9, 0.9, 0.9, 0.8));
                    }
                }
                if let Some(l) = d.world.get::<Light>(*e) {
                    let p = m.w_axis.truncate();
                    if l.kind == LightKind::Point {
                        for (u, w) in [(Vec3::X, Vec3::Z), (Vec3::X, Vec3::Y), (Vec3::Y, Vec3::Z)] {
                            for i in 0..48 {
                                let a = i as f32 / 48.0 * std::f32::consts::TAU;
                                let b = (i + 1) as f32 / 48.0 * std::f32::consts::TAU;
                                v.line(p + (u * a.cos() + w * a.sin()) * l.range, p + (u * b.cos() + w * b.sin()) * l.range, Color::rgba(1.0, 0.9, 0.4, 0.5));
                            }
                        }
                    } else {
                        let dir = m.transform_vector3(-Vec3::Z).normalize_or_zero();
                        v.overlay_line(p, p + dir * 2.0, Color::YELLOW);
                    }
                }
            }
            views.push(v);
        }

        // Game view.
        if self.center_tab == CenterTab::Game {
            let d = &self.docs[active];
            let aspect = self.game_rect.width().max(1.0) / self.game_rect.height().max(1.0);
            let mut v = RenderView::new(self.game_target);
            v.time = elapsed;
            if let Some((view, proj, pos, clear, sky)) = find_primary_camera(&d.world).and_then(|c| camera_view(&d.world, c, aspect)) {
                v.sky = sky;
                v.view = view;
                v.proj = proj;
                v.camera_pos = pos;
                v.clear_color = clear;
                self.game_cam = (proj * view, pos);
                let culled = extract_world(&d.world, &mut self.db, &mut v, &ExtractOptions { frustum_cull: self.settings.frustum_cull, ..Default::default() });
                self.stats.culled = culled;
            } else {
                v.clear_color = Color::rgb(0.02, 0.02, 0.025);
                v.sky = false;
            }
            views.push(v);
        }
        self.stats.extract_ms = t0.elapsed().as_secs_f32() * 1000.0;

        for mv in &mut self.model_viewers {
            if let Some(v) = mv.render_view(&mut self.db) {
                views.push(v);
            }
        }
        for mv in &mut self.material_viewers {
            if let Some(v) = mv.render_view(&mut self.db) {
                views.push(v);
            }
        }

        // Asset thumbnails, a few per frame.
        let mut budget = 3;
        while budget > 0 {
            let Some(id) = self.browser.thumbs.queue.pop() else { break };
            let Some(thumb) = self.browser.thumbs.thumbs.get_mut(&id) else { continue };
            let Some(target) = thumb.target else { continue };
            let kind = self.db.entry(id).map(|e| e.kind).or_else(|| self.db.sub_asset(id).map(|s| s.kind));
            let (model_id, material) = match kind {
                Some(AssetKind::Material) => (builtin::SPHERE, id),
                _ => (id, AssetId::NONE),
            };
            let Some(model) = self.db.model(model_id) else {
                self.browser.thumbs.queue.insert(0, id);
                budget -= 1;
                continue;
            };
            let mut cam = EditorCamera { yaw: 0.6, pitch: -0.35, fov_degrees: 35.0, ..Default::default() };
            cam.focus(&model.aabb());
            let mut v = RenderView::new(target);
            v.view = cam.view();
            v.proj = cam.proj(1.0);
            v.camera_pos = cam.position();
            v.clear_color = Color::rgb(0.16, 0.17, 0.19);
            v.sky = false; // thumbnails stay on a neutral background
            v.shadows = false;
            push_model(&mut v, &model, model_id, material, Mat4::IDENTITY, Color::WHITE, None, false, None);
            views.push(v);
            thumb.rendered = true;
            budget -= 1;
        }
        views
    }
}

/// Default content for a new scene: sun, camera, ground.
pub fn populate_default_scene(w: &mut World) {
    let sun = w.spawn_named("Sun");
    let t = w.get_mut::<Transform>(sun).unwrap();
    t.translation = Vec3::new(0.0, 10.0, 0.0);
    t.set_euler_degrees(Vec3::new(-50.0, -35.0, 0.0));
    w.insert(sun, Light { kind: LightKind::Directional, intensity: 3.0, ..Default::default() });

    let cam = w.spawn_named("Main Camera");
    let t = w.get_mut::<Transform>(cam).unwrap();
    t.translation = Vec3::new(0.0, 4.0, 10.0);
    t.look_at(Vec3::new(0.0, 0.5, 0.0), Vec3::Y);
    w.insert(cam, Camera::default());

    let ground = w.spawn_named("Ground");
    w.insert(ground, MeshRenderer { model: builtin::PLANE, tint: Color::rgb(0.45, 0.47, 0.5), ..Default::default() });
    w.insert(ground, dumb_ecs::Collider::fitted(dumb_ecs::ColliderShape::Box));
}

/// Vector icon for lights (`Some`) and cameras (`None`) in the scene view.
fn draw_icon(p: &egui::Painter, c: egui::Pos2, light: Option<(LightKind, Color)>) {
    let white = egui::Color32::from_gray(235);
    match light {
        Some((LightKind::Directional, col)) => {
            let fill: egui::Color32 = egui::Rgba::from_rgb(col.r, col.g, col.b).into();
            p.circle_filled(c, 3.5, fill);
            for i in 0..8 {
                let a = i as f32 / 8.0 * std::f32::consts::TAU;
                let d = egui::vec2(a.cos(), a.sin());
                p.line_segment([c + d * 5.5, c + d * 8.0], egui::Stroke::new(1.5, fill));
            }
        }
        Some((LightKind::Point, col)) => {
            let fill: egui::Color32 = egui::Rgba::from_rgb(col.r, col.g, col.b).into();
            p.circle_filled(c - egui::vec2(0.0, 1.5), 4.5, fill);
            p.rect_filled(egui::Rect::from_center_size(c + egui::vec2(0.0, 5.0), egui::vec2(4.0, 3.0)), 0.5, white);
        }
        None => {
            p.rect_filled(egui::Rect::from_center_size(c - egui::vec2(2.0, 0.0), egui::vec2(9.0, 7.0)), 1.5, white);
            p.add(egui::Shape::convex_polygon(
                vec![c + egui::vec2(2.5, 0.0), c + egui::vec2(7.0, -3.5), c + egui::vec2(7.0, 3.5)],
                white,
                egui::Stroke::NONE,
            ));
        }
    }
}

impl Editor {
    /// AI / streaming lines for the full performance overlay.
    fn sim_lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.play == PlayMode::Edit {
            return v;
        }
        let (ai, ss) = (self.stats.ai, self.stats.stream);
        if ai.agents > 0 {
            v.push(format!("AI {} agents · {} thinking · {} asleep", ai.agents, ai.thinking, ai.sleeping));
        }
        if ss.cells > 0 {
            v.push(format!("streaming {}/{} cells · {} loading", ss.loaded, ss.cells, ss.loading));
        }
        v
    }
}

//! Standalone game player: `dumb-player [project_dir] [scene]`.
//! An exported game runs it with no arguments: the project is the executable's folder.

// Release games get no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use dumb_asset::AssetDatabase;
use dumb_core::{Input, Time};
use dumb_ecs::{SceneData, World};
use dumb_render::{camera_view, extract_world, find_primary_camera, EguiFrame, ExtractOptions, RenderTargetId, RenderView, Renderer};
use dumb_runtime::{feed_input, simulate, Project};
use dumb_script::ScriptHost;
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{Window, WindowId};

struct Game {
    window: Arc<Window>,
    renderer: Renderer,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    db: AssetDatabase,
    world: World,
    scripts: ScriptHost,
    time: Time,
    input: Input,
    physics: dumb_physics::Physics,
    streamer: dumb_runtime::streaming::Streamer,
    target: RenderTargetId,
    last: Instant,
    limiter: dumb_runtime::FrameLimiter,
    max_fps: u32,
    monitor: dumb_runtime::perf::SystemMonitor,
    overlay: dumb_runtime::perf::PerfOverlay,
    sim: dumb_runtime::SimStats,
    hud: dumb_ecs::Hud,
    hud_view: dumb_runtime::hud_view::HudView,
    web: dumb_runtime::web::WebLayer,
}

struct App {
    project: Project,
    scene: Option<String>,
    game: Option<Game>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.game.is_some() {
            return;
        }
        let window = Arc::new(
            el.create_window(
                Window::default_attributes()
                    .with_title(if self.project.is_packaged() { self.project.settings.name.clone() } else { format!("{} — Dumb Engine", self.project.settings.name) })
                    .with_inner_size(winit::dpi::LogicalSize::new(self.project.settings.window_size[0] as f64, self.project.settings.window_size[1] as f64))
                    .with_fullscreen(self.project.settings.fullscreen.then_some(winit::window::Fullscreen::Borderless(None))),
            )
            .expect("window"),
        );
        let size = window.inner_size();
        let mut renderer = Renderer::new(
            window.display_handle().unwrap().as_raw(),
            window.window_handle().unwrap().as_raw(),
            [size.width, size.height],
        )
        .unwrap_or_else(|e| panic!("renderer: {e}"));
        renderer.set_vsync(self.project.settings.vsync);
        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(egui_ctx.clone(), egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, None);
        let mut db = AssetDatabase::open(&self.project.root).expect("open project");
        let mut world = World::new();
        let mut scripts = self.project.script_host();
        if let Err(e) = scripts.load(&mut [&mut world]) {
            log::warn!("scripts: {e}");
        }
        let scene = self.scene.clone().unwrap_or(self.project.settings.startup_scene.clone());
        match db.id_for_path(&scene).and_then(|id| db.read_asset_text(id)) {
            Some(text) => match SceneData::from_ron(&text) {
                Ok(s) => {
                    s.instantiate(&mut world);
                }
                Err(e) => log::error!("scene parse error: {e}"),
            },
            None => log::error!("scene `{scene}` not found"),
        }
        db.update();
        let target = renderer.create_target(size.width.max(1), size.height.max(1));
        self.game = Some(Game {
            window,
            renderer,
            egui_ctx,
            egui_state,
            db,
            world,
            scripts,
            time: Time::new(),
            input: Input::default(),
            physics: {
                let mut p = dumb_physics::Physics::new(dumb_core::Vec3::from(self.project.settings.gravity));
                self.project.configure_physics(&mut p);
                p
            },
            streamer: Default::default(),
            target,
            last: Instant::now(),
            limiter: Default::default(),
            max_fps: self.project.settings.max_fps,
            monitor: Default::default(),
            // DUMB_PERF=full|fps overrides the project setting (benchmarks, screenshots).
            overlay: match std::env::var("DUMB_PERF").as_deref() {
                Ok("full") => dumb_runtime::perf::PerfOverlay::Full,
                Ok("fps") => dumb_runtime::perf::PerfOverlay::Minimal,
                _ => self.project.settings.perf_overlay,
            },
            sim: Default::default(),
            hud: Default::default(),
            hud_view: Default::default(),
            web: dumb_runtime::web::WebLayer::new(&self.project.root),
        });
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(g) = &mut self.game else { return };
        let _ = g.egui_state.on_window_event(&g.window, &event);
        feed_input(&mut g.input, &event);
        match event {
            WindowEvent::CloseRequested => {
                g.scripts.unload(&mut [&mut g.world]);
                el.exit();
            }
            WindowEvent::Resized(s) => g.renderer.resize([s.width, s.height]),
            // F3: cycle the performance overlay.
            WindowEvent::KeyboardInput { event: ref k, .. }
                if k.state.is_pressed() && !k.repeat && k.physical_key == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::F3) =>
            {
                g.overlay = g.overlay.next();
            }
            WindowEvent::RedrawRequested => {
                let dt = g.last.elapsed().as_secs_f32();
                g.last = Instant::now();
                g.time.advance(dt);
                g.db.update();
                g.scripts.update(&mut [&mut g.world]);
                g.sim = simulate(&mut g.world, &mut g.scripts, &g.db, &g.time, &g.input, Some(&mut g.physics), Some(&mut g.streamer), &self.project.settings, &mut g.hud);

                let size = g.window.inner_size();
                g.renderer.resize_target(g.target, size.width.max(1), size.height.max(1));
                let mut view = RenderView::new(g.target);
                let aspect = size.width.max(1) as f32 / size.height.max(1) as f32;
                if let Some((v, p, pos, clear, sky)) = find_primary_camera(&g.world).and_then(|c| camera_view(&g.world, c, aspect)) {
                    view.sky = sky;
                    view.view = v;
                    view.proj = p;
                    view.camera_pos = pos;
                    view.clear_color = clear;
                }
                view.time = g.time.elapsed as f32;
                let _extract = dumb_core::profiler::Scope::new("extract");
                extract_world(&g.world, &mut g.db, &mut view, &ExtractOptions { frustum_cull: true, ..Default::default() });

                drop(_extract);
                g.monitor.update(&g.renderer);
                let raw = g.egui_state.take_egui_input(&g.window);
                let tex = g.renderer.target_texture(g.target);
                let rs = g.renderer.stats;
                let extras = dumb_runtime::perf::OverlayExtras { entities: g.world.entity_count(), lines: sim_lines(&g.sim) };
                let (overlay, monitor) = (g.overlay, &g.monitor);
                let (view_proj, cam_pos, t) = (view.proj * view.view, view.camera_pos, g.time.elapsed);
                let (hud, hud_view, db, renderer) = (&mut g.hud, &mut g.hud_view, &mut g.db, &mut g.renderer);
                let mut webs = Vec::new();
                let out = g.egui_ctx.run_ui(raw, |ui| {
                    egui::CentralPanel::no_frame().show(ui, |ui| {
                        let r = ui.max_rect();
                        if let Some(t) = tex {
                            ui.painter().image(t, r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                        }
                        webs = hud_view.draw(ui, r, hud, view_proj, cam_pos, db, renderer, t, true);
                        dumb_runtime::perf::overlay(ui, r, overlay, monitor, &rs, &extras);
                    });
                });
                g.egui_state.handle_platform_output(&g.window, out.platform_output);
                // Web panels (HUD): place them, run queued JavaScript, collect page messages.
                g.web.sync(&*g.window, &webs, out.pixels_per_point);
                for (id, js) in g.hud.web_eval.drain(..) {
                    g.web.eval(&id, &js);
                }
                g.hud.web_messages = g.web.take_messages();
                let prims = g.egui_ctx.tessellate(out.shapes, out.pixels_per_point);
                let _render = dumb_core::profiler::Scope::new("render");
                g.renderer.draw_frame(
                    Some(&mut g.db),
                    &[view],
                    Some(EguiFrame { primitives: &prims, textures: &out.textures_delta, pixels_per_point: out.pixels_per_point }),
                );
                let mut delta = out.textures_delta;
                delta.clear();
                drop(_render);
                g.input.end_frame();
                {
                    dumb_core::profile_scope!("frame cap wait");
                    g.limiter.wait(g.max_fps);
                }
                dumb_core::profiler::end_frame();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        if let Some(g) = &self.game {
            g.window.request_redraw();
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut args = std::env::args().skip(1);
    // An exported game has project.ron next to the executable.
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf()));
    let default = match exe_dir {
        Some(d) if d.join("project.ron").exists() => d,
        _ => "project".into(),
    };
    let project = Project::open(args.next().map(std::path::PathBuf::from).unwrap_or(default));
    let scene = args.next();
    let el = EventLoop::new().expect("event loop");
    el.set_control_flow(ControlFlow::Poll);
    let mut app = App { project, scene, game: None };
    el.run_app(&mut app).expect("run");
}

/// AI / streaming lines for the full overlay.
fn sim_lines(s: &dumb_runtime::SimStats) -> Vec<String> {
    let mut v = Vec::new();
    if s.ai.agents > 0 {
        v.push(format!("AI {} agents · {} thinking · {} asleep", s.ai.agents, s.ai.thinking, s.ai.sleeping));
    }
    if s.stream.cells > 0 {
        v.push(format!("streaming {}/{} cells · {} loading", s.stream.loaded, s.stream.cells, s.stream.loading));
    }
    v
}

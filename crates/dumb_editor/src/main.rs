//! Dumb Engine editor: `dumb-editor [project_dir]`.

mod asset_browser;
mod build_window;
mod camera;
mod code_editor;
mod console;
mod editor;
mod gizmo;
mod hierarchy;
mod inspector;
mod material_viewer;
mod model_viewer;
mod picking;
mod prefs;
mod profiler_window;
mod project_manager;
mod script_tools;
mod viewport;
mod web_browser;
mod widgets;

use dumb_render::{EguiFrame, Renderer};
use editor::{Editor, ProjectRequest};
use project_manager::{project_hub_ui, HubAction, NewProjectForm};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{Window, WindowId};

struct Running {
    window: Arc<Window>,
    renderer: Renderer,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    /// `None` while no project is open (the start screen is shown).
    editor: Option<Editor>,
    hub_form: NewProjectForm,
    hub_error: Option<String>,
    last: Instant,
    limiter: dumb_runtime::FrameLimiter,
    focused: bool,
    /// Native web views (browser window, HUD web panels) of the open project.
    web: Option<dumb_runtime::web::WebLayer>,
}

struct App {
    project_dir: Option<PathBuf>,
    log: console::LogBuffer,
    running: Option<Running>,
}

impl Running {
    fn open_project(&mut self, dir: &std::path::Path, log: &console::LogBuffer) {
        self.web = None;
        if let Some(mut old) = self.editor.take() {
            old.shutdown();
            old.release(&mut self.renderer);
        }
        let editor = Editor::new(dir, &mut self.renderer, log.clone());
        self.window.set_title(&format!("{} — Dumb Engine", editor.project.settings.name));
        self.editor = Some(editor);
        self.hub_error = None;
    }

    fn close_project(&mut self) {
        self.web = None;
        if let Some(mut old) = self.editor.take() {
            old.shutdown();
            old.release(&mut self.renderer);
        }
        self.window.set_title("Dumb Engine");
    }

    fn frame(&mut self, el: &ActiveEventLoop, log: &console::LogBuffer) {
        let dt = self.last.elapsed().as_secs_f32();
        self.last = Instant::now();
        let Running { window, renderer, egui_ctx, egui_state, editor, hub_form, hub_error, web, .. } = self;

        let mut hub_action = None;
        let (out, views) = match editor {
            Some(ed) => {
                {
                    dumb_core::profile_scope!("update");
                    ed.pre_frame(dt, renderer);
                }
                let raw = egui_state.take_egui_input(window);
                let out = {
                    dumb_core::profile_scope!("editor ui");
                    egui_ctx.run_ui(raw, |ui| ed.ui(ui, renderer))
                };
                let views = {
                    dumb_core::profile_scope!("extract");
                    ed.build_views(renderer)
                };
                (out, views)
            }
            None => {
                let raw = egui_state.take_egui_input(window);
                let out = egui_ctx.run_ui(raw, |ui| hub_action = project_hub_ui(ui, hub_form, hub_error));
                (out, Vec::new())
            }
        };
        egui_state.handle_platform_output(window, out.platform_output);
        if let Some(ed) = editor.as_mut() {
            dumb_core::profile_scope!("web views");
            let frame = std::mem::take(&mut ed.web);
            let layer = web.get_or_insert_with(|| dumb_runtime::web::WebLayer::new(&ed.project.root));
            layer.sync(&**window, &frame.requests, out.pixels_per_point);
            for (id, js) in &frame.evals {
                layer.eval(id, js);
            }
            if let Some(id) = frame.devtools {
                layer.open_devtools(&id);
            }
            ed.hud.web_messages = layer.take_messages();
            ed.web_browser.error = layer.error.clone();
        }
        let prims = {
            dumb_core::profile_scope!("tessellate");
            egui_ctx.tessellate(out.shapes, out.pixels_per_point)
        };
        dumb_core::profile_scope!("render");
        renderer.draw_frame(
            editor.as_mut().map(|e| &mut e.db),
            &views,
            Some(EguiFrame { primitives: &prims, textures: &out.textures_delta, pixels_per_point: out.pixels_per_point }),
        );
        let mut delta = out.textures_delta;
        delta.clear();

        let mut request = None;
        if let Some(ed) = editor {
            ed.post_frame();
            if let Some(t) = ed.window_title.take() {
                window.set_title(&t);
            }
            request = ed.request.take();
        }
        if let Some(HubAction::Open(dir)) = hub_action {
            request = Some(ProjectRequest::Open(dir));
        }
        match request {
            Some(ProjectRequest::Open(dir)) => self.open_project(&dir, log),
            Some(ProjectRequest::Close) => self.close_project(),
            Some(ProjectRequest::Quit) => {
                self.close_project();
                el.exit();
            }
            None => {}
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.running.is_some() {
            return;
        }
        let window = Arc::new(
            el.create_window(
                Window::default_attributes()
                    .with_title("Dumb Engine")
                    .with_inner_size(winit::dpi::LogicalSize::new(1680.0, 980.0))
                    .with_maximized(true),
            )
            .expect("create window"),
        );
        let size = window.inner_size();
        let renderer = match Renderer::new(
            window.display_handle().unwrap().as_raw(),
            window.window_handle().unwrap().as_raw(),
            [size.width.max(1), size.height.max(1)],
        ) {
            Ok(r) => r,
            Err(e) => {
                log::error!("Vulkan initialization failed: {e}");
                el.exit();
                return;
            }
        };
        let egui_ctx = egui::Context::default();
        editor::apply_style(&egui_ctx, prefs::EditorSettings::load().theme);
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(8192),
        );
        let mut running = Running {
            window,
            renderer,
            egui_ctx,
            egui_state,
            editor: None,
            hub_form: NewProjectForm::default(),
            hub_error: None,
            last: Instant::now(),
            limiter: Default::default(),
            focused: true,
            web: None,
        };
        if let Some(dir) = self.project_dir.take() {
            running.open_project(&dir, &self.log);
        }
        self.running = Some(running);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(r) = &mut self.running else { return };
        let response = r.egui_state.on_window_event(&r.window, &event);
        if let Some(ed) = &mut r.editor {
            ed.on_window_event(&event, response.consumed);
        }
        match event {
            WindowEvent::CloseRequested => match &mut r.editor {
                // The request runs after the next frame (possibly after the unsaved-changes prompt).
                Some(ed) => ed.request(ProjectRequest::Quit),
                None => el.exit(),
            },
            WindowEvent::Resized(s) => r.renderer.resize([s.width, s.height]),
            WindowEvent::Focused(f) => r.focused = f,
            WindowEvent::DroppedFile(path) => {
                if let Some(ed) = &mut r.editor {
                    ed.import_dropped(&path);
                } else if project_manager::is_project(&path) {
                    r.open_project(&path, &self.log);
                }
            }
            WindowEvent::RedrawRequested => {
                r.frame(el, &self.log);
                // Frame cap from Preferences (start screen: 60).
                let cap = r.editor.as_ref().map_or(60, |e| e.settings.frame_cap(r.focused));
                {
                    dumb_core::profile_scope!("frame cap wait");
                    r.limiter.wait(cap);
                }
                dumb_core::profiler::end_frame();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        if let Some(r) = &self.running {
            r.window.request_redraw();
        }
    }
}

fn main() {
    let log = console::LogBuffer::install();
    // An explicit path wins; otherwise reopen the last project, or show the start screen.
    let project_dir = std::env::args().nth(1).map(PathBuf::from).or_else(project_manager::last_project);
    let el = EventLoop::new().expect("event loop");
    el.set_control_flow(ControlFlow::Poll);
    let mut app = App { project_dir, log, running: None };
    el.run_app(&mut app).expect("event loop failed");
}

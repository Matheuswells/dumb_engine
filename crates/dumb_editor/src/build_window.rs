//! File ▸ Build & Export: package the project as a standalone game folder.

use dumb_runtime::export::{export, ExportEvent, ExportOptions};
use dumb_runtime::Project;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

enum Msg {
    Event(ExportEvent),
    Done(Result<PathBuf, String>),
}

struct Job {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    started: Instant,
}

#[derive(Default)]
pub struct BuildWindow {
    pub open: bool,
    job: Option<Job>,
    step: String,
    progress: f32,
    log: Vec<(bool, String)>,
    result: Option<Result<PathBuf, String>>,
    show_log: bool,
}

/// What the editor should do after this frame.
#[derive(Default)]
pub struct BuildRequest {
    /// Save open scenes before exporting.
    pub save_scenes: bool,
    pub settings_changed: bool,
}

impl BuildWindow {
    fn out_dir(project: &Project) -> PathBuf {
        let s = &project.settings;
        if s.build.out_dir.trim().is_empty() {
            project.root.join("Builds").join(sanitize(&s.name))
        } else {
            let p = PathBuf::from(s.build.out_dir.trim());
            if p.is_absolute() {
                p
            } else {
                project.root.join(p)
            }
        }
    }

    fn start(&mut self, project: &Project) {
        let s = &project.settings;
        let opts = ExportOptions {
            project_root: project.root.clone(),
            out_dir: Self::out_dir(project),
            game_name: s.name.clone(),
            release: s.build.release,
            include_scripts: s.build.include_scripts,
            startup_scene: s.startup_scene.clone(),
            skip_compile: false,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        std::thread::spawn(move || {
            let tx2 = tx.clone();
            let r = export(&opts, &c, &mut |e| {
                let _ = tx2.send(Msg::Event(e));
            });
            let _ = tx.send(Msg::Done(r));
        });
        self.job = Some(Job { rx, cancel, started: Instant::now() });
        self.log.clear();
        self.result = None;
        self.progress = 0.0;
        self.step = "Starting".into();
    }

    fn poll(&mut self, run_after: bool) {
        let Some(job) = &self.job else { return };
        loop {
            match job.rx.try_recv() {
                Ok(Msg::Event(ExportEvent::Step(s, p))) => {
                    self.log.push((false, format!("== {s}")));
                    self.step = s;
                    self.progress = p;
                }
                Ok(Msg::Event(ExportEvent::Log(l))) => self.log.push((false, l)),
                Ok(Msg::Event(ExportEvent::Warning(l))) => self.log.push((true, l)),
                Ok(Msg::Done(r)) => {
                    match &r {
                        Ok(exe) => {
                            log::info!("build finished: {}", exe.display());
                            if run_after {
                                run_game(exe);
                            }
                        }
                        Err(e) => {
                            log::error!("build failed: {e}");
                            self.show_log = true;
                        }
                    }
                    self.result = Some(r);
                    self.job = None;
                    return;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.result = Some(Err("build thread stopped".into()));
                    self.job = None;
                    return;
                }
            }
        }
    }

    /// Start an export with the project's build settings (MCP `build_export`).
    pub fn start_export(&mut self, project: &Project) -> Result<PathBuf, String> {
        if self.job.is_some() {
            return Err("an export is already running".into());
        }
        self.start(project);
        Ok(Self::out_dir(project))
    }

    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Progress, result and the last `log_lines` log lines (MCP `get_export_status`).
    pub fn status_json(&self, log_lines: usize) -> serde_json::Value {
        let state = match (&self.job, &self.result) {
            (Some(_), _) => "running",
            (None, Some(Ok(_))) => "succeeded",
            (None, Some(Err(_))) => "failed",
            (None, None) => "idle",
        };
        let log: Vec<String> = self.log.iter().rev().take(log_lines).rev().map(|(w, l)| if *w { format!("warning: {l}") } else { l.clone() }).collect();
        serde_json::json!({
            "state": state,
            "step": self.step,
            "progress": self.progress,
            "seconds": self.job.as_ref().map(|j| j.started.elapsed().as_secs_f32()),
            "executable": match &self.result { Some(Ok(p)) => Some(p.display().to_string()), _ => None },
            "error": match &self.result { Some(Err(e)) => Some(e.clone()), _ => None },
            "warnings": self.log.iter().filter(|(w, _)| *w).count(),
            "log": log,
        })
    }

    pub fn ui(&mut self, ctx: &egui::Context, project: &mut Project, scenes: &[String], dirty_scenes: bool) -> BuildRequest {
        let mut req = BuildRequest::default();
        self.poll(project.settings.build.run_after);
        if self.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if !self.open {
            return req;
        }
        let mut open = self.open;
        egui::Window::new("📦 Build & Export").open(&mut open).default_width(560.0).resizable(true).collapsible(false).show(ctx, |ui| {
            let running = self.job.is_some();
            ui.add_enabled_ui(!running, |ui| {
                let before = project.settings.clone();
                let s = &mut project.settings;
                egui::Grid::new("build_opts").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Game name");
                    ui.text_edit_singleline(&mut s.name);
                    ui.end_row();

                    ui.label("Startup scene");
                    egui::ComboBox::from_id_salt("build_scene")
                        .selected_text(if s.startup_scene.is_empty() { "(none)" } else { s.startup_scene.as_str() })
                        .width(300.0)
                        .show_ui(ui, |ui| {
                            for sc in scenes {
                                ui.selectable_value(&mut s.startup_scene, sc.clone(), sc);
                            }
                        });
                    ui.end_row();

                    ui.label("Configuration");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut s.build.release, true, "Release").on_hover_text("Optimized: fast game, slower build");
                        ui.radio_value(&mut s.build.release, false, "Debug").on_hover_text("Fast build, slow game, keeps a console");
                    });
                    ui.end_row();

                    ui.label("Output folder");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut s.build.out_dir).hint_text(format!("Builds/{}", sanitize(&s.name))).desired_width(300.0));
                        if ui.button("…").on_hover_text("Choose a folder").clicked() {
                            if let Some(d) = rfd::FileDialog::new().set_directory(&project.root).pick_folder() {
                                s.build.out_dir = dumb_runtime::strip_unc(&d).display().to_string();
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("");
                    ui.vertical(|ui| {
                        ui.checkbox(&mut s.build.include_scripts, "Include scripts");
                        ui.checkbox(&mut s.build.run_after, "Run the game when done");
                    });
                    ui.end_row();
                });
                if project.settings != before {
                    req.settings_changed = true;
                }
            });
            let out = Self::out_dir(project);
            ui.weak(format!("Output: {}", out.display()));
            if dirty_scenes {
                ui.colored_label(egui::Color32::from_rgb(255, 190, 80), "Unsaved scenes will be saved before building.");
            }
            ui.separator();

            ui.horizontal(|ui| {
                if running {
                    if ui.button("⏹ Cancel").clicked() {
                        if let Some(j) = &self.job {
                            j.cancel.store(true, Ordering::Relaxed);
                        }
                    }
                } else if ui.add(egui::Button::new(egui::RichText::new("🔨 Build").strong()).min_size(egui::vec2(110.0, 26.0))).clicked() {
                    req.save_scenes = dirty_scenes;
                    self.start(project);
                }
                if let Some(Ok(exe)) = &self.result {
                    if ui.button("▶ Run").clicked() {
                        run_game(exe);
                    }
                }
                if out.exists() && ui.button("📂 Open folder").clicked() {
                    #[cfg(windows)]
                    let _ = std::process::Command::new("explorer").arg(&out).spawn();
                }
                ui.toggle_value(&mut self.show_log, "Log");
            });

            if let Some(job) = &self.job {
                ui.add(egui::ProgressBar::new(self.progress).text(format!("{} · {:.0}s", self.step, job.started.elapsed().as_secs_f32())).animate(true));
            }
            match &self.result {
                Some(Ok(exe)) => {
                    ui.colored_label(egui::Color32::from_rgb(120, 220, 120), format!("✔ Built {}", exe.display()));
                }
                Some(Err(e)) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, format!("✖ {e}"));
                }
                None => {}
            }
            let warnings = self.log.iter().filter(|(w, _)| *w).count();
            if warnings > 0 && !running {
                ui.weak(format!("{warnings} warning(s)/error(s) in the log"));
            }
            if self.show_log || running {
                egui::Frame::NONE.fill(ui.visuals().extreme_bg_color).inner_margin(6.0).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, true]).stick_to_bottom(true).show(ui, |ui| {
                        for (warn, l) in &self.log {
                            let t = egui::RichText::new(l).monospace().size(11.0);
                            if *warn {
                                ui.label(t.color(egui::Color32::from_rgb(255, 150, 120)));
                            } else if l.starts_with("==") {
                                ui.label(t.strong());
                            } else {
                                ui.label(t.weak());
                            }
                        }
                    });
                });
            }
        });
        self.open = open;
        req
    }
}

fn run_game(exe: &std::path::Path) {
    if let Err(e) = std::process::Command::new(exe).current_dir(exe.parent().unwrap_or(std::path::Path::new("."))).spawn() {
        log::error!("could not start {}: {e}", exe.display());
    }
}

fn sanitize(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect();
    if s.trim().is_empty() {
        "Game".into()
    } else {
        s.trim().to_string()
    }
}

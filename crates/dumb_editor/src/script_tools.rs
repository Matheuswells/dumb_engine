//! Script tooling in the editor: the New Script dialog, the Scripts panel (files, systems with
//! profiling and toggles, problems, build output) and opening sources in a code editor.

use dumb_script::project::{self, MessageLevel, ScriptFile, ScriptTemplate};
use dumb_script::{ScriptHost, ScriptStatus};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Whether VS Code's `code` command is on PATH (checked once).
fn has_vscode() -> bool {
    static HAS: OnceLock<bool> = OnceLock::new();
    *HAS.get_or_init(|| {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "where", "code"]);
        hide_window(&mut cmd);
        cmd.output().is_ok_and(|o| o.status.success())
    })
}

fn hide_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd;
}

static CODE_EDITOR: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Custom command from Preferences (`{file}` and `{line}` are replaced). Empty = automatic.
pub fn set_code_editor(cmd: &str) {
    if let Ok(mut c) = CODE_EDITOR.lock() {
        *c = cmd.trim().to_string();
    }
}

/// Open a source file at a line in the configured code editor: the built-in one (default),
/// VS Code (`vscode`) or a custom command.
pub fn open_in_code_editor(path: &Path, line: Option<u32>, workspace: Option<&Path>) {
    let custom = CODE_EDITOR.lock().map(|c| c.clone()).unwrap_or_default();
    if custom.is_empty() || custom.eq_ignore_ascii_case("builtin") || custom.eq_ignore_ascii_case("built-in") {
        crate::code_editor::request_open(path, line);
        return;
    }
    if custom.eq_ignore_ascii_case("vscode") {
        open_vscode(path, line, workspace);
        return;
    }
    let line_s = line.unwrap_or(1).to_string();
    let command = custom.replace("{file}", &path.display().to_string()).replace("{line}", &line_s);
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", &command]);
    hide_window(&mut cmd);
    if let Err(e) = cmd.spawn() {
        log::error!("could not run code editor command `{command}`: {e}");
    }
}

/// Open in VS Code when installed, otherwise with the default app.
pub fn open_externally(path: &Path, line: Option<u32>) {
    open_vscode(path, line, None);
}

fn open_vscode(path: &Path, line: Option<u32>, workspace: Option<&Path>) {
    let mut cmd = std::process::Command::new("cmd");
    if has_vscode() {
        cmd.args(["/C", "code"]);
        if let Some(ws) = workspace {
            cmd.arg(ws);
        }
        let target = match line {
            Some(l) => format!("{}:{l}", path.display()),
            None => path.display().to_string(),
        };
        cmd.arg("-g").arg(target);
    } else {
        cmd.args(["/C", "start", ""]).arg(path);
    }
    hide_window(&mut cmd);
    if let Err(e) = cmd.spawn() {
        log::error!("could not open {}: {e}", path.display());
    }
}

/// Open a folder as a workspace in VS Code, or in Explorer.
pub fn open_folder_in_code_editor(dir: &Path) {
    let mut cmd = if has_vscode() {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "code"]).arg(dir);
        c
    } else {
        let mut c = std::process::Command::new("explorer");
        c.arg(dir);
        c
    };
    hide_window(&mut cmd);
    let _ = cmd.spawn();
}

// ---------------------------------------------------------------------------- New Script dialog

pub struct NewScriptDialog {
    pub open: bool,
    name: String,
    template: ScriptTemplate,
    open_after: bool,
    error: Option<String>,
}

impl Default for NewScriptDialog {
    fn default() -> Self {
        NewScriptDialog { open: false, name: String::new(), template: ScriptTemplate::Behaviour, open_after: true, error: None }
    }
}

/// What the New Script dialog produced.
pub struct CreatedScript {
    pub path: PathBuf,
    pub open_after: bool,
}

impl NewScriptDialog {
    pub fn show(&mut self) {
        self.open = true;
        self.error = None;
        self.name.clear();
    }

    pub fn ui(&mut self, ctx: &egui::Context, crate_dir: &Path, needs_crate: &mut bool) -> Option<CreatedScript> {
        if !self.open {
            return None;
        }
        let mut result = None;
        let modal = egui::Modal::new(egui::Id::new("new_script_modal")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("New Script");
            ui.add_space(4.0);
            let r = ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("Name, e.g. EnemyAI").desired_width(f32::INFINITY));
            r.request_focus();
            ui.add_space(4.0);
            for t in ScriptTemplate::ALL {
                ui.radio_value(&mut self.template, t, t.label());
            }
            let type_name = project::to_pascal(&self.name);
            let module = project::to_snake(&self.name);
            if !module.is_empty() {
                let what = match self.template {
                    ScriptTemplate::Behaviour | ScriptTemplate::Component => format!("component `{type_name}` in "),
                    ScriptTemplate::System => format!("system `{module}` in "),
                    ScriptTemplate::Empty => String::new(),
                };
                ui.weak(format!("Creates {what}Scripts/src/{module}.rs"));
            }
            ui.checkbox(&mut self.open_after, "Open in code editor");
            if let Some(e) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, e);
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.add_enabled(!module.is_empty(), egui::Button::new("Create")).clicked() || enter) && !module.is_empty() {
                    if !crate_dir.join("Cargo.toml").exists() {
                        *needs_crate = true;
                    }
                    match project::create_script(crate_dir, &self.name, self.template) {
                        Ok(path) => {
                            result = Some(CreatedScript { path, open_after: self.open_after });
                            self.open = false;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                if ui.button("Cancel").clicked() {
                    self.open = false;
                }
            });
        });
        if modal.should_close() {
            self.open = false;
        }
        result
    }
}

// ---------------------------------------------------------------------------- Scripts panel

pub enum PanelAction {
    NewScript,
    CreateCrate,
    Reload,
    Unload,
}

#[derive(Default)]
pub struct ScriptsPanelState {
    files: Vec<ScriptFile>,
    files_checked: Option<std::time::Instant>,
    renaming: Option<(PathBuf, String)>,
    confirm_delete: Option<PathBuf>,
    show_output: bool,
    show_warnings: bool,
    pub error: Option<String>,
}

impl ScriptsPanelState {
    fn refresh(&mut self, crate_dir: &Path) {
        if self.files_checked.is_none_or(|t| t.elapsed().as_millis() > 1000) {
            self.files = project::list_scripts(crate_dir);
            self.files_checked = Some(std::time::Instant::now());
        }
    }

    pub fn invalidate(&mut self) {
        self.files_checked = None;
    }
}

pub fn scripts_panel(ui: &mut egui::Ui, host: &mut ScriptHost, st: &mut ScriptsPanelState, trash_dir: &Path) -> Option<PanelAction> {
    let mut action = None;
    let crate_dir = host.crate_dir().to_path_buf();

    if !host.has_crate() {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            ui.label("This project has no scripts crate yet.");
            ui.weak(format!("It will be created at {}", crate_dir.display()));
            if ui.button("➕ Create scripts crate").clicked() {
                action = Some(PanelAction::CreateCrate);
            }
        });
        return action;
    }
    st.refresh(&crate_dir);

    // ---- toolbar
    ui.horizontal(|ui| {
        if ui.button("➕ New Script").clicked() {
            action = Some(PanelAction::NewScript);
        }
        ui.separator();
        let building = host.is_building();
        if ui.add_enabled(!building, egui::Button::new("🔨 Build")).clicked() {
            host.start_build();
        }
        if ui.add_enabled(!building, egui::Button::new("🗑 Clean build")).on_hover_text("cargo clean -p, then build").clicked() {
            host.clean_build();
        }
        if ui.button("⟳ Reload").clicked() {
            action = Some(PanelAction::Reload);
        }
        if ui.button("⏹ Unload").clicked() {
            action = Some(PanelAction::Unload);
        }
        ui.separator();
        ui.checkbox(&mut host.auto_build, "Build on save").on_hover_text("Rebuild when a file in Scripts/src changes");
        ui.checkbox(&mut host.auto_reload, "Auto-reload");
        ui.separator();
        if ui.button("📝 Open in code editor").clicked() {
            open_folder_in_code_editor(&crate_dir);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            match &host.status {
                ScriptStatus::Building => {
                    ui.spinner();
                    ui.label("building…");
                }
                ScriptStatus::BuildFailed => {
                    ui.colored_label(egui::Color32::LIGHT_RED, "build failed");
                }
                ScriptStatus::Error(e) => {
                    ui.colored_label(egui::Color32::LIGHT_RED, "error").on_hover_text(e);
                }
                ScriptStatus::Loaded { components, systems } => {
                    let t = host.last_build_time.map(|d| format!(" · built in {:.1}s", d.as_secs_f32())).unwrap_or_default();
                    ui.weak(format!("{components} components · {systems} systems{t}"));
                }
                ScriptStatus::NotLoaded => {
                    ui.weak("not loaded");
                }
            }
        });
    });
    if let Some(e) = st.error.clone() {
        ui.horizontal(|ui| {
            ui.colored_label(egui::Color32::LIGHT_RED, e);
            if ui.small_button("✖").clicked() {
                st.error = None;
            }
        });
    }
    ui.separator();

    ui.columns(3, |cols| {
        // ---- script files
        cols[0].strong(format!("Scripts ({})", st.files.len()));
        egui::ScrollArea::vertical().id_salt("script_files").auto_shrink([false, false]).show(&mut cols[0], |ui| {
            for f in st.files.clone() {
                if let Some((p, text)) = &mut st.renaming {
                    if *p == f.path {
                        let outcome = crate::widgets::inline_text_edit(ui, egui::Id::new(("rename_script", f.path.clone())), text, None, 160.0);
                        if !matches!(outcome, crate::widgets::InlineEdit::Editing) {
                            let new = project::to_snake(text);
                            let cancelled = matches!(outcome, crate::widgets::InlineEdit::Cancel);
                            if !cancelled && !new.is_empty() && new != f.module {
                                let dst = f.path.with_file_name(format!("{new}.rs"));
                                if dst.exists() {
                                    st.error = Some(format!("{new}.rs already exists"));
                                } else if let Err(e) = std::fs::rename(&f.path, &dst) {
                                    st.error = Some(e.to_string());
                                }
                            }
                            st.renaming = None;
                            st.invalidate();
                        }
                        continue;
                    }
                }
                let errors = host.messages.iter().filter(|m| m.level == MessageLevel::Error && m.file.ends_with(f.path.file_name().unwrap())).count();
                let icon = if errors > 0 { "⛔" } else if f.registered { "📜" } else { "📄" };
                let label = format!("{icon} {}.rs", f.module);
                let r = ui.selectable_label(false, label).on_hover_text(format!(
                    "{}\n{} lines{}",
                    f.path.display(),
                    f.lines,
                    if f.registered { "" } else { "\nhelper module (no register fn)" }
                ));
                if r.double_clicked() || r.clicked() {
                    open_in_code_editor(&f.path, None, Some(&crate_dir));
                }
                r.context_menu(|ui| {
                    if ui.button("Open").clicked() {
                        open_in_code_editor(&f.path, None, Some(&crate_dir));
                        ui.close();
                    }
                    if ui.button("Rename").clicked() {
                        st.renaming = Some((f.path.clone(), f.module.clone()));
                        ui.close();
                    }
                    if ui.button("Delete").clicked() {
                        st.confirm_delete = Some(f.path.clone());
                        ui.close();
                    }
                    if ui.button("Show in Explorer").clicked() {
                        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", f.path.display())).spawn();
                        ui.close();
                    }
                });
            }
            if st.files.is_empty() {
                ui.weak("No scripts yet — click ➕ New Script.");
            }
        });

        // ---- systems with profiling
        let total = host.total_time_ms();
        cols[1].strong(format!("Systems · {total:.2} ms/frame"));
        egui::ScrollArea::vertical().id_salt("script_systems").auto_shrink([false, false]).show(&mut cols[1], |ui| {
            let systems: Vec<String> = host.systems().iter().map(|s| s.name.clone()).collect();
            for name in systems {
                ui.horizontal(|ui| {
                    let mut on = !host.disabled.contains(&name);
                    if ui.checkbox(&mut on, "").on_hover_text("Run this system while playing").changed() {
                        if on {
                            host.disabled.remove(&name);
                        } else {
                            host.disabled.insert(name.clone());
                        }
                    }
                    let ms = host.timings.get(&name).copied().unwrap_or(0.0);
                    let err = host.errors.iter().find(|(n, _)| *n == name).map(|(_, e)| e.clone());
                    match err {
                        Some(e) => {
                            ui.colored_label(egui::Color32::LIGHT_RED, format!("⚠ {name}")).on_hover_text(format!("panicked: {e}"));
                        }
                        None => {
                            ui.label(&name);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.monospace(format!("{ms:6.3} ms"));
                        let frac = if total > 0.0 { ms / total } else { 0.0 };
                        let (r, _) = ui.allocate_exact_size(egui::vec2(60.0, 8.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 2.0, egui::Color32::from_gray(45));
                        let mut fill = r;
                        fill.set_width(r.width() * frac.clamp(0.0, 1.0));
                        ui.painter().rect_filled(fill, 2.0, egui::Color32::from_rgb(90, 150, 230));
                    });
                });
            }
            if host.systems().is_empty() {
                ui.weak("No systems registered.");
            }
            if !host.errors.is_empty() && ui.small_button("Clear panic markers").clicked() {
                host.errors.clear();
            }
            ui.separator();
            ui.strong("Components");
            for c in host.component_names() {
                let file = project::source_for_type(&crate_dir, &c);
                let short = c.rsplit("::").next().unwrap_or(&c).to_string();
                let r = ui.add(egui::Label::new(format!("📜 {short}")).sense(egui::Sense::click())).on_hover_text(&c);
                if r.clicked() {
                    if let Some(f) = &file {
                        open_in_code_editor(f, None, Some(&crate_dir));
                    }
                }
            }
        });

        // ---- problems + output
        let errors = host.messages.iter().filter(|m| m.level == MessageLevel::Error).count();
        let warnings = host.messages.len() - errors;
        cols[2].horizontal(|ui| {
            ui.strong(format!("Problems · {errors} errors, {warnings} warnings"));
            ui.toggle_value(&mut st.show_warnings, "warnings");
            ui.toggle_value(&mut st.show_output, "raw output");
        });
        egui::ScrollArea::vertical().id_salt("script_problems").stick_to_bottom(st.show_output).auto_shrink([false, false]).show(&mut cols[2], |ui| {
            if st.show_output {
                for l in &host.build_log {
                    let color = if l.contains("error") {
                        egui::Color32::LIGHT_RED
                    } else if l.contains("warning") {
                        egui::Color32::from_rgb(240, 200, 90)
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.label(egui::RichText::new(l).monospace().size(11.0).color(color));
                }
                return;
            }
            for m in &host.messages {
                if m.level == MessageLevel::Warning && !st.show_warnings {
                    continue;
                }
                let (icon, color) = match m.level {
                    MessageLevel::Error => ("⛔", egui::Color32::LIGHT_RED),
                    MessageLevel::Warning => ("⚠", egui::Color32::from_rgb(240, 200, 90)),
                };
                let file = m.file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                let r = ui
                    .add(egui::Label::new(egui::RichText::new(format!("{icon} {file}:{}  {}", m.line, m.text)).color(color)).sense(egui::Sense::click()))
                    .on_hover_text(format!("{}:{}:{}\nclick to open", m.file.display(), m.line, m.column));
                if r.clicked() {
                    open_in_code_editor(&m.file, Some(m.line), Some(&crate_dir));
                }
            }
            if host.messages.is_empty() {
                ui.weak(if host.is_building() { "Building…" } else { "No problems." });
            }
        });
    });

    // ---- delete confirmation
    if let Some(path) = st.confirm_delete.clone() {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        egui::Modal::new(egui::Id::new("delete_script_modal")).show(ui.ctx(), |ui| {
            ui.heading(format!("Delete {name}?"));
            ui.label("Components it defines are kept on entities as \"script not loaded\" until removed.");
            ui.weak("The file is moved to Library/Trash/Scripts.");
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                    let dst = trash_dir.join(format!("{stamp}_{name}"));
                    let res = std::fs::create_dir_all(trash_dir).and_then(|_| std::fs::rename(&path, &dst));
                    if let Err(e) = res {
                        st.error = Some(e.to_string());
                    }
                    st.confirm_delete = None;
                    st.invalidate();
                }
                if ui.button("Cancel").clicked() {
                    st.confirm_delete = None;
                }
            });
        });
    }
    action
}

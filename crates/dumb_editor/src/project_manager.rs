//! Project manager: recent projects, open, create.

use dumb_runtime::{Project, ProjectSettings};
use std::path::{Path, PathBuf};

fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("dumb-engine")
}

pub fn recent_projects() -> Vec<PathBuf> {
    std::fs::read_to_string(config_dir().join("recent.ron"))
        .ok()
        .and_then(|s| ron::from_str::<Vec<PathBuf>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.join("Assets").exists())
        .collect()
}

pub fn last_project() -> Option<PathBuf> {
    recent_projects().into_iter().next()
}

pub fn remember(path: &Path) {
    // Test and automation runs (DUMB_NO_RECENT=1) must not change the user's list.
    if std::env::var_os("DUMB_NO_RECENT").is_some() {
        return;
    }
    let path = dumb_runtime::strip_unc(&path.canonicalize().unwrap_or(path.to_path_buf()));
    let mut list = recent_projects();
    list.retain(|p| p != &path);
    list.insert(0, path);
    list.truncate(10);
    let _ = std::fs::create_dir_all(config_dir());
    if let Ok(s) = ron::ser::to_string_pretty(&list, ron::ser::PrettyConfig::default()) {
        let _ = std::fs::write(config_dir().join("recent.ron"), s);
    }
}

/// Create the standard project layout.
pub fn create_project(dir: &Path, name: &str) -> std::io::Result<()> {
    // No placeholder folders: the browser shows what exists, and folders appear as content is added
    // (saving a scene creates Scenes/, extracting a material creates Materials/, ...).
    std::fs::create_dir_all(dir.join("Assets").join("Scenes"))?;
    let mut world = dumb_ecs::World::new();
    crate::editor::populate_default_scene(&mut world);
    let scene = dumb_ecs::SceneData::capture(&world).to_ron().map_err(std::io::Error::other)?;
    std::fs::write(dir.join("Assets").join("Scenes").join("Main.scene"), scene)?;
    let package = format!("{}_scripts", dumb_script::project::crate_name(name));
    let p = Project {
        root: dir.to_path_buf(),
        settings: ProjectSettings {
            name: name.into(),
            startup_scene: "Scenes/Main.scene".into(),
            scripts_workspace: "Scripts".into(),
            scripts_package: package.clone(),
            ..Default::default()
        },
    };
    p.save()?;
    // Each project gets its own scripts crate with one example script.
    let engine = dumb_runtime::engine_root();
    let scripts = dir.join("Scripts");
    dumb_script::project::create_scripts_crate(&scripts, &package, &engine, Some(&engine.join("Cargo.lock")))?;
    if let Err(e) = dumb_script::project::create_script(&scripts, "ExampleBehaviour", dumb_script::project::ScriptTemplate::Behaviour) {
        log::warn!("starter script: {e}");
    }
    Ok(())
}

/// Whether a folder looks like a Dumb Engine project.
pub fn is_project(dir: &Path) -> bool {
    dir.join("project.ron").exists() || dir.join("Assets").is_dir()
}

/// Folder picker for "Open Project". `Ok(None)` when cancelled.
pub fn pick_project_folder() -> Result<Option<PathBuf>, String> {
    let Some(dir) = rfd::FileDialog::new().set_title("Open Project").pick_folder() else { return Ok(None) };
    if is_project(&dir) {
        Ok(Some(dir))
    } else {
        Err(format!("{} is not a Dumb Engine project (no project.ron or Assets/ folder)", dir.display()))
    }
}

fn default_location() -> String {
    let base = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Documents").join("Dumb Projects").display().to_string()
}

/// "New Project" dialog.
#[derive(Default)]
pub struct NewProjectForm {
    pub open: bool,
    name: String,
    location: String,
    error: Option<String>,
}

impl NewProjectForm {
    pub fn show(&mut self) {
        self.open = true;
        self.error = None;
        if self.name.is_empty() {
            self.name = "New Project".into();
        }
        if self.location.is_empty() {
            self.location = default_location();
        }
    }

    /// Returns the created project's folder.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<PathBuf> {
        if !self.open {
            return None;
        }
        let mut result = None;
        let modal = egui::Modal::new(egui::Id::new("new_project_modal")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Create New Project");
            ui.add_space(6.0);
            egui::Grid::new("new_project_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(360.0));
                ui.end_row();
                ui.label("Location");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.location).desired_width(326.0));
                    if ui.button("…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_title("Project location").pick_folder() {
                            self.location = d.display().to_string();
                        }
                    }
                });
                ui.end_row();
            });
            let name = self.name.trim();
            let dir = PathBuf::from(self.location.trim()).join(name);
            ui.weak(format!("Will be created at {}", dir.display()));
            if let Some(e) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, e);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let valid = !name.is_empty() && !self.location.trim().is_empty() && !name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']);
                if ui.add_enabled(valid, egui::Button::new("Create")).clicked() {
                    if is_project(&dir) {
                        self.error = Some("A project already exists in that folder — use Open Project".into());
                    } else if dir.exists() && std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some()) {
                        self.error = Some("That folder already exists and is not empty".into());
                    } else {
                        match create_project(&dir, name) {
                            Ok(()) => {
                                result = Some(dir.clone());
                                self.open = false;
                            }
                            Err(e) => self.error = Some(e.to_string()),
                        }
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

pub enum HubAction {
    Open(PathBuf),
}

/// Start screen shown when no project is open.
pub fn project_hub_ui(ui: &mut egui::Ui, form: &mut NewProjectForm, error: &mut Option<String>) -> Option<HubAction> {
    let mut action = None;
    egui::CentralPanel::default_margins().show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.heading(egui::RichText::new("Dumb Engine").size(34.0).strong());
            ui.weak("No project open");
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 330.0).max(0.0) / 2.0);
                if ui.add_sized([160.0, 34.0], egui::Button::new("➕ New Project…")).clicked() {
                    form.show();
                }
                if ui.add_sized([160.0, 34.0], egui::Button::new("📂 Open Project…")).clicked() {
                    match pick_project_folder() {
                        Ok(Some(dir)) => action = Some(HubAction::Open(dir)),
                        Ok(None) => {}
                        Err(e) => *error = Some(e),
                    }
                }
            });
            if let Some(e) = error.clone() {
                ui.add_space(8.0);
                ui.colored_label(egui::Color32::LIGHT_RED, e);
            }
            ui.add_space(28.0);
            let recent = recent_projects();
            if !recent.is_empty() {
                ui.strong("Recent projects");
                ui.add_space(4.0);
                for p in recent {
                    let name = Project::open(&p).settings.name;
                    let r = ui.add_sized([420.0, 40.0], egui::Button::new(format!("{name}\n{}", p.display())).wrap_mode(egui::TextWrapMode::Wrap));
                    if r.clicked() {
                        action = Some(HubAction::Open(p.clone()));
                    }
                }
            }
        });
    });
    if let Some(dir) = form.ui(ui.ctx()) {
        action = Some(HubAction::Open(dir));
    }
    action
}

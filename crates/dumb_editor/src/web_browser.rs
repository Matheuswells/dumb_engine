//! View ▸ Web Browser: a web page inside the editor (docs, dashboards, testing HUD pages).
//! The page itself is a native web view placed over this window's content area.

use dumb_runtime::hud_view::WebRequest;

pub const BROWSER_ID: &str = "editor_browser";

/// Web work for the main loop this frame (it owns the native window).
#[derive(Default)]
pub struct WebFrame {
    pub requests: Vec<WebRequest>,
    /// (web id, JavaScript)
    pub evals: Vec<(String, String)>,
    pub devtools: Option<String>,
}

pub struct WebBrowser {
    pub open: bool,
    url: String,
    /// What is loaded (changes when Enter/Go is pressed).
    current: String,
    pub error: Option<String>,
}

impl Default for WebBrowser {
    fn default() -> Self {
        WebBrowser { open: false, url: "https://www.rust-lang.org".into(), current: "https://www.rust-lang.org".into(), error: None }
    }
}

impl WebBrowser {
    pub fn open_url(&mut self, url: &str) {
        self.open = true;
        self.url = url.to_string();
        self.current = url.to_string();
    }

    pub fn ui(&mut self, ctx: &egui::Context, out: &mut WebFrame, html_files: &[String]) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("🌐 Web Browser").open(&mut open).default_size([900.0, 620.0]).min_size([320.0, 200.0]).resizable(true).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("◀").on_hover_text("Back").clicked() {
                    out.evals.push((BROWSER_ID.into(), "history.back()".into()));
                }
                if ui.button("▶").on_hover_text("Forward").clicked() {
                    out.evals.push((BROWSER_ID.into(), "history.forward()".into()));
                }
                if ui.button("⟳").on_hover_text("Reload").clicked() {
                    out.evals.push((BROWSER_ID.into(), "location.reload()".into()));
                }
                let w = (ui.available_width() - 150.0).max(80.0);
                let r = ui.add(egui::TextEdit::singleline(&mut self.url).desired_width(w).hint_text("https://… or a project .html file"));
                let go = ui.button("Go").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go {
                    self.current = self.url.trim().to_string();
                }
                egui::ComboBox::from_id_salt("web_project_pages").selected_text("Project").width(70.0).show_ui(ui, |ui| {
                    if html_files.is_empty() {
                        ui.weak("No .html files in Assets");
                    }
                    for f in html_files {
                        if ui.selectable_label(false, f).clicked() {
                            self.url = f.clone();
                            self.current = f.clone();
                        }
                    }
                });
                if ui.small_button("🔧").on_hover_text("Developer tools").clicked() {
                    out.devtools = Some(BROWSER_ID.into());
                }
            });
            if let Some(e) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, e);
            }
            let rect = ui.available_rect_before_wrap();
            ui.allocate_rect(rect, egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, egui::Color32::WHITE);
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "loading…", egui::FontId::proportional(14.0), egui::Color32::GRAY);
            // Only show the page while the window body is really on screen.
            if rect.width() > 20.0 && rect.height() > 20.0 {
                out.requests.push(WebRequest { id: BROWSER_ID.into(), url: self.current.clone(), rect });
            }
        });
        self.open = open;
    }
}

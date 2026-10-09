//! Built-in code editor for scripts: tabs, Rust syntax highlighting, line numbers, find/replace,
//! go to line, auto-indent, comment toggling, word completion, outline, rustfmt, `cargo check`
//! diagnostics in the gutter, and git (status, changed-line markers, diff, commit, pull/push).

pub mod git;
pub mod highlight;
pub mod lsp;
pub mod syntax;

use dumb_script::project::{parse_build_messages, BuildMessage, MessageLevel};
use egui::text::{CCursor, CCursorRange};
use egui::{Color32, FontId, Key, Modifiers};
use git::{FileStatus, LineChange};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

/// Files other parts of the editor asked to open (Scripts panel, inspector, console...).
static OPEN_REQUESTS: Mutex<Vec<(PathBuf, Option<u32>)>> = Mutex::new(Vec::new());

pub fn request_open(path: &Path, line: Option<u32>) {
    if let Ok(mut q) = OPEN_REQUESTS.lock() {
        q.push((path.to_path_buf(), line));
    }
}

struct Buffer {
    path: PathBuf,
    text: String,
    saved: String,
    mtime: Option<SystemTime>,
    is_rust: bool,
    changes: Vec<(usize, LineChange)>,
    changed_on_disk: bool,
    scroll_to_line: Option<usize>,
    /// Select this char range on the next frame (find, go to line).
    select: Option<(usize, usize)>,
    job_cache: Option<(u64, egui::text::LayoutJob)>,
}

impl Buffer {
    fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = text.replace("\r\n", "\n");
        Ok(Buffer {
            path: path.to_path_buf(),
            saved: text.clone(),
            text,
            mtime: mtime(path),
            is_rust: path.extension().is_some_and(|e| e == "rs"),
            changes: Vec::new(),
            changed_on_disk: false,
            scroll_to_line: None,
            select: None,
            job_cache: None,
        })
    }

    fn dirty(&self) -> bool {
        self.text != self.saved
    }

    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Side {
    #[default]
    Files,
    Outline,
    Git,
}

#[derive(Default)]
struct Find {
    open: bool,
    query: String,
    replace: String,
    case_sensitive: bool,
    focus: bool,
}

#[derive(Default)]
struct GitState {
    root: Option<PathBuf>,
    checked: bool,
    branch: String,
    files: Vec<FileStatus>,
    log: Vec<String>,
    last_refresh: Option<Instant>,
    message: String,
    job: Option<(String, Receiver<Result<String, String>>)>,
    output: String,
    diff: Option<String>,
}

struct Completion {
    /// Char index where the word being completed starts.
    start: usize,
    items: Vec<String>,
    selected: usize,
}

pub struct CodeEditor {
    pub open: bool,
    buffers: Vec<Buffer>,
    active: usize,
    side: Side,
    find: Find,
    goto: Option<String>,
    check: Option<Receiver<Vec<String>>>,
    check_started: Option<Instant>,
    diagnostics: Vec<BuildMessage>,
    pub format_on_save: bool,
    pub check_on_save: bool,
    font_size: f32,
    git: GitState,
    completion: Option<Completion>,
    status: String,
    confirm_close: Option<usize>,
    last_disk_check: Option<Instant>,
    /// Cursor (byte offset) and (line, column) in the active buffer, from the last frame.
    last_cursor_byte: Option<usize>,
    cursor: (usize, usize),
    force_completion: bool,
    /// Highlight log macro calls and show their latest runtime output at the end of the line.
    pub highlight_logs: bool,
    /// Live syntax errors per file, and when each buffer last changed (for debouncing).
    live: std::collections::HashMap<PathBuf, Vec<syntax::LiveDiag>>,
    checked: std::collections::HashMap<PathBuf, (u64, Instant)>,
    edits: std::collections::HashMap<PathBuf, (u64, Instant)>,
    lsp: Option<lsp::Lsp>,
    /// None = not checked yet.
    ra_available: Option<bool>,
    ra_install: Option<Receiver<Result<(), String>>>,
    /// Script log output by source location (from the console), refreshed every frame.
    runtime_logs: Vec<LogAt>,
}

/// The latest log message emitted from a source line.
#[derive(Clone, Debug)]
pub struct LogAt {
    pub file: String,
    pub line: u32,
    pub level: log::Level,
    pub message: String,
    pub count: u32,
}

impl Default for CodeEditor {
    fn default() -> Self {
        CodeEditor {
            open: false,
            buffers: Vec::new(),
            active: 0,
            side: Side::Files,
            find: Find::default(),
            goto: None,
            check: None,
            check_started: None,
            diagnostics: Vec::new(),
            format_on_save: true,
            check_on_save: true,
            font_size: 13.0,
            git: GitState::default(),
            completion: None,
            status: String::new(),
            confirm_close: None,
            last_disk_check: None,
            last_cursor_byte: None,
            cursor: (0, 0),
            force_completion: false,
            highlight_logs: true,
            live: Default::default(),
            checked: Default::default(),
            edits: Default::default(),
            lsp: None,
            ra_available: None,
            ra_install: None,
            runtime_logs: Vec::new(),
        }
    }
}

/// Byte offset of a char index.
fn byte_of(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map_or(s.len(), |(b, _)| b)
}

fn char_of(s: &str, bi: usize) -> usize {
    s[..bi.min(s.len())].chars().count()
}

/// (line, column), both 0-based, of a char index.
fn line_col(s: &str, ci: usize) -> (usize, usize) {
    let b = byte_of(s, ci);
    let before = &s[..b];
    let line = before.matches('\n').count();
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count());
    (line, col)
}

/// Char index of the start of a 0-based line.
fn line_start(s: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    let mut n = 0;
    for (ci, ch) in s.chars().enumerate() {
        if ch == '\n' {
            n += 1;
            if n == line {
                return ci + 1;
            }
        }
    }
    s.chars().count()
}

impl CodeEditor {
    pub fn has_unsaved(&self) -> bool {
        self.buffers.iter().any(|b| b.dirty())
    }

    pub fn open_file(&mut self, path: &Path, line: Option<u32>) {
        self.open = true;
        let path = dumb_runtime::strip_unc(&path.canonicalize().unwrap_or(path.to_path_buf()));
        let i = match self.buffers.iter().position(|b| b.path == path) {
            Some(i) => i,
            None => match Buffer::load(&path) {
                Ok(b) => {
                    self.buffers.push(b);
                    let i = self.buffers.len() - 1;
                    // Start at the top (the text edit would otherwise scroll to its default cursor).
                    self.buffers[i].select = Some((0, 0));
                    self.buffers[i].scroll_to_line = Some(0);
                    self.refresh_changes(i);
                    i
                }
                Err(e) => {
                    self.status = e;
                    return;
                }
            },
        };
        self.active = i;
        if let Some(l) = line {
            let b = &mut self.buffers[i];
            let l = (l as usize).saturating_sub(1);
            b.scroll_to_line = Some(l);
            let s = line_start(&b.text, l);
            b.select = Some((s, s));
        }
    }

    fn save(&mut self, i: usize) {
        if self.format_on_save && self.buffers[i].is_rust {
            let _ = self.format(i);
        }
        let b = &mut self.buffers[i];
        let text = if cfg!(windows) && b.saved.contains('\r') { b.text.replace('\n', "\r\n") } else { b.text.clone() };
        match std::fs::write(&b.path, text) {
            Ok(()) => {
                b.saved = b.text.clone();
                b.mtime = mtime(&b.path);
                b.changed_on_disk = false;
                self.status = format!("Saved {}", b.name());
                let is_rust = b.is_rust;
                self.refresh_changes(i);
                self.git.last_refresh = None;
                if let (Some(l), true) = (&self.lsp, is_rust) {
                    l.did_save(&self.buffers[i].path);
                }
                if self.check_on_save && is_rust {
                    self.start_check();
                }
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    /// Run rustfmt on a buffer. Keeps the cursor on the same line.
    fn format(&mut self, i: usize) -> Result<(), String> {
        let b = &mut self.buffers[i];
        let mut cmd = std::process::Command::new("rustfmt");
        cmd.args(["--edition", "2021", "--emit", "stdout", "--quiet"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().map_err(|e| format!("rustfmt not found ({e}); install it with `rustup component add rustfmt`"))?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(b.text.as_bytes()).map_err(|e| e.to_string())?;
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let first = err.lines().find(|l| l.contains("error")).unwrap_or("syntax error").to_string();
            self.status = format!("rustfmt: {first}");
            return Err(first);
        }
        let formatted = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        if formatted != b.text {
            b.text = formatted;
            b.job_cache = None;
        }
        self.status = "Formatted with rustfmt".into();
        Ok(())
    }

    fn start_check(&mut self) {
        if self.check.is_some() {
            return;
        }
        let Some(dir) = self.crate_dir() else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new("cargo");
            cmd.args(["check", "--message-format=short"]).current_dir(&dir);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000);
            }
            let lines = match cmd.output() {
                Ok(o) => String::from_utf8_lossy(&o.stderr).lines().map(str::to_string).collect(),
                Err(e) => vec![format!("error: cargo: {e}")],
            };
            let _ = tx.send(lines);
        });
        self.check = Some(rx);
        self.check_started = Some(Instant::now());
    }

    /// The crate (folder with Cargo.toml) containing the active file.
    fn crate_dir(&self) -> Option<PathBuf> {
        let b = self.buffers.get(self.active)?;
        let mut d = b.path.parent();
        while let Some(p) = d {
            if p.join("Cargo.toml").exists() {
                return Some(p.to_path_buf());
            }
            d = p.parent();
        }
        None
    }

    fn refresh_changes(&mut self, i: usize) {
        let Some(b) = self.buffers.get_mut(i) else { return };
        if !self.git.checked {
            self.git.root = b.path.parent().and_then(git::repo_root);
            self.git.checked = true;
        }
        b.changes = match &self.git.root {
            Some(r) => git::line_changes(r, &b.path),
            None => Vec::new(),
        };
    }

    fn refresh_git(&mut self) {
        let Some(root) = self.git.root.clone() else { return };
        self.git.branch = git::branch(&root);
        self.git.files = git::status(&root).unwrap_or_default();
        self.git.log = git::log(&root, 12);
        self.git.last_refresh = Some(Instant::now());
    }

    fn git_job(&mut self, title: &str, args: Vec<String>) {
        let Some(root) = self.git.root.clone() else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = String::new();
            // `args` may hold several commands separated by "&&".
            for cmd in args.split(|a| a == "&&") {
                let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
                match git::run(&root, &refs) {
                    Ok(s) => out.push_str(&s),
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                }
            }
            let _ = tx.send(Ok(out));
        });
        self.git.job = Some((title.to_string(), rx));
    }

    fn poll(&mut self, host_messages: &[BuildMessage]) {
        if let Some(rx) = &self.check {
            if let Ok(lines) = rx.try_recv() {
                let dir = self.crate_dir().unwrap_or_default();
                self.diagnostics = parse_build_messages(&lines, &dir);
                let (e, w) = count(&self.diagnostics);
                self.status = format!("cargo check: {e} error(s), {w} warning(s) in {:.1}s", self.check_started.map_or(0.0, |t| t.elapsed().as_secs_f32()));
                self.check = None;
            }
        }
        // Builds started elsewhere (build on save) also report problems.
        for m in host_messages {
            if !self.diagnostics.iter().any(|d| d.file == m.file && d.line == m.line && d.text == m.text) && self.check.is_none() && self.check_started.is_none() {
                self.diagnostics.push(m.clone());
            }
        }
        if let Some((title, rx)) = &self.git.job {
            if let Ok(r) = rx.try_recv() {
                self.git.output = match r {
                    Ok(s) => format!("{title}: ok\n{}", s.trim()),
                    Err(e) => format!("{title} failed:\n{e}"),
                };
                self.git.job = None;
                self.git.last_refresh = None;
                for i in 0..self.buffers.len() {
                    self.refresh_changes(i);
                }
            }
        }
        // Files changed by other programs.
        if self.last_disk_check.is_none_or(|t| t.elapsed().as_secs_f32() > 1.0) {
            self.last_disk_check = Some(Instant::now());
            for b in &mut self.buffers {
                let m = mtime(&b.path);
                if m != b.mtime {
                    b.mtime = m;
                    if !b.dirty() {
                        if let Ok(t) = std::fs::read_to_string(&b.path) {
                            b.text = t.replace("\r\n", "\n");
                            b.saved = b.text.clone();
                            b.job_cache = None;
                        }
                    } else {
                        b.changed_on_disk = true;
                    }
                }
            }
        }
    }

    /// Draw the editor window. `script_dir` is the scripts crate (for the file list).
    pub fn ui(&mut self, ctx: &egui::Context, script_dir: &Path, host_messages: &[BuildMessage], logs: Vec<LogAt>) {
        self.runtime_logs = logs;
        if let Ok(mut q) = OPEN_REQUESTS.lock() {
            for (p, l) in q.drain(..) {
                self.open_file(&p, l);
            }
        }
        if !self.open {
            return;
        }
        self.poll(host_messages);
        self.live_checks(ctx);
        if self.check.is_some() || self.git.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        if self.git.root.is_some() && self.git.last_refresh.is_none_or(|t| t.elapsed().as_secs_f32() > 5.0) {
            self.refresh_git();
        }
        if !self.git.checked {
            self.git.root = git::repo_root(script_dir);
            self.git.checked = true;
        }

        let title = match self.buffers.get(self.active) {
            Some(b) => format!("📝 Code — {}{}", b.name(), if b.dirty() { " •" } else { "" }),
            None => "📝 Code".to_string(),
        };
        let mut open = self.open;
        egui::Window::new(title)
            .id(egui::Id::new("code_editor"))
            .open(&mut open)
            .default_size([1000.0, 680.0])
            .min_size([500.0, 300.0])
            .resizable(true)
            .collapsible(true)
            .show(ctx, |ui| {
                self.shortcuts(ui);
                self.toolbar(ui);
                egui::Panel::bottom("code_status").exact_size(22.0).show(ui, |ui| self.status_bar(ui));
                if !self.all_problems().is_empty() {
                    egui::Panel::bottom("code_problems").resizable(true).default_size(110.0).size_range(40.0..=400.0).show(ui, |ui| self.problems(ui));
                }
                egui::Panel::left("code_side").resizable(true).default_size(200.0).size_range(120.0..=420.0).show(ui, |ui| self.side_panel(ui, script_dir));
                egui::CentralPanel::default().show(ui, |ui| {
                    self.tabs(ui);
                    if self.find.open {
                        self.find_bar(ui);
                    }
                    if self.buffers.is_empty() {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.weak("Open a script from the list on the left,\nor double-click one in the Scripts panel.");
                        });
                        return;
                    }
                    self.disk_banner(ui);
                    self.editor(ui);
                });
            });
        if let Some(i) = self.confirm_close {
            let mut close = false;
            let mut keep = false;
            egui::Modal::new(egui::Id::new("code_confirm_close")).show(ctx, |ui| {
                ui.label(format!("{} has unsaved changes.", self.buffers.get(i).map(|b| b.name()).unwrap_or_default()));
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.save(i);
                        close = true;
                    }
                    if ui.button("Discard").clicked() {
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = true;
                    }
                });
            });
            if close {
                self.close_buffer(i);
                self.confirm_close = None;
            } else if keep {
                self.confirm_close = None;
            }
        }
        self.open = open;
    }

    fn close_buffer(&mut self, i: usize) {
        if i < self.buffers.len() {
            self.buffers.remove(i);
            if self.active >= self.buffers.len() {
                self.active = self.buffers.len().saturating_sub(1);
            }
        }
    }

    fn shortcuts(&mut self, ui: &mut egui::Ui) {
        // Only while the pointer or focus is in this window.
        let ctrl = Modifiers::COMMAND;
        let consume = |ui: &mut egui::Ui, m: Modifiers, k: Key| ui.input_mut(|i| i.consume_key(m, k));
        if consume(ui, ctrl, Key::S) && !self.buffers.is_empty() {
            self.save(self.active);
        }
        if consume(ui, ctrl, Key::F) {
            self.find.open = true;
            self.find.focus = true;
            // Search for the selection.
            if let Some(sel) = self.selection_text(ui) {
                if !sel.contains('\n') && !sel.is_empty() {
                    self.find.query = sel;
                }
            }
        }
        if consume(ui, ctrl, Key::H) {
            self.find.open = true;
            self.find.focus = true;
        }
        if consume(ui, ctrl, Key::G) {
            self.goto = Some(String::new());
        }
        if consume(ui, ctrl | Modifiers::SHIFT, Key::I) && !self.buffers.is_empty() {
            let _ = self.format(self.active);
        }
        if consume(ui, Modifiers::NONE, Key::F3) {
            self.find_next(true);
        }
        if consume(ui, Modifiers::SHIFT, Key::F3) {
            self.find_next(false);
        }
        if consume(ui, Modifiers::NONE, Key::F7) {
            self.start_check();
        }
        if consume(ui, ctrl, Key::W) && !self.buffers.is_empty() {
            self.request_close(self.active);
        }
        if self.find.open && consume(ui, Modifiers::NONE, Key::Escape) {
            self.find.open = false;
        }
    }

    fn request_close(&mut self, i: usize) {
        if self.buffers[i].dirty() {
            self.confirm_close = Some(i);
        } else {
            self.close_buffer(i);
        }
    }

    fn editor_id(&self) -> egui::Id {
        egui::Id::new(("code_text", self.buffers.get(self.active).map(|b| b.path.clone())))
    }

    fn selection_text(&self, ui: &egui::Ui) -> Option<String> {
        let b = self.buffers.get(self.active)?;
        let st = egui::TextEdit::load_state(ui.ctx(), self.editor_id())?;
        let r = st.cursor.char_range()?.as_sorted_char_range();
        let (a, z) = (byte_of(&b.text, r.start.0), byte_of(&b.text, r.end.0));
        Some(b.text[a..z].to_string())
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let has = !self.buffers.is_empty();
            let dirty = self.buffers.get(self.active).is_some_and(|b| b.dirty());
            if ui.add_enabled(has && dirty, egui::Button::new("💾 Save")).on_hover_text("Ctrl+S").clicked() {
                self.save(self.active);
            }
            if ui.add_enabled(self.has_unsaved(), egui::Button::new("Save all")).clicked() {
                for i in 0..self.buffers.len() {
                    if self.buffers[i].dirty() {
                        self.save(i);
                    }
                }
            }
            ui.separator();
            if ui.add_enabled(has, egui::Button::new("✨ Format")).on_hover_text("rustfmt (Ctrl+Shift+I)").clicked() {
                let _ = self.format(self.active);
            }
            let checking = self.check.is_some();
            if ui.add_enabled(has && !checking, egui::Button::new(if checking { "⏳ Checking…" } else { "🔍 Check" })).on_hover_text("cargo check (F7)").clicked() {
                self.start_check();
            }
            if ui.button("🔎 Find").on_hover_text("Ctrl+F · Ctrl+H replace · F3 next").clicked() {
                self.find.open = !self.find.open;
                self.find.focus = self.find.open;
            }
            if ui.add_enabled(has, egui::Button::new("Go to line")).on_hover_text("Ctrl+G").clicked() {
                self.goto = Some(String::new());
            }
            ui.separator();
            ui.checkbox(&mut self.format_on_save, "Format on save");
            ui.checkbox(&mut self.check_on_save, "Check on save");
            ui.checkbox(&mut self.highlight_logs, "Highlight logs")
                .on_hover_text("Tint info!/warn!/error! calls by level and show their latest output while the game runs");
            ui.separator();
            if ui.small_button("A−").clicked() {
                self.font_size = (self.font_size - 1.0).max(8.0);
            }
            ui.weak(format!("{:.0}", self.font_size));
            if ui.small_button("A+").clicked() {
                self.font_size = (self.font_size + 1.0).min(32.0);
            }
            if let Some(b) = self.buffers.get(self.active) {
                if ui.small_button("Open externally").on_hover_text("Open in VS Code / the default app").clicked() {
                    crate::script_tools::open_externally(&b.path, None);
                }
            }
        });
        if let Some(g) = &mut self.goto {
            let mut go = None;
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.label("Go to line:");
                let r = ui.add(egui::TextEdit::singleline(g).desired_width(80.0));
                r.request_focus();
                if r.lost_focus() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    go = g.trim().parse::<usize>().ok();
                    cancel = true;
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    cancel = true;
                }
            });
            if let (Some(l), Some(b)) = (go, self.buffers.get_mut(self.active)) {
                let l = l.saturating_sub(1);
                b.scroll_to_line = Some(l);
                let s = line_start(&b.text, l);
                b.select = Some((s, s));
            }
            if cancel {
                self.goto = None;
            }
        }
        ui.separator();
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        egui::ScrollArea::horizontal().id_salt("code_tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, b) in self.buffers.iter().enumerate() {
                    let errs = self.diagnostics.iter().filter(|d| d.file == b.path && d.level == MessageLevel::Error).count();
                    let mut label = egui::RichText::new(format!("{}{}", b.name(), if b.dirty() { " •" } else { "" }));
                    if errs > 0 {
                        label = label.color(Color32::from_rgb(255, 120, 110));
                    }
                    let r = ui.add(egui::Button::selectable(self.active == i, label)).on_hover_text(b.path.display().to_string());
                    if r.clicked() {
                        self.active = i;
                    }
                    if r.middle_clicked() {
                        close = Some(i);
                    }
                    if ui.small_button("✖").clicked() {
                        close = Some(i);
                    }
                    ui.add_space(6.0);
                }
            });
        });
        if let Some(i) = close {
            self.request_close(i);
        }
    }

    fn disk_banner(&mut self, ui: &mut egui::Ui) {
        let Some(b) = self.buffers.get_mut(self.active) else { return };
        if !b.changed_on_disk {
            return;
        }
        ui.horizontal(|ui| {
            ui.colored_label(Color32::from_rgb(255, 190, 80), "This file changed on disk.");
            if ui.button("Reload").clicked() {
                if let Ok(t) = std::fs::read_to_string(&b.path) {
                    b.text = t.replace("\r\n", "\n");
                    b.saved = b.text.clone();
                    b.job_cache = None;
                }
                b.changed_on_disk = false;
            }
            if ui.button("Keep mine").clicked() {
                b.changed_on_disk = false;
            }
        });
    }

    fn find_bar(&mut self, ui: &mut egui::Ui) {
        let mut next = None;
        let mut replace_one = false;
        let mut replace_all = false;
        ui.horizontal(|ui| {
            ui.label("Find");
            let r = ui.add(egui::TextEdit::singleline(&mut self.find.query).desired_width(200.0).hint_text("text"));
            if self.find.focus {
                r.request_focus();
                self.find.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                next = Some(!ui.input(|i| i.modifiers.shift));
                r.request_focus();
            }
            if ui.small_button("▲").on_hover_text("Previous (Shift+F3)").clicked() {
                next = Some(false);
            }
            if ui.small_button("▼").on_hover_text("Next (F3)").clicked() {
                next = Some(true);
            }
            ui.checkbox(&mut self.find.case_sensitive, "Aa");
            let n = self.buffers.get(self.active).map_or(0, |b| self.matches(&b.text).len());
            ui.weak(if self.find.query.is_empty() { String::new() } else { format!("{n} match(es)") });
            ui.separator();
            ui.label("Replace");
            ui.add(egui::TextEdit::singleline(&mut self.find.replace).desired_width(160.0));
            replace_one = ui.small_button("Replace").clicked();
            replace_all = ui.small_button("All").clicked();
            if ui.small_button("✖").clicked() {
                self.find.open = false;
            }
        });
        if replace_all {
            if let Some(b) = self.buffers.get(self.active) {
                let ms = self.matches(&b.text);
                let mut t = b.text.clone();
                for r in ms.iter().rev() {
                    t.replace_range(r.clone(), &self.find.replace);
                }
                self.status = format!("Replaced {} occurrence(s)", ms.len());
                let b = &mut self.buffers[self.active];
                b.text = t;
            }
        } else if replace_one {
            // Replace the selected match, then go to the next one.
            if let Some(sel) = self.selection_text(ui) {
                let eq = if self.find.case_sensitive { sel == self.find.query } else { sel.to_lowercase() == self.find.query.to_lowercase() };
                if eq && !sel.is_empty() {
                    if let Some(st) = egui::TextEdit::load_state(ui.ctx(), self.editor_id()) {
                        if let Some(r) = st.cursor.char_range() {
                            let r = r.as_sorted_char_range();
                            let b = &mut self.buffers[self.active];
                            let (a, z) = (byte_of(&b.text, r.start.0), byte_of(&b.text, r.end.0));
                            b.text.replace_range(a..z, &self.find.replace);
                        }
                    }
                }
            }
            next = Some(true);
        }
        if let Some(fwd) = next {
            self.find_next(fwd);
        }
    }

    fn matches(&self, text: &str) -> Vec<std::ops::Range<usize>> {
        let q = &self.find.query;
        if q.is_empty() {
            return Vec::new();
        }
        if self.find.case_sensitive {
            text.match_indices(q.as_str()).map(|(i, m)| i..i + m.len()).collect()
        } else {
            // Lowercasing can change byte lengths for some scripts; only use it for ASCII text.
            let (t, ql) = (text.to_ascii_lowercase(), q.to_ascii_lowercase());
            t.match_indices(ql.as_str()).map(|(i, m)| i..i + m.len()).collect()
        }
    }

    fn find_next(&mut self, forward: bool) {
        let Some(b) = self.buffers.get(self.active) else { return };
        let ms = self.matches(&b.text);
        if ms.is_empty() {
            return;
        }
        let pos = self.last_cursor_byte.unwrap_or(0);
        let m = if forward {
            ms.iter().find(|r| r.start >= pos).or(ms.first())
        } else {
            ms.iter().rev().find(|r| r.end < pos).or(ms.last())
        }
        .cloned()
        .unwrap();
        let b = &mut self.buffers[self.active];
        let (a, z) = (char_of(&b.text, m.start), char_of(&b.text, m.end));
        b.select = Some((a, z));
        b.scroll_to_line = Some(b.text[..m.start].matches('\n').count());
        self.last_cursor_byte = Some(if forward { m.end } else { m.start });
    }
}

fn count(d: &[BuildMessage]) -> (usize, usize) {
    (d.iter().filter(|m| m.level == MessageLevel::Error).count(), d.iter().filter(|m| m.level == MessageLevel::Warning).count())
}

/// Words offered by completion besides the identifiers in open files.
const API_WORDS: &[&str] = &[
    "ScriptContext", "Transform", "Name", "MeshRenderer", "Camera", "Light", "Animator", "RigidBody", "Collider", "CharacterController", "Entity", "World",
    "Vec2", "Vec3", "Vec4", "Quat", "Mat4", "Color", "Key", "MouseButton", "Component", "Editor", "Reflect", "register_scripts", "query", "query_mut",
    "delta_seconds", "elapsed", "is_pressed", "just_pressed", "spawn", "despawn", "insert", "get", "get_mut", "translation", "rotation", "scale",
    "info", "warn", "error", "debug", "trace", "raycast", "overlap_sphere", "collision_events", "apply_impulse", "add_force", "move_velocity", "jump",
];

impl CodeEditor {
    fn editor(&mut self, ui: &mut egui::Ui) {
        let id = self.editor_id();
        let font = FontId::monospace(self.font_size);
        let pal = highlight::Palette::for_visuals(ui.visuals());
        let i = self.active;
        let ctx = ui.ctx().clone();

        // Pre-edit keyboard handling (before the TextEdit sees the keys).
        let focused = ctx.memory(|m| m.has_focus(id));
        if focused {
            self.pre_edit_keys(ui, id);
        }

        // Search hits are highlighted in the text.
        let marks: Vec<(std::ops::Range<usize>, Color32)> = if self.find.open {
            let b = &self.buffers[i];
            self.matches(&b.text).into_iter().map(|r| (r, Color32::from_rgba_unmultiplied(255, 200, 0, 60))).collect()
        } else {
            Vec::new()
        };

        let lines = self.buffers[i].text.lines().count().max(1) + usize::from(self.buffers[i].text.ends_with('\n'));
        let digits = lines.to_string().len().max(3);
        let char_w = ctx.fonts_mut(|f| f.glyph_width(&font, '0'));
        let gutter_w = char_w * digits as f32 + 22.0;

        // Diagnostics, log calls and their runtime output for this buffer.
        let all_diags = self.diags_for(i);
        let highlight_logs = self.highlight_logs && self.buffers[i].is_rust;
        let log_lines: Vec<(usize, log::Level)> = if highlight_logs {
            self.buffers[i].text.lines().enumerate().filter_map(|(n, l)| log_call_level(l).map(|lv| (n, lv))).collect()
        } else {
            Vec::new()
        };
        let runtime_logs = if highlight_logs { self.logs_for(i) } else { Vec::new() };

        let b = &mut self.buffers[i];
        let is_rust = b.is_rust;
        let mut cache = b.job_cache.take();
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap: f32| {
            let s = text.as_str();
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            s.hash(&mut h);
            marks.len().hash(&mut h);
            for (r, _) in &marks {
                r.start.hash(&mut h);
            }
            font.size.to_bits().hash(&mut h);
            ui.visuals().dark_mode.hash(&mut h);
            let key = h.finish();
            let job = match &cache {
                Some((k, j)) if *k == key => j.clone(),
                _ => {
                    let mut j = highlight::layout(s, font.clone(), &pal, &marks, is_rust);
                    j.wrap.max_width = f32::INFINITY;
                    cache = Some((key, j.clone()));
                    j
                }
            };
            ui.ctx().fonts_mut(|f| f.layout_job(job))
        };

        let scroll = egui::ScrollArea::both().id_salt(("code_scroll", b.path.clone())).auto_shrink([false, false]);
        let mut cursor_char = None;
        let mut scroll_line = b.scroll_to_line.take();
        let select = b.select.take();
        let changes = b.changes.clone();
        let mut gutter_click = None;
        let mut out_galley = None;

        scroll.show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (gutter, _) = ui.allocate_exact_size(egui::vec2(gutter_w, 1.0), egui::Sense::hover());
                if let Some((a, z)) = select {
                    let mut st = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
                    st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(z))));
                    st.store(&ctx, id);
                    ctx.memory_mut(|m| m.request_focus(id));
                }
                let output = egui::TextEdit::multiline(&mut b.text)
                    .id(id)
                    .font(font.clone())
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .desired_rows(30)
                    .frame(egui::Frame::NONE)
                    .margin(egui::Margin::symmetric(6, 2))
                    .layouter(&mut layouter)
                    .show(ui);
                cursor_char = output.cursor_range.map(|c| c.primary.index.0);

                // Gutter: line numbers, git change bars, diagnostics.
                let painter = ui.painter().clone();
                let top = output.galley_pos.y;
                let full = egui::Rect::from_min_max(egui::pos2(gutter.left(), top), egui::pos2(gutter.right(), top + output.galley.rect.height().max(ui.clip_rect().height())));
                painter.rect_filled(full, 0.0, ui.visuals().faint_bg_color);
                let cur_line = cursor_char.map(|c| line_col(&b.text, c).0);
                let clip = ui.clip_rect();
                let mut line = 0usize;
                let mut new_line = true;
                for row in &output.galley.rows {
                    let y0 = top + row.pos.y;
                    let h = row.rect().height().max(font.size);
                    if new_line && y0 + h >= clip.top() && y0 <= clip.bottom() {
                        let cur = cur_line == Some(line);
                        if cur {
                            painter.rect_filled(egui::Rect::from_min_size(egui::pos2(gutter.right(), y0), egui::vec2(clip.right() - gutter.right(), h)), 0.0, Color32::from_white_alpha(6));
                        }
                        let col = if cur { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
                        painter.text(egui::pos2(gutter.right() - 12.0, y0), egui::Align2::RIGHT_TOP, (line + 1).to_string(), font.clone(), col);
                        if let Some((_, c)) = changes.iter().find(|(l, _)| *l == line) {
                            let color = match c {
                                LineChange::Added => Color32::from_rgb(90, 180, 90),
                                LineChange::Modified => Color32::from_rgb(80, 140, 220),
                                LineChange::Deleted => Color32::from_rgb(220, 80, 80),
                            };
                            let r = if *c == LineChange::Deleted {
                                egui::Rect::from_min_size(egui::pos2(gutter.right() - 6.0, y0 - 2.0), egui::vec2(5.0, 4.0))
                            } else {
                                egui::Rect::from_min_size(egui::pos2(gutter.right() - 5.0, y0), egui::vec2(3.0, h))
                            };
                            painter.rect_filled(r, 1.0, color);
                        }
                        let text_left = output.galley_pos.x + row.rect().left();
                        let text_right = output.galley_pos.x + row.rect().right();
                        // Text after the end of the line (log output, error message).
                        let mut inline_x = text_right + 28.0;
                        let small = FontId::proportional((font.size - 1.0).max(9.0));

                        // Log calls: tinted by level, with their latest runtime output.
                        if let Some((_, lv)) = log_lines.iter().find(|(l, _)| *l == line) {
                            let c = log_color(*lv);
                            let r = egui::Rect::from_min_max(egui::pos2(text_left - 2.0, y0), egui::pos2(text_right + 4.0, y0 + h));
                            painter.rect_filled(r, 3.0, c.gamma_multiply(0.14));
                            painter.rect_filled(egui::Rect::from_min_size(egui::pos2(gutter.right() - 9.0, y0 + 2.0), egui::vec2(3.0, h - 4.0)), 1.0, c);
                            if let Some(out) = runtime_logs.iter().rev().find(|l| l.line as usize == line + 1) {
                                let txt = if out.count > 1 { format!("» {}  ×{}", one_line(&out.message), out.count) } else { format!("» {}", one_line(&out.message)) };
                                let g = painter.layout_no_wrap(txt, small.clone(), c);
                                let r = egui::Rect::from_min_size(egui::pos2(inline_x - 6.0, y0), egui::vec2(g.size().x + 12.0, h));
                                painter.rect_filled(r, 3.0, c.gamma_multiply(0.12));
                                painter.galley(egui::pos2(inline_x, y0 + (h - g.size().y) * 0.5), g.clone(), c);
                                inline_x += g.size().x + 24.0;
                            }
                        }

                        // Diagnostics: gutter dot, squiggle under the exact range, message inline.
                        let here: Vec<&syntax::LiveDiag> = all_diags.iter().filter(|d| d.line == line).collect();
                        if let Some(worst) = here.iter().find(|d| d.error).or(here.first()) {
                            let color = if worst.error { Color32::from_rgb(240, 80, 70) } else { Color32::from_rgb(230, 180, 60) };
                            painter.circle_filled(egui::pos2(gutter.left() + 6.0, y0 + h * 0.5), 3.5, color);
                            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(gutter.right(), y0), egui::pos2(clip.right(), y0 + h)), 0.0, color.gamma_multiply(0.07));
                            let ls = line_start(&b.text, line);
                            let line_chars = b.text.lines().nth(line).map_or(0, |s| s.chars().count());
                            for d in &here {
                                let c = if d.error { Color32::from_rgb(240, 80, 70) } else { Color32::from_rgb(230, 180, 60) };
                                let end_col = if d.end_line > d.line { line_chars } else { d.end_col.min(line_chars.max(d.col + 1)) };
                                let x0 = output.galley_pos.x + output.galley.pos_from_cursor(CCursor::new(ls + d.col.min(line_chars))).left();
                                let x1 = if end_col > line_chars { text_right + 6.0 } else { output.galley_pos.x + output.galley.pos_from_cursor(CCursor::new(ls + end_col)).left() };
                                squiggle(&painter, x0, x1.max(x0 + 6.0), y0 + h - 1.0, c);
                                let hr = egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1.max(x0 + 6.0), y0 + h));
                                ui.interact(hr, egui::Id::new(("squiggle", line, d.col)), egui::Sense::hover()).on_hover_text(format!("{}  ({})", d.text, d.source));
                            }
                            let msg = one_line(&worst.text);
                            let more = if here.len() > 1 { format!("  (+{})", here.len() - 1) } else { String::new() };
                            let g = painter.layout_no_wrap(format!("■ {msg}{more}"), small.clone(), color);
                            painter.galley(egui::pos2(inline_x, y0 + (h - g.size().y) * 0.5), g, color.gamma_multiply(0.9));
                            let hr = egui::Rect::from_min_size(egui::pos2(gutter.left(), y0), egui::vec2(gutter_w, h));
                            let resp = ui.interact(hr, egui::Id::new(("diag", line)), egui::Sense::click());
                            let text = here.iter().map(|d| format!("{} ({})", d.text, d.source)).collect::<Vec<_>>().join("\n");
                            if resp.on_hover_text(text).clicked() {
                                gutter_click = Some(line);
                            }
                        }
                    }
                    if row.ends_with_newline {
                        line += 1;
                        new_line = true;
                    } else {
                        new_line = false;
                    }
                }
                if let Some(l) = scroll_line.take() {
                    if let Some(row) = output.galley.rows.get(l) {
                        let r = egui::Rect::from_min_size(egui::pos2(output.galley_pos.x, top + row.pos.y), egui::vec2(10.0, row.rect().height()));
                        ui.scroll_to_rect(r, Some(egui::Align::Center));
                    }
                }
                out_galley = Some((output.galley.clone(), output.galley_pos));
            });
        });
        b.job_cache = cache;
        if let Some(l) = gutter_click {
            let s = line_start(&b.text, l);
            b.select = Some((s, s));
        }
        if let Some(c) = cursor_char {
            self.last_cursor_byte = Some(byte_of(&self.buffers[i].text, c));
            self.cursor = line_col(&self.buffers[i].text, c);
        }
        if focused {
            self.post_edit(ui, id, out_galley);
        } else {
            self.completion = None;
        }
    }

    /// Keys handled before the text edit: Tab/Shift+Tab indent, Ctrl+/ comments,
    /// completion navigation.
    fn pre_edit_keys(&mut self, ui: &mut egui::Ui, id: egui::Id) {
        let ctx = ui.ctx().clone();
        let Some(mut st) = egui::TextEdit::load_state(&ctx, id) else { return };
        let Some(range) = st.cursor.char_range() else { return };
        let consume = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        let b = &mut self.buffers[self.active];
        let r = range.as_sorted_char_range();
        let (a, z) = (r.start.0, r.end.0);

        // Completion popup navigation.
        if let Some(c) = &mut self.completion {
            if consume(Modifiers::NONE, Key::ArrowDown) {
                c.selected = (c.selected + 1) % c.items.len().max(1);
            }
            if consume(Modifiers::NONE, Key::ArrowUp) {
                c.selected = (c.selected + c.items.len().max(1) - 1) % c.items.len().max(1);
            }
            if consume(Modifiers::NONE, Key::Escape) {
                self.completion = None;
            } else if consume(Modifiers::NONE, Key::Tab) || consume(Modifiers::NONE, Key::Enter) {
                if let Some(word) = c.items.get(c.selected).cloned() {
                    let (sb, eb) = (byte_of(&b.text, c.start), byte_of(&b.text, a));
                    b.text.replace_range(sb..eb, &word);
                    let nc = c.start + word.chars().count();
                    st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(nc))));
                    st.store(&ctx, id);
                }
                self.completion = None;
                return;
            }
        }
        if consume(Modifiers::COMMAND, Key::Space) {
            self.completion = Some(Completion { start: a, items: Vec::new(), selected: 0 });
            self.force_completion = true;
        }

        let (l0, _) = line_col(&b.text, a);
        let (l1, c1) = line_col(&b.text, z);
        let l1 = if z > a && c1 == 0 { l1.saturating_sub(1) } else { l1 };
        let multi = l1 > l0;

        if consume(Modifiers::NONE, Key::Tab) {
            if multi {
                let n = edit_lines(&mut b.text, l0, l1, |l| format!("    {l}"));
                let s = line_start(&b.text, l0);
                st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(s), CCursor::new(s + n))));
            } else {
                let (sb, eb) = (byte_of(&b.text, a), byte_of(&b.text, z));
                b.text.replace_range(sb..eb, "    ");
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(a + 4))));
            }
            st.store(&ctx, id);
        } else if consume(Modifiers::SHIFT, Key::Tab) {
            let n = edit_lines(&mut b.text, l0, l1, |l| {
                let strip = l.len() - l.trim_start_matches(' ').len();
                l[strip.min(4)..].to_string()
            });
            let s = line_start(&b.text, l0);
            st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(s), CCursor::new(s + n))));
            st.store(&ctx, id);
        } else if consume(Modifiers::COMMAND, Key::Slash) {
            // Toggle `// ` on the selected lines.
            let all_commented = b.text.lines().skip(l0).take(l1 - l0 + 1).filter(|l| !l.trim().is_empty()).all(|l| l.trim_start().starts_with("//"));
            let n = edit_lines(&mut b.text, l0, l1, |l| {
                if l.trim().is_empty() {
                    l.to_string()
                } else if all_commented {
                    let ind = l.len() - l.trim_start().len();
                    let rest = &l[ind..];
                    let rest = rest.strip_prefix("// ").or_else(|| rest.strip_prefix("//")).unwrap_or(rest);
                    format!("{}{}", &l[..ind], rest)
                } else {
                    let ind = l.len() - l.trim_start().len();
                    format!("{}// {}", &l[..ind], &l[ind..])
                }
            });
            let s = line_start(&b.text, l0);
            st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(s), CCursor::new(s + n))));
            st.store(&ctx, id);
        } else if consume(Modifiers::COMMAND, Key::D) {
            // Duplicate line.
            let s = line_start(&b.text, l0);
            let e = line_start(&b.text, l1 + 1);
            let (sb, eb) = (byte_of(&b.text, s), byte_of(&b.text, e));
            let mut chunk = b.text[sb..eb].to_string();
            if !chunk.ends_with('\n') {
                chunk.insert(0, '\n');
                b.text.push_str(&chunk);
            } else {
                b.text.insert_str(eb, &chunk);
            }
            let n = chunk.chars().count();
            st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(a + n))));
            st.store(&ctx, id);
        }
    }

    /// After typing: auto-indent new lines, dedent `}`, and update completion.
    fn post_edit(&mut self, ui: &mut egui::Ui, id: egui::Id, galley: Option<(std::sync::Arc<egui::Galley>, egui::Pos2)>) {
        let ctx = ui.ctx().clone();
        let typed_enter = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key: Key::Enter, pressed: true, .. })));
        let typed: String = ctx.input(|i| i.events.iter().filter_map(|e| if let egui::Event::Text(t) = e { Some(t.clone()) } else { None }).collect());
        let Some(mut st) = egui::TextEdit::load_state(&ctx, id) else { return };
        let Some(range) = st.cursor.char_range() else { return };
        let c = range.primary.index.0;
        let b = &mut self.buffers[self.active];

        if typed_enter && self.completion.is_none() {
            let bc = byte_of(&b.text, c);
            if bc > 0 && b.text.as_bytes()[bc - 1] == b'\n' {
                let prev_start = b.text[..bc - 1].rfind('\n').map_or(0, |p| p + 1);
                let prev = &b.text[prev_start..bc - 1];
                let mut indent: String = prev.chars().take_while(|ch| *ch == ' ' || *ch == '\t').collect();
                let t = prev.trim_end();
                if t.ends_with('{') || t.ends_with('(') || t.ends_with('[') || t.ends_with("=>") {
                    indent.push_str("    ");
                }
                // Between braces: `{|}` -> put the `}` on its own line.
                let next_is_close = b.text[bc..].trim_start_matches([' ', '\t']).starts_with(['}', ')', ']']);
                let mut insert = indent.clone();
                if next_is_close && (t.ends_with('{') || t.ends_with('(') || t.ends_with('[')) {
                    let base: String = prev.chars().take_while(|ch| *ch == ' ' || *ch == '\t').collect();
                    insert = format!("{indent}\n{base}");
                    // Remove whitespace already before the closing bracket.
                    let ws = b.text[bc..].len() - b.text[bc..].trim_start_matches([' ', '\t']).len();
                    b.text.replace_range(bc..bc + ws, "");
                }
                b.text.insert_str(bc, &insert);
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(c + indent.chars().count()))));
                st.store(&ctx, id);
                return;
            }
        }
        if typed == "}" {
            // `    }` typed on an indented blank line: dedent one level.
            let bc = byte_of(&b.text, c);
            let ls = b.text[..bc].rfind('\n').map_or(0, |p| p + 1);
            let before = &b.text[ls..bc.saturating_sub(1)];
            if before.len() >= 4 && before.chars().all(|ch| ch == ' ') {
                b.text.replace_range(ls..ls + 4, "");
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(c - 4))));
                st.clone().store(&ctx, id);
            }
        }

        // Completion: the identifier being typed.
        let bc = byte_of(&b.text, c);
        let start_b = b.text[..bc].rfind(|ch: char| !(ch.is_alphanumeric() || ch == '_')).map_or(0, |p| p + 1);
        let prefix = b.text[start_b..bc].to_string();
        let typing = !typed.is_empty() && typed.chars().all(|ch| ch.is_alphanumeric() || ch == '_');
        if (typing && prefix.chars().count() >= 3) || std::mem::take(&mut self.force_completion) {
            let items = self.complete(&prefix);
            self.completion = if items.is_empty() { None } else { Some(Completion { start: char_of(&self.buffers[self.active].text, start_b), items, selected: 0 }) };
        } else if !typed.is_empty() && !typing {
            self.completion = None;
        }
        if let (Some(comp), Some((g, pos))) = (&self.completion, galley) {
            let r = g.pos_from_cursor(CCursor::new(c));
            let at = pos + r.left_bottom().to_vec2();
            let mut pick = None;
            egui::Area::new(egui::Id::new("code_completion")).order(egui::Order::Foreground).fixed_pos(at).show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(180.0);
                    for (k, it) in comp.items.iter().enumerate().take(10) {
                        let r = ui.add(egui::Button::selectable(k == comp.selected, egui::RichText::new(it).monospace()));
                        if r.clicked() {
                            pick = Some(k);
                        }
                    }
                    ui.weak("Tab/Enter to insert · Esc");
                });
            });
            if let Some(k) = pick {
                let word = comp.items[k].clone();
                let start = comp.start;
                let b = &mut self.buffers[self.active];
                let (sb, eb) = (byte_of(&b.text, start), byte_of(&b.text, c));
                b.text.replace_range(sb..eb, &word);
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(start + word.chars().count()))));
                st.store(&ctx, id);
                ctx.memory_mut(|m| m.request_focus(id));
                self.completion = None;
            }
        }
    }

    fn complete(&self, prefix: &str) -> Vec<String> {
        let mut words: BTreeSet<String> = BTreeSet::new();
        for b in &self.buffers {
            if !b.is_rust {
                continue;
            }
            for (r, t) in highlight::tokenize(&b.text) {
                if matches!(t, highlight::Tok::Plain | highlight::Tok::Type | highlight::Tok::Function | highlight::Tok::Macro) {
                    let w = b.text[r].trim_end_matches('!');
                    if w.len() >= 3 && w.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') && w.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        words.insert(w.to_string());
                    }
                }
            }
        }
        for w in API_WORDS {
            words.insert(w.to_string());
        }
        for w in ["impl", "struct", "enum", "match", "while", "return", "continue", "break", "where", "trait", "const", "static", "unsafe", "Option", "Result", "Some", "None", "Ok", "Err", "String", "Vec", "Default"] {
            words.insert(w.to_string());
        }
        let lower = prefix.to_lowercase();
        let mut out: Vec<String> = words.into_iter().filter(|w| w != prefix && w.to_lowercase().starts_with(&lower)).collect();
        out.sort_by_key(|w| (!w.starts_with(prefix), w.len()));
        out.truncate(30);
        out
    }

    fn side_panel(&mut self, ui: &mut egui::Ui, script_dir: &Path) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.side, Side::Files, "Files");
            ui.selectable_value(&mut self.side, Side::Outline, "Outline");
            let changed = self.git.files.len();
            ui.selectable_value(&mut self.side, Side::Git, if changed > 0 { format!("Git ({changed})") } else { "Git".into() });
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("code_side_scroll").auto_shrink([false, false]).show(ui, |ui| match self.side {
            Side::Files => self.files_tab(ui, script_dir),
            Side::Outline => self.outline_tab(ui),
            Side::Git => self.git_tab(ui),
        });
    }

    fn files_tab(&mut self, ui: &mut egui::Ui, script_dir: &Path) {
        let mut files = Vec::new();
        for name in ["Cargo.toml", "build.rs"] {
            let p = script_dir.join(name);
            if p.exists() {
                files.push(p);
            }
        }
        let mut src: Vec<PathBuf> = walk(&script_dir.join("src"));
        src.sort();
        if src.is_empty() && files.is_empty() {
            ui.weak("No scripts crate yet. Create a script from the Scripts panel.");
            return;
        }
        let mut open = None;
        for p in src.iter().chain(files.iter()) {
            let rel = p.strip_prefix(script_dir).unwrap_or(p).to_string_lossy().replace('\\', "/");
            let st = self.git.files.iter().find(|f| same_path(&f.path, p)).map(|f| f.letter());
            let errs = self.diagnostics.iter().filter(|d| same_path(&d.file, p) && d.level == MessageLevel::Error).count();
            let warns = self.diagnostics.iter().filter(|d| same_path(&d.file, p) && d.level == MessageLevel::Warning).count();
            let active = self.buffers.get(self.active).is_some_and(|b| same_path(&b.path, p));
            ui.horizontal(|ui| {
                let mut text = egui::RichText::new(format!("{} {rel}", if p.extension().is_some_and(|e| e == "rs") { "📄" } else { "⚙" }));
                if errs > 0 {
                    text = text.color(Color32::from_rgb(255, 120, 110));
                } else if warns > 0 {
                    text = text.color(Color32::from_rgb(230, 190, 90));
                }
                if ui.add(egui::Button::selectable(active, text)).clicked() {
                    open = Some(p.clone());
                }
                if let Some(s) = st {
                    let c = match s {
                        "U" | "A" => Color32::from_rgb(110, 200, 110),
                        "D" => Color32::from_rgb(220, 90, 90),
                        _ => Color32::from_rgb(220, 180, 90),
                    };
                    ui.colored_label(c, s).on_hover_text("git status");
                }
            });
        }
        if let Some(p) = open {
            self.open_file(&p, None);
        }
    }

    fn outline_tab(&mut self, ui: &mut egui::Ui) {
        let Some(b) = self.buffers.get(self.active) else {
            ui.weak("No file open.");
            return;
        };
        let items = outline(&b.text);
        if items.is_empty() {
            ui.weak("No items.");
        }
        let mut go = None;
        for (line, depth, kind, name) in items {
            ui.horizontal(|ui| {
                ui.add_space(depth as f32 * 12.0);
                ui.weak(kind);
                if ui.link(name).clicked() {
                    go = Some(line);
                }
            });
        }
        if let Some(l) = go {
            let b = &mut self.buffers[self.active];
            b.scroll_to_line = Some(l);
            let s = line_start(&b.text, l);
            b.select = Some((s, s));
        }
    }

    fn git_tab(&mut self, ui: &mut egui::Ui) {
        let Some(root) = self.git.root.clone() else {
            ui.weak("This project is not in a git repository.");
            let dir = self.crate_dir().and_then(|d| d.parent().map(|p| p.to_path_buf()));
            if let Some(dir) = dir {
                if ui.button("Initialize repository").on_hover_text(format!("git init in {}", dir.display())).clicked() {
                    match git::run(&dir, &["init"]) {
                        Ok(_) => {
                            let ignore = dir.join(".gitignore");
                            if !ignore.exists() {
                                let _ = std::fs::write(ignore, "Library/\nBuilds/\nScripts/target/\n*.blend1\n");
                            }
                            self.git.root = git::repo_root(&dir);
                            self.git.last_refresh = None;
                            self.git.output = "Initialized a repository".into();
                        }
                        Err(e) => self.git.output = e,
                    }
                }
            }
            if !self.git.output.is_empty() {
                ui.weak(&self.git.output);
            }
            return;
        };
        ui.horizontal(|ui| {
            ui.strong(format!("⎇ {}", self.git.branch)).on_hover_text(root.display().to_string());
            if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                self.git.last_refresh = None;
            }
        });
        let busy = self.git.job.is_some();
        ui.add(egui::TextEdit::multiline(&mut self.git.message).hint_text("Commit message").desired_rows(2).desired_width(f32::INFINITY));
        ui.horizontal_wrapped(|ui| {
            let can_commit = !busy && !self.git.message.trim().is_empty() && !self.git.files.is_empty();
            if ui.add_enabled(can_commit, egui::Button::new("✔ Commit all")).on_hover_text("git add -A, then commit").clicked() {
                if self.has_unsaved() {
                    for i in 0..self.buffers.len() {
                        if self.buffers[i].dirty() {
                            self.save(i);
                        }
                    }
                }
                let msg = std::mem::take(&mut self.git.message);
                self.git_job("commit", vec!["add".into(), "-A".into(), "&&".into(), "commit".into(), "-m".into(), msg]);
            }
            if ui.add_enabled(!busy, egui::Button::new("⬇ Pull")).clicked() {
                self.git_job("pull", vec!["pull".into(), "--ff-only".into()]);
            }
            if ui.add_enabled(!busy, egui::Button::new("⬆ Push")).clicked() {
                self.git_job("push", vec!["push".into()]);
            }
            if ui.button("Diff").on_hover_text("Changes in all files vs the last commit").clicked() {
                self.git.diff = Some(git::diff_text(&root, None));
            }
            if busy {
                ui.spinner();
            }
        });
        if !self.git.output.is_empty() {
            ui.label(egui::RichText::new(&self.git.output).small().weak());
        }
        ui.separator();
        ui.strong(format!("Changes ({})", self.git.files.len()));
        let mut open = None;
        let mut diff_of = None;
        for f in &self.git.files {
            let rel = f.path.strip_prefix(&root).unwrap_or(&f.path).to_string_lossy().replace('\\', "/");
            ui.horizontal(|ui| {
                ui.monospace(f.letter());
                let r = ui.add(egui::Label::new(rel).truncate().sense(egui::Sense::click()));
                if r.double_clicked() && f.path.extension().is_some_and(|e| e == "rs" || e == "toml") {
                    open = Some(f.path.clone());
                }
                r.context_menu(|ui| {
                    if ui.button("Show diff").clicked() {
                        diff_of = Some(f.path.clone());
                        ui.close();
                    }
                    if f.code.trim() != "??" && ui.button("Discard changes").clicked() {
                        let rel = f.path.strip_prefix(&root).unwrap_or(&f.path).to_string_lossy().replace('\\', "/");
                        let _ = git::run(&root, &["checkout", "--", &rel]);
                        ui.close();
                    }
                });
            });
        }
        if let Some(p) = open {
            self.open_file(&p, None);
        }
        if let Some(p) = diff_of {
            self.git.diff = Some(git::diff_text(&root, Some(&p)));
            self.git.last_refresh = None;
        }
        ui.separator();
        ui.strong("History");
        for l in &self.git.log {
            ui.label(egui::RichText::new(l).small().monospace());
        }
        if let Some(d) = &self.git.diff {
            let mut keep = true;
            egui::Window::new("Diff").open(&mut keep).default_size([700.0, 500.0]).show(ui.ctx(), |ui| {
                egui::ScrollArea::both().show(ui, |ui| {
                    if d.trim().is_empty() {
                        ui.weak("No changes.");
                    }
                    for l in d.lines() {
                        let c = if l.starts_with('+') && !l.starts_with("+++") {
                            Color32::from_rgb(120, 210, 120)
                        } else if l.starts_with('-') && !l.starts_with("---") {
                            Color32::from_rgb(230, 110, 100)
                        } else if l.starts_with("@@") {
                            Color32::from_rgb(110, 160, 230)
                        } else {
                            ui.visuals().weak_text_color()
                        };
                        ui.label(egui::RichText::new(l).monospace().color(c));
                    }
                });
            });
            if !keep {
                self.git.diff = None;
            }
        }
    }

    /// Every problem: live ones for open files (syntax, rust-analyzer, cargo) and cargo's for the rest.
    fn all_problems(&self) -> Vec<(PathBuf, syntax::LiveDiag)> {
        let mut out: Vec<(PathBuf, syntax::LiveDiag)> = Vec::new();
        for i in 0..self.buffers.len() {
            for d in self.diags_for(i) {
                out.push((self.buffers[i].path.clone(), d));
            }
        }
        for d in &self.diagnostics {
            if !self.buffers.iter().any(|b| same_path(&b.path, &d.file)) {
                let line = (d.line as usize).saturating_sub(1);
                let col = (d.column as usize).saturating_sub(1);
                out.push((d.file.clone(), syntax::LiveDiag { line, col, end_line: line, end_col: col + 1, error: d.level == MessageLevel::Error, text: d.text.clone(), source: "cargo" }));
            }
        }
        out
    }

    fn problems(&mut self, ui: &mut egui::Ui) {
        let all = self.all_problems();
        let e = all.iter().filter(|(_, d)| d.error).count();
        ui.horizontal(|ui| {
            ui.strong(format!("Problems: {e} error(s), {} warning(s)", all.len() - e));
            if ui.small_button("Clear build output").clicked() {
                self.diagnostics.clear();
            }
        });
        let mut go = None;
        egui::ScrollArea::vertical().id_salt("code_problems_scroll").auto_shrink([false, false]).show(ui, |ui| {
            for (file, d) in &all {
                let (icon, c) = if d.error { ("✖", Color32::from_rgb(240, 90, 80)) } else { ("⚠", Color32::from_rgb(230, 180, 60)) };
                let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let text = format!("{icon} {name}:{}:{}  {}   [{}]", d.line + 1, d.col + 1, one_line(&d.text), d.source);
                let r = ui.add(egui::Label::new(egui::RichText::new(text).color(c).monospace().size(11.5)).truncate().sense(egui::Sense::click()));
                if r.on_hover_text(&d.text).clicked() {
                    go = Some((file.clone(), d.line, d.col));
                }
            }
        });
        if let Some((f, l, c)) = go {
            self.open_file(&f, Some(l as u32 + 1));
            if let Some(b) = self.buffers.get_mut(self.active) {
                let s = line_start(&b.text, l) + c;
                b.select = Some((s, s));
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            if let Some(b) = self.buffers.get(self.active) {
                let lines = b.text.lines().count();
                let (l, c) = self.cursor;
                ui.monospace(format!("Ln {}, Col {}", l + 1, c + 1));
                ui.separator();
                ui.weak(format!("{lines} lines · {} chars", b.text.chars().count()));
                ui.separator();
                ui.weak(if b.is_rust { "Rust" } else { "Text" });
                ui.weak("UTF-8 · LF · spaces: 4");
                if self.git.root.is_some() {
                    ui.separator();
                    ui.weak(format!("⎇ {}", self.git.branch));
                    let changed = b.changes.len();
                    if changed > 0 {
                        ui.weak(format!("{changed} changed line(s)"));
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.analyzer_status(ui);
                ui.separator();
                let (e, w) = count(&self.diagnostics);
                if e + w > 0 {
                    ui.colored_label(if e > 0 { Color32::from_rgb(240, 90, 80) } else { Color32::from_rgb(230, 180, 60) }, format!("✖ {e}  ⚠ {w}"));
                }
                if self.check.is_some() {
                    ui.spinner();
                }
                ui.weak(&self.status);
            });
        });
    }
}

/// Replace lines `l0..=l1` with `f(line)`. Returns the new char length of that block.
fn edit_lines(text: &mut String, l0: usize, l1: usize, f: impl Fn(&str) -> String) -> usize {
    let s = byte_of(text, line_start(text, l0));
    let e_char = line_start(text, l1 + 1);
    let mut e = byte_of(text, e_char);
    let had_nl = e > s && text.as_bytes().get(e - 1) == Some(&b'\n');
    if had_nl {
        e -= 1;
    }
    let block: Vec<String> = text[s..e].split('\n').map(&f).collect();
    let joined = block.join("\n");
    let n = joined.chars().count();
    text.replace_range(s..e, &joined);
    n
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else if p.extension().is_some_and(|x| x == "rs" || x == "toml" || x == "ron" || x == "wgsl" || x == "md") {
                out.push(p);
            }
        }
    }
    out
}

fn same_path(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| p.to_string_lossy().replace('\\', "/").trim_start_matches("//?/").to_ascii_lowercase();
    n(a) == n(b)
}

/// Items for the outline: (line, depth, kind, name).
pub fn outline(src: &str) -> Vec<(usize, usize, &'static str, String)> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut in_block_comment = false;
    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim();
        if in_block_comment {
            if line.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        if line.starts_with("/*") && !line.contains("*/") {
            in_block_comment = true;
            continue;
        }
        if line.starts_with("//") {
            continue;
        }
        let mut words = line.split(|c: char| c.is_whitespace() || c == '(' || c == '<' || c == '{' || c == ':' || c == ';').filter(|w| !w.is_empty());
        let mut kind = None;
        let mut name = None;
        while let Some(w) = words.next() {
            match w {
                "pub" | "pub(crate)" | "async" | "unsafe" | "const" | "extern" if kind.is_none() => {
                    if w == "const" {
                        if let Some(n) = words.clone().next() {
                            if n != "fn" {
                                kind = Some("const");
                                name = Some(n.to_string());
                                break;
                            }
                        }
                    }
                }
                "fn" | "struct" | "enum" | "trait" | "mod" | "type" | "static" | "macro_rules!" => {
                    kind = Some(match w {
                        "fn" => "fn",
                        "struct" => "struct",
                        "enum" => "enum",
                        "trait" => "trait",
                        "mod" => "mod",
                        "type" => "type",
                        "static" => "static",
                        _ => "macro",
                    });
                    name = words.next().map(|n| n.trim_end_matches('!').to_string());
                    break;
                }
                "impl" => {
                    kind = Some("impl");
                    let rest = line.split_once("impl").map(|x| x.1).unwrap_or("");
                    let rest = rest.split('{').next().unwrap_or("").trim();
                    // skip generic params `impl<T>`
                    let rest = if rest.starts_with('<') { rest.split_once('>').map(|x| x.1.trim()).unwrap_or(rest) } else { rest };
                    name = Some(rest.to_string());
                    break;
                }
                _ => break,
            }
        }
        if let (Some(k), Some(n)) = (kind, name) {
            if !n.is_empty() {
                out.push((i, depth.max(0) as usize, k, n));
            }
        }
        let opens = raw.matches('{').count() as i32;
        let closes = raw.matches('}').count() as i32;
        depth += opens - closes;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_items() {
        let src = "use x;\npub struct Foo {\n    a: u32,\n}\nimpl<T> Bar for Foo {\n    fn go(&self) {}\n    pub fn stop() {}\n}\nconst N: usize = 3;\n// fn commented()\nmacro_rules! m { () => {} }\n";
        let o = outline(src);
        let names: Vec<_> = o.iter().map(|(_, d, k, n)| format!("{d}{k}:{n}")).collect();
        assert_eq!(names, ["0struct:Foo", "0impl:Bar for Foo", "1fn:go", "1fn:stop", "0const:N", "0macro:m"]);
    }

    #[test]
    fn line_helpers() {
        let s = "ab\ncdé\n\nx";
        assert_eq!(line_start(s, 1), 3);
        assert_eq!(line_start(s, 3), 8);
        assert_eq!(line_col(s, 5), (1, 2));
        let mut t = "a\n  b\nc".to_string();
        edit_lines(&mut t, 0, 1, |l| format!("// {l}"));
        assert_eq!(t, "// a\n//   b\nc");
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;

    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, ed: &mut CodeEditor, dir: &Path) {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))), events, ..Default::default() };
        let out = ctx.run_ui(input, |ui| ed.ui(ui.ctx(), dir, &[], Vec::new()));
        let mut delta = out.textures_delta;
        delta.clear();
    }

    fn key(k: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn typing_indents_and_saves() {
        let dir = std::env::temp_dir().join(format!("dumb_code_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.rs");
        std::fs::write(&file, "fn a() {}").unwrap();
        let ctx = egui::Context::default();
        let mut ed = CodeEditor { format_on_save: false, check_on_save: false, ..Default::default() };
        ed.open_file(&file, None);
        ed.buffers[0].select = Some((8, 8)); // between the braces
        for _ in 0..3 {
            frame(&ctx, vec![], &mut ed, &dir);
        }
        ed.buffers[0].select = Some((8, 8));
        frame(&ctx, vec![], &mut ed, &dir);
        frame(&ctx, vec![key(Key::Enter, Modifiers::NONE)], &mut ed, &dir);
        assert_eq!(ed.buffers[0].text, "fn a() {\n    \n}", "auto-indent between braces");
        frame(&ctx, vec![egui::Event::Text("let x = 1;".into())], &mut ed, &dir);
        frame(&ctx, vec![key(Key::Tab, Modifiers::NONE)], &mut ed, &dir);
        assert_eq!(ed.buffers[0].text, "fn a() {\n    let x = 1;    \n}");
        frame(&ctx, vec![key(Key::Slash, Modifiers::COMMAND)], &mut ed, &dir);
        assert!(ed.buffers[0].text.contains("    // let x = 1;"), "{}", ed.buffers[0].text);
        assert!(ed.buffers[0].dirty());
        frame(&ctx, vec![key(Key::S, Modifiers::COMMAND)], &mut ed, &dir);
        assert!(!ed.buffers[0].dirty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), ed.buffers[0].text);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------------------------
// Live diagnostics (syntax check + rust-analyzer), inline messages and log highlights.

/// Wait this long after the last keystroke before re-checking (errors don't flicker while typing).
const CHECK_DELAY: f32 = 0.35;

impl CodeEditor {
    fn live_checks(&mut self, ctx: &egui::Context) {
        use std::hash::{Hash, Hasher};
        if let Some(rx) = &self.ra_install {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    self.status = "rust-analyzer installed".into();
                    self.ra_available = Some(true);
                    self.ra_install = None;
                }
                Ok(Err(e)) => {
                    self.status = format!("installing rust-analyzer failed: {e}");
                    self.ra_install = None;
                }
                Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(300)),
            }
        }
        // rust-analyzer: started once, for the crate of the first Rust file.
        if self.lsp.is_none() && self.buffers.iter().any(|b| b.is_rust) {
            if self.ra_available.is_none() {
                self.ra_available = Some(lsp::available());
            }
            if self.ra_available == Some(true) {
                if let Some(root) = self.crate_dir() {
                    match lsp::Lsp::start(&root) {
                        Ok(l) => self.lsp = Some(l),
                        Err(e) => {
                            self.status = e;
                            self.ra_available = Some(false);
                        }
                    }
                }
            }
        }
        if let Some(l) = &mut self.lsp {
            let bufs: Vec<(PathBuf, String)> = self.buffers.iter().map(|b| (b.path.clone(), b.text.clone())).collect();
            l.poll(&|p: &Path| bufs.iter().find(|(bp, _)| same_path(bp, p)).map(|(_, t)| t.clone()));
            if !l.ready || l.status != "ready" {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }

        // Re-check a buffer once typing has paused for CHECK_DELAY.
        let now = Instant::now();
        for b in &self.buffers {
            if !b.is_rust {
                continue;
            }
            let mut h = std::collections::hash_map::DefaultHasher::new();
            b.text.hash(&mut h);
            let h = h.finish();
            let entry = self.edits.entry(b.path.clone()).or_insert((h, now - std::time::Duration::from_secs(1)));
            if entry.0 != h {
                // Text changed this frame: restart the delay.
                *entry = (h, now);
            }
            let done = self.checked.get(&b.path).is_some_and(|(ch, _)| *ch == h);
            if done {
                continue;
            }
            if entry.1.elapsed().as_secs_f32() < CHECK_DELAY {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                continue;
            }
            self.live.insert(b.path.clone(), syntax::check(&b.text));
            if let Some(l) = &mut self.lsp {
                l.did_open_or_change(&b.path, &b.text);
            }
            self.checked.insert(b.path.clone(), (h, now));
        }
    }

    /// All diagnostics for a buffer: syntax, rust-analyzer and the last cargo check/build.
    fn diags_for(&self, i: usize) -> Vec<syntax::LiveDiag> {
        let b = &self.buffers[i];
        let mut v = self.live.get(&b.path).cloned().unwrap_or_default();
        if let Some(l) = &self.lsp {
            for d in l.diags_for(&b.path) {
                // rust-analyzer repeats syntax errors: keep one.
                if !v.iter().any(|x| x.line == d.line && x.col == d.col) {
                    v.push(d);
                }
            }
        }
        for d in self.diagnostics.iter().filter(|d| same_path(&d.file, &b.path)) {
            let line = (d.line as usize).saturating_sub(1);
            let col = (d.column as usize).saturating_sub(1);
            if v.iter().any(|x| x.line == line && x.text == d.text) {
                continue;
            }
            let end = b.text.lines().nth(line).map_or(col + 1, |s| col + s.chars().skip(col).take_while(|c| c.is_alphanumeric() || *c == '_').count().max(1));
            v.push(syntax::LiveDiag { line, col, end_line: line, end_col: end, error: d.level == MessageLevel::Error, text: d.text.clone(), source: "cargo" });
        }
        v.sort_by_key(|d| (d.line, d.col));
        v
    }

    /// Latest runtime log per line of a buffer.
    fn logs_for(&self, i: usize) -> Vec<LogAt> {
        let p = self.buffers[i].path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
        self.runtime_logs
            .iter()
            .filter(|l| {
                let f = l.file.replace('\\', "/").to_ascii_lowercase();
                let f = f.trim_start_matches("./");
                !f.is_empty() && (p.ends_with(&format!("/{f}")) || p == f)
            })
            .cloned()
            .collect()
    }

    fn analyzer_status(&mut self, ui: &mut egui::Ui) {
        match (&self.lsp, self.ra_available) {
            (Some(l), _) => {
                let ready = l.ready && l.status == "ready";
                let t = if ready { "rust-analyzer ✔".to_string() } else { format!("rust-analyzer: {}", l.status) };
                ui.weak(t).on_hover_text("Live errors and warnings while you type");
            }
            (None, Some(false)) => {
                if self.ra_install.is_some() {
                    ui.spinner();
                    ui.weak("installing rust-analyzer…");
                } else {
                    if ui
                        .small_button("Install rust-analyzer")
                        .on_hover_text("rustup component add rust-analyzer\nAdds type and name errors while you type (syntax errors already work)")
                        .clicked()
                    {
                        self.ra_install = Some(lsp::install());
                    }
                    ui.weak("syntax check");
                }
            }
            _ => {}
        }
    }
}

/// Level of a log macro call on this line (`info!(`, `log::warn!(`...).
pub fn log_call_level(line: &str) -> Option<log::Level> {
    if line.trim_start().starts_with("//") {
        return None;
    }
    for (name, lv) in [("error!", log::Level::Error), ("warn!", log::Level::Warn), ("info!", log::Level::Info), ("debug!", log::Level::Debug), ("trace!", log::Level::Trace)] {
        if let Some(i) = line.find(name) {
            let before = line[..i].chars().next_back();
            if before.is_none_or(|c| !(c.is_alphanumeric() || c == '_')) {
                return Some(lv);
            }
        }
    }
    None
}

pub fn log_color(l: log::Level) -> Color32 {
    match l {
        log::Level::Error => Color32::from_rgb(240, 90, 80),
        log::Level::Warn => Color32::from_rgb(235, 185, 70),
        log::Level::Info => Color32::from_rgb(110, 170, 240),
        log::Level::Debug => Color32::from_rgb(150, 150, 160),
        log::Level::Trace => Color32::from_rgb(120, 120, 130),
    }
}

/// Wavy underline from x0 to x1 at y.
fn squiggle(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    let mut pts = Vec::new();
    let mut x = x0;
    let mut up = true;
    while x <= x1.max(x0 + 4.0) {
        pts.push(egui::pos2(x, y + if up { -1.5 } else { 1.0 }));
        up = !up;
        x += 3.0;
    }
    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.2, color)));
}

#[cfg(test)]
mod log_tests {
    use super::*;

    #[test]
    fn finds_log_calls() {
        assert_eq!(log_call_level("    warn!(\"hello\")"), Some(log::Level::Warn));
        assert_eq!(log_call_level("    log::error!(\"x {}\", 1);"), Some(log::Level::Error));
        assert_eq!(log_call_level("    // info!(\"off\")"), None);
        assert_eq!(log_call_level("    my_info!(\"no\")"), None);
    }
}

/// First line of a message, shortened for inline display.
fn one_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("").trim();
    if l.chars().count() > 140 {
        format!("{}…", l.chars().take(140).collect::<String>())
    } else {
        l.to_string()
    }
}

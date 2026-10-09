//! Log capture and the console panel.
//!
//! Engine logs are captured at the level set by `RUST_LOG` (default Info). Script logs (target
//! `script::<module>`, forwarded from the script library) are always captured at every level;
//! the console's level toggles decide what is shown.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

const CAPACITY: usize = 5000;

pub struct LogLine {
    pub level: log::Level,
    pub target: String,
    pub message: String,
    pub time: SystemTime,
    /// Consecutive identical messages folded into this line.
    pub repeat: u32,
    /// Source location of the log macro (script logs).
    pub file: Option<String>,
    pub line: Option<u32>,
}

impl LogLine {
    pub fn is_script(&self) -> bool {
        self.target.starts_with("script")
    }

    /// Short source label: the script module (`player`) or the engine module (`database`).
    pub fn source(&self) -> &str {
        self.target.rsplit("::").next().unwrap_or(&self.target)
    }
}

#[derive(Clone)]
pub struct LogBuffer(pub Arc<Mutex<VecDeque<LogLine>>>);

struct Logger {
    buf: LogBuffer,
    filter: log::LevelFilter,
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        if m.target().starts_with("script") {
            return true;
        }
        m.level() <= self.filter && !noisy(m.target())
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let msg = r.args().to_string();
        eprintln!("[{:5}] {}: {}", r.level(), r.target(), msg);
        if let Ok(mut b) = self.buf.0.lock() {
            if let Some(last) = b.back_mut() {
                if last.message == msg && last.level == r.level() && last.target == r.target() {
                    last.repeat += 1;
                    last.time = SystemTime::now();
                    return;
                }
            }
            if b.len() >= CAPACITY {
                b.pop_front();
            }
            b.push_back(LogLine {
                level: r.level(),
                target: r.target().to_string(),
                message: msg,
                time: SystemTime::now(),
                repeat: 1,
                file: r.file().map(str::to_string),
                line: r.line(),
            });
        }
    }

    fn flush(&self) {}
}

fn noisy(target: &str) -> bool {
    ["naga", "winit", "egui", "gpu_allocator", "notify", "calloop", "sctk", "rapier", "parry"].iter().any(|n| target.starts_with(n))
}

impl LogBuffer {
    pub fn install() -> LogBuffer {
        let buf = LogBuffer(Arc::new(Mutex::new(VecDeque::new())));
        let filter = match std::env::var("RUST_LOG").ok().as_deref() {
            Some("debug") => log::LevelFilter::Debug,
            Some("trace") => log::LevelFilter::Trace,
            Some("warn") => log::LevelFilter::Warn,
            _ => log::LevelFilter::Info,
        };
        let logger = Box::leak(Box::new(Logger { buf: buf.clone(), filter }));
        let _ = log::set_logger(logger);
        // Scripts may log at any level; `Logger::enabled` filters engine targets.
        log::set_max_level(log::LevelFilter::Trace);
        buf
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceFilter {
    #[default]
    All,
    Engine,
    Scripts,
}

pub struct ConsoleState {
    pub filter: String,
    /// Shown levels, indexed by `log::Level as usize - 1` (Error..Trace).
    pub show: [bool; 5],
    pub source: SourceFilter,
    pub auto_scroll: bool,
}

impl Default for ConsoleState {
    fn default() -> Self {
        ConsoleState { filter: String::new(), show: [true, true, true, true, false], source: SourceFilter::All, auto_scroll: true }
    }
}

fn level_style(l: log::Level, ui: &egui::Ui) -> (&'static str, egui::Color32) {
    match l {
        log::Level::Error => ("⛔", egui::Color32::from_rgb(255, 110, 100)),
        log::Level::Warn => ("⚠", egui::Color32::from_rgb(240, 200, 90)),
        log::Level::Info => ("ℹ", ui.visuals().text_color()),
        log::Level::Debug => ("🔧", egui::Color32::from_rgb(140, 180, 230)),
        log::Level::Trace => ("·", egui::Color32::GRAY),
    }
}

pub fn console_ui(ui: &mut egui::Ui, buf: &LogBuffer, st: &mut ConsoleState) {
    let Ok(mut lines) = buf.0.lock() else { return };

    let mut counts = [0u32; 5];
    for l in lines.iter() {
        counts[l.level as usize - 1] += l.repeat;
    }

    ui.horizontal(|ui| {
        if ui.button("Clear").clicked() {
            lines.clear();
        }
        ui.separator();
        for (i, level) in [log::Level::Error, log::Level::Warn, log::Level::Info, log::Level::Debug, log::Level::Trace].into_iter().enumerate() {
            let (icon, color) = level_style(level, ui);
            let text = egui::RichText::new(format!("{icon} {level} {}", counts[i])).color(if st.show[i] { color } else { ui.visuals().weak_text_color() });
            ui.toggle_value(&mut st.show[i], text);
        }
        ui.separator();
        ui.selectable_value(&mut st.source, SourceFilter::All, "All");
        ui.selectable_value(&mut st.source, SourceFilter::Engine, "⚙ Engine");
        ui.selectable_value(&mut st.source, SourceFilter::Scripts, "📜 Scripts");
        ui.separator();
        ui.add(egui::TextEdit::singleline(&mut st.filter).hint_text("🔍 filter text or source").desired_width(180.0));
        ui.checkbox(&mut st.auto_scroll, "Auto-scroll");
    });
    ui.separator();

    let filter = st.filter.to_lowercase();
    let visible: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| st.show[l.level as usize - 1])
        .filter(|(_, l)| match st.source {
            SourceFilter::All => true,
            SourceFilter::Engine => !l.is_script(),
            SourceFilter::Scripts => l.is_script(),
        })
        .filter(|(_, l)| filter.is_empty() || l.message.to_lowercase().contains(&filter) || l.target.to_lowercase().contains(&filter))
        .map(|(i, _)| i)
        .collect();

    let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(st.auto_scroll).show_rows(ui, row_h, visible.len(), |ui, range| {
        for &i in &visible[range] {
            let l = &lines[i];
            let (icon, color) = level_style(l.level, ui);
            let secs = l.time.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs()) % 86400;
            ui.horizontal(|ui| {
                ui.set_height(row_h);
                ui.label(egui::RichText::new(format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)).monospace().weak());
                ui.label(egui::RichText::new(icon).color(color));
                let src = if l.is_script() { format!("📜 {}", l.source()) } else { l.source().to_string() };
                ui.label(egui::RichText::new(format!("[{src}]")).monospace().color(if l.is_script() { egui::Color32::from_rgb(150, 200, 140) } else { ui.visuals().weak_text_color() }));
                if l.repeat > 1 {
                    ui.label(egui::RichText::new(format!("×{}", l.repeat)).small().strong().background_color(ui.visuals().faint_bg_color));
                }
                let r = ui.add(egui::Label::new(egui::RichText::new(&l.message).monospace().color(color)).truncate().sense(egui::Sense::click()));
                r.on_hover_text(format!("{}\n\n{}\n(click to copy)", l.target, l.message)).clicked().then(|| ui.ctx().copy_text(l.message.clone()));
            });
        }
    });
}

impl LogBuffer {
    /// The latest script log per source line, with how many times that line logged
    /// (for showing output inline in the code editor).
    pub fn latest_by_location(&self) -> Vec<crate::code_editor::LogAt> {
        let Ok(b) = self.0.lock() else { return Vec::new() };
        let mut out: Vec<crate::code_editor::LogAt> = Vec::new();
        for l in b.iter().filter(|l| l.is_script()) {
            let (Some(file), Some(line)) = (&l.file, l.line) else { continue };
            match out.iter_mut().find(|o| o.line == line && &o.file == file) {
                Some(o) => {
                    o.message = l.message.clone();
                    o.level = l.level;
                    o.count += l.repeat;
                }
                None => out.push(crate::code_editor::LogAt { file: file.clone(), line, level: l.level, message: l.message.clone(), count: l.repeat }),
            }
        }
        out
    }
}

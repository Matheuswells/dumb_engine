//! Performance data for overlays and the profiler: system RAM/CPU (sampled), GPU time and VRAM,
//! and the on-screen performance overlay shared by the editor and the game.

use dumb_core::profiler;
use dumb_render::{RenderStats, Renderer};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Samples kept by the system monitor (one every `SAMPLE_EVERY`).
pub const SAMPLES: usize = 240;
pub const SAMPLE_EVERY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSample {
    /// Resident memory of this process (bytes).
    pub process_ram: u64,
    pub ram_used: u64,
    pub ram_total: u64,
    /// CPU used by this process, % of the whole machine.
    pub process_cpu: f32,
    /// CPU used by everything, %.
    pub system_cpu: f32,
    pub cores: u32,
    pub vram_used: u64,
    pub vram_budget: u64,
    pub engine_vram: u64,
    /// GPU time per frame (ms) and the share of the frame the GPU was busy with us (%).
    pub gpu_ms: f32,
    pub gpu_busy: f32,
}

pub struct SystemMonitor {
    sys: sysinfo::System,
    pid: Option<sysinfo::Pid>,
    last: Option<Instant>,
    pub sample: SystemSample,
    pub history: VecDeque<SystemSample>,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    pub fn new() -> Self {
        SystemMonitor { sys: sysinfo::System::new(), pid: sysinfo::get_current_pid().ok(), last: None, sample: SystemSample::default(), history: VecDeque::new() }
    }

    /// Refresh the numbers (cheap to call every frame: real work happens twice a second).
    pub fn update(&mut self, renderer: &Renderer) {
        if self.last.is_some_and(|t| t.elapsed() < SAMPLE_EVERY) {
            return;
        }
        self.last = Some(Instant::now());
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let cores = self.sys.cpus().len().max(1) as u32;
        let mut s = SystemSample {
            ram_used: self.sys.used_memory(),
            ram_total: self.sys.total_memory(),
            system_cpu: self.sys.global_cpu_usage(),
            cores,
            ..Default::default()
        };
        if let Some(pid) = self.pid {
            self.sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), false);
            if let Some(p) = self.sys.process(pid) {
                s.process_ram = p.memory();
                s.process_cpu = p.cpu_usage() / cores as f32;
            }
        }
        let (used, budget) = renderer.vram();
        s.vram_used = used;
        s.vram_budget = budget;
        s.engine_vram = renderer.engine_vram();
        let (frame, _) = profiler::stats(30, |f| Some(f.frame_ms));
        let (gpu, _) = profiler::stats(30, |f| f.gpu_ms);
        s.gpu_ms = gpu;
        s.gpu_busy = if frame > 0.0 { (gpu / frame * 100.0).min(100.0) } else { 0.0 };
        self.sample = s;
        if self.history.len() >= SAMPLES {
            self.history.pop_front();
        }
        self.history.push_back(s);
    }
}

/// What the on-screen overlay shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PerfOverlay {
    #[default]
    Off,
    /// FPS and frame time.
    Minimal,
    /// Graphs, CPU/GPU/RAM/VRAM, rendering counters and the top profiler scopes.
    Full,
}

impl PerfOverlay {
    /// F3 cycles Off → Minimal → Full.
    pub fn next(self) -> Self {
        match self {
            PerfOverlay::Off => PerfOverlay::Minimal,
            PerfOverlay::Minimal => PerfOverlay::Full,
            PerfOverlay::Full => PerfOverlay::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PerfOverlay::Off => "Off",
            PerfOverlay::Minimal => "FPS only",
            PerfOverlay::Full => "Full",
        }
    }
}

pub fn mb(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.2} GB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MB", b as f64 / (1u64 << 20) as f64)
    }
}

/// Extra numbers the caller knows about (entities, AI...).
#[derive(Clone, Debug, Default)]
pub struct OverlayExtras {
    pub entities: usize,
    pub lines: Vec<String>,
}

/// Draw the overlay in the top-left corner of `rect`.
pub fn overlay(ui: &mut egui::Ui, rect: egui::Rect, mode: PerfOverlay, mon: &SystemMonitor, rs: &RenderStats, extras: &OverlayExtras) {
    if mode == PerfOverlay::Off {
        return;
    }
    let history = profiler::history();
    let (avg, max) = profiler::stats(60, |f| Some(f.frame_ms));
    let fps = if avg > 0.0 { 1000.0 / avg } else { 0.0 };
    let fps_color = if fps >= 55.0 {
        egui::Color32::from_rgb(120, 220, 130)
    } else if fps >= 28.0 {
        egui::Color32::from_rgb(235, 200, 90)
    } else {
        egui::Color32::from_rgb(240, 100, 90)
    };
    // Drawn in the view's own layer, so windows stay on top of it.
    let inner = egui::Rect::from_min_max(rect.left_top() + egui::vec2(10.0, 10.0), rect.right_bottom());
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(12, 14, 18, 200))
                .corner_radius(6.0)
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(18)))
                .inner_margin(egui::Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    let mono = |s: String| egui::RichText::new(s).monospace().size(11.5).color(egui::Color32::from_gray(215));
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("{fps:.0} FPS")).monospace().size(14.0).strong().color(fps_color));
                        ui.label(mono(format!("{avg:.2} ms  (max {max:.1})")));
                    });
                    if mode == PerfOverlay::Minimal {
                        return;
                    }
                    ui.set_min_width(250.0);
                    frame_graph(ui, &history, 250.0, 42.0);
                    let s = &mon.sample;
                    let cpu_ms = history.last().map_or(0.0, |f| (f.frame_ms - f.time_of("frame cap wait") - f.time_of("gpu wait (vsync)")).max(0.0));
                    ui.label(mono(format!("CPU  {cpu_ms:5.2} ms   {:4.0}% proc  {:3.0}% sys", s.process_cpu, s.system_cpu)));
                    if s.gpu_ms > 0.0 {
                        ui.label(mono(format!("GPU  {:5.2} ms   {:4.0}% busy", s.gpu_ms, s.gpu_busy)));
                    }
                    ui.label(mono(format!("RAM  {}  (system {} / {})", mb(s.process_ram), mb(s.ram_used), mb(s.ram_total))));
                    ui.label(mono(format!("VRAM {} / {}  (engine {})", mb(s.vram_used), mb(s.vram_budget), mb(s.engine_vram))));
                    ui.label(mono(format!(
                        "{} draws · {} objs · {:.1}k tris · {} shd",
                        rs.draw_calls,
                        rs.instances,
                        rs.triangles as f64 / 1000.0,
                        rs.shadow_draws
                    )));
                    if extras.entities > 0 {
                        ui.label(mono(format!("{} entities", extras.entities)));
                    }
                    for l in &extras.lines {
                        ui.label(mono(l.clone()));
                    }
                    // Heaviest top-level scopes of the last frame.
                    if let Some(f) = history.last() {
                        let mut top: Vec<(&str, f32)> = Vec::new();
                        for sc in f.scopes.iter().filter(|s| s.depth == 0 && s.name != "frame cap wait") {
                            match top.iter_mut().find(|t| t.0 == sc.name) {
                                Some(t) => t.1 += sc.dur_us / 1000.0,
                                None => top.push((sc.name, sc.dur_us / 1000.0)),
                            }
                        }
                        top.sort_by(|a, b| b.1.total_cmp(&a.1));
                        for (n, ms) in top.iter().take(5) {
                            ui.label(egui::RichText::new(format!("  {ms:6.2} ms  {n}")).monospace().size(10.5).color(egui::Color32::from_gray(150)));
                        }
                    }
                });
        });
}

/// Bars of recent frame times (CPU) with the GPU time drawn as a line.
pub fn frame_graph(ui: &mut egui::Ui, history: &[profiler::FrameRecord], w: f32, h: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 8));
    let n = 120.min(history.len());
    if n == 0 {
        return;
    }
    let frames = &history[history.len() - n..];
    // Scale: at least 33 ms so a smooth 60 fps sits low in the graph.
    let top = frames.iter().map(|f| f.frame_ms).fold(33.3f32, f32::max);
    let bw = w / 120.0;
    for (i, f) in frames.iter().enumerate() {
        let x = rect.right() - (n - i) as f32 * bw;
        let bh = (f.frame_ms / top * h).min(h);
        let c = if f.frame_ms <= 16.8 {
            egui::Color32::from_rgb(90, 190, 110)
        } else if f.frame_ms <= 33.4 {
            egui::Color32::from_rgb(220, 180, 70)
        } else {
            egui::Color32::from_rgb(230, 90, 80)
        };
        p.rect_filled(egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - bh), egui::pos2(x + bw - 0.5, rect.bottom())), 0.0, c.gamma_multiply(0.85));
    }
    let gpu: Vec<egui::Pos2> =
        frames.iter().enumerate().filter_map(|(i, f)| f.gpu_ms.map(|g| egui::pos2(rect.right() - (n - i) as f32 * bw + bw * 0.5, rect.bottom() - (g / top * h).min(h)))).collect();
    if gpu.len() > 1 {
        p.add(egui::Shape::line(gpu, egui::Stroke::new(1.2, egui::Color32::from_rgb(120, 170, 255))));
    }
    for (ms, label) in [(16.7, "60"), (33.3, "30")] {
        let y = rect.bottom() - ms / top * h;
        if y > rect.top() {
            p.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(0.5, egui::Color32::from_white_alpha(40)));
            p.text(egui::pos2(rect.left() + 2.0, y - 1.0), egui::Align2::LEFT_BOTTOM, label, egui::FontId::monospace(8.0), egui::Color32::from_white_alpha(90));
        }
    }
}

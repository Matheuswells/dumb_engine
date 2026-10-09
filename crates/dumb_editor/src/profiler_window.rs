//! Window ▸ Profiler: frame graph, per-frame timeline (flame chart), scope statistics,
//! counters and system usage (CPU, RAM, GPU, VRAM).

use dumb_core::profiler::{self, FrameRecord};
use dumb_runtime::perf::{mb, SystemMonitor, SystemSample};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Timeline,
    Scopes,
    System,
    Counters,
}

#[derive(Default)]
pub struct ProfilerWindow {
    pub open: bool,
    tab: Tab,
    /// Selected frame (profiler index); None = follow the latest.
    selected: Option<u64>,
    /// Frames averaged in the scope table.
    window: usize,
    filter: String,
}

fn name_color(name: &str) -> egui::Color32 {
    let mut h: u32 = 2166136261;
    for b in name.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    let hue = (h % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.45, 0.75, 1.0).into()
}

impl ProfilerWindow {
    pub fn ui(&mut self, ctx: &egui::Context, mon: &SystemMonitor, gpu_name: &str) {
        if !self.open {
            return;
        }
        if self.window == 0 {
            self.window = 120;
        }
        let history = profiler::history();
        let mut open = self.open;
        egui::Window::new("📊 Profiler").open(&mut open).default_size([900.0, 560.0]).resizable(true).show(ctx, |ui| {
            // ---- toolbar
            ui.horizontal(|ui| {
                let paused = profiler::paused();
                if ui.button(if paused { "▶ Resume" } else { "⏸ Pause" }).on_hover_text("Freeze the history to inspect a spike").clicked() {
                    profiler::set_paused(!paused);
                }
                if self.selected.is_some() && ui.button("Follow latest").clicked() {
                    self.selected = None;
                }
                ui.separator();
                let (avg, max) = profiler::stats(self.window, |f| Some(f.frame_ms));
                let mut sorted: Vec<f32> = history.iter().rev().take(self.window).map(|f| f.frame_ms).collect();
                sorted.sort_by(|a, b| b.total_cmp(a));
                let low = sorted.get(sorted.len() / 100).copied().unwrap_or(max);
                ui.label(format!("avg {avg:.2} ms ({:.0} fps) · max {max:.2} ms · 1% low {:.0} fps", 1000.0 / avg.max(0.001), 1000.0 / low.max(0.001)));
                let (gavg, _) = profiler::stats(self.window, |f| f.gpu_ms);
                if gavg > 0.0 {
                    ui.label(egui::RichText::new(format!("· GPU {gavg:.2} ms")).color(egui::Color32::from_rgb(120, 170, 255)));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::DragValue::new(&mut self.window).range(10..=profiler::HISTORY).suffix(" frames")).on_hover_text("Frames used for averages");
                    ui.label("Average over");
                });
            });

            // ---- frame graph (click to select)
            let h = 90.0;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::click_and_drag());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
            if !history.is_empty() {
                let n = history.len();
                let top = history.iter().map(|f| f.frame_ms).fold(33.4f32, f32::max).min(250.0);
                let bw = rect.width() / profiler::HISTORY as f32;
                let x_of = |i: usize| rect.right() - (n - i) as f32 * bw;
                for (i, f) in history.iter().enumerate() {
                    let x = x_of(i);
                    let bh = (f.frame_ms / top * h).min(h);
                    let c = if f.frame_ms <= 16.8 {
                        egui::Color32::from_rgb(80, 175, 100)
                    } else if f.frame_ms <= 33.4 {
                        egui::Color32::from_rgb(215, 175, 70)
                    } else {
                        egui::Color32::from_rgb(225, 85, 75)
                    };
                    let sel = self.selected == Some(f.index);
                    p.rect_filled(egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - bh), egui::pos2(x + bw.max(1.0), rect.bottom())), 0.0, if sel { egui::Color32::WHITE } else { c });
                }
                let gpu: Vec<egui::Pos2> = history.iter().enumerate().filter_map(|(i, f)| f.gpu_ms.map(|g| egui::pos2(x_of(i) + bw * 0.5, rect.bottom() - (g / top * h).min(h)))).collect();
                if gpu.len() > 1 {
                    p.add(egui::Shape::line(gpu, egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 170, 255))));
                }
                for (ms, label) in [(16.7, "16.7 ms (60 fps)"), (33.3, "33.3 ms (30 fps)")] {
                    let y = rect.bottom() - ms / top * h;
                    p.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(0.5, egui::Color32::from_white_alpha(50)));
                    p.text(egui::pos2(rect.left() + 4.0, y - 1.0), egui::Align2::LEFT_BOTTOM, label, egui::FontId::monospace(9.0), egui::Color32::from_white_alpha(120));
                }
                if let Some(pos) = resp.interact_pointer_pos() {
                    let i = n as isize - ((rect.right() - pos.x) / bw).ceil() as isize;
                    if let Some(f) = history.get(i.max(0) as usize) {
                        self.selected = Some(f.index);
                        profiler::set_paused(true);
                    }
                }
                if let Some(hp) = resp.hover_pos() {
                    let i = n as isize - ((rect.right() - hp.x) / bw).ceil() as isize;
                    if let Some(f) = history.get(i.max(0) as usize) {
                        let gpu = f.gpu_ms.map(|g| format!("\nGPU {g:.2} ms")).unwrap_or_default();
                        resp.on_hover_text(format!("frame {}\n{:.2} ms{gpu}\nclick to inspect", f.index, f.frame_ms));
                    }
                }
            }

            ui.horizontal(|ui| {
                for (t, n) in [(Tab::Timeline, "Timeline"), (Tab::Scopes, "Scopes"), (Tab::System, "System"), (Tab::Counters, "Counters")] {
                    ui.selectable_value(&mut self.tab, t, n);
                }
            });
            ui.separator();
            let frame = match self.selected {
                Some(i) => history.iter().find(|f| f.index == i).or(history.last()),
                None => history.last(),
            };
            match self.tab {
                Tab::Timeline => {
                    if let Some(f) = frame {
                        timeline(ui, f);
                    }
                }
                Tab::Scopes => self.scopes(ui, &history),
                Tab::System => system(ui, mon, gpu_name),
                Tab::Counters => {
                    if let Some(f) = frame {
                        egui::Grid::new("prof_counters").striped(true).show(ui, |ui| {
                            for (n, v) in &f.counters {
                                ui.label(*n);
                                ui.monospace(format!("{v}"));
                                ui.end_row();
                            }
                        });
                        if f.counters.is_empty() {
                            ui.weak("No counters this frame.");
                        }
                    }
                }
            }
        });
        self.open = open;
    }

    fn scopes(&mut self, ui: &mut egui::Ui, history: &[FrameRecord]) {
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.text_edit_singleline(&mut self.filter);
        });
        let frames: Vec<&FrameRecord> = history.iter().rev().take(self.window).collect();
        let n = frames.len().max(1) as f32;
        let avg_frame = frames.iter().map(|f| f.frame_ms).sum::<f32>() / n;
        // name -> (total ms, max ms in one frame, calls)
        let mut rows: Vec<(&str, f32, f32, u32, u16)> = Vec::new();
        for f in &frames {
            let mut per: Vec<(&str, f32, u32, u16)> = Vec::new();
            for s in &f.scopes {
                match per.iter_mut().find(|p| p.0 == s.name) {
                    Some(p) => {
                        p.1 += s.dur_us / 1000.0;
                        p.2 += 1;
                    }
                    None => per.push((s.name, s.dur_us / 1000.0, 1, s.depth)),
                }
            }
            for (name, ms, calls, depth) in per {
                match rows.iter_mut().find(|r| r.0 == name) {
                    Some(r) => {
                        r.1 += ms;
                        r.2 = r.2.max(ms);
                        r.3 += calls;
                    }
                    None => rows.push((name, ms, ms, calls, depth)),
                }
            }
        }
        rows.sort_by(|a, b| b.1.total_cmp(&a.1));
        let filter = self.filter.to_lowercase();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("prof_scopes").num_columns(6).striped(true).spacing([18.0, 4.0]).show(ui, |ui| {
                for h in ["Scope", "Avg ms", "Max ms", "Calls/frame", "% frame", ""] {
                    ui.strong(h);
                }
                ui.end_row();
                for (name, total, max, calls, depth) in rows.iter().filter(|r| filter.is_empty() || r.0.to_lowercase().contains(&filter)) {
                    let avg = total / n;
                    ui.horizontal(|ui| {
                        ui.add_space(*depth as f32 * 10.0);
                        let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 2.0, name_color(name));
                        ui.label(*name);
                    });
                    ui.monospace(format!("{avg:.3}"));
                    ui.monospace(format!("{max:.3}"));
                    ui.monospace(format!("{:.1}", *calls as f32 / n));
                    let pct = if avg_frame > 0.0 { avg / avg_frame * 100.0 } else { 0.0 };
                    ui.monospace(format!("{pct:.1}%"));
                    ui.add(egui::ProgressBar::new((pct / 100.0).clamp(0.0, 1.0)).desired_width(90.0).desired_height(8.0));
                    ui.end_row();
                }
            });
        });
    }
}

/// Flame chart of one frame: one lane per thread, nested scopes stacked below their parent.
fn timeline(ui: &mut egui::Ui, f: &FrameRecord) {
    ui.label(format!("Frame {} · {:.2} ms{}", f.index, f.frame_ms, f.gpu_ms.map(|g| format!(" · GPU {g:.2} ms")).unwrap_or_default()));
    let total_us = (f.frame_ms * 1000.0).max(1.0);
    let row_h = 18.0;
    let mut threads: Vec<u16> = f.scopes.iter().map(|s| s.thread).collect();
    threads.sort_unstable();
    threads.dedup();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let width = ui.available_width();
        for t in threads {
            let depth = f.scopes.iter().filter(|s| s.thread == t).map(|s| s.depth).max().unwrap_or(0) as f32 + 1.0;
            ui.weak(if t == 0 { "Main thread".to_string() } else { format!("Worker {t}") });
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, depth * row_h), egui::Sense::hover());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
            for (k, s) in f.scopes.iter().filter(|s| s.thread == t).enumerate() {
                let x0 = rect.left() + s.start_us / total_us * rect.width();
                let w = (s.dur_us / total_us * rect.width()).max(1.0);
                let r = egui::Rect::from_min_size(egui::pos2(x0, rect.top() + s.depth as f32 * row_h), egui::vec2(w, row_h - 2.0));
                let c = if s.name.contains("wait") { egui::Color32::from_gray(70) } else { name_color(s.name) };
                p.rect_filled(r, 2.0, c);
                if w > 40.0 {
                    p.text(
                        r.left_center() + egui::vec2(3.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        format!("{} {:.2}", s.name, s.dur_us / 1000.0),
                        egui::FontId::proportional(10.5),
                        egui::Color32::from_gray(20),
                    );
                }
                let resp = ui.interact(r, egui::Id::new(("tl", t, k)), egui::Sense::hover());
                resp.on_hover_text(format!("{}\n{:.3} ms\nstarts at {:.3} ms", s.name, s.dur_us / 1000.0, s.start_us / 1000.0));
            }
            ui.add_space(4.0);
        }
        if f.scopes.is_empty() {
            ui.weak("No scopes recorded in this frame.");
        }
    });
}

fn system(ui: &mut egui::Ui, mon: &SystemMonitor, gpu_name: &str) {
    let s = &mon.sample;
    egui::Grid::new("prof_sys").num_columns(2).spacing([20.0, 4.0]).show(ui, |ui| {
        ui.label("CPU (this process)");
        ui.monospace(format!("{:.1}% of {} cores", s.process_cpu, s.cores));
        ui.end_row();
        ui.label("CPU (system)");
        ui.monospace(format!("{:.1}%", s.system_cpu));
        ui.end_row();
        ui.label("RAM (this process)");
        ui.monospace(mb(s.process_ram));
        ui.end_row();
        ui.label("RAM (system)");
        ui.monospace(format!("{} / {}", mb(s.ram_used), mb(s.ram_total)));
        ui.end_row();
        ui.label("GPU");
        ui.monospace(gpu_name);
        ui.end_row();
        ui.label("GPU time / busy");
        ui.monospace(format!("{:.2} ms / {:.0}%", s.gpu_ms, s.gpu_busy));
        ui.end_row();
        ui.label("VRAM (device)");
        ui.monospace(format!("{} / {}", mb(s.vram_used), mb(s.vram_budget)));
        ui.end_row();
        ui.label("VRAM (engine)");
        ui.monospace(mb(s.engine_vram));
        ui.end_row();
    });
    ui.add_space(6.0);
    let hist: Vec<SystemSample> = mon.history.iter().copied().collect();
    let w = ui.available_width();
    let graphs: [(&str, egui::Color32, f32, Box<dyn Fn(&SystemSample) -> f32>); 5] = [
        ("CPU % (process)", egui::Color32::from_rgb(110, 200, 130), 100.0, Box::new(|s| s.process_cpu)),
        ("CPU % (system)", egui::Color32::from_rgb(90, 150, 110), 100.0, Box::new(|s| s.system_cpu)),
        ("RAM MB (process)", egui::Color32::from_rgb(220, 180, 90), 0.0, Box::new(|s| s.process_ram as f32 / 1_048_576.0)),
        ("GPU busy %", egui::Color32::from_rgb(120, 170, 255), 100.0, Box::new(|s| s.gpu_busy)),
        ("VRAM MB (device)", egui::Color32::from_rgb(190, 130, 230), 0.0, Box::new(|s| s.vram_used as f32 / 1_048_576.0)),
    ];
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (name, color, fixed_max, f) in graphs {
            let vals: Vec<f32> = hist.iter().map(&f).collect();
            let max = if fixed_max > 0.0 { fixed_max } else { vals.iter().cloned().fold(1.0, f32::max) * 1.15 };
            let cur = vals.last().copied().unwrap_or(0.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(name).color(color));
                ui.monospace(format!("{cur:.1}"));
            });
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w - 16.0, 44.0), egui::Sense::hover());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
            let n = dumb_runtime::perf::SAMPLES as f32;
            let pts: Vec<egui::Pos2> = vals
                .iter()
                .enumerate()
                .map(|(i, v)| egui::pos2(rect.right() - (vals.len() - i) as f32 / n * rect.width(), rect.bottom() - (v / max).clamp(0.0, 1.0) * rect.height()))
                .collect();
            if pts.len() > 1 {
                p.add(egui::Shape::line(pts, egui::Stroke::new(1.5, color)));
            }
        }
        ui.weak("Sampled twice a second (last 2 minutes).");
    });
}

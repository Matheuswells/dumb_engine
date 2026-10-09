//! Standalone 3D asset viewer and animation viewer.
//!
//! Inspects meshes, materials, textures, skeleton/bones, animations, collision proxies,
//! LODs, bounding boxes and the node/transform hierarchy of an imported model.

use crate::editor::Action;
use crate::viewport::PreviewViewport;
use dumb_asset::model::{AnimationEvent, Trs};
use crate::asset_browser::{DragAsset, ThumbnailCache};
use dumb_asset::{AssetDatabase, AssetKind, ImportSettings, ModelData, NormalsMode, Pivot};
use dumb_core::{AssetId, Color, Mat4, Vec3};
use dumb_render::{screen_size, skeleton_lines, DrawItem, RenderView, Renderer};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Import,
    Lod,
    Nodes,
    Meshes,
    Materials,
    Textures,
    Skeleton,
    Animations,
    Collision,
}

pub struct AnimState {
    pub clip: Option<usize>,
    pub time: f32,
    pub playing: bool,
    pub speed: f32,
    pub looping: bool,
    pub blend_clip: Option<usize>,
    pub blend: f32,
    pub root_motion: bool,
    pub new_event: String,
}

impl Default for AnimState {
    fn default() -> Self {
        AnimState { clip: None, time: 0.0, playing: true, speed: 1.0, looping: true, blend_clip: None, blend: 0.0, root_motion: true, new_event: String::new() }
    }
}

pub struct ModelViewer {
    pub id: AssetId,
    pub open: bool,
    /// Animation viewer layout (timeline-first) instead of the asset inspector layout.
    pub animation_mode: bool,
    pub preview: PreviewViewport,
    tab: Tab,
    wireframe: bool,
    show_bounds: bool,
    show_skeleton: bool,
    show_collision: bool,
    show_grid: bool,
    selected_node: Option<usize>,
    /// Previewed LOD level; `None` = pick by screen size like the game does.
    lod: Option<u32>,
    /// Level drawn last frame and the projected size it was picked from.
    shown_lod: usize,
    screen_size: f32,
    /// Import settings being edited (applied with "Apply", which re-imports).
    draft: Option<ImportSettings>,
    /// Last "auto-detect textures" report.
    report: Vec<String>,
    pub anim: AnimState,
    textures: HashMap<usize, egui::TextureId>,
    framed_version: u64,
    tex_id_base: u64,
}

impl ModelViewer {
    pub fn new(id: AssetId, renderer: &mut Renderer, animation_mode: bool, tex_id_base: u64) -> Self {
        ModelViewer {
            id,
            open: true,
            animation_mode,
            preview: PreviewViewport::new(renderer),
            tab: if animation_mode { Tab::Animations } else { Tab::Materials },
            wireframe: false,
            show_bounds: false,
            show_skeleton: animation_mode,
            show_collision: false,
            show_grid: true,
            selected_node: None,
            lod: None,
            shown_lod: 0,
            screen_size: 1.0,
            draft: None,
            report: Vec::new(),
            anim: AnimState::default(),
            textures: HashMap::new(),
            framed_version: 0,
            tex_id_base,
        }
    }

    pub fn tick(&mut self, dt: f32, model: Option<&ModelData>) {
        let Some(m) = model else { return };
        if self.anim.clip.is_none() && !m.animations.is_empty() {
            self.anim.clip = Some(0);
        }
        if let Some(c) = self.anim.clip.and_then(|c| m.animations.get(c)) {
            if self.anim.playing {
                self.anim.time += dt * self.anim.speed;
                let d = c.duration.max(1e-3);
                if self.anim.looping {
                    self.anim.time = self.anim.time.rem_euclid(d);
                } else if self.anim.time >= d {
                    self.anim.time = d;
                    self.anim.playing = false;
                }
            }
        }
    }

    fn pose(&self, m: &ModelData) -> Vec<Trs> {
        let a = self.anim.clip.map(|c| (c, self.anim.time));
        let b = self.anim.blend_clip.map(|c| (c, self.anim.time, self.anim.blend));
        m.sample_pose(a, b)
    }

    fn node_visible(&self, m: &ModelData, n: usize) -> bool {
        m.node_visible_at_lod(n, self.shown_lod)
    }

    /// Build this viewer's render view.
    pub fn render_view(&mut self, db: &mut AssetDatabase) -> Option<RenderView> {
        let model = db.model(self.id)?;
        let version = db.version(self.id);
        // Frame once; re-imports from setting changes keep the camera where the user put it.
        if self.framed_version == 0 {
            self.framed_version = version;
            self.preview.camera.focus(&model.aabb());
        }
        let mut v = RenderView::new(self.preview.target);
        let aspect = self.preview.aspect();
        v.view = self.preview.camera.view();
        v.proj = self.preview.camera.proj(aspect);
        v.camera_pos = self.preview.camera.position();
        v.clear_color = Color::rgb(0.12, 0.13, 0.15);
        v.wireframe = self.wireframe;
        self.screen_size = screen_size(&v, &model.bounds(), &Mat4::IDENTITY);
        self.shown_lod = match self.lod {
            Some(l) => (l as usize).min(model.lod_count() - 1),
            // Below the cull size the game draws nothing; the viewer keeps the last level.
            None => model.select_lod(self.screen_size).unwrap_or(model.lod_count() - 1),
        };
        if self.show_grid {
            let b = model.aabb();
            let step = 10f32.powf((b.extents().length().max(0.01) * 0.2).log10().floor());
            v.grid(20, step);
        }

        let pose = self.pose(&model);
        let globals = model.global_matrices(&pose);
        for (ni, node) in model.nodes.iter().enumerate() {
            let Some(mi) = node.mesh else { continue };
            if !self.node_visible(&model, ni) {
                continue;
            }
            let joint_offset = node.skin.filter(|s| *s < model.skins.len()).map(|s| {
                let off = v.joints.len() as u32;
                v.joints.extend(model.joint_matrices(s, &globals, globals[ni]));
                off
            });
            for (pi, prim) in model.meshes[mi].primitives.iter().enumerate() {
                v.draws.push(DrawItem {
                    model: self.id,
                    mesh: mi as u32,
                    primitive: pi as u32,
                    material: prim.material.map_or(AssetId::NONE, |m| model.material_id(self.id, m)),
                    transform: globals[ni],
                    tint: Color::WHITE,
                    joint_offset,
                    selected: self.selected_node == Some(ni),
                    lod: self.shown_lod as u8,
                    cast_shadows: true,
                    sphere: dumb_core::Vec4::ZERO,
                });
            }
        }
        if self.show_bounds {
            v.aabb_lines(&model.aabb(), &Mat4::IDENTITY, Color::rgba(0.3, 0.9, 0.4, 0.9));
            for (ni, node) in model.nodes.iter().enumerate() {
                if let Some(mi) = node.mesh {
                    if self.node_visible(&model, ni) {
                        v.aabb_lines(&model.meshes[mi].aabb(), &globals[ni], Color::rgba(0.9, 0.9, 0.3, 0.5));
                    }
                }
            }
        }
        if self.show_collision {
            for &ni in &model.collision_nodes {
                let Some(mi) = model.nodes[ni].mesh else { continue };
                for p in &model.meshes[mi].primitives {
                    for t in p.indices.chunks_exact(3) {
                        let pts = [t[0], t[1], t[2]].map(|i| globals[ni].transform_point3(Vec3::from(p.vertices[i as usize].position)));
                        for k in 0..3 {
                            v.overlay_line(pts[k], pts[(k + 1) % 3], Color::rgba(1.0, 0.5, 0.1, 0.9));
                        }
                    }
                }
            }
        }
        if self.show_skeleton {
            skeleton_lines(&mut v, &model, &globals, Mat4::IDENTITY, self.selected_node);
        }
        if let Some(n) = self.selected_node {
            let p = globals.get(n).map(|m| m.w_axis.truncate()).unwrap_or_default();
            let s = model.aabb().extents().length() * 0.08;
            v.overlay_line(p, p + Vec3::X * s, Color::RED);
            v.overlay_line(p, p + Vec3::Y * s, Color::GREEN);
            v.overlay_line(p, p + Vec3::Z * s, Color::BLUE);
        }
        if self.anim.root_motion {
            if let Some(ci) = self.anim.clip {
                self.root_motion_lines(&model, ci, &mut v);
            }
        }
        Some(v)
    }

    fn root_node(m: &ModelData) -> Option<usize> {
        m.skins.first().and_then(|s| s.joints.first().copied()).or_else(|| m.roots.first().copied())
    }

    fn root_motion_lines(&self, m: &ModelData, ci: usize, v: &mut RenderView) {
        let Some(root) = Self::root_node(m) else { return };
        let Some(clip) = m.animations.get(ci) else { return };
        let n = 90;
        let mut prev = None;
        for i in 0..=n {
            let t = clip.duration * i as f32 / n as f32;
            let mut pose = m.rest_pose();
            clip.sample_into(t, &mut pose);
            let p = m.global_matrices(&pose)[root].w_axis.truncate();
            let ground = Vec3::new(p.x, 0.0, p.z);
            if let Some(q) = prev {
                v.overlay_line(q, ground, Color::rgba(1.0, 0.3, 0.8, 0.9));
            }
            prev = Some(ground);
        }
        let mut pose = m.rest_pose();
        clip.sample_into(self.anim.time, &mut pose);
        let p = m.global_matrices(&pose)[root].w_axis.truncate();
        let s = m.aabb().extents().length() * 0.05;
        v.overlay_line(Vec3::new(p.x - s, 0.0, p.z), Vec3::new(p.x + s, 0.0, p.z), Color::WHITE);
        v.overlay_line(Vec3::new(p.x, 0.0, p.z - s), Vec3::new(p.x, 0.0, p.z + s), Color::WHITE);
        v.overlay_line(Vec3::new(p.x, 0.0, p.z), p, Color::rgba(1.0, 1.0, 1.0, 0.4));
    }

    pub fn ui(&mut self, ctx: &egui::Context, db: &mut AssetDatabase, renderer: &mut Renderer, actions: &mut Vec<Action>, thumbs: &mut ThumbnailCache) {
        let title = format!(
            "{} {}",
            if self.animation_mode { "🎞 Animation Viewer —" } else { "📦 Model Viewer —" },
            db.display_name(self.id)
        );
        let mut open = self.open;
        egui::Window::new(title)
            .id(egui::Id::new(("model_viewer", self.id, self.animation_mode)))
            .open(&mut open)
            .default_size([1100.0, 720.0])
            .resizable(true)
            .show(ctx, |ui| {
                let model = db.model(self.id);
                let Some(model) = model else {
                    match db.entry(self.id).map(|e| e.state.clone()) {
                        Some(dumb_asset::LoadState::Failed(e)) => {
                            ui.colored_label(egui::Color32::LIGHT_RED, e);
                        }
                        _ => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Importing…");
                            });
                        }
                    }
                    return;
                };
                self.toolbar(ui, &model);
                ui.separator();
                let avail = ui.available_size();
                let timeline_h = if model.animations.is_empty() { 0.0 } else { 120.0 };
                ui.horizontal_top(|ui| {
                    let side_w = if self.animation_mode { 260.0 } else { 340.0 };
                    let vp = egui::vec2((avail.x - side_w - 12.0).max(200.0), (avail.y - timeline_h - 8.0).max(200.0));
                    ui.vertical(|ui| {
                        self.preview.show(ui, renderer, vp);
                        if timeline_h > 0.0 {
                            self.timeline(ui, db, &model, vp.x);
                        }
                    });
                    ui.vertical(|ui| {
                        ui.set_width(side_w);
                        self.side_panel(ui, db, renderer, &model, actions, thumbs);
                    });
                });
            });
        self.open = open;
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, m: &ModelData) {
        ui.horizontal_wrapped(|ui| {
            ui.toggle_value(&mut self.wireframe, "⬜ Wireframe");
            ui.toggle_value(&mut self.show_bounds, "⛶ Bounds");
            ui.toggle_value(&mut self.show_skeleton, "🔩 Skeleton");
            ui.toggle_value(&mut self.show_collision, "🛡 Collision");
            ui.toggle_value(&mut self.show_grid, "# Grid");
            ui.toggle_value(&mut self.preview.auto_rotate, "⟲ Turntable");
            if !m.animations.is_empty() {
                ui.toggle_value(&mut self.anim.root_motion, "🎯 Root motion");
            }
            if m.lod_count() > 1 {
                ui.label("LOD");
                let text = match self.lod {
                    Some(l) => format!("LOD{l}"),
                    None => format!("Auto (LOD{})", self.shown_lod),
                };
                egui::ComboBox::from_id_salt(("mv_lod", self.id)).selected_text(text).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.lod, None, "Auto (by screen size)");
                    for l in 0..m.lod_count() as u32 {
                        ui.selectable_value(&mut self.lod, Some(l), format!("LOD{l} — {} tris", m.triangle_count_at(l as usize)));
                    }
                });
            }
            if ui.button("⛶ Frame").clicked() {
                self.preview.camera.focus(&m.aabb());
            }
            let b = m.aabb();
            ui.weak(format!(
                "{} verts · {} tris (LOD{} · screen {:.0}%) · size {:.2}×{:.2}×{:.2} · {}",
                m.vertex_count(),
                m.triangle_count_at(self.shown_lod),
                self.shown_lod,
                self.screen_size * 100.0,
                b.max.x - b.min.x,
                b.max.y - b.min.y,
                b.max.z - b.min.z,
                m.source_format
            ));
        });
    }

    fn timeline(&mut self, ui: &mut egui::Ui, db: &mut AssetDatabase, m: &ModelData, width: f32) {
        let Some(ci) = self.anim.clip else { return };
        let Some(clip) = m.animations.get(ci) else { return };
        let dur = clip.duration.max(1e-3);
        ui.horizontal(|ui| {
            if ui.button(if self.anim.playing { "⏸" } else { "▶" }).clicked() {
                self.anim.playing = !self.anim.playing;
                if self.anim.playing && !self.anim.looping && self.anim.time >= dur {
                    self.anim.time = 0.0;
                }
            }
            if ui.button("⏮").clicked() {
                self.anim.time = 0.0;
            }
            if ui.button("⏭").clicked() {
                self.anim.time = dur;
            }
            ui.toggle_value(&mut self.anim.looping, "🔁");
            ui.label("Speed");
            ui.add(egui::Slider::new(&mut self.anim.speed, 0.0..=3.0).max_decimals(2));
            ui.label(format!("{:.2}s / {:.2}s  (frame {:.0} @30)", self.anim.time, dur, self.anim.time * 30.0));
        });

        // Timeline bar with ticks, events and a draggable playhead.
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 40.0), egui::Sense::click_and_drag());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 3.0, egui::Color32::from_gray(28));
        let x_of = |t: f32| rect.min.x + (t / dur).clamp(0.0, 1.0) * rect.width();
        let step = if dur > 10.0 { 1.0 } else if dur > 2.0 { 0.25 } else { 0.1 };
        let mut t = 0.0;
        while t <= dur + 1e-4 {
            let x = x_of(t);
            let major = (t / (step * 4.0)).fract().abs() < 1e-3;
            p.line_segment(
                [egui::pos2(x, rect.max.y - if major { 14.0 } else { 7.0 }), egui::pos2(x, rect.max.y)],
                egui::Stroke::new(1.0, egui::Color32::from_gray(if major { 140 } else { 80 })),
            );
            if major {
                p.text(egui::pos2(x + 2.0, rect.min.y + 2.0), egui::Align2::LEFT_TOP, format!("{t:.1}"), egui::FontId::monospace(10.0), egui::Color32::GRAY);
            }
            t += step;
        }
        // Keyframe density strip.
        for ch in clip.channels.iter().take(64) {
            for kt in &ch.times {
                let x = x_of(*kt);
                p.line_segment([egui::pos2(x, rect.center().y - 2.0), egui::pos2(x, rect.center().y + 2.0)], egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(120, 170, 255, 40)));
            }
        }
        let events = self.events(db, &clip.name);
        for ev in &events {
            let x = x_of(ev.time);
            p.add(egui::Shape::convex_polygon(
                vec![egui::pos2(x, rect.max.y - 2.0), egui::pos2(x - 5.0, rect.max.y - 11.0), egui::pos2(x + 5.0, rect.max.y - 11.0)],
                egui::Color32::from_rgb(255, 190, 60),
                egui::Stroke::NONE,
            ));
        }
        let hx = x_of(self.anim.time);
        p.line_segment([egui::pos2(hx, rect.min.y), egui::pos2(hx, rect.max.y)], egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 80, 80)));
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.dragged() || resp.clicked() {
                self.anim.time = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0) * dur;
                self.anim.playing = false;
            }
        }
        if let Some(h) = resp.hover_pos() {
            if let Some(ev) = events.iter().find(|e| (x_of(e.time) - h.x).abs() < 6.0) {
                resp.clone().on_hover_text(format!("event `{}` @ {:.2}s", ev.name, ev.time));
            }
        }
    }

    fn events(&self, db: &AssetDatabase, clip: &str) -> Vec<AnimationEvent> {
        db.entry(self.id)
            .and_then(|e| e.meta.animation_events.iter().find(|(c, _)| c == clip).map(|(_, v)| v.clone()))
            .unwrap_or_default()
    }

    fn set_events(&self, db: &mut AssetDatabase, clip: &str, events: Vec<AnimationEvent>) {
        if let Some(meta) = db.meta_mut(self.id) {
            match meta.animation_events.iter_mut().find(|(c, _)| c == clip) {
                Some((_, v)) => *v = events,
                None => meta.animation_events.push((clip.to_string(), events)),
            }
        }
        db.save_meta(self.id);
    }

    fn side_panel(&mut self, ui: &mut egui::Ui, db: &mut AssetDatabase, renderer: &mut Renderer, m: &Arc<ModelData>, actions: &mut Vec<Action>, thumbs: &mut ThumbnailCache) {
        ui.horizontal_wrapped(|ui| {
            let tabs: &[(Tab, &str)] = if self.animation_mode {
                &[(Tab::Animations, "Animations"), (Tab::Skeleton, "Bones"), (Tab::Nodes, "Nodes")]
            } else {
                &[
                    (Tab::Import, "Import"),
                    (Tab::Materials, "Materials"),
                    (Tab::Lod, "LOD"),
                    (Tab::Nodes, "Nodes"),
                    (Tab::Meshes, "Meshes"),
                    (Tab::Textures, "Textures"),
                    (Tab::Skeleton, "Skeleton"),
                    (Tab::Animations, "Animations"),
                    (Tab::Collision, "Collision"),
                ]
            };
            for (t, n) in tabs {
                ui.selectable_value(&mut self.tab, *t, *n);
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
            Tab::Import => self.import_tab(ui, db),
            Tab::Lod => self.lod_tab(ui, db, m),
            Tab::Nodes => self.nodes_tab(ui, db, m),
            Tab::Meshes => {
                for (i, mesh) in m.meshes.iter().enumerate() {
                    egui::CollapsingHeader::new(format!("🔺 {}", mesh.name)).id_salt(("mesh", i)).show(ui, |ui| {
                        let b = mesh.aabb();
                        ui.label(format!("{} vertices · {} triangles", mesh.vertex_count(), mesh.triangle_count()));
                        ui.label(format!("bounds min {:.3?}\nbounds max {:.3?}", b.min.to_array(), b.max.to_array()));
                        for (pi, p) in mesh.primitives.iter().enumerate() {
                            let mat = p.material.and_then(|i| m.material_names.get(i)).cloned().unwrap_or_else(|| "default".into());
                            let skinned = p.vertices.iter().any(|v| v.weights.iter().any(|w| *w > 0.0));
                            ui.weak(format!("prim {pi}: {} idx, material `{mat}`{}", p.indices.len(), if skinned { ", skinned" } else { "" }));
                        }
                        let users: Vec<String> = m.nodes.iter().filter(|n| n.mesh == Some(i)).map(|n| n.name.clone()).collect();
                        ui.weak(format!("used by: {}", users.join(", ")));
                    });
                }
            }
            Tab::Materials => self.materials_tab(ui, db, renderer, m, actions, thumbs),
            Tab::Textures => {
                for (i, t) in m.textures.iter().enumerate() {
                    let tid = *self.textures.entry(i).or_insert_with(|| {
                        let id = egui::TextureId::User(self.tex_id_base + i as u64);
                        renderer.register_egui_image(id, t.width, t.height, &t.rgba8);
                        id
                    });
                    ui.label(format!("🖼 {} — {}×{}", t.name, t.width, t.height));
                    let w = ui.available_width().min(300.0);
                    let h = w * t.height as f32 / t.width.max(1) as f32;
                    ui.image(egui::load::SizedTexture::new(tid, egui::vec2(w, h)));
                    ui.separator();
                }
                if m.textures.is_empty() {
                    ui.weak("No embedded textures.");
                }
            }
            Tab::Skeleton => {
                if m.skins.is_empty() {
                    ui.weak("No skeleton.");
                }
                for (si, s) in m.skins.iter().enumerate() {
                    ui.strong(format!("🔩 {} — {} bones", s.name, s.joints.len()));
                    let pose = self.pose(m);
                    let globals = m.global_matrices(&pose);
                    for &j in &s.joints {
                        let depth = {
                            let mut d = 0;
                            let mut c = j;
                            while let Some(p) = m.nodes[c].parent {
                                if !s.joints.contains(&p) {
                                    break;
                                }
                                d += 1;
                                c = p;
                            }
                            d
                        };
                        ui.horizontal(|ui| {
                            ui.add_space(depth as f32 * 10.0);
                            if ui.selectable_label(self.selected_node == Some(j), &m.nodes[j].name).clicked() {
                                self.selected_node = Some(j);
                                self.show_skeleton = true;
                            }
                        });
                        if self.selected_node == Some(j) {
                            let (sc, r, t) = globals[j].to_scale_rotation_translation();
                            ui.weak(format!("   pos {}\n   rot {:.3?}\n   scale {}", crate::inspector::vec3_label(t), r.to_array(), crate::inspector::vec3_label(sc)));
                        }
                    }
                    let _ = si;
                }
            }
            Tab::Animations => self.animations_tab(ui, db, m),
            Tab::Collision => {
                ui.strong("Collision proxies");
                if m.collision_nodes.is_empty() {
                    ui.weak("None. Name meshes UCX_/UBX_/USP_/COL_ in Blender, or tick \"Collision proxy\" on a node in the Nodes tab.");
                }
                for &n in &m.collision_nodes {
                    if ui.selectable_label(self.selected_node == Some(n), format!("🛡 {}", m.nodes[n].name)).clicked() {
                        self.selected_node = Some(n);
                        self.show_collision = true;
                    }
                }
            }
        });
    }

    fn nodes_tab(&mut self, ui: &mut egui::Ui, db: &mut AssetDatabase, m: &ModelData) {
        fn node_ui(ui: &mut egui::Ui, m: &ModelData, n: usize, sel: &mut Option<usize>) {
            let node = &m.nodes[n];
            let icon = if node.skin.is_some() {
                "🔩"
            } else if node.mesh.is_some() {
                "🔺"
            } else if m.collision_nodes.contains(&n) {
                "🛡"
            } else {
                "⬜"
            };
            let label = format!("{icon} {}", node.name);
            if node.children.is_empty() {
                if ui.selectable_label(*sel == Some(n), label).clicked() {
                    *sel = Some(n);
                }
            } else {
                egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), ui.make_persistent_id(("node", n)), false)
                    .show_header(ui, |ui| {
                        if ui.selectable_label(*sel == Some(n), label).clicked() {
                            *sel = Some(n);
                        }
                    })
                    .body(|ui| {
                        for c in &node.children {
                            node_ui(ui, m, *c, sel);
                        }
                    });
            }
        }
        for r in &m.roots {
            node_ui(ui, m, *r, &mut self.selected_node);
        }
        if let Some(n) = self.selected_node.filter(|n| *n < m.nodes.len()) {
            ui.separator();
            let node = &m.nodes[n];
            ui.strong(&node.name);
            let l = node.local;
            ui.label(format!("Local position {}", crate::inspector::vec3_label(l.translation)));
            let (y, x, z) = l.rotation.to_euler(dumb_core::EulerRot::YXZ);
            ui.label(format!("Local rotation {:.1}°, {:.1}°, {:.1}°", x.to_degrees(), y.to_degrees(), z.to_degrees()));
            ui.label(format!("Local scale {}", crate::inspector::vec3_label(l.scale)));
            if let Some(mi) = node.mesh {
                ui.label(format!("Mesh: {}", m.meshes[mi].name));
            }
            if let Some(s) = node.skin {
                ui.label(format!("Skin: {}", m.skins[s].name));
            }
            if node.mesh.is_some() {
                self.node_flags(ui, db, m, n);
            }
        }
    }

    fn animations_tab(&mut self, ui: &mut egui::Ui, db: &mut AssetDatabase, m: &ModelData) {
        if m.animations.is_empty() {
            ui.weak("No animations.");
            return;
        }
        ui.strong("Clips");
        for (i, a) in m.animations.iter().enumerate() {
            let r = ui.selectable_label(self.anim.clip == Some(i), format!("🎞 {}  ({:.2}s, {} ch)", a.name, a.duration, a.channels.len()));
            if r.clicked() {
                self.anim.clip = Some(i);
                self.anim.time = 0.0;
                self.anim.playing = true;
            }
        }
        ui.separator();
        ui.strong("Blend preview");
        let names: Vec<&str> = m.animations.iter().map(|a| a.name.as_str()).collect();
        egui::ComboBox::from_label("blend with")
            .selected_text(self.anim.blend_clip.map_or("—", |i| names[i]))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.anim.blend_clip, None, "—");
                for (i, n) in names.iter().enumerate() {
                    ui.selectable_value(&mut self.anim.blend_clip, Some(i), *n);
                }
            });
        ui.add_enabled(self.anim.blend_clip.is_some(), egui::Slider::new(&mut self.anim.blend, 0.0..=1.0).text("weight"));

        if let Some(ci) = self.anim.clip {
            let clip_name = m.animations[ci].name.clone();
            ui.separator();
            ui.strong("Events");
            let mut events = self.events(db, &clip_name);
            let mut changed = false;
            let mut remove = None;
            for (i, ev) in events.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    changed |= ui.add(egui::DragValue::new(&mut ev.time).speed(0.01).range(0.0..=m.animations[ci].duration).suffix("s")).changed();
                    changed |= ui.text_edit_singleline(&mut ev.name).changed();
                    if ui.small_button("✖").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                events.remove(i);
                changed = true;
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.anim.new_event).hint_text("event name").desired_width(120.0));
                if ui.button(format!("➕ at {:.2}s", self.anim.time)).clicked() && !self.anim.new_event.is_empty() {
                    events.push(AnimationEvent { time: self.anim.time, name: std::mem::take(&mut self.anim.new_event) });
                    changed = true;
                }
            });
            if changed {
                events.sort_by(|a, b| a.time.total_cmp(&b.time));
                self.set_events(db, &clip_name, events);
            }
        }
    }
}

impl ModelViewer {
    /// Free this viewer's GPU resources (render target and texture previews).
    pub fn release(&self, renderer: &mut Renderer) {
        renderer.destroy_target(self.preview.target);
        for t in self.textures.values() {
            renderer.free_egui_image(*t);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Editing: import settings, material slots, LODs and node flags. Everything is stored in the
// model's `.meta` (`ImportSettings`) and applied on re-import, so the source file is untouched.

impl ModelViewer {
    /// Select a side-panel tab by name (dev hook `DUMB_OPEN=model:path@lod`).
    pub fn select_tab(&mut self, name: &str) {
        self.tab = match name.to_ascii_lowercase().as_str() {
            "import" => Tab::Import,
            "materials" => Tab::Materials,
            "lod" => Tab::Lod,
            "nodes" => Tab::Nodes,
            "meshes" => Tab::Meshes,
            "textures" => Tab::Textures,
            "skeleton" => Tab::Skeleton,
            "animations" => Tab::Animations,
            "collision" => Tab::Collision,
            _ => return,
        };
    }

    fn draft(&mut self, db: &AssetDatabase) -> &mut ImportSettings {
        self.draft.get_or_insert_with(|| db.import_settings(self.id).cloned().unwrap_or_default())
    }

    fn dirty(&self, db: &AssetDatabase) -> bool {
        self.draft.as_ref().is_some_and(|d| Some(d) != db.import_settings(self.id))
    }

    fn apply(&mut self, db: &mut AssetDatabase) {
        if let Some(s) = self.draft.clone() {
            db.set_import_settings(self.id, s);
        }
    }

    /// Change settings that should apply right away (material slots, node flags).
    fn edit_now(&mut self, db: &mut AssetDatabase, f: impl FnOnce(&mut ImportSettings)) {
        f(self.draft(db));
        self.apply(db);
    }

    fn apply_bar(&mut self, ui: &mut egui::Ui, db: &mut AssetDatabase) {
        let dirty = self.dirty(db);
        ui.horizontal(|ui| {
            if ui.add_enabled(dirty, egui::Button::new("✔ Apply")).on_hover_text("Re-import with these settings").clicked() {
                self.apply(db);
            }
            if ui.add_enabled(dirty, egui::Button::new("↺ Revert")).clicked() {
                self.draft = None;
            }
            if db.entry(self.id).is_some_and(|e| e.state == dumb_asset::LoadState::Loading) {
                ui.spinner();
                ui.weak("re-importing…");
            } else if dirty {
                ui.colored_label(egui::Color32::from_rgb(255, 190, 80), "unapplied changes");
            }
        });
    }

    fn import_tab(&mut self, ui: &mut egui::Ui, db: &mut AssetDatabase) {
        ui.weak("Applied every time the model is imported; the source file is not modified.");
        ui.add_space(4.0);
        let id = self.id;
        let s = self.draft(db);
        egui::Grid::new(("mv_import", id)).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Scale");
            ui.add(egui::DragValue::new(&mut s.scale).speed(0.01).range(0.0001..=10000.0));
            ui.end_row();
            ui.label("Rotation");
            ui.horizontal(|ui| {
                for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                    ui.add(egui::DragValue::new(&mut s.rotation[i]).speed(1.0).prefix(format!("{axis} ")).suffix("°"));
                }
            });
            ui.end_row();
            ui.label("");
            ui.horizontal(|ui| {
                if ui.small_button("Z-up to Y-up").on_hover_text("Rotate -90° on X").clicked() {
                    s.rotation = [-90.0, 0.0, 0.0];
                }
                if ui.small_button("Turn 180°").clicked() {
                    s.rotation[1] = (s.rotation[1] + 180.0) % 360.0;
                }
                if ui.small_button("Reset").clicked() {
                    s.rotation = [0.0; 3];
                }
            });
            ui.end_row();
            ui.label("Pivot");
            egui::ComboBox::from_id_salt(("mv_pivot", id)).selected_text(format!("{:?}", s.pivot)).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.pivot, Pivot::Keep, "Keep (as authored)");
                ui.selectable_value(&mut s.pivot, Pivot::Center, "Center of bounds");
                ui.selectable_value(&mut s.pivot, Pivot::BottomCenter, "Bottom center (props)");
            });
            ui.end_row();
            ui.label("Normals");
            egui::ComboBox::from_id_salt(("mv_normals", id)).selected_text(format!("{:?}", s.normals)).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.normals, NormalsMode::Import, "Import (from file)");
                ui.selectable_value(&mut s.normals, NormalsMode::Smooth, "Smooth (recompute)");
                ui.selectable_value(&mut s.normals, NormalsMode::Flat, "Flat (faceted)");
            });
            ui.end_row();
        });
        ui.add_space(6.0);
        self.apply_bar(ui, db);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("⟳ Reimport from source").clicked() {
                db.reimport(self.id);
            }
            if ui.button("Reset all settings").on_hover_text("Back to defaults (not applied until Apply)").clicked() {
                self.draft = Some(ImportSettings::default());
            }
        });
    }

    fn lod_tab(&mut self, ui: &mut egui::Ui, db: &mut AssetDatabase, m: &ModelData) {
        let levels = m.lod_count();
        ui.strong("Levels");
        egui::Grid::new(("mv_lod_levels", self.id)).num_columns(3).striped(true).show(ui, |ui| {
            let full = m.triangle_count_at(0).max(1);
            for l in 0..levels {
                let tris = m.triangle_count_at(l);
                let label = if self.shown_lod == l { format!("▶ LOD{l}") } else { format!("   LOD{l}") };
                if ui.selectable_label(self.lod == Some(l as u32), label).on_hover_text("Preview this level").clicked() {
                    self.lod = if self.lod == Some(l as u32) { None } else { Some(l as u32) };
                }
                ui.label(format!("{tris} tris"));
                ui.weak(format!("{:.0}%", tris as f32 * 100.0 / full as f32));
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.lod, None, "Auto");
            ui.weak(format!("screen size {:.1}% = LOD{}", self.screen_size * 100.0, self.shown_lod));
        });
        if !m.lods.is_empty() {
            ui.weak(format!("{} authored LOD group(s) (_LODn nodes).", m.lods.len()));
        }

        ui.separator();
        ui.strong("Generate");
        let view_size = self.screen_size;
        let s = self.draft(db);
        ui.checkbox(&mut s.lod.generate, "Generate LODs at import (meshoptimizer)");
        ui.add_enabled_ui(s.lod.generate, |ui| {
            let mut remove = None;
            for (i, r) in s.lod.ratios.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("LOD{}", i + 1));
                    let mut pct = *r * 100.0;
                    if ui.add(egui::Slider::new(&mut pct, 1.0..=95.0).suffix("% tris").max_decimals(0)).changed() {
                        *r = pct / 100.0;
                    }
                    if ui.small_button("✖").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                s.lod.ratios.remove(i);
            }
            ui.horizontal(|ui| {
                if s.lod.ratios.len() < 7 && ui.small_button("➕ Level").clicked() {
                    let last = s.lod.ratios.last().copied().unwrap_or(1.0);
                    s.lod.ratios.push((last * 0.5).max(0.01));
                }
                ui.label("Max error");
                ui.add(egui::DragValue::new(&mut s.lod.max_error).speed(0.001).range(0.001..=1.0));
            });
        });

        ui.separator();
        ui.strong("Switching");
        ui.weak("A level is used while the object covers at least this much of the screen height.");
        let want = if s.lod.generate { levels.max(s.lod.ratios.len() + 1) } else { levels };
        s.lod.screen_sizes.resize(want, 0.0);
        for (i, sz) in s.lod.screen_sizes.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("LOD{i}"));
                let mut pct = *sz * 100.0;
                if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix("%").max_decimals(1)).changed() {
                    *sz = pct / 100.0;
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("Cull below");
            let mut pct = s.lod.cull_screen_size * 100.0;
            if ui.add(egui::Slider::new(&mut pct, 0.0..=10.0).suffix("%").max_decimals(2)).on_hover_text("0 = never cull").changed() {
                s.lod.cull_screen_size = pct / 100.0;
            }
        });
        if ui.small_button("Cull at the current view distance").clicked() {
            s.lod.cull_screen_size = view_size;
        }
        ui.add_space(6.0);
        self.apply_bar(ui, db);
    }

    fn materials_tab(
        &mut self,
        ui: &mut egui::Ui,
        db: &mut AssetDatabase,
        renderer: &mut Renderer,
        m: &ModelData,
        actions: &mut Vec<Action>,
        thumbs: &mut ThumbnailCache,
    ) {
        if m.materials.is_empty() {
            ui.weak("No material slots.");
            return;
        }
        ui.horizontal(|ui| {
            if ui.button("🔍 Auto-detect all").on_hover_text("Find textures in the project by name for every slot").clicked() {
                self.report.clear();
                for slot in 0..m.materials.len() {
                    self.auto_detect(db, m, slot);
                }
            }
            if ui.button("Reset all").on_hover_text("Use the imported materials again").clicked() {
                self.edit_now(db, |s| s.material_remap.clear());
            }
        });
        if !self.report.is_empty() {
            egui::CollapsingHeader::new("Last auto-detect").default_open(true).show(ui, |ui| {
                for l in &self.report {
                    ui.weak(l);
                }
            });
        }
        ui.separator();
        let remap = db.import_settings(self.id).map(|s| s.material_remap.clone()).unwrap_or_default();
        for slot in 0..m.materials.len() {
            let name = m.material_names.get(slot).cloned().unwrap_or_else(|| format!("Slot {slot}"));
            let embedded = self.id.sub("material", slot);
            let assigned = remap.iter().find(|(s, _)| *s == slot).map(|(_, id)| *id);
            let current = assigned.unwrap_or(embedded);
            let users = m.meshes.iter().flat_map(|mesh| &mesh.primitives).filter(|p| p.material == Some(slot)).count();
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, resp) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), egui::Sense::click());
                    ui.painter().rect_filled(r, 4.0, egui::Color32::from_gray(38));
                    match thumbs.get(db, renderer, current) {
                        Some(t) => {
                            ui.painter().image(t, r.shrink(2.0), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                        }
                        None => {
                            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "⏳", egui::FontId::proportional(16.0), egui::Color32::LIGHT_GRAY);
                        }
                    }
                    // Drop a material from the asset browser onto the thumbnail.
                    if resp.dnd_hover_payload::<DragAsset>().is_some() {
                        ui.painter().rect_stroke(r, 4.0, egui::Stroke::new(2.0, egui::Color32::LIGHT_BLUE), egui::StrokeKind::Inside);
                    }
                    if let Some(p) = resp.dnd_release_payload::<DragAsset>() {
                        if db.entry(p.0).is_some_and(|e| e.kind == AssetKind::Material) {
                            let id = p.0;
                            self.edit_now(db, |s| set_remap(s, slot, Some(id)));
                        }
                    }
                    if resp.on_hover_text("Drop a material here · click to edit").clicked() {
                        actions.push(Action::OpenMaterial(current));
                    }
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.strong(&name);
                            ui.weak(format!("slot {slot} · {users} primitive(s)"));
                        });
                        let mut sel = assigned;
                        let text = match assigned {
                            Some(id) => db.display_name(id),
                            None => "Imported (embedded)".to_string(),
                        };
                        let w = ui.available_width().clamp(80.0, 220.0);
                        egui::ComboBox::from_id_salt(("mv_slot", self.id, slot)).selected_text(text).width(w).truncate().height(320.0).show_ui(ui, |ui| {
                            ui.selectable_value(&mut sel, None, "Imported (embedded)");
                            for id in db.assets_of_kind(AssetKind::Material.tag()) {
                                ui.selectable_value(&mut sel, Some(id), db.display_name(id));
                            }
                        });
                        if sel != assigned {
                            self.edit_now(db, |s| set_remap(s, slot, sel));
                        }
                        ui.horizontal_wrapped(|ui| {
                            if ui.small_button("🔍 Auto-detect").on_hover_text("Find this slot's textures by name and create a material").clicked() {
                                self.report.clear();
                                self.auto_detect(db, m, slot);
                            }
                            if assigned.is_none() && ui.small_button("⇪ Extract").on_hover_text("Copy into an editable .mat and use it").clicked() {
                                if let Some(mat) = db.material(embedded) {
                                    match write_material(db, &self.model_stem(db), &name, &mat) {
                                        Ok(id) => self.edit_now(db, |s| set_remap(s, slot, Some(id))),
                                        Err(e) => self.report = vec![format!("extract failed: {e}")],
                                    }
                                }
                            }
                            if ui.small_button("✏ Edit").clicked() {
                                actions.push(Action::OpenMaterial(current));
                            }
                        });
                    });
                });
            });
        }
    }

    fn model_stem(&self, db: &AssetDatabase) -> String {
        db.entry(self.id).map(|e| e.stem().to_string()).unwrap_or_else(|| "model".into())
    }

    /// Guess a slot's textures from the project and assign a generated material.
    fn auto_detect(&mut self, db: &mut AssetDatabase, m: &ModelData, slot: usize) {
        let name = m.material_names.get(slot).cloned().unwrap_or_else(|| format!("Slot{slot}"));
        let base = m.materials.get(slot).cloned().unwrap_or_default();
        let stem = self.model_stem(db);
        let g = dumb_asset::autotex::guess_material(db, &stem, &name, &base);
        if g.found.is_empty() {
            self.report.push(format!("{name}: no matching textures found"));
            return;
        }
        let list = g.found.iter().map(|(c, _, p)| format!("{c:?}: {}", p.rsplit('/').next().unwrap_or(p))).collect::<Vec<_>>().join(", ");
        // Re-use the material this slot already points at instead of creating copies.
        let existing = db.import_settings(self.id).and_then(|s| s.material_remap.iter().find(|(s, _)| *s == slot).map(|(_, id)| *id));
        let result = match existing.filter(|id| db.entry(*id).is_some_and(|e| e.kind == AssetKind::Material)) {
            Some(id) => db.set_material(id, g.material).map(|_| id),
            None => write_material(db, &stem, &name, &g.material),
        };
        match result {
            Ok(id) => {
                self.edit_now(db, |s| set_remap(s, slot, Some(id)));
                self.report.push(format!("{name}: {list}"));
            }
            Err(e) => self.report.push(format!("{name}: {e}")),
        }
    }

    fn node_flags(&mut self, ui: &mut egui::Ui, db: &mut AssetDatabase, m: &ModelData, n: usize) {
        let name = m.nodes[n].name.clone();
        let settings = db.import_settings(self.id).cloned().unwrap_or_default();
        let mut hidden = settings.hidden_nodes.contains(&name);
        let by_name = m.collision_nodes.contains(&n) && !settings.collision_nodes.contains(&name);
        let mut collision = m.collision_nodes.contains(&n);
        ui.horizontal(|ui| {
            if ui.checkbox(&mut hidden, "Hidden").on_hover_text("Don't render this node").changed() {
                self.edit_now(db, |s| toggle(&mut s.hidden_nodes, &name, hidden));
            }
            let r = ui.add_enabled(!by_name, egui::Checkbox::new(&mut collision, "Collision proxy"));
            let r = if by_name { r.on_disabled_hover_text("Marked by its name (UCX_/UBX_/USP_/COL_)") } else { r.on_hover_text("Use as a collider shape, not rendered") };
            if r.changed() {
                self.edit_now(db, |s| toggle(&mut s.collision_nodes, &name, collision));
            }
        });
    }
}

fn toggle(list: &mut Vec<String>, name: &str, on: bool) {
    list.retain(|x| x != name);
    if on {
        list.push(name.to_string());
    }
}

fn set_remap(s: &mut ImportSettings, slot: usize, id: Option<AssetId>) {
    s.material_remap.retain(|(x, _)| *x != slot);
    if let Some(id) = id {
        s.material_remap.push((slot, id));
        s.material_remap.sort_by_key(|(x, _)| *x);
    }
}

/// Write `Materials/<model>_<material>.mat` (unique name) and return its id.
fn write_material(db: &mut AssetDatabase, model: &str, material: &str, m: &dumb_asset::MaterialData) -> Result<AssetId, String> {
    let clean = |s: &str| s.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
    let base = if material.is_empty() || material.eq_ignore_ascii_case(model) { clean(model) } else { format!("{}_{}", clean(model), clean(material)) };
    let mut rel = format!("Materials/{base}.mat");
    let mut n = 1;
    while db.assets_root.join(&rel).exists() {
        rel = format!("Materials/{base} {n}.mat");
        n += 1;
    }
    db.write_asset(&rel, &m.to_ron())
}

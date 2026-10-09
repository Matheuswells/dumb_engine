//! Asset browser panel and asset inspector.

use crate::editor::Action;
use dumb_asset::{AssetDatabase, AssetKind, LoadState, MaterialData};
use dumb_core::AssetId;
use dumb_render::{RenderTargetId, Renderer};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct DragAsset(pub AssetId);

#[derive(Clone, Debug, PartialEq)]
enum RenameTarget {
    Asset(AssetId),
    Folder(String),
}

pub struct Thumb {
    pub texture: egui::TextureId,
    pub version: u64,
    pub target: Option<RenderTargetId>,
    pub rendered: bool,
}

#[derive(Default)]
pub struct ThumbnailCache {
    pub thumbs: HashMap<AssetId, Thumb>,
    /// Model/material thumbnails waiting to be rendered.
    pub queue: Vec<AssetId>,
    next_image_id: u64,
}

pub const THUMB_SIZE: u32 = 128;

impl ThumbnailCache {
    /// Thumbnail texture for an asset, scheduling generation if needed.
    pub fn get(&mut self, db: &mut AssetDatabase, renderer: &mut Renderer, id: AssetId) -> Option<egui::TextureId> {
        // File assets and sub-assets (textures/materials embedded in models) both get thumbnails.
        let (kind, version) = match db.entry(id) {
            Some(e) => (e.kind, e.version),
            None => (db.sub_asset(id)?.kind, db.version(id)),
        };
        if let Some(t) = self.thumbs.get(&id) {
            if t.version == version && version != 0 {
                return t.rendered.then_some(t.texture);
            }
        }
        match kind {
            AssetKind::Texture => {
                let tex = db.texture(id)?;
                let (w, h, px) = downscale(&tex.rgba8, tex.width, tex.height, THUMB_SIZE);
                let tid = match self.thumbs.get(&id) {
                    Some(t) => t.texture,
                    None => {
                        self.next_image_id += 1;
                        egui::TextureId::User(1_000_000 + self.next_image_id)
                    }
                };
                renderer.register_egui_image(tid, w, h, &px);
                self.thumbs.insert(id, Thumb { texture: tid, version: db.version(id), target: None, rendered: true });
                Some(tid)
            }
            AssetKind::Model | AssetKind::Material => {
                if kind == AssetKind::Model {
                    db.model(id)?;
                } else {
                    db.material(id)?;
                }
                let version = db.version(id);
                match self.thumbs.get_mut(&id) {
                    Some(t) => {
                        t.version = version;
                        t.rendered = false;
                    }
                    None => {
                        let target = renderer.create_target(THUMB_SIZE, THUMB_SIZE);
                        let texture = renderer.target_texture(target)?;
                        self.thumbs.insert(id, Thumb { texture, version, target: Some(target), rendered: false });
                    }
                }
                if !self.queue.contains(&id) {
                    self.queue.push(id);
                }
                None
            }
            _ => None,
        }
    }
}

fn downscale(px: &[u8], w: u32, h: u32, max: u32) -> (u32, u32, Vec<u8>) {
    if w <= max && h <= max {
        return (w, h, px.to_vec());
    }
    let s = (w.max(h) as f32 / max as f32).max(1.0);
    let (nw, nh) = (((w as f32 / s) as u32).max(1), ((h as f32 / s) as u32).max(1));
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let sx = ((x as f32 + 0.5) * s) as u32;
            let sy = ((y as f32 + 0.5) * s) as u32;
            let i = ((sy.min(h - 1) * w + sx.min(w - 1)) * 4) as usize;
            out.extend_from_slice(&px[i..i + 4]);
        }
    }
    (nw, nh, out)
}

pub struct BrowserState {
    pub folder: String,
    pub search: String,
    pub tile: f32,
    rename: Option<(RenameTarget, String)>,
    pub thumbs: ThumbnailCache,
    pub error: Option<String>,
}

impl Default for BrowserState {
    fn default() -> Self {
        BrowserState { folder: String::new(), search: String::new(), tile: 84.0, rename: None, thumbs: ThumbnailCache::default(), error: None }
    }
}

fn unique_path(db: &AssetDatabase, folder: &str, stem: &str, ext: &str) -> String {
    let mut n = 0;
    loop {
        let name = if n == 0 { format!("{stem}.{ext}") } else { format!("{stem} {n}.{ext}") };
        let rel = if folder.is_empty() { name } else { format!("{folder}/{name}") };
        if !db.assets_root.join(&rel).exists() {
            return rel;
        }
        n += 1;
    }
}

fn reveal_in_explorer(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    }
}

pub fn browser_ui(
    ui: &mut egui::Ui,
    db: &mut AssetDatabase,
    renderer: &mut Renderer,
    st: &mut BrowserState,
    selected: Option<AssetId>,
    actions: &mut Vec<Action>,
) {
    // ---- toolbar
    ui.horizontal(|ui| {
        if ui.button("📥 Import…").clicked() {
            if let Some(files) = rfd::FileDialog::new()
                .add_filter("Assets", &["glb", "gltf", "blend", "fbx", "obj", "stl", "ply", "dae", "usd", "usdz", "png", "jpg", "jpeg", "tga", "bmp", "hdr", "wav", "ogg"])
                .pick_files()
            {
                for f in files {
                    match db.import_external(&f, &st.folder) {
                        Ok(id) => log::info!("imported {}", db.display_name(id)),
                        Err(e) => st.error = Some(e),
                    }
                }
            }
        }
        ui.menu_button("➕ Create", |ui| {
            if ui.button("📁 Folder").clicked() {
                let base = if st.folder.is_empty() { String::new() } else { format!("{}/", st.folder) };
                let mut n = 0;
                let name = loop {
                    let candidate = if n == 0 { "New Folder".to_string() } else { format!("New Folder {n}") };
                    if !db.assets_root.join(format!("{base}{candidate}")).exists() {
                        break candidate;
                    }
                    n += 1;
                };
                if let Ok(rel) = db.create_folder(&st.folder, &name) {
                    st.rename = Some((RenameTarget::Folder(rel), name));
                }
                ui.close();
            }
            if ui.button("🎨 Material").clicked() {
                let rel = unique_path(db, &st.folder, "New Material", "mat");
                match db.write_asset(&rel, &MaterialData::default().to_ron()) {
                    Ok(id) => actions.push(Action::SelectAsset(id)),
                    Err(e) => st.error = Some(e),
                }
                ui.close();
            }
            if ui.button("📜 Script").on_hover_text("New Rust script in the project's Scripts crate").clicked() {
                actions.push(Action::NewScript);
                ui.close();
            }
            if ui.button("🌍 Scene").clicked() {
                let rel = unique_path(db, &st.folder, "New Scene", "scene");
                match db.write_asset(&rel, &dumb_ecs::SceneData::default().to_ron().unwrap_or_default()) {
                    Ok(id) => actions.push(Action::SelectAsset(id)),
                    Err(e) => st.error = Some(e),
                }
                ui.close();
            }
        });
        if ui.button("⟳").on_hover_text("Rescan the Assets folder").clicked() {
            db.scan();
        }
        ui.separator();
        // Breadcrumbs
        if ui.link("Assets").clicked() {
            st.folder.clear();
        }
        let parts: Vec<String> = st.folder.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect();
        for i in 0..parts.len() {
            ui.label("›");
            if ui.link(&parts[i]).clicked() {
                st.folder = parts[..=i].join("/");
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(egui::Slider::new(&mut st.tile, 48.0..=160.0).show_value(false));
            ui.add(egui::TextEdit::singleline(&mut st.search).hint_text("🔍 search  (t:model, l:label)").desired_width(220.0));
            if db.is_busy() {
                ui.spinner();
                ui.weak("importing…");
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

    egui::Panel::left("asset_folders").resizable(true).default_size(200.0).show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            folder_tree(ui, db, st, "", "Assets", actions);
        });
    });

    egui::CentralPanel::no_frame().show(ui, |ui| {
        let (folders, assets): (Vec<String>, Vec<AssetId>) = if st.search.trim().is_empty() {
            (db.folders(&st.folder), db.assets_in(&st.folder))
        } else {
            (Vec::new(), db.search(&st.search))
        };
        let tile = st.tile;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for f in folders {
                    let name = f.rsplit('/').next().unwrap_or(&f).to_string();
                    let (r, painter_rect) = tile_frame(ui, tile, false);
                    ui.painter().text(
                        painter_rect.center() - egui::vec2(0.0, 8.0),
                        egui::Align2::CENTER_CENTER,
                        "📁",
                        egui::FontId::proportional(tile * 0.45),
                        egui::Color32::from_rgb(230, 190, 90),
                    );
                    if !tile_label(ui, painter_rect, &name, st, &RenameTarget::Folder(f.clone()), db) {
                        if r.double_clicked() {
                            st.folder = f.clone();
                        }
                        if let Some(p) = r.dnd_release_payload::<DragAsset>() {
                            move_into(db, p.0, &f, st);
                        }
                        if r.dnd_hover_payload::<DragAsset>().is_some() {
                            ui.painter().rect_stroke(r.rect, 4.0, egui::Stroke::new(2.0, egui::Color32::LIGHT_BLUE), egui::StrokeKind::Inside);
                        }
                        r.context_menu(|ui| {
                            if ui.button("Open").clicked() {
                                st.folder = f.clone();
                                ui.close();
                            }
                            if ui.button("Rename").clicked() {
                                st.rename = Some((RenameTarget::Folder(f.clone()), name.clone()));
                                ui.close();
                            }
                            if ui.button("Delete").clicked() {
                                if let Err(e) = db.delete_path(&f) {
                                    st.error = Some(e);
                                }
                                ui.close();
                            }
                            if ui.button("Show in Explorer").clicked() {
                                reveal_in_explorer(&db.assets_root.join(&f));
                                ui.close();
                            }
                        });
                    }
                }
                for id in assets {
                    let Some(entry) = db.entry(id) else { continue };
                    let (kind, name, state) = (entry.kind, entry.stem().to_string(), entry.state.clone());
                    let (r, rect) = tile_frame(ui, tile, selected == Some(id));
                    let img_rect = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 4.0), egui::vec2(tile - 12.0, tile - 12.0));
                    match st.thumbs.get(db, renderer, id) {
                        Some(tex) => {
                            ui.painter().image(tex, img_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                        }
                        None => {
                            ui.painter().text(img_rect.center(), egui::Align2::CENTER_CENTER, kind.icon(), egui::FontId::proportional(tile * 0.4), egui::Color32::LIGHT_GRAY);
                        }
                    }
                    match state {
                        LoadState::Loading => {
                            ui.painter().text(img_rect.right_top() + egui::vec2(-4.0, 4.0), egui::Align2::RIGHT_TOP, "⏳", egui::FontId::proportional(14.0), egui::Color32::WHITE);
                        }
                        LoadState::Failed(_) => {
                            ui.painter().text(img_rect.right_top() + egui::vec2(-4.0, 4.0), egui::Align2::RIGHT_TOP, "⚠", egui::FontId::proportional(14.0), egui::Color32::LIGHT_RED);
                        }
                        _ => {}
                    }
                    if tile_label(ui, rect, &name, st, &RenameTarget::Asset(id), db) {
                        continue;
                    }
                    let r = r.on_hover_text(db.entry(id).map_or(String::new(), |e| format!("{}\n{:?}", e.path, e.kind)));
                    if r.drag_started() {
                        r.dnd_set_drag_payload(DragAsset(id));
                    }
                    if r.clicked() {
                        actions.push(Action::SelectAsset(id));
                    }
                    if r.double_clicked() {
                        actions.push(Action::OpenAsset(id));
                    }
                    r.context_menu(|ui| {
                        if ui.button("Open").clicked() {
                            actions.push(Action::OpenAsset(id));
                            ui.close();
                        }
                        if kind == AssetKind::Model {
                            if ui.button("Open in Animation Viewer").clicked() {
                                actions.push(Action::OpenAnimationViewer(id));
                                ui.close();
                            }
                            if ui.button("Add to scene").clicked() {
                                actions.push(Action::SpawnAsset { asset: id, at: None, parent: None });
                                ui.close();
                            }
                        }
                        if ui.button("Rename").clicked() {
                            st.rename = Some((RenameTarget::Asset(id), db.entry(id).map_or(String::new(), |e| e.name().to_string())));
                            ui.close();
                        }
                        if ui.button("Reimport").clicked() {
                            db.reimport(id);
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            if let Some(e) = db.entry(id) {
                                let p = e.path.clone();
                                if let Err(e) = db.delete_path(&p) {
                                    st.error = Some(e);
                                }
                            }
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Show in Explorer").clicked() {
                            if let Some(p) = db.abs_path(id) {
                                reveal_in_explorer(&p);
                            }
                            ui.close();
                        }
                        if ui.button("Copy path").clicked() {
                            if let Some(e) = db.entry(id) {
                                ui.ctx().copy_text(e.path.clone());
                            }
                            ui.close();
                        }
                    });
                }
            });
        });
    });
}

/// Allocate a tile. Returns (response, rect).
fn tile_frame(ui: &mut egui::Ui, tile: f32, selected: bool) -> (egui::Response, egui::Rect) {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(tile, tile + 18.0), egui::Sense::click_and_drag());
    let bg = if selected {
        ui.visuals().selection.bg_fill.gamma_multiply(0.6)
    } else if r.hovered() {
        ui.visuals().widgets.hovered.weak_bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    (r, rect)
}

/// Draw the tile label or an inline rename box. Returns true while renaming this tile.
fn tile_label(ui: &mut egui::Ui, rect: egui::Rect, name: &str, st: &mut BrowserState, target: &RenameTarget, db: &mut AssetDatabase) -> bool {
    let label_rect = egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - 18.0), rect.max);
    if let Some((t, text)) = &mut st.rename {
        if t == target {
            let edit_id = egui::Id::new(("rename", format!("{t:?}")));
            let outcome = crate::widgets::inline_text_edit(ui, edit_id, text, Some(label_rect), label_rect.width());
            if !matches!(outcome, crate::widgets::InlineEdit::Editing) {
                let mut new_name = text.trim().to_string();
                let cancelled = matches!(outcome, crate::widgets::InlineEdit::Cancel);
                let target = t.clone();
                st.rename = None;
                if !cancelled && !new_name.is_empty() {
                    let res = match target {
                        RenameTarget::Asset(id) => {
                            // Keep the extension if it was removed while typing.
                            let old = db.entry(id).map(|e| e.name().to_string()).unwrap_or_default();
                            if let Some((_, ext)) = old.rsplit_once('.') {
                                if !new_name.to_ascii_lowercase().ends_with(&format!(".{}", ext.to_ascii_lowercase())) {
                                    new_name = format!("{new_name}.{ext}");
                                }
                            }
                            if new_name == old {
                                Ok(())
                            } else {
                                db.rename_asset(id, &new_name)
                            }
                        }
                        RenameTarget::Folder(f) => {
                            let parent = f.rsplit_once('/').map_or("", |(p, _)| p);
                            let new_rel = if parent.is_empty() { new_name.clone() } else { format!("{parent}/{new_name}") };
                            if new_rel == f {
                                Ok(())
                            } else {
                                db.move_folder(&f, &new_rel)
                            }
                        }
                    };
                    if let Err(e) = res {
                        st.error = Some(e);
                    }
                }
            }
            return true;
        }
    }
    let galley = ui.painter().layout(name.to_string(), egui::FontId::proportional(11.5), ui.visuals().text_color(), rect.width() - 4.0);
    let pos = egui::pos2(label_rect.center().x - galley.size().x.min(rect.width() - 4.0) / 2.0, label_rect.min.y + 2.0);
    ui.painter().with_clip_rect(label_rect).galley(pos, galley, ui.visuals().text_color());
    false
}

fn move_into(db: &mut AssetDatabase, id: AssetId, folder: &str, st: &mut BrowserState) {
    let Some(e) = db.entry(id) else { return };
    let rel = if folder.is_empty() { e.name().to_string() } else { format!("{folder}/{}", e.name()) };
    if rel != e.path {
        if let Err(err) = db.move_asset(id, &rel) {
            st.error = Some(err);
        }
    }
}

fn folder_tree(ui: &mut egui::Ui, db: &mut AssetDatabase, st: &mut BrowserState, path: &str, name: &str, actions: &mut Vec<Action>) {
    let subs = db.folders(path);
    let selected = st.folder == path;
    let id = ui.make_persistent_id(("folder", path));
    let header = |ui: &mut egui::Ui, st: &mut BrowserState, db: &mut AssetDatabase| {
        let r = ui.add(egui::Button::selectable(selected, format!("📁 {name}")).sense(egui::Sense::click()));
        if r.clicked() {
            st.folder = path.to_string();
            st.search.clear();
        }
        if let Some(p) = r.dnd_release_payload::<DragAsset>() {
            move_into(db, p.0, path, st);
        }
        if r.dnd_hover_payload::<DragAsset>().is_some() {
            ui.painter().rect_stroke(r.rect, 2.0, egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE), egui::StrokeKind::Inside);
        }
    };
    if subs.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            header(ui, st, db);
        });
    } else {
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, path.is_empty())
            .show_header(ui, |ui| header(ui, st, db))
            .body(|ui| {
                for s in subs {
                    let n = s.rsplit('/').next().unwrap_or(&s).to_string();
                    folder_tree(ui, db, st, &s, &n, actions);
                }
            });
    }
}

/// Inspector for a selected asset: metadata, import settings, dependencies, references.
pub fn asset_inspector(ui: &mut egui::Ui, db: &mut AssetDatabase, id: AssetId, actions: &mut Vec<Action>) {
    let Some(e) = db.entry(id).cloned() else {
        ui.label("Asset not found.");
        return;
    };
    ui.heading(format!("{} {}", e.kind.icon(), e.stem()));
    ui.weak(&e.path);
    ui.separator();
    egui::Grid::new("asset_meta").num_columns(2).striped(true).show(ui, |ui| {
        ui.label("Type");
        ui.label(format!("{:?}", e.kind));
        ui.end_row();
        ui.label("Id");
        ui.label(egui::RichText::new(e.id.to_string()).monospace().small());
        ui.end_row();
        ui.label("Size");
        ui.label(format_bytes(e.size));
        ui.end_row();
        if let Some(m) = e.modified {
            ui.label("Modified");
            let ago = std::time::SystemTime::now().duration_since(m).map_or(0, |d| d.as_secs());
            ui.label(format_ago(ago));
            ui.end_row();
        }
        ui.label("State");
        match &e.state {
            LoadState::Failed(err) => {
                ui.colored_label(egui::Color32::LIGHT_RED, "Failed").on_hover_text(err);
            }
            s => {
                ui.label(format!("{s:?}"));
            }
        }
        ui.end_row();
        if let Some(t) = e.import_time {
            ui.label("Import time");
            ui.label(format!("{:.0} ms", t.as_secs_f64() * 1000.0));
            ui.end_row();
        }
        ui.label("Version");
        ui.label(e.version.to_string());
        ui.end_row();
    });
    if let LoadState::Failed(err) = &e.state {
        ui.colored_label(egui::Color32::LIGHT_RED, err);
    }

    ui.separator();
    ui.strong("Import settings");
    let mut meta_changed = false;
    let mut reimport = false;
    if let Some(m) = db.meta_mut(id) {
        meta_changed |= ui.checkbox(&mut m.preload, "Preload on project open").changed();
        if e.kind == AssetKind::Model {
            ui.horizontal(|ui| {
                ui.label("Scale");
                let r = ui.add(egui::DragValue::new(&mut m.import.scale).speed(0.01).range(0.0001..=1000.0));
                meta_changed |= r.changed();
                reimport |= r.drag_stopped() || r.lost_focus();
            });
        }
        ui.horizontal(|ui| {
            ui.label("Labels");
            let mut labels = m.labels.join(", ");
            if ui.text_edit_singleline(&mut labels).changed() {
                m.labels = labels.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                meta_changed = true;
            }
        });
    }
    if meta_changed {
        db.save_meta(id);
    }
    ui.horizontal(|ui| {
        if ui.button("⟳ Reimport").clicked() || reimport {
            db.reimport(id);
        }
        if ui.button("Open").clicked() {
            actions.push(Action::OpenAsset(id));
        }
    });

    if e.kind == AssetKind::Model {
        if let Some(m) = db.model_loaded(id) {
            ui.separator();
            ui.strong("Contents");
            ui.label(format!(
                "{} nodes · {} meshes · {} materials · {} textures\n{} skins · {} animations · {} verts · {} tris\nformat: {}",
                m.nodes.len(),
                m.meshes.len(),
                m.materials.len(),
                m.textures.len(),
                m.skins.len(),
                m.animations.len(),
                m.vertex_count(),
                m.triangle_count(),
                m.source_format
            ));
        }
    }

    ui.separator();
    ui.collapsing(format!("Dependencies ({})", e.deps.len()), |ui| {
        for d in &e.deps {
            if ui.link(db.display_name(*d)).clicked() {
                actions.push(Action::SelectAsset(*d));
            }
        }
    });
    let refs = db.references(id);
    ui.collapsing(format!("Referenced by ({})", refs.len()), |ui| {
        for r in refs {
            if ui.link(db.display_name(r)).clicked() {
                actions.push(Action::SelectAsset(r));
            }
        }
    });
}

pub fn format_bytes(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.2} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.2} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

fn format_ago(s: u64) -> String {
    match s {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

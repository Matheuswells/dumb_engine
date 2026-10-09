//! Material editor with a live 3D preview.
//!
//! Sections mirror how materials are authored: Surface, Normal, Metal/Rough, Occlusion,
//! Emission, UV and Rendering. Each texture slot shows a thumbnail, accepts textures dragged
//! from the asset browser, and sits next to the values it modulates.

use crate::asset_browser::{DragAsset, ThumbnailCache};
use crate::editor::Action;
use crate::inspector::{asset_picker, InspectCtx};
use crate::viewport::PreviewViewport;
use dumb_asset::{AlphaMode, AssetDatabase, AssetKind, MaterialData, NormalFormat, RoughnessSource};
use dumb_core::{builtin, AssetId, Color, Mat4, Quat, Vec3};
use dumb_render::{push_model, PointLightData, RenderView, Renderer};

pub struct MaterialViewer {
    pub id: AssetId,
    pub open: bool,
    pub preview: PreviewViewport,
    mesh: AssetId,
    data: Option<MaterialData>,
    loaded_version: u64,
    // Preview options (not saved in the material).
    show_point_light: bool,
    show_sky: bool,
    exposure: f32,
    sun_angle: f32,
    preview_scale: f32,
}

const SIDE: f32 = 400.0;

impl MaterialViewer {
    pub fn new(id: AssetId, renderer: &mut Renderer) -> Self {
        let mut preview = PreviewViewport::new(renderer);
        preview.camera.distance = 2.2;
        preview.camera.pitch = -0.25;
        MaterialViewer {
            id,
            open: true,
            preview,
            mesh: builtin::SPHERE,
            data: None,
            loaded_version: u64::MAX,
            show_point_light: true,
            show_sky: true,
            exposure: 1.0,
            sun_angle: 35.0,
            preview_scale: 1.0,
        }
    }

    fn editable(db: &AssetDatabase, id: AssetId) -> bool {
        db.entry(id).is_some_and(|e| e.kind == AssetKind::Material)
    }

    pub fn render_view(&mut self, db: &mut AssetDatabase) -> Option<RenderView> {
        let model = db.model(self.mesh)?;
        let mut v = RenderView::new(self.preview.target);
        v.view = self.preview.camera.view();
        v.proj = self.preview.camera.proj(self.preview.aspect());
        v.camera_pos = self.preview.camera.position();
        v.clear_color = Color::rgb(0.1, 0.105, 0.12);
        v.sky = self.show_sky;
        v.lighting.exposure = self.exposure;
        v.lighting.sun_dir = Quat::from_rotation_y(self.sun_angle.to_radians()) * Vec3::new(-0.4, -1.0, -0.3).normalize();
        if self.show_point_light {
            v.lighting.points.push(PointLightData { position: Vec3::new(1.2, 0.8, 1.4), range: 6.0, color: Color::rgb(1.0, 0.85, 0.7), intensity: 6.0 });
        }
        let base = if self.mesh == builtin::PLANE { 0.15 } else { 1.0 };
        let scale = Mat4::from_scale(Vec3::splat(base * self.preview_scale));
        push_model(&mut v, &model, self.mesh, self.id, scale, Color::WHITE, None, false, None);
        Some(v)
    }

    pub fn ui(&mut self, ctx: &egui::Context, db: &mut AssetDatabase, renderer: &mut Renderer, actions: &mut Vec<Action>, thumbs: &mut ThumbnailCache) {
        let version = db.version(self.id);
        if version != self.loaded_version || self.data.is_none() {
            if let Some(m) = db.material(self.id) {
                self.data = Some(m);
                self.loaded_version = version;
            }
        }
        let editable = Self::editable(db, self.id);
        let mut open = self.open;
        egui::Window::new(format!("🎨 Material — {}", db.display_name(self.id)))
            .id(egui::Id::new(("material_viewer", self.id)))
            .open(&mut open)
            .default_size([1000.0, 640.0])
            .show(ctx, |ui| {
                self.toolbar(ui, db, editable, actions);
                ui.separator();
                // Sizes derive from the window's current size and never exceed it, so the
                // window can't grow on its own.
                let avail = ui.available_size();
                ui.horizontal_top(|ui| {
                    let size = egui::vec2((avail.x - SIDE - 16.0).max(220.0), avail.y.max(220.0));
                    ui.vertical(|ui| {
                        self.preview.show(ui, renderer, size - egui::vec2(0.0, 26.0));
                        self.preview_controls(ui);
                    });
                    ui.allocate_ui_with_layout(egui::vec2(SIDE, avail.y.max(220.0)), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_width(SIDE);
                        let Some(mut data) = self.data.clone() else {
                            ui.spinner();
                            return;
                        };
                        let changed = egui::ScrollArea::vertical()
                            .max_width(SIDE)
                            .auto_shrink([false, false])
                            .show(ui, |ui| ui.add_enabled_ui(editable, |ui| material_ui(ui, &mut data, db, renderer, thumbs, actions)).inner)
                            .inner;
                        if changed && editable {
                            if let Err(e) = db.set_material(self.id, data.clone()) {
                                log::error!("material save failed: {e}");
                            }
                            self.loaded_version = db.version(self.id);
                        }
                        self.data = Some(data);
                    });
                });
            });
        self.open = open;
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, db: &mut AssetDatabase, editable: bool, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label("Preview:");
            for (id, n) in [(builtin::SPHERE, "Sphere"), (builtin::CUBE, "Cube"), (builtin::PLANE, "Plane"), (builtin::CYLINDER, "Cylinder")] {
                ui.selectable_value(&mut self.mesh, id, n);
            }
            ui.separator();
            if editable {
                if ui.button("⟲ Revert").on_hover_text("Reload from disk").clicked() {
                    self.loaded_version = u64::MAX;
                    db.reimport(self.id);
                }
                if ui.button("Reset").on_hover_text("Default values (keeps nothing)").clicked() {
                    if let Err(e) = db.set_material(self.id, MaterialData::default()) {
                        log::error!("{e}");
                    }
                    self.loaded_version = u64::MAX;
                }
                if ui.button("📄 Duplicate").clicked() {
                    let name = format!("{} copy", db.display_name(self.id));
                    actions.push(Action::ExtractMaterial(self.id, name));
                }
            } else {
                ui.colored_label(egui::Color32::from_rgb(240, 190, 90), "Imported material (read-only)");
                if ui.button("Extract to .mat").clicked() {
                    actions.push(Action::ExtractMaterial(self.id, db.display_name(self.id).replace('/', "_")));
                }
            }
        });
    }

    fn preview_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.toggle_value(&mut self.preview.auto_rotate, "⟲ Turntable");
            ui.toggle_value(&mut self.show_sky, "☀ Sky");
            ui.toggle_value(&mut self.show_point_light, "💡 Point light");
            ui.label("Sun");
            ui.add(egui::DragValue::new(&mut self.sun_angle).speed(1.0).suffix("°"));
            ui.label("Exposure");
            ui.add(egui::DragValue::new(&mut self.exposure).speed(0.01).range(0.1..=4.0));
            ui.label("Size");
            ui.add(egui::DragValue::new(&mut self.preview_scale).speed(0.01).range(0.1..=4.0));
        });
    }
}

// ------------------------------------------------------------------------------------------ editing

fn header(ui: &mut egui::Ui, id: &str, title: &str, add: impl FnOnce(&mut egui::Ui) -> bool) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(egui::RichText::new(title).strong()).id_salt(id).default_open(true).show(ui, |ui| {
        changed = add(ui);
    });
    changed
}

fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> bool) -> bool {
    ui.horizontal(|ui| {
        ui.add_sized([96.0, 18.0], egui::Label::new(label).truncate());
        add(ui)
    })
    .inner
}

fn color(ui: &mut egui::Ui, c: &mut Color, alpha: bool) -> bool {
    let mut a = c.to_array();
    let r = if alpha {
        ui.color_edit_button_rgba_unmultiplied(&mut a).changed()
    } else {
        let mut rgb = [a[0], a[1], a[2]];
        let ch = ui.color_edit_button_rgb(&mut rgb).changed();
        a[..3].copy_from_slice(&rgb);
        ch
    };
    *c = Color::rgba(a[0], a[1], a[2], a[3]);
    r
}

fn slider(ui: &mut egui::Ui, v: &mut f32, range: std::ops::RangeInclusive<f32>) -> bool {
    ui.add(egui::Slider::new(v, range)).changed()
}

/// Thumbnail + picker for one texture slot. Drop a texture on the thumbnail to assign it,
/// right-click it to clear.
fn texture_slot(
    ui: &mut egui::Ui,
    salt: &str,
    slot: &mut AssetId,
    db: &mut AssetDatabase,
    renderer: &mut Renderer,
    thumbs: &mut ThumbnailCache,
    actions: &mut Vec<Action>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let (r, resp) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::click());
        ui.painter().rect_filled(r, 3.0, egui::Color32::from_gray(38));
        if slot.is_none() {
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "➕", egui::FontId::proportional(16.0), egui::Color32::from_gray(110));
        } else {
            match thumbs.get(db, renderer, *slot) {
                Some(tex) => {
                    ui.painter().image(tex, r.shrink(2.0), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
                None => {
                    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "⏳", egui::FontId::proportional(16.0), egui::Color32::LIGHT_GRAY);
                }
            }
        }
        let enabled = ui.is_enabled();
        let mut resp = resp;
        if !slot.is_none() {
            let info = db.texture(*slot).map(|t| format!("\n{}×{}", t.width, t.height)).unwrap_or_default();
            resp = resp.on_hover_text(format!("{}{info}\nclick: select in browser · right-click: clear", db.display_name(*slot)));
        } else {
            resp = resp.on_hover_text("Drop a texture here");
        }
        if resp.clicked() && !slot.is_none() {
            actions.push(Action::SelectAsset(db.resolve_parent(*slot)));
        }
        if enabled {
            if resp.dnd_hover_payload::<DragAsset>().is_some() {
                ui.painter().rect_stroke(r, 3.0, egui::Stroke::new(2.0, egui::Color32::LIGHT_BLUE), egui::StrokeKind::Inside);
            }
            if let Some(p) = resp.dnd_release_payload::<DragAsset>() {
                if db.entry(p.0).is_some_and(|e| e.kind == AssetKind::Texture) {
                    *slot = p.0;
                    changed = true;
                }
            }
            if resp.secondary_clicked() && !slot.is_none() {
                *slot = AssetId::NONE;
                changed = true;
            }
        }
        ui.vertical(|ui| {
            let names = |_e: dumb_core::Entity| String::new();
            let mut ictx = InspectCtx { db, entity_name: &names, open_asset: None };
            changed |= asset_picker(ui, slot, Some("texture"), &mut ictx, egui::Id::new(("mat_tex", salt)), !enabled);
            if let Some(a) = ictx.open_asset {
                actions.push(Action::SelectAsset(a));
            }
        });
    });
    changed
}

/// The whole material editor. Returns true when anything changed.
fn material_ui(
    ui: &mut egui::Ui,
    m: &mut MaterialData,
    db: &mut AssetDatabase,
    renderer: &mut Renderer,
    thumbs: &mut ThumbnailCache,
    actions: &mut Vec<Action>,
) -> bool {
    let mut c = false;

    c |= header(ui, "surface", "Surface", |ui| {
        let mut c = texture_slot(ui, "albedo", &mut m.albedo_texture, db, renderer, thumbs, actions);
        c |= row(ui, "Color", |ui| color(ui, &mut m.albedo, true));
        c |= row(ui, "Shading", |ui| {
            let mut ch = ui.radio_value(&mut m.unlit, false, "Lit").changed();
            ch |= ui.radio_value(&mut m.unlit, true, "Unlit").changed();
            ch
        });
        c
    });

    c |= header(ui, "normal", "Normal map", |ui| {
        let mut c = texture_slot(ui, "normal", &mut m.normal_texture, db, renderer, thumbs, actions);
        c |= row(ui, "Strength", |ui| slider(ui, &mut m.normal_scale, 0.0..=3.0));
        c |= row(ui, "Format", |ui| {
            let mut ch = ui.radio_value(&mut m.normal_format, NormalFormat::OpenGL, "OpenGL (Y+)").changed();
            ch |= ui.radio_value(&mut m.normal_format, NormalFormat::DirectX, "DirectX (Y−)").changed();
            ch
        });
        ui.weak("Maps named *_dx / DirectX need DirectX; Blender and glTF use OpenGL.");
        c
    });

    c |= header(ui, "metal_rough", "Metallic / Roughness", |ui| {
        let mut c = row(ui, "Texture type", |ui| {
            let names = [(RoughnessSource::Packed, "glTF packed (G=rough, B=metal)"), (RoughnessSource::RoughnessMap, "Roughness map"), (RoughnessSource::SmoothnessMap, "Smoothness / gloss map")];
            let current = names.iter().find(|(k, _)| *k == m.roughness_source).map_or("", |(_, n)| *n);
            let mut ch = false;
            egui::ComboBox::from_id_salt("rough_src").selected_text(current).width(250.0).show_ui(ui, |ui| {
                for (k, n) in names {
                    ch |= ui.selectable_value(&mut m.roughness_source, k, n).changed();
                }
            });
            ch
        });
        if m.roughness_source == RoughnessSource::Packed {
            c |= texture_slot(ui, "mr", &mut m.metallic_roughness_texture, db, renderer, thumbs, actions);
        } else {
            ui.label(if m.roughness_source == RoughnessSource::SmoothnessMap { "Smoothness map" } else { "Roughness map" });
            c |= texture_slot(ui, "mr", &mut m.metallic_roughness_texture, db, renderer, thumbs, actions);
            ui.label("Metallic map");
            c |= texture_slot(ui, "metal", &mut m.metallic_texture, db, renderer, thumbs, actions);
        }
        c |= row(ui, "Metallic", |ui| slider(ui, &mut m.metallic, 0.0..=1.0));
        c |= row(ui, "Roughness", |ui| slider(ui, &mut m.roughness, 0.0..=1.0));
        if m.roughness_source != RoughnessSource::Packed {
            ui.weak("Sliders multiply the maps (a missing map counts as white).");
        }
        c
    });

    c |= header(ui, "ao", "Ambient occlusion", |ui| {
        let mut c = texture_slot(ui, "ao", &mut m.ao_texture, db, renderer, thumbs, actions);
        c |= row(ui, "Strength", |ui| slider(ui, &mut m.ao_strength, 0.0..=1.0));
        c
    });

    c |= header(ui, "emission", "Emission", |ui| {
        let mut c = texture_slot(ui, "emission", &mut m.emission_texture, db, renderer, thumbs, actions);
        c |= row(ui, "Color", |ui| color(ui, &mut m.emission, false));
        c |= row(ui, "Intensity", |ui| ui.add(egui::Slider::new(&mut m.emission_strength, 0.0..=100.0).logarithmic(true)).changed());
        if m.emission_texture.is_none() && m.emission == Color::BLACK {
            ui.weak("Set a color above black to make the surface glow.");
        }
        c
    });

    c |= header(ui, "uv", "UV", |ui| {
        let mut c = row(ui, "Tiling", |ui| {
            let mut ch = ui.add(egui::DragValue::new(&mut m.uv_tiling.x).speed(0.05).prefix("x ")).changed();
            ch |= ui.add(egui::DragValue::new(&mut m.uv_tiling.y).speed(0.05).prefix("y ")).changed();
            if ui.small_button("1:1").clicked() {
                m.uv_tiling = dumb_core::Vec2::ONE;
                ch = true;
            }
            for k in [2.0, 4.0, 8.0] {
                if ui.small_button(format!("{k}×")).clicked() {
                    m.uv_tiling = dumb_core::Vec2::splat(k);
                    ch = true;
                }
            }
            ch
        });
        c |= row(ui, "Offset", |ui| {
            let mut ch = ui.add(egui::DragValue::new(&mut m.uv_offset.x).speed(0.01).prefix("x ")).changed();
            ch |= ui.add(egui::DragValue::new(&mut m.uv_offset.y).speed(0.01).prefix("y ")).changed();
            ch
        });
        c
    });

    c |= header(ui, "render", "Rendering", |ui| {
        let mut c = row(ui, "Transparency", |ui| {
            let mut ch = ui.radio_value(&mut m.alpha_mode, AlphaMode::Opaque, "Opaque").changed();
            ch |= ui.radio_value(&mut m.alpha_mode, AlphaMode::Mask, "Cutout").changed();
            ch |= ui.radio_value(&mut m.alpha_mode, AlphaMode::Blend, "Blend").changed();
            ch
        });
        if m.alpha_mode == AlphaMode::Mask {
            c |= row(ui, "Cutoff", |ui| slider(ui, &mut m.alpha_cutoff, 0.0..=1.0));
        }
        c |= row(ui, "Double sided", |ui| ui.checkbox(&mut m.double_sided, "").changed());
        c
    });
    c
}

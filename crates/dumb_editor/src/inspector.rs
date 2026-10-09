//! Reflection-driven property editing. Works for engine components, script components
//! loaded from a DLL, materials and anything else implementing `Reflect`.

use crate::asset_browser::DragAsset;
use crate::hierarchy::DragEntity;
use dumb_asset::AssetDatabase;
use dumb_core::{AssetId, Color, Entity, EulerRot, Quat, Vec3};
use dumb_ecs::{ComponentId, Name, World};
use dumb_reflect::{FieldAttrs, Reflect, ReflectMut};

/// Context for widgets that need more than the value itself.
pub struct InspectCtx<'a> {
    pub db: &'a mut AssetDatabase,
    pub entity_name: &'a dyn Fn(Entity) -> String,
    /// Set when the user asks to open an asset (double-click on a reference).
    pub open_asset: Option<AssetId>,
}

pub fn pretty(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = true;
    for c in name.chars() {
        if c == '_' {
            out.push(' ');
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn drag_f32(ui: &mut egui::Ui, v: &mut f32, speed: f64, readonly: bool) -> bool {
    ui.add_enabled(!readonly, egui::DragValue::new(v).speed(speed).max_decimals(4)).changed()
}

fn vec_row(ui: &mut egui::Ui, vals: &mut [f32], speed: f64, readonly: bool) -> bool {
    let mut changed = false;
    let labels = ["X", "Y", "Z", "W"];
    let colors = [
        egui::Color32::from_rgb(200, 80, 80),
        egui::Color32::from_rgb(90, 180, 80),
        egui::Color32::from_rgb(80, 120, 220),
        egui::Color32::GRAY,
    ];
    let w = ((ui.available_width() - vals.len() as f32 * 18.0) / vals.len() as f32).max(30.0);
    for (i, v) in vals.iter_mut().enumerate() {
        ui.label(egui::RichText::new(labels[i]).color(colors[i]).strong());
        changed |= ui.add_enabled(!readonly, egui::DragValue::new(v).speed(speed).max_decimals(3)).changed();
        let _ = w;
    }
    changed
}

/// Property grid row: label on the left, widget on the right.
fn row(ui: &mut egui::Ui, label: &str, tooltip: Option<&str>, add: impl FnOnce(&mut egui::Ui) -> bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let r = ui.add_sized([110.0, 18.0], egui::Label::new(label).truncate());
        if let Some(t) = tooltip {
            r.on_hover_text(t);
        }
        changed = add(ui);
    });
    changed
}

/// Edit any reflected value. Returns true if it changed.
pub fn reflect_ui(ui: &mut egui::Ui, label: &str, value: &mut dyn Reflect, attrs: &FieldAttrs, ctx: &mut InspectCtx, salt: egui::Id) -> bool {
    if attrs.hidden {
        return false;
    }
    let ro = attrs.readonly;
    let speed = attrs.speed.unwrap_or(0.05);
    let tip = attrs.tooltip;
    match value.reflect_mut() {
        ReflectMut::Struct(s) => {
            let mut changed = false;
            let fields = s.fields();
            if label.is_empty() {
                for (i, f) in fields.iter().enumerate() {
                    changed |= reflect_ui(ui, &pretty(f.name), s.field_mut(i), &f.attrs, ctx, salt.with(i));
                }
            } else {
                egui::CollapsingHeader::new(label).id_salt(salt).default_open(true).show(ui, |ui| {
                    for (i, f) in fields.iter().enumerate() {
                        changed |= reflect_ui(ui, &pretty(f.name), s.field_mut(i), &f.attrs, ctx, salt.with(i));
                    }
                });
            }
            changed
        }
        ReflectMut::Enum(e) => row(ui, label, tip, |ui| {
            let names = e.variants();
            let mut idx = e.variant_index();
            let before = idx;
            ui.add_enabled_ui(!ro, |ui| {
                egui::ComboBox::from_id_salt(salt).selected_text(names[idx]).show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut idx, i, *n);
                    }
                });
            });
            if idx != before {
                e.set_variant_index(idx);
                true
            } else {
                false
            }
        }),
        ReflectMut::List(l) => {
            let mut changed = false;
            egui::CollapsingHeader::new(format!("{label} [{}]", l.len())).id_salt(salt).show(ui, |ui| {
                let mut remove = None;
                for i in 0..l.len() {
                    ui.horizontal(|ui| {
                        if ui.small_button("✖").clicked() {
                            remove = Some(i);
                        }
                        ui.vertical(|ui| {
                            changed |= reflect_ui(ui, &format!("[{i}]"), l.get_mut(i), &FieldAttrs::default(), ctx, salt.with(i));
                        });
                    });
                }
                if let Some(i) = remove {
                    l.remove(i);
                    changed = true;
                }
                if ui.small_button("➕ Add").clicked() {
                    l.push_default();
                    changed = true;
                }
            });
            changed
        }
        ReflectMut::Bool(v) => row(ui, label, tip, |ui| ui.add_enabled(!ro, egui::Checkbox::without_text(v)).changed()),
        ReflectMut::F32(v) => row(ui, label, tip, |ui| match attrs.range {
            Some((a, b)) => ui.add_enabled(!ro, egui::Slider::new(v, a as f32..=b as f32)).changed(),
            None => drag_f32(ui, v, speed, ro),
        }),
        ReflectMut::F64(v) => row(ui, label, tip, |ui| match attrs.range {
            Some((a, b)) => ui.add_enabled(!ro, egui::Slider::new(v, a..=b)).changed(),
            None => ui.add_enabled(!ro, egui::DragValue::new(v).speed(speed)).changed(),
        }),
        ReflectMut::I32(v) => row(ui, label, tip, |ui| int_widget(ui, v, attrs, ro)),
        ReflectMut::U32(v) => row(ui, label, tip, |ui| int_widget(ui, v, attrs, ro)),
        ReflectMut::I64(v) => row(ui, label, tip, |ui| int_widget(ui, v, attrs, ro)),
        ReflectMut::U64(v) => row(ui, label, tip, |ui| int_widget(ui, v, attrs, ro)),
        ReflectMut::String(v) => row(ui, label, tip, |ui| {
            ui.add_enabled(!ro, egui::TextEdit::singleline(v).desired_width(f32::INFINITY)).changed()
        }),
        ReflectMut::Vec2(v) => row(ui, label, tip, |ui| {
            let mut a = v.to_array();
            let c = vec_row(ui, &mut a, speed, ro);
            *v = a.into();
            c
        }),
        ReflectMut::Vec3(v) => row(ui, label, tip, |ui| {
            if attrs.color {
                let mut c = v.to_array();
                let ch = ui.color_edit_button_rgb(&mut c).changed();
                *v = c.into();
                return ch;
            }
            let mut a = v.to_array();
            let c = vec_row(ui, &mut a, speed, ro);
            *v = a.into();
            c
        }),
        ReflectMut::Vec4(v) => row(ui, label, tip, |ui| {
            let mut a = v.to_array();
            let c = if attrs.color { ui.color_edit_button_rgba_unmultiplied(&mut a).changed() } else { vec_row(ui, &mut a, speed, ro) };
            *v = a.into();
            c
        }),
        ReflectMut::Quat(q) => row(ui, label, tip, |ui| {
            // Euler angles in degrees, YXZ order (yaw, pitch, roll).
            let (y, x, z) = q.to_euler(EulerRot::YXZ);
            let mut a = [x.to_degrees(), y.to_degrees(), z.to_degrees()];
            let c = vec_row(ui, &mut a, 0.5, ro);
            if c {
                *q = Quat::from_euler(EulerRot::YXZ, a[1].to_radians(), a[0].to_radians(), a[2].to_radians()).normalize();
            }
            c
        }),
        ReflectMut::Color(c) => row(ui, label, tip, |ui| color_widget(ui, c, ro)),
        ReflectMut::Asset(a) => row(ui, label, tip, |ui| asset_picker(ui, a, attrs.asset_kind, ctx, salt, ro)),
        ReflectMut::Entity(e) => row(ui, label, tip, |ui| {
            let text = if e.is_none() { "None".to_string() } else { (ctx.entity_name)(*e) };
            let r = ui.add(egui::Button::new(format!("🔗 {text}")).min_size(egui::vec2(ui.available_width(), 0.0)));
            let mut changed = false;
            if !ro {
                if let Some(p) = r.dnd_release_payload::<DragEntity>() {
                    *e = p.0;
                    changed = true;
                }
                r.context_menu(|ui| {
                    if ui.button("Clear").clicked() {
                        *e = Entity::NONE;
                        changed = true;
                        ui.close();
                    }
                });
            }
            changed
        }),
        ReflectMut::Opaque => {
            ui.label(format!("{label}: <opaque>"));
            false
        }
    }
}

fn int_widget<T: egui::emath::Numeric>(ui: &mut egui::Ui, v: &mut T, attrs: &FieldAttrs, ro: bool) -> bool {
    match attrs.range {
        Some((a, b)) => ui.add_enabled(!ro, egui::Slider::new(v, T::from_f64(a)..=T::from_f64(b))).changed(),
        None => ui.add_enabled(!ro, egui::DragValue::new(v).speed(0.2)).changed(),
    }
}

fn color_widget(ui: &mut egui::Ui, c: &mut Color, ro: bool) -> bool {
    let mut a = c.to_array();
    let changed = ui.add_enabled_ui(!ro, |ui| ui.color_edit_button_rgba_unmultiplied(&mut a).changed()).inner;
    *c = Color::rgba(a[0], a[1], a[2], a[3]);
    changed
}

/// Combo box of assets of a kind, plus drag-and-drop from the asset browser.
pub fn asset_picker(ui: &mut egui::Ui, a: &mut AssetId, kind: Option<&str>, ctx: &mut InspectCtx, salt: egui::Id, ro: bool) -> bool {
    let mut changed = false;
    let name = ctx.db.display_name(*a);
    // Leave room for the open button and truncate long names: a combo wider than the space it
    // was given makes resizable windows/panels grow a little more every frame.
    let reserve = if a.is_none() { 0.0 } else { ui.spacing().interact_size.y + ui.spacing().item_spacing.x + 4.0 };
    let width = (ui.available_width() - reserve - 4.0).max(40.0);
    // The combo truncates its text against the *available* width, so give it a child area of
    // exactly `width`.
    let height = ui.spacing().interact_size.y;
    let ir = ui.allocate_ui_with_layout(egui::vec2(width, height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_max_width(width);
        if ro {
            ui.disable();
        }
        let pad = ui.spacing().button_padding.x;
        egui::ComboBox::from_id_salt(salt)
            .selected_text(name)
            .width(width - 2.0 * pad)
            .truncate()
            .height(320.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(a.is_none(), "None").clicked() {
                    *a = AssetId::NONE;
                    changed = true;
                }
                let list = match kind {
                    Some(k) => ctx.db.assets_of_kind(k),
                    None => ctx.db.search(""),
                };
                for id in list {
                    let n = ctx.db.display_name(id);
                    if ui.selectable_label(*a == id, n).clicked() {
                        *a = id;
                        changed = true;
                    }
                }
            })
    });
    let r = ir.inner.response;
    if !ro {
        if let Some(p) = r.dnd_release_payload::<DragAsset>() {
            let kind_ok = kind.is_none_or(|k| ctx.db.entry(p.0).is_some_and(|e| e.kind.tag() == k));
            if kind_ok {
                *a = p.0;
                changed = true;
            }
        }
    }
    if !a.is_none() && ui.small_button("↗").on_hover_text("Open").clicked() {
        ctx.open_asset = Some(ctx.db.resolve_parent(*a));
    }
    changed
}

pub enum ComponentAction {
    None,
    /// Open the source file of a script component (type name).
    EditScript(String),
    Remove(ComponentId),
    Reset(ComponentId),
    RemoveMissing(String),
}

/// Full inspector for one entity. Returns (changed, action).
pub fn entity_inspector(ui: &mut egui::Ui, world: &mut World, e: Entity, ctx: &mut InspectCtx) -> (bool, ComponentAction) {
    let mut changed = false;
    let mut action = ComponentAction::None;
    let name_id = world.component_id_of::<Name>();

    // Header: name + entity id.
    ui.horizontal(|ui| {
        if let Some(n) = world.get_mut::<Name>(e) {
            changed |= ui.add(egui::TextEdit::singleline(&mut n.name).desired_width(ui.available_width() - 80.0).font(egui::TextStyle::Heading)).changed();
        }
        ui.weak(format!("#{}", e.index));
    });
    ui.separator();

    for id in world.components_of(e) {
        if Some(id) == name_id {
            continue;
        }
        let Some(desc) = world.descriptor(id) else { continue };
        let short = desc.short_name().to_string();
        let source = desc.source.clone();
        let desc_name = desc.name.clone();
        let header = match &source {
            Some(_) => format!("📜 {short}"),
            None => format!("⚙ {short}"),
        };
        let salt = egui::Id::new(("comp", e.index, id.0));
        let resp = egui::CollapsingHeader::new(egui::RichText::new(header).strong())
            .id_salt(salt)
            .default_open(true)
            .show(ui, |ui| {
                if let Some(r) = world.get_reflect_mut(e, id) {
                    changed |= reflect_ui(ui, "", r, &FieldAttrs::default(), ctx, salt);
                }
            });
        resp.header_response.context_menu(|ui| {
            if ui.button("Reset").clicked() {
                action = ComponentAction::Reset(id);
                ui.close();
            }
            if ui.button("Remove component").clicked() {
                action = ComponentAction::Remove(id);
                ui.close();
            }
            if let Some(s) = &source {
                ui.separator();
                if ui.button("📝 Edit Script").clicked() {
                    action = ComponentAction::EditScript(desc_name.clone());
                    ui.close();
                }
                ui.weak(format!("from script library `{s}`"));
            }
        });
    }

    for (name, _) in world.missing_components(e).to_vec() {
        ui.horizontal(|ui| {
            ui.colored_label(egui::Color32::from_rgb(240, 180, 80), format!("⚠ {name} (script not loaded)"))
                .on_hover_text("The type is not registered. Its data is kept and restored when the scripts load.");
            if ui.small_button("Remove").clicked() {
                action = ComponentAction::RemoveMissing(name.clone());
            }
        });
    }
    (changed, action)
}

/// "Add Component" button with a searchable popup. Returns the chosen component id.
pub fn add_component_menu(ui: &mut egui::Ui, world: &World, e: Entity, search: &mut String) -> Option<ComponentId> {
    let mut chosen = None;
    ui.vertical_centered(|ui| {
        let btn = ui.add_sized([220.0, 26.0], egui::Button::new("➕ Add Component"));
        egui::Popup::menu(&btn).width(260.0).show(|ui| {
            ui.add(egui::TextEdit::singleline(search).hint_text("search…"));
            let s = search.to_lowercase();
            let mut types: Vec<(ComponentId, String, bool)> = world
                .component_types()
                .filter(|(id, _)| !world.has_id(e, *id))
                .map(|(id, d)| (id, d.short_name().to_string(), d.source.is_some()))
                .filter(|(_, n, _)| s.is_empty() || n.to_lowercase().contains(&s))
                .collect();
            types.sort_by(|a, b| a.2.cmp(&b.2).then(a.1.cmp(&b.1)));
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                let mut last_group = None;
                for (id, n, script) in types {
                    if last_group != Some(script) {
                        ui.weak(if script { "Scripts" } else { "Engine" });
                        last_group = Some(script);
                    }
                    if ui.button(n).clicked() {
                        chosen = Some(id);
                        ui.close();
                    }
                }
            });
        });
    });
    chosen
}

/// Compact transform readout used by viewers.
pub fn vec3_label(v: Vec3) -> String {
    format!("{:.3}, {:.3}, {:.3}", v.x, v.y, v.z)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::tests::frame;

    /// A long asset name must not make the material inspector wider than its column
    /// (that fed back into the window size every frame).
    #[test]
    fn long_texture_name_does_not_widen_the_inspector() {
        let dir = std::env::temp_dir().join(format!("dumb_insp_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Assets")).unwrap();
        let long = format!("{}.png", "an_extremely_long_texture_file_name_from_some_asset_pack".repeat(3));
        std::fs::write(dir.join("Assets").join(&long), b"not decoded in this test").unwrap();
        let mut db = AssetDatabase::open(&dir).unwrap();
        let tex = db.search("t:texture")[0];
        let mut mat = dumb_asset::MaterialData { albedo_texture: tex, ..Default::default() };
        let ctx = egui::Context::default();
        let names = |_e: Entity| String::new();
        for _ in 0..5 {
            frame(&ctx, vec![], |ui| {
                ui.allocate_ui(egui::vec2(360.0, 500.0), |ui| {
                    ui.set_width(360.0);
                    let mut ictx = InspectCtx { db: &mut db, entity_name: &names, open_asset: None };
                    reflect_ui(ui, "", &mut mat, &FieldAttrs::default(), &mut ictx, egui::Id::new("m"));
                    assert!(ui.min_rect().width() <= 360.5, "inspector grew to {}", ui.min_rect().width());
                });
            });
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

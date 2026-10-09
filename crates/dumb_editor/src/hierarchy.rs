//! Scene hierarchy: virtualized tree with drag-and-drop reparenting.

use crate::asset_browser::DragAsset;
use crate::editor::{Action, Selection};
use dumb_core::Entity;
use dumb_ecs::{Camera, Light, MeshRenderer, Name, PrefabInstance, World};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug)]
pub struct DragEntity(pub Entity);

#[derive(Default)]
pub struct HierarchyState {
    pub expanded: HashSet<Entity>,
    pub filter: String,
    pub renaming: Option<(Entity, String)>,
    cache_version: u64,
    children: HashMap<Entity, Vec<Entity>>,
    roots: Vec<Entity>,
    /// Scroll to this entity next frame.
    pub reveal: Option<Entity>,
}

impl HierarchyState {
    fn rebuild(&mut self, world: &World) {
        if self.cache_version == world.structure_version && !self.roots.is_empty() {
            return;
        }
        self.cache_version = world.structure_version;
        self.children.clear();
        self.roots.clear();
        for e in world.entities() {
            match world.parent(e) {
                Some(p) => self.children.entry(p).or_default().push(e),
                None => self.roots.push(e),
            }
        }
    }

    /// Expand all ancestors of `e`.
    pub fn reveal_entity(&mut self, world: &World, e: Entity) {
        let mut cur = e;
        while let Some(p) = world.parent(cur) {
            self.expanded.insert(p);
            cur = p;
        }
        self.reveal = Some(e);
    }
}

fn icon(world: &World, e: Entity) -> &'static str {
    if world.has::<Camera>(e) {
        "🎥"
    } else if world.has::<Light>(e) {
        "💡"
    } else if world.has::<PrefabInstance>(e) {
        "📦"
    } else if world.has::<MeshRenderer>(e) {
        "📦"
    } else {
        "⬜"
    }
}

pub fn entity_label(world: &World, e: Entity) -> String {
    world.get::<Name>(e).map(|n| n.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("Entity {}", e.index))
}

pub fn hierarchy_ui(ui: &mut egui::Ui, world: &World, st: &mut HierarchyState, sel: &Selection, actions: &mut Vec<Action>) {
    st.rebuild(world);
    ui.horizontal(|ui| {
        ui.menu_button("➕", |ui| create_menu(ui, None, actions));
        ui.add(egui::TextEdit::singleline(&mut st.filter).hint_text("🔍 search").desired_width(f32::INFINITY));
    });
    ui.separator();

    // Flatten visible rows.
    let mut rows: Vec<(Entity, usize, bool)> = Vec::new();
    if st.filter.is_empty() {
        let mut stack: Vec<(Entity, usize)> = st.roots.iter().rev().map(|e| (*e, 0)).collect();
        while let Some((e, depth)) = stack.pop() {
            let kids = st.children.get(&e);
            let has_kids = kids.is_some_and(|k| !k.is_empty());
            rows.push((e, depth, has_kids));
            if has_kids && st.expanded.contains(&e) {
                for c in kids.unwrap().iter().rev() {
                    stack.push((*c, depth + 1));
                }
            }
        }
    } else {
        let f = st.filter.to_lowercase();
        for e in world.entities() {
            if entity_label(world, e).to_lowercase().contains(&f) {
                rows.push((e, 0, false));
            }
        }
    }

    let row_h = 20.0;
    let reveal_idx = st.reveal.take().and_then(|r| rows.iter().position(|(e, _, _)| *e == r));
    let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, true]).id_salt("hierarchy_scroll");
    if let Some(i) = reveal_idx {
        scroll = scroll.vertical_scroll_offset((i as f32 * row_h - 100.0).max(0.0));
    }
    let n = rows.len();
    let out = scroll.show_rows(ui, row_h, n, |ui, range| {
        for (e, depth, has_kids) in rows[range].iter().copied() {
            let selected = sel.entities.contains(&e);
            ui.horizontal(|ui| {
                ui.set_height(row_h);
                ui.add_space(depth as f32 * 14.0);
                if has_kids {
                    let open = st.expanded.contains(&e);
                    if ui.add(egui::Button::new(if open { "⏷" } else { "⏵" }).frame(false).small()).clicked() {
                        if open {
                            st.expanded.remove(&e);
                        } else {
                            st.expanded.insert(e);
                        }
                    }
                } else {
                    ui.add_space(16.0);
                }

                if let Some((re, text)) = &mut st.renaming {
                    if *re == e {
                        match crate::widgets::inline_text_edit(ui, egui::Id::new(("rename_entity", e)), text, None, 160.0) {
                            crate::widgets::InlineEdit::Commit => {
                                actions.push(Action::Rename(e, text.clone()));
                                st.renaming = None;
                            }
                            crate::widgets::InlineEdit::Cancel => st.renaming = None,
                            crate::widgets::InlineEdit::Editing => {}
                        }
                        return;
                    }
                }

                let label = format!("{} {}", icon(world, e), entity_label(world, e));
                let r = ui.add(egui::Button::selectable(selected, label).sense(egui::Sense::click_and_drag()));
                if r.drag_started() {
                    r.dnd_set_drag_payload(DragEntity(e));
                }
                if r.clicked() {
                    let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.shift);
                    actions.push(Action::Select(e, additive));
                }
                if r.double_clicked() {
                    actions.push(Action::Focus(e));
                }
                if let Some(p) = r.dnd_release_payload::<DragEntity>() {
                    if p.0 != e {
                        actions.push(Action::Reparent(p.0, Some(e)));
                        st.expanded.insert(e);
                    }
                }
                if let Some(p) = r.dnd_release_payload::<DragAsset>() {
                    actions.push(Action::SpawnAsset { asset: p.0, at: None, parent: Some(e) });
                }
                if r.dnd_hover_payload::<DragEntity>().is_some() {
                    ui.painter().rect_stroke(r.rect, 2.0, egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE), egui::StrokeKind::Inside);
                }
                r.context_menu(|ui| {
                    if !selected {
                        actions.push(Action::Select(e, false));
                    }
                    if ui.button("Rename  (F2)").clicked() {
                        st.renaming = Some((e, entity_label(world, e)));
                        ui.close();
                    }
                    if ui.button("Duplicate  (Ctrl+D)").clicked() {
                        actions.push(Action::DuplicateSelection);
                        ui.close();
                    }
                    if ui.button("Delete  (Del)").clicked() {
                        actions.push(Action::DeleteSelection);
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button("Create child", |ui| create_menu(ui, Some(e), actions));
                    if world.parent(e).is_some() && ui.button("Unparent").clicked() {
                        actions.push(Action::Reparent(e, None));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("📦 Create prefab").clicked() {
                        actions.push(Action::CreatePrefab(e));
                        ui.close();
                    }
                    if world.has::<PrefabInstance>(e) && ui.button("Apply to prefab").clicked() {
                        actions.push(Action::ApplyPrefab(e));
                        ui.close();
                    }
                    if ui.button("Focus  (F)").clicked() {
                        actions.push(Action::Focus(e));
                        ui.close();
                    }
                });
            });
        }
    });

    // Drop on empty space: unparent / spawn at root.
    let rest = ui.allocate_rect(
        egui::Rect::from_min_max(egui::pos2(out.inner_rect.min.x, out.inner_rect.max.y.min(ui.max_rect().max.y)), ui.max_rect().max),
        egui::Sense::click(),
    );
    if rest.clicked() {
        actions.push(Action::ClearSelection);
    }
    if let Some(p) = rest.dnd_release_payload::<DragEntity>() {
        actions.push(Action::Reparent(p.0, None));
    }
    if let Some(p) = rest.dnd_release_payload::<DragAsset>() {
        actions.push(Action::SpawnAsset { asset: p.0, at: None, parent: None });
    }
    rest.context_menu(|ui| create_menu(ui, None, actions));
}

/// "Create" menu entries (shared by the hierarchy and the main menu).
pub fn create_menu(ui: &mut egui::Ui, parent: Option<Entity>, actions: &mut Vec<Action>) {
    use dumb_core::builtin;
    if ui.button("⬜ Empty").clicked() {
        actions.push(Action::CreateEmpty(parent));
        ui.close();
    }
    ui.separator();
    for (id, name) in [(builtin::CUBE, "Cube"), (builtin::SPHERE, "Sphere"), (builtin::PLANE, "Plane"), (builtin::CYLINDER, "Cylinder")] {
        if ui.button(format!("🔷 {name}")).clicked() {
            actions.push(Action::SpawnAsset { asset: id, at: None, parent });
            ui.close();
        }
    }
    ui.separator();
    if ui.button("☀ Directional light").clicked() {
        actions.push(Action::CreateLight(dumb_ecs::LightKind::Directional, parent));
        ui.close();
    }
    if ui.button("💡 Point light").clicked() {
        actions.push(Action::CreateLight(dumb_ecs::LightKind::Point, parent));
        ui.close();
    }
    if ui.button("🎥 Camera").clicked() {
        actions.push(Action::CreateCamera(parent));
        ui.close();
    }
}

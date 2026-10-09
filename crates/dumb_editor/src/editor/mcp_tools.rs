//! The editor side of the MCP tools (catalog in `mcp/tools.rs`).
//!
//! Calls run on the main thread between frames and reuse the same paths as the UI, so edits
//! join the undo history, mark scenes dirty and show up live.

use super::*;
use crate::mcp::convert::{asset_json, json_to_value, patch_serde, reflect_to_json, resolve_asset, schema, value_to_json};
use crate::mcp::{done, encode_png, ok, Args, Call, Content, ToolResult};
use dumb_asset::MaterialData;
use dumb_core::Key;
use dumb_ecs::ComponentId;
use serde_json::{json, Value as Json};

/// A tool call that finishes on a later frame.
pub(super) struct Waiting {
    call: Call,
    kind: WaitKind,
}

enum WaitKind {
    Build,
    Export,
    Step,
    Capture { view: CenterTab, frames: u8, max_width: u32, deadline: Instant },
}

enum Outcome {
    Now(ToolResult),
    Later(WaitKind),
}

impl From<ToolResult> for Outcome {
    fn from(r: ToolResult) -> Self {
        Outcome::Now(r)
    }
}

fn key_from_name(s: &str) -> Option<Key> {
    // `Key` is a fieldless `repr(u16)` enum numbered 0..=F12.
    (0..=Key::F12 as u16).map(|i| unsafe { std::mem::transmute::<u16, Key>(i) }).find(|k| format!("{k:?}").eq_ignore_ascii_case(s))
}

fn mouse_from_name(s: &str) -> Option<dumb_core::MouseButton> {
    use dumb_core::MouseButton::*;
    match s.to_ascii_lowercase().as_str() {
        "mouseleft" | "lmb" => Some(Left),
        "mouseright" | "rmb" => Some(Right),
        "mousemiddle" | "mmb" => Some(Middle),
        _ => None,
    }
}

fn r3(v: f32) -> f64 {
    (v as f64 * 1000.0).round() / 1000.0
}

fn v3(v: Vec3) -> [f64; 3] {
    [r3(v.x), r3(v.y), r3(v.z)]
}

fn camera_json(c: &EditorCamera) -> Json {
    json!({ "position": v3(c.position()), "pivot": v3(c.pivot), "distance": r3(c.distance), "yaw": r3(c.yaw.to_degrees()), "pitch": r3(c.pitch.to_degrees()), "forward": v3(c.forward()), "fov": c.fov_degrees })
}

fn play_mode_name(p: PlayMode) -> &'static str {
    match p {
        PlayMode::Edit => "edit",
        PlayMode::Playing => "playing",
        PlayMode::Paused => "paused",
    }
}

fn script_status_json(s: &ScriptStatus) -> Json {
    match s {
        ScriptStatus::NotLoaded => json!({ "state": "not_loaded" }),
        ScriptStatus::Loaded { components, systems } => json!({ "state": "loaded", "components": components, "systems": systems }),
        ScriptStatus::Building => json!({ "state": "building" }),
        ScriptStatus::BuildFailed => json!({ "state": "build_failed" }),
        ScriptStatus::Error(e) => json!({ "state": "error", "error": e }),
    }
}

impl Editor {
    /// Run (or start) one MCP tool call.
    pub fn mcp_handle(&mut self, call: Call, renderer: &mut Renderer) {
        let outcome = {
            let args = Args(&call.args);
            let tool = call.tool.clone();
            self.mcp_tool(&tool, &args, renderer)
        };
        match outcome {
            Outcome::Now(r) => call.reply(r),
            Outcome::Later(kind) => self.mcp_waiting.push(Waiting { call, kind }),
        }
    }

    /// Finish waiting calls once their frame work is done. Call after the frame was drawn.
    pub fn mcp_after_render(&mut self, renderer: &mut Renderer) {
        if self.mcp_steps > 0 && self.play == PlayMode::Paused {
            self.step_once = true;
            self.mcp_steps -= 1;
        }
        let waiting = std::mem::take(&mut self.mcp_waiting);
        for mut w in waiting {
            let finished = match &mut w.kind {
                WaitKind::Build => (!self.scripts.is_building()).then(|| self.script_status(40)),
                WaitKind::Export => (!self.build_window.is_running()).then(|| {
                    let st = self.build_window.status_json(60);
                    if st["state"] == "failed" {
                        Err(serde_json::to_string_pretty(&st).unwrap_or_default())
                    } else {
                        ok(st)
                    }
                }),
                WaitKind::Step => (self.mcp_steps == 0 && !self.step_once).then(|| done(format!("Stepped; t = {:.3}s", self.time.elapsed))),
                WaitKind::Capture { view, frames, max_width, deadline } => {
                    self.center_tab = *view;
                    if *frames > 0 {
                        *frames -= 1;
                        (Instant::now() > *deadline).then(|| Err("the view was not rendered (is the editor window minimized?)".to_string()))
                    } else {
                        let target = if *view == CenterTab::Game { self.game_target } else { self.docs[self.active].target };
                        let max_width = *max_width;
                        Some(match renderer.read_target(target) {
                            Some((w, h, px)) => encode_png(w, h, px, max_width).map(|png| vec![Content::Image(png)]),
                            None => Err("could not read the view".into()),
                        })
                    }
                }
            };
            match finished {
                Some(r) => w.call.reply(r),
                None => self.mcp_waiting.push(w),
            }
        }
    }

    // ------------------------------------------------------------------ helpers

    fn entity_arg(&self, j: &Json) -> Result<Entity, String> {
        let w = &self.docs[self.active].world;
        let by_bits = |bits: u64| {
            let e = Entity::from_bits(bits);
            if w.is_alive(e) {
                Ok(e)
            } else {
                Err(format!("no entity with id {bits} in the active scene (ids come from list_entities)"))
            }
        };
        match j {
            Json::Number(n) => by_bits(n.as_u64().ok_or("entity ids are non-negative integers")?),
            Json::String(s) => {
                if let Ok(bits) = s.trim().parse::<u64>() {
                    return by_bits(bits);
                }
                let exact: Vec<Entity> = w.entities().filter(|e| w.get::<Name>(*e).is_some_and(|n| n.name == *s)).collect();
                let found = if exact.is_empty() { w.entities().filter(|e| w.get::<Name>(*e).is_some_and(|n| n.name.eq_ignore_ascii_case(s))).collect() } else { exact };
                match found.as_slice() {
                    [e] => Ok(*e),
                    [] => Err(format!("no entity named `{s}` in the active scene")),
                    many => Err(format!("{} entities are named `{s}`; use an id: {:?}", many.len(), many.iter().map(|e| e.to_bits()).collect::<Vec<_>>())),
                }
            }
            _ => Err(format!("expected an entity id or name, got {j}")),
        }
    }

    fn entity(&self, a: &Args, k: &str) -> Result<Entity, String> {
        self.entity_arg(a.req(k)?)
    }

    fn entity_list(&self, a: &Args, k: &str) -> Result<Vec<Entity>, String> {
        match a.get(k) {
            None => Ok(Vec::new()),
            Some(Json::Array(items)) => items.iter().map(|j| self.entity_arg(j)).collect(),
            Some(j) => Ok(vec![self.entity_arg(j)?]),
        }
    }

    fn asset(&self, a: &Args, k: &str) -> Result<AssetId, String> {
        let id = resolve_asset(a.req(k)?, &self.db)?;
        if id.is_none() {
            return Err(format!("`{k}` must name an asset"));
        }
        Ok(id)
    }

    fn component(&self, name: &str) -> Result<ComponentId, String> {
        let w = &self.docs[self.active].world;
        if let Some(id) = w.component_id(name) {
            return Ok(id);
        }
        let found: Vec<ComponentId> = w.component_types().filter(|(_, d)| d.short_name().eq_ignore_ascii_case(name) || d.name.eq_ignore_ascii_case(name)).map(|(id, _)| id).collect();
        match found.as_slice() {
            [id] => Ok(*id),
            [] => {
                let mut names: Vec<&str> = w.component_types().map(|(_, d)| d.short_name()).collect();
                names.sort();
                Err(format!("no component type `{name}` (available: {})", names.join(", ")))
            }
            _ => Err(format!("`{name}` is ambiguous; use the full type name")),
        }
    }

    fn entity_summary(&self, e: Entity) -> Json {
        let w = &self.docs[self.active].world;
        let comps: Vec<&str> = w.components_of(e).into_iter().filter_map(|c| w.descriptor(c)).map(|d| d.short_name()).collect();
        json!({ "id": e.to_bits(), "name": entity_label(w, e), "components": comps })
    }

    fn entity_tree(&self, e: Entity, depth: usize, max_depth: usize, budget: &mut usize) -> Json {
        *budget = budget.saturating_sub(1);
        let mut j = self.entity_summary(e);
        let children = self.docs[self.active].world.children(e);
        if !children.is_empty() {
            if depth >= max_depth || *budget == 0 {
                j["child_count"] = json!(children.len());
            } else {
                let mut out = Vec::new();
                for c in children {
                    if *budget == 0 {
                        break;
                    }
                    out.push(self.entity_tree(c, depth + 1, max_depth, budget));
                }
                j["children"] = Json::Array(out);
            }
        }
        j
    }

    fn entity_details(&self, e: Entity) -> Json {
        let w = &self.docs[self.active].world;
        let mut comps = serde_json::Map::new();
        for c in w.components_of(e) {
            let (Some(d), Some(r)) = (w.descriptor(c), w.get_reflect(e, c)) else { continue };
            comps.insert(d.short_name().to_string(), reflect_to_json(r, &self.db));
        }
        let missing: serde_json::Map<String, Json> = w.missing_components(e).iter().map(|(n, v)| (n.clone(), value_to_json(v, &self.db))).collect();
        let m = w.global_matrix(e);
        let (scale, rot, pos) = m.to_scale_rotation_translation();
        let mut j = json!({
            "id": e.to_bits(),
            "name": entity_label(w, e),
            "parent": w.parent(e).map(|p| json!({ "id": p.to_bits(), "name": entity_label(w, p) })),
            "children": w.children(e).into_iter().map(|c| json!({ "id": c.to_bits(), "name": entity_label(w, c) })).collect::<Vec<_>>(),
            "world": { "position": v3(pos), "rotation": rot.to_array().map(r3), "scale": v3(scale) },
            "components": comps,
        });
        if let Some(t) = w.get::<Transform>(e) {
            j["rotation_euler_degrees"] = json!(v3(t.euler_degrees() + Vec3::ZERO));
        }
        if !missing.is_empty() {
            j["unloaded_components"] = Json::Object(missing);
        }
        j
    }

    /// Apply `{ field: value }` JSON to a component of `e`, adding it first if needed.
    fn set_component_json(&mut self, e: Entity, id: ComponentId, value: Option<&Json>) -> Result<(), String> {
        let added = !self.docs[self.active].world.has_id(e, id);
        let d = &mut self.docs[self.active];
        if added {
            d.world.insert_value(e, id, None);
        }
        if let Some(j) = value {
            let r = d.world.get_reflect(e, id).ok_or("component not found")?;
            match json_to_value(j, r, &self.db, "") {
                Ok(v) => dumb_reflect::apply(d.world.get_reflect_mut(e, id).ok_or("component not found")?, &v),
                Err(err) => {
                    if added {
                        d.world.remove_id(e, id);
                    }
                    return Err(err);
                }
            }
        }
        d.mark_changed();
        Ok(())
    }

    /// Resolve a path relative to the project root, refusing anything outside it.
    fn project_path(&self, rel: &str) -> Result<PathBuf, String> {
        let p = Path::new(rel.trim());
        if p.is_absolute() || p.components().any(|c| !matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir)) {
            return Err(format!("`{rel}` must be a relative path inside the project"));
        }
        Ok(self.project.root.join(p))
    }

    /// Path relative to `Assets/` when `abs` is inside it.
    fn assets_rel(&self, abs: &Path) -> Option<String> {
        abs.strip_prefix(&self.db.assets_root).ok().map(|r| r.to_string_lossy().replace('\\', "/"))
    }

    fn check_unsaved(&mut self, a: &Args) -> Result<(), String> {
        if !self.docs.iter().any(|d| d.dirty) {
            return Ok(());
        }
        match a.str("unsaved")? {
            Some("save") => {
                if self.play != PlayMode::Edit {
                    self.stop();
                }
                self.save_all_scenes();
                if self.docs.iter().any(|d| d.dirty) {
                    return Err("saving failed; see get_console_logs".into());
                }
                Ok(())
            }
            Some("discard") => Ok(()),
            Some(o) => Err(format!("`unsaved` must be \"save\" or \"discard\", got {o}")),
            None => {
                let names: Vec<&str> = self.docs.iter().filter(|d| d.dirty).map(|d| d.name.as_str()).collect();
                Err(format!("unsaved scenes: {}. Pass unsaved: \"save\" or \"discard\"", names.join(", ")))
            }
        }
    }

    fn edit_mode_only(&self, what: &str) -> Result<(), String> {
        if self.play == PlayMode::Edit {
            Ok(())
        } else {
            Err(format!("stop play mode before you {what}"))
        }
    }

    fn take_status(&mut self) -> String {
        self.status.as_ref().map(|(s, _)| s.clone()).unwrap_or_else(|| "done".into())
    }

    fn material_data(&mut self, id: AssetId) -> Result<MaterialData, String> {
        if let Some(e) = self.db.entry(id).filter(|e| e.kind == AssetKind::Material) {
            let path = e.path.clone();
            let text = self.db.read_asset_text(id).ok_or_else(|| format!("could not read {path}"))?;
            return MaterialData::from_ron(&text);
        }
        if let Some(m) = self.db.material(id) {
            return Ok(m);
        }
        if let Some(s) = self.db.sub_asset(id) {
            self.db.model(s.parent);
            return Err("the model that owns this material is still loading; try again".into());
        }
        Err("not a material".into())
    }

    fn script_status(&self, log_lines: usize) -> ToolResult {
        let s = &self.scripts;
        let systems: Vec<Json> = s
            .systems()
            .iter()
            .map(|sys| json!({ "name": sys.name, "enabled": !s.disabled.contains(&sys.name), "ms": s.timings.get(&sys.name).copied() }))
            .collect();
        let messages: Vec<Json> = s
            .messages
            .iter()
            .map(|m| json!({ "level": format!("{:?}", m.level).to_lowercase(), "file": m.file.display().to_string(), "line": m.line, "column": m.column, "text": m.text }))
            .collect();
        let errors: Vec<Json> = s.errors.iter().map(|(sys, e)| json!({ "system": sys, "error": e })).collect();
        ok(json!({
            "status": script_status_json(&s.status),
            "crate_dir": s.crate_dir().display().to_string(),
            "has_crate": s.has_crate(),
            "package": s.package,
            "building": s.is_building(),
            "last_build_seconds": s.last_build_time.map(|t| t.as_secs_f32()),
            "build_on_save": s.auto_build,
            "auto_reload": s.auto_reload,
            "components": s.component_names(),
            "systems": systems,
            "compiler_messages": messages,
            "runtime_errors": errors,
            "build_log": s.build_log.iter().rev().take(log_lines).rev().collect::<Vec<_>>(),
        }))
    }

    fn reload_scripts(&mut self) -> ToolResult {
        let mut worlds: Vec<&mut World> = self.docs.iter_mut().map(|d| &mut d.world).collect();
        self.scripts.load(&mut worlds)?;
        self.script_status(0)
    }

    // ------------------------------------------------------------------ dispatch

    fn mcp_tool(&mut self, tool: &str, a: &Args, renderer: &mut Renderer) -> Outcome {
        let r: ToolResult = match tool {
            // -------------------------------------------------- editor & project
            "get_editor_state" => {
                let d = &self.docs[self.active];
                let st = &self.stats;
                ok(json!({
                    "project": { "name": self.project.settings.name, "root": self.project.root.display().to_string(), "startup_scene": self.project.settings.startup_scene },
                    "scenes": self.docs.iter().enumerate().map(|(i, d)| json!({ "index": i, "name": d.name, "path": d.asset.and_then(|a| self.db.entry(a)).map(|e| e.path.clone()), "dirty": d.dirty, "active": i == self.active, "entities": d.world.entity_count() })).collect::<Vec<_>>(),
                    "play_mode": play_mode_name(self.play),
                    "time": { "elapsed": self.time.elapsed, "time_scale": self.time.time_scale },
                    "view": if self.center_tab == CenterTab::Game { "game" } else { "scene" },
                    "selection": d.selection.entities.iter().filter(|e| d.world.is_alive(**e)).map(|e| json!({ "id": e.to_bits(), "name": entity_label(&d.world, *e) })).collect::<Vec<_>>(),
                    "selected_asset": d.selection.asset.map(|id| asset_json(id, &self.db)),
                    "scripts": script_status_json(&self.scripts.status),
                    "gizmo": { "mode": format!("{:?}", self.gizmo.mode).to_lowercase(), "space": format!("{:?}", self.gizmo.space).to_lowercase(), "snap": self.gizmo.snap },
                    "undo_steps": d.undo.len(),
                    "redo_steps": d.redo.len(),
                    "fps": st.fps.round(),
                    "status": self.status.as_ref().map(|(s, _)| s.clone()),
                    "assets_busy": self.db.is_busy(),
                }))
            }
            "list_recent_projects" => ok(json!(recent_projects().iter().map(|p| p.display().to_string()).collect::<Vec<_>>())),
            "open_project" | "create_project" => (|| {
                let dir = PathBuf::from(a.req_str("path")?);
                if tool == "create_project" {
                    if crate::project_manager::is_project(&dir) {
                        return Err(format!("{} already contains a project; use open_project", dir.display()));
                    }
                    let name = a.str("name")?.map(str::to_string).unwrap_or_else(|| dir.file_name().map_or("Project".into(), |n| n.to_string_lossy().into_owned()));
                    crate::project_manager::create_project(&dir, &name).map_err(|e| e.to_string())?;
                } else if !crate::project_manager::is_project(&dir) {
                    return Err(format!("{} is not a Dumb Engine project (no project.ron)", dir.display()));
                }
                let same = |p: &Path| dumb_runtime::strip_unc(&p.canonicalize().unwrap_or(p.to_path_buf()));
                if same(&dir) == same(&self.project.root) {
                    return done("That project is already open");
                }
                self.check_unsaved(a)?;
                self.request = Some(ProjectRequest::Open(dir.clone()));
                done(format!("Opening {} (takes effect after this frame)", dir.display()))
            })(),
            "close_project" => self.check_unsaved(a).and_then(|_| {
                self.request = Some(ProjectRequest::Close);
                done("Closing the project")
            }),
            "get_project_settings" => serde_json::to_value(&self.project.settings).map_err(|e| e.to_string()).and_then(ok),
            "set_project_settings" => (|| {
                let patch = Json::Object(a.object("settings")?.ok_or("missing argument `settings`")?.clone());
                self.project.settings = patch_serde(&self.project.settings, &patch)?;
                self.project.save().map_err(|e| format!("could not save project.ron: {e}"))?;
                self.update_title();
                serde_json::to_value(&self.project.settings).map_err(|e| e.to_string()).and_then(ok)
            })(),
            "get_editor_settings" => serde_json::to_value(&self.settings).map_err(|e| e.to_string()).and_then(ok),
            "set_editor_settings" => (|| {
                let patch = Json::Object(a.object("settings")?.ok_or("missing argument `settings`")?.clone());
                // Saved and applied by `post_frame`, like changes from the Preferences window.
                self.settings = patch_serde(&self.settings, &patch)?;
                serde_json::to_value(&self.settings).map_err(|e| e.to_string()).and_then(ok)
            })(),
            // -------------------------------------------------- scenes
            "list_scenes" => {
                let assets: Vec<String> = self.db.search("t:scene").into_iter().filter_map(|id| self.db.entry(id).map(|e| e.path.clone())).collect();
                ok(json!({
                    "open": self.docs.iter().enumerate().map(|(i, d)| json!({ "index": i, "name": d.name, "path": d.asset.and_then(|a| self.db.entry(a)).map(|e| e.path.clone()), "dirty": d.dirty, "active": i == self.active })).collect::<Vec<_>>(),
                    "assets": assets,
                    "startup_scene": self.project.settings.startup_scene,
                }))
            }
            "new_scene" => self.edit_mode_only("open a scene").map(|_| {
                self.new_scene(renderer);
                vec![Content::Text(format!("New scene at index {}", self.active))]
            }),
            "open_scene" => (|| {
                let id = self.asset(a, "scene")?;
                if self.db.entry(id).map(|e| e.kind) != Some(AssetKind::Scene) {
                    return Err("not a scene asset".into());
                }
                self.open_scene(id, renderer);
                if self.docs[self.active].asset != Some(id) {
                    return Err(self.take_status());
                }
                done(format!("Opened {} (index {})", self.docs[self.active].name, self.active))
            })(),
            "activate_scene" => (|| {
                self.edit_mode_only("switch scenes")?;
                let i = a.usize("index")?.ok_or("missing argument `index`")?;
                if i >= self.docs.len() {
                    return Err(format!("no scene tab {i} ({} open)", self.docs.len()));
                }
                self.active = i;
                self.update_title();
                done(format!("Active scene: {}", self.docs[i].name))
            })(),
            "close_scene" => (|| {
                self.edit_mode_only("close scenes")?;
                let i = a.usize("index")?.unwrap_or(self.active);
                if i >= self.docs.len() {
                    return Err(format!("no scene tab {i}"));
                }
                if self.docs.len() == 1 {
                    return Err("the last scene tab cannot be closed; open or create another first".into());
                }
                if self.docs[i].dirty && !a.bool("discard")?.unwrap_or(false) {
                    return Err(format!("{} has unsaved changes; save it or pass discard: true", self.docs[i].name));
                }
                let d = self.docs.remove(i);
                renderer.destroy_target(d.target);
                if self.active >= i && self.active > 0 {
                    self.active -= 1;
                }
                self.active = self.active.min(self.docs.len() - 1);
                self.update_title();
                done(format!("Closed {}", d.name))
            })(),
            "save_scene" => (|| {
                self.edit_mode_only("save")?;
                let save_as = match a.str("name")? {
                    Some(n) => {
                        let n = n.trim().trim_end_matches(".scene").replace(['/', '\\', ':'], "_");
                        if n.is_empty() {
                            return Err("`name` is empty".into());
                        }
                        self.doc().name = n;
                        true
                    }
                    None => false,
                };
                self.save_active(save_as);
                if self.docs[self.active].dirty {
                    return Err(self.take_status());
                }
                done(self.take_status())
            })(),
            "save_all_scenes" => self.edit_mode_only("save").and_then(|_| {
                self.save_all_scenes();
                let failed: Vec<&str> = self.docs.iter().filter(|d| d.dirty).map(|d| d.name.as_str()).collect();
                if failed.is_empty() {
                    done("All scenes saved")
                } else {
                    Err(format!("could not save: {}", failed.join(", ")))
                }
            }),
            "set_startup_scene" => (|| {
                let path = match a.get("scene") {
                    Some(_) => {
                        let id = self.asset(a, "scene")?;
                        self.db.entry(id).filter(|e| e.kind == AssetKind::Scene).map(|e| e.path.clone()).ok_or("not a scene asset")?
                    }
                    None => self.docs[self.active].asset.and_then(|id| self.db.entry(id)).map(|e| e.path.clone()).ok_or("the active scene was never saved; save_scene first")?,
                };
                self.project.settings.startup_scene = path.clone();
                self.project.save().map_err(|e| e.to_string())?;
                done(format!("Startup scene: {path}"))
            })(),
            // -------------------------------------------------- entities
            "list_entities" => (|| {
                let limit = a.usize("limit")?.unwrap_or(500).max(1);
                let w = &self.docs[self.active].world;
                let name = a.str("name")?.map(str::to_lowercase);
                let comp = a.str("component")?.map(|c| self.component(c)).transpose()?;
                if name.is_some() || comp.is_some() {
                    let matches: Vec<Entity> = w
                        .entities()
                        .filter(|e| name.as_ref().is_none_or(|n| entity_label(w, *e).to_lowercase().contains(n.as_str())))
                        .filter(|e| comp.is_none_or(|c| w.has_id(*e, c)))
                        .collect();
                    let list: Vec<Json> = matches
                        .iter()
                        .take(limit)
                        .map(|e| {
                            let mut j = self.entity_summary(*e);
                            j["parent"] = w.parent(*e).map_or(Json::Null, |p| json!(p.to_bits()));
                            j
                        })
                        .collect();
                    return ok(json!({ "count": matches.len(), "entities": list, "truncated": matches.len() > limit }));
                }
                let max_depth = a.usize("max_depth")?.unwrap_or(usize::MAX);
                let roots = match a.get("root") {
                    Some(r) => vec![self.entity_arg(r)?],
                    None => w.roots(),
                };
                let mut budget = limit;
                let mut out = Vec::new();
                for r in roots {
                    if budget == 0 {
                        break;
                    }
                    out.push(self.entity_tree(r, 0, max_depth, &mut budget));
                }
                ok(json!({ "scene": self.docs[self.active].name, "entity_count": w.entity_count(), "entities": out, "truncated": budget == 0 }))
            })(),
            "get_entity" => self.entity(a, "entity").and_then(|e| ok(self.entity_details(e))),
            "create_entity" => (|| {
                let parent = a.get("parent").map(|p| self.entity_arg(p)).transpose()?;
                let kind = a.str("kind")?.unwrap_or("empty");
                self.actions.push(match kind {
                    "empty" => Action::CreateEmpty(parent),
                    "point_light" => Action::CreateLight(LightKind::Point, parent),
                    "directional_light" => Action::CreateLight(LightKind::Directional, parent),
                    "camera" => Action::CreateCamera(parent),
                    k => return Err(format!("unknown kind `{k}` (empty, point_light, directional_light, camera)")),
                });
                self.apply_actions(renderer);
                let e = self.docs[self.active].selection.primary().ok_or("entity was not created")?;
                self.finish_new_entity(e, a)
            })(),
            "spawn_asset" => (|| {
                let id = self.asset(a, "asset")?;
                let kind = if builtin::is_builtin(id) { Some(AssetKind::Model) } else { self.db.entry(id).map(|e| e.kind) };
                if !matches!(kind, Some(AssetKind::Model | AssetKind::Prefab)) {
                    return Err("only models and prefabs can be spawned (use open_scene for scenes, assign_material for materials)".into());
                }
                let parent = a.get("parent").map(|p| self.entity_arg(p)).transpose()?;
                let at = a.vec3("position")?;
                let e = self.spawn_asset(id, at, parent, renderer).ok_or("could not spawn the asset")?;
                let mut no_pos = a.0.clone();
                no_pos.remove("position");
                self.finish_new_entity(e, &Args(&no_pos))
            })(),
            "delete_entities" => (|| {
                let list = self.entity_list(a, "entities")?;
                let d = self.doc();
                for e in &list {
                    if d.world.is_alive(*e) {
                        d.world.despawn_recursive(*e);
                    }
                }
                d.selection.entities.retain(|e| d.world.is_alive(*e));
                d.mark_changed();
                done(format!("Deleted {} entities", list.len()))
            })(),
            "duplicate_entities" => (|| {
                let list = self.entity_list(a, "entities")?;
                self.doc().selection.entities = list;
                self.actions.push(Action::DuplicateSelection);
                self.apply_actions(renderer);
                let new: Vec<Json> = self.docs[self.active].selection.entities.clone().into_iter().map(|e| self.entity_summary(e)).collect();
                ok(json!(new))
            })(),
            "rename_entity" => (|| {
                let e = self.entity(a, "entity")?;
                let name = a.req_str("name")?.to_string();
                self.actions.push(Action::Rename(e, name));
                self.apply_actions(renderer);
                ok(self.entity_summary(e))
            })(),
            "set_parent" => (|| {
                let e = self.entity(a, "entity")?;
                let p = a.get("parent").map(|p| self.entity_arg(p)).transpose()?;
                if let Some(p) = p {
                    if p == e || self.docs[self.active].world.is_ancestor(e, p) {
                        return Err("an entity cannot be parented to itself or its own child".into());
                    }
                }
                self.actions.push(Action::Reparent(e, p));
                self.apply_actions(renderer);
                ok(self.entity_details(e))
            })(),
            "set_transform" => (|| {
                let e = self.entity(a, "entity")?;
                let quat = a.floats::<4>("rotation_quat")?;
                let (pos, euler, scale, look) = (a.vec3("position")?, a.vec3("rotation")?, a.vec3("scale")?, a.vec3("look_at")?);
                let d = self.doc();
                d.world.update_transforms();
                let parent_inv = d.world.parent(e).map(|p| d.world.global_matrix(p).inverse());
                let t = d.world.get_mut::<Transform>(e).ok_or("entity has no Transform")?;
                if let Some(p) = pos {
                    t.translation = p;
                }
                if let Some(r) = euler {
                    t.set_euler_degrees(r);
                }
                if let Some(q) = quat {
                    t.rotation = dumb_core::Quat::from_array(q).normalize();
                }
                if let Some(s) = scale {
                    t.scale = s;
                }
                if let Some(target) = look {
                    let local = parent_inv.map_or(target, |m| m.transform_point3(target));
                    t.look_at(local, Vec3::Y);
                }
                d.mark_changed();
                ok(self.entity_details(e))
            })(),
            "set_component" => (|| {
                let e = self.entity(a, "entity")?;
                let id = self.component(a.req_str("component")?)?;
                if let Some(v) = a.get("value") {
                    if !v.is_object() {
                        return Err("`value` must be an object of fields".into());
                    }
                }
                self.set_component_json(e, id, a.get("value"))?;
                let w = &self.docs[self.active].world;
                let r = w.get_reflect(e, id).ok_or("component not found")?;
                ok(json!({ "entity": e.to_bits(), "component": w.descriptor(id).map(|d| d.short_name()), "value": reflect_to_json(r, &self.db) }))
            })(),
            "remove_component" => (|| {
                let e = self.entity(a, "entity")?;
                let name = a.req_str("component")?;
                let d = &mut self.docs[self.active];
                if d.world.missing_components(e).iter().any(|(n, _)| n == name || n.rsplit("::").next() == Some(name)) {
                    let full = d.world.missing_components(e).iter().find(|(n, _)| n == name || n.rsplit("::").next() == Some(name)).map(|(n, _)| n.clone()).unwrap();
                    d.world.remove_missing(e, &full);
                    d.mark_changed();
                    return done(format!("Removed {full}"));
                }
                let id = self.component(name)?;
                let d = &mut self.docs[self.active];
                if !d.world.remove_id(e, id) {
                    return Err(format!("entity has no {name}"));
                }
                d.mark_changed();
                done(format!("Removed {name}"))
            })(),
            "list_component_types" => (|| {
                let only = a.str("component")?.map(|c| self.component(c)).transpose()?;
                let with_schema = only.is_some() || a.bool("schema")?.unwrap_or(false);
                // Default instances come from a scratch world, so the scene is untouched.
                let mut scratch = self.new_world();
                let probe = scratch.spawn();
                let mut out = Vec::new();
                let types: Vec<(ComponentId, String, String, bool)> = self.docs[self.active].world.component_types().map(|(id, d)| (id, d.name.clone(), d.short_name().to_string(), d.source.is_some())).collect();
                for (id, full, short, script) in types {
                    if only.is_some_and(|o| o != id) {
                        continue;
                    }
                    let mut j = json!({ "name": short, "type": full, "source": if script { "script" } else { "engine" } });
                    if with_schema {
                        if let Some(sid) = scratch.component_id(&full) {
                            scratch.insert_value(probe, sid, None);
                            if let Some(r) = scratch.get_reflect(probe, sid) {
                                j["schema"] = schema(r);
                                j["default"] = reflect_to_json(r, &self.db);
                            }
                        }
                    }
                    out.push(j);
                }
                out.sort_by(|x, y| x["source"].as_str().cmp(&y["source"].as_str()).then(x["name"].as_str().cmp(&y["name"].as_str())));
                ok(json!(out))
            })(),
            "select_entities" => (|| {
                let list = self.entity_list(a, "entities")?;
                let add = a.bool("add")?.unwrap_or(false);
                let d = self.doc();
                d.selection.show_asset = false;
                if !add {
                    d.selection.entities.clear();
                }
                for e in list {
                    if !d.selection.entities.contains(&e) {
                        d.selection.entities.push(e);
                    }
                    d.hierarchy.reveal_entity(&d.world, e);
                }
                done(format!("{} selected", d.selection.entities.len()))
            })(),
            "focus_entity" => self.entity(a, "entity").and_then(|e| {
                self.actions.push(Action::Focus(e));
                self.apply_actions(renderer);
                self.center_tab = CenterTab::Scene;
                done(format!("Focused {}", entity_label(&self.docs[self.active].world, e)))
            }),
            "undo" | "redo" => (|| {
                self.edit_mode_only(tool)?;
                let steps = a.usize("steps")?.unwrap_or(1).max(1);
                let d = self.doc();
                // Commit an edit still in progress so it can be undone too.
                d.end_frame(true, false);
                d.end_frame(true, false);
                let mut n = 0;
                for _ in 0..steps {
                    let before = (d.undo.len(), d.redo.len());
                    if tool == "undo" {
                        d.undo()
                    } else {
                        d.redo()
                    }
                    if (d.undo.len(), d.redo.len()) == before {
                        break;
                    }
                    n += 1;
                }
                if n == 0 {
                    return Err(format!("nothing to {tool}"));
                }
                done(format!("{} {n} step(s); {} undo / {} redo left", if tool == "undo" { "Undid" } else { "Redid" }, d.undo.len(), d.redo.len()))
            })(),
            // -------------------------------------------------- prefabs & materials
            "create_prefab" => (|| {
                self.edit_mode_only("create prefabs")?;
                let e = self.entity(a, "entity")?;
                self.actions.push(Action::CreatePrefab(e));
                self.apply_actions(renderer);
                let pi = self.docs[self.active].world.get::<PrefabInstance>(e).map(|p| p.prefab).ok_or_else(|| self.take_status())?;
                ok(json!({ "prefab": asset_json(pi, &self.db), "status": self.take_status() }))
            })(),
            "apply_prefab" => (|| {
                self.edit_mode_only("apply prefabs")?;
                let e = self.entity(a, "entity")?;
                if !self.docs[self.active].world.has::<PrefabInstance>(e) {
                    return Err("entity is not a prefab instance".into());
                }
                self.apply_prefab(e);
                done(self.take_status())
            })(),
            "assign_material" => (|| {
                let e = self.entity(a, "entity")?;
                let m = self.asset(a, "material")?;
                let d = self.doc();
                let mr = d.world.get_mut::<MeshRenderer>(e).ok_or("entity has no MeshRenderer")?;
                mr.material = m;
                d.mark_changed();
                done(format!("Assigned {}", self.db.display_name(m)))
            })(),
            "create_material" => (|| {
                let name = a.req_str("name")?.trim().trim_end_matches(".mat").replace(['/', '\\', ':'], "_");
                if name.is_empty() {
                    return Err("`name` is empty".into());
                }
                let folder = a.str("folder")?.unwrap_or("Materials").trim_matches('/').to_string();
                let mut m = MaterialData::default();
                if let Some(v) = a.get("values") {
                    let val = json_to_value(v, &m, &self.db, "")?;
                    dumb_reflect::apply(&mut m, &val);
                }
                let prefix = if folder.is_empty() { String::new() } else { format!("{folder}/") };
                let mut rel = format!("{prefix}{name}.mat");
                let mut n = 1;
                while self.db.assets_root.join(&rel).exists() {
                    rel = format!("{prefix}{name} {n}.mat");
                    n += 1;
                }
                let id = self.db.write_asset(&rel, &m.to_ron())?;
                ok(json!({ "material": asset_json(id, &self.db), "values": reflect_to_json(&m, &self.db) }))
            })(),
            "get_material" => (|| {
                let id = self.asset(a, "material")?;
                let m = self.material_data(id)?;
                let editable = self.db.entry(id).is_some_and(|e| e.kind == AssetKind::Material);
                ok(json!({ "material": asset_json(id, &self.db), "editable": editable, "values": reflect_to_json(&m, &self.db) }))
            })(),
            "set_material" => (|| {
                let id = self.asset(a, "material")?;
                if !self.db.entry(id).is_some_and(|e| e.kind == AssetKind::Material) {
                    return Err("only .mat assets can be edited; use extract_material to make an editable copy of an imported material".into());
                }
                let mut m = self.material_data(id)?;
                let val = json_to_value(a.req("values")?, &m, &self.db, "")?;
                dumb_reflect::apply(&mut m, &val);
                self.db.set_material(id, m.clone())?;
                ok(json!({ "material": asset_json(id, &self.db), "values": reflect_to_json(&m, &self.db) }))
            })(),
            "extract_material" => (|| {
                let id = self.asset(a, "material")?;
                self.material_data(id)?;
                let name = a.str("name")?.map(str::to_string).unwrap_or_else(|| self.db.display_name(id).rsplit('/').next().unwrap_or("Material").to_string());
                self.actions.push(Action::ExtractMaterial(id, name));
                self.apply_actions(renderer);
                done(self.take_status())
            })(),
            // -------------------------------------------------- assets & files
            "list_assets" => (|| {
                let limit = a.usize("limit")?.unwrap_or(500).max(1);
                let folder = a.str("folder")?.map(|f| f.trim_start_matches("Assets/").trim_matches('/').to_string());
                let kind = a.str("kind")?.map(str::to_lowercase);
                let query = a.str("query")?.unwrap_or("");
                let ids = self.db.search(query);
                let list: Vec<&dumb_asset::AssetEntry> = ids
                    .iter()
                    .filter_map(|id| self.db.entry(*id))
                    .filter(|e| folder.as_ref().is_none_or(|f| f.is_empty() || e.path.starts_with(&format!("{f}/"))))
                    .filter(|e| kind.as_ref().is_none_or(|k| e.kind.tag() == k))
                    .collect();
                let out: Vec<Json> = list.iter().take(limit).map(|e| json!({ "path": e.path, "kind": e.kind.tag(), "id": e.id.0.to_string(), "size": e.size })).collect();
                ok(json!({ "count": list.len(), "assets": out, "truncated": list.len() > limit, "builtin_models": ["Cube", "Sphere", "Plane", "Cylinder"] }))
            })(),
            "list_folders" => (|| {
                let folder = a.str("folder")?.unwrap_or("").trim_start_matches("Assets/").trim_matches('/').to_string();
                ok(json!(self.db.folders(&folder)))
            })(),
            "get_asset" => (|| {
                let id = self.asset(a, "asset")?;
                if let Some(s) = self.db.sub_asset(id) {
                    return ok(json!({ "asset": asset_json(id, &self.db), "sub_asset_of": asset_json(s.parent, &self.db), "kind": s.kind.tag(), "index": s.index }));
                }
                let mut j = match self.db.entry(id) {
                    Some(e) => json!({
                        "asset": asset_json(id, &self.db),
                        "kind": e.kind.tag(),
                        "size": e.size,
                        "state": format!("{:?}", e.state),
                        "version": e.version,
                        "import_seconds": e.import_time.map(|t| t.as_secs_f32()),
                        "meta": serde_json::to_value(&e.meta).unwrap_or_default(),
                        "dependencies": e.deps.iter().map(|d| asset_json(*d, &self.db)).collect::<Vec<_>>(),
                    }),
                    None if builtin::is_builtin(id) => json!({ "asset": asset_json(id, &self.db), "kind": "model", "builtin": true }),
                    None => return Err("unknown asset".into()),
                };
                j["referenced_by"] = json!(self.db.references(id).into_iter().map(|r| asset_json(r, &self.db)).collect::<Vec<_>>());
                let is_model = builtin::is_builtin(id) || self.db.entry(id).is_some_and(|e| e.kind == AssetKind::Model);
                if is_model {
                    match self.db.model(id) {
                        Some(m) => {
                            let b = m.aabb();
                            j["model"] = json!({
                                "source_format": m.source_format,
                                "nodes": m.nodes.len(),
                                "meshes": m.meshes.iter().map(|x| x.name.clone()).collect::<Vec<_>>(),
                                "materials": m.material_names.iter().enumerate().map(|(i, n)| json!({ "name": n, "id": id.sub("material", i).0.to_string() })).collect::<Vec<_>>(),
                                "textures": m.textures.len(),
                                "animations": m.animations.iter().map(|c| json!({ "name": c.name, "duration": c.duration })).collect::<Vec<_>>(),
                                "skinned": !m.skins.is_empty(),
                                "lod_groups": m.lods.len(),
                                "collision_proxies": m.collision_nodes.len(),
                                "bounds": { "min": b.min.to_array(), "max": b.max.to_array() },
                            });
                        }
                        None => j["model"] = json!("loading; call again for model details"),
                    }
                }
                ok(j)
            })(),
            "set_asset_meta" => (|| {
                let id = self.db.resolve_parent(self.asset(a, "asset")?);
                let old = self.db.entry(id).map(|e| e.meta.clone()).ok_or("only file assets have a .meta")?;
                let patch = Json::Object(a.object("meta")?.ok_or("missing argument `meta`")?.clone());
                if patch.get("id").is_some() {
                    return Err("an asset's id cannot be changed".into());
                }
                let new = patch_serde(&old, &patch)?;
                if let Some(m) = self.db.meta_mut(id) {
                    m.preload = new.preload;
                    m.labels = new.labels.clone();
                    m.animation_events = new.animation_events.clone();
                }
                self.db.save_meta(id);
                if new.import != old.import {
                    self.db.set_import_settings(id, new.import.clone());
                }
                ok(json!({ "asset": asset_json(id, &self.db), "meta": serde_json::to_value(&new).unwrap_or_default(), "reimporting": new.import != old.import }))
            })(),
            "reimport_asset" => self.asset(a, "asset").and_then(|id| {
                self.db.reimport(id);
                done(format!("Reimporting {}", self.db.display_name(id)))
            }),
            "import_file" => (|| {
                let src = PathBuf::from(a.req_str("source")?);
                if !src.is_file() {
                    return Err(format!("{} is not a file", src.display()));
                }
                let folder = a.str("folder")?.unwrap_or("").trim_start_matches("Assets/").trim_matches('/').to_string();
                self.project_path(&format!("Assets/{folder}"))?;
                std::fs::create_dir_all(self.db.assets_root.join(&folder)).map_err(|e| e.to_string())?;
                let id = self.db.import_external(&src, &folder)?;
                ok(json!({ "asset": asset_json(id, &self.db) }))
            })(),
            "create_folder" => (|| {
                let p = a.req_str("path")?.trim_start_matches("Assets/").trim_matches('/').to_string();
                self.project_path(&format!("Assets/{p}"))?;
                let (parent, name) = p.rsplit_once('/').unwrap_or(("", &p));
                done(format!("Created Assets/{}", self.db.create_folder(parent, name)?))
            })(),
            "move_asset" => (|| {
                let id = self.asset(a, "asset")?;
                let to = a.req_str("to")?.trim_start_matches("Assets/").trim_matches('/').to_string();
                self.project_path(&format!("Assets/{to}"))?;
                if to.contains('/') {
                    self.db.move_asset(id, &to)?;
                } else {
                    self.db.rename_asset(id, &to)?;
                }
                ok(json!({ "asset": asset_json(id, &self.db) }))
            })(),
            "delete_asset" => (|| {
                let p = a.req_str("path")?.trim_start_matches("Assets/").trim_matches('/').to_string();
                if p.is_empty() {
                    return Err("refusing to delete the whole Assets folder".into());
                }
                self.project_path(&format!("Assets/{p}"))?;
                if !self.db.assets_root.join(&p).exists() {
                    return Err(format!("Assets/{p} does not exist"));
                }
                self.db.delete_path(&p)?;
                done(format!("Moved Assets/{p} to Library/Trash"))
            })(),
            "list_files" => (|| {
                let dir = self.project_path(a.str("path")?.unwrap_or("."))?;
                let mut out: Vec<Json> = std::fs::read_dir(&dir)
                    .map_err(|e| format!("{}: {e}", dir.display()))?
                    .flatten()
                    .filter(|e| !e.file_name().to_string_lossy().ends_with(".meta"))
                    .map(|e| {
                        let md = e.metadata().ok();
                        let is_dir = md.as_ref().is_some_and(|m| m.is_dir());
                        json!({ "name": e.file_name().to_string_lossy(), "dir": is_dir, "size": if is_dir { None } else { md.map(|m| m.len()) } })
                    })
                    .collect();
                out.sort_by(|x, y| y["dir"].as_bool().cmp(&x["dir"].as_bool()).then(x["name"].as_str().cmp(&y["name"].as_str())));
                ok(json!(out))
            })(),
            "read_file" => (|| {
                let p = self.project_path(a.req_str("path")?)?;
                let md = std::fs::metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                if md.len() > 2 << 20 {
                    return Err(format!("file is {} bytes; only text files up to 2 MB can be read", md.len()));
                }
                std::fs::read_to_string(&p).map_err(|e| format!("{}: {e} (binary files cannot be read)", p.display())).map(|t| vec![Content::Text(t)])
            })(),
            "write_file" => (|| {
                let rel = a.req_str("path")?;
                let p = self.project_path(rel)?;
                let content = a.req_str("content")?;
                match self.assets_rel(&p) {
                    Some(r) if !r.is_empty() => {
                        let id = self.db.write_asset(&r, content)?;
                        if matches!(self.db.entry(id).map(|e| e.kind), Some(AssetKind::Model | AssetKind::Texture)) {
                            self.db.reimport(id);
                        }
                        ok(json!({ "written": rel, "bytes": content.len(), "asset": asset_json(id, &self.db) }))
                    }
                    _ => {
                        if let Some(dir) = p.parent() {
                            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                        }
                        std::fs::write(&p, content).map_err(|e| format!("{}: {e}", p.display()))?;
                        if p.starts_with(self.scripts.crate_dir()) {
                            self.scripts_ui.invalidate();
                        }
                        ok(json!({ "written": rel, "bytes": content.len() }))
                    }
                }
            })(),
            // -------------------------------------------------- scripts
            "get_script_status" => a.usize("log_lines").and_then(|n| self.script_status(n.unwrap_or(40))),
            "build_scripts" => {
                return (|| {
                    if !self.scripts.has_crate() {
                        return Err("this project has no scripts crate yet; create_script creates it".to_string());
                    }
                    if !self.scripts.is_building() {
                        if a.bool("clean")?.unwrap_or(false) {
                            self.scripts.clean_build();
                        } else {
                            self.scripts.start_build();
                        }
                    }
                    if a.bool("wait")?.unwrap_or(true) {
                        Ok(Outcome::Later(WaitKind::Build))
                    } else {
                        Ok(Outcome::Now(done("Build started; check get_script_status")))
                    }
                })()
                .unwrap_or_else(|e| Outcome::Now(Err(e)));
            }
            "reload_scripts" => self.reload_scripts(),
            "unload_scripts" => {
                let mut worlds: Vec<&mut World> = self.docs.iter_mut().map(|d| &mut d.world).collect();
                self.scripts.unload(&mut worlds);
                done("Scripts unloaded (script components are kept as data)")
            }
            "create_script" => (|| {
                use dumb_script::project::{create_script, ScriptTemplate};
                let name = a.req_str("name")?;
                let template = match a.str("template")?.unwrap_or("behaviour") {
                    "behaviour" | "behavior" => ScriptTemplate::Behaviour,
                    "component" => ScriptTemplate::Component,
                    "system" => ScriptTemplate::System,
                    "empty" => ScriptTemplate::Empty,
                    t => return Err(format!("unknown template `{t}` (behaviour, component, system, empty)")),
                };
                if !self.ensure_scripts_crate() {
                    return Err(self.take_status());
                }
                let path = create_script(self.scripts.crate_dir(), name, template)?;
                self.scripts_ui.invalidate();
                self.scripts.mark_sources_seen();
                if a.bool("build")?.unwrap_or(true) {
                    self.scripts.start_build();
                }
                let rel = path.strip_prefix(&self.project.root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                let source = std::fs::read_to_string(&path).unwrap_or_default();
                ok(json!({ "path": rel, "building": self.scripts.is_building(), "source": source }))
            })(),
            "set_system_enabled" => (|| {
                let name = a.req_str("system")?;
                let enabled = a.bool("enabled")?.ok_or("missing argument `enabled`")?;
                if !self.scripts.systems().iter().any(|s| s.name == name) {
                    let names: Vec<&str> = self.scripts.systems().iter().map(|s| s.name.as_str()).collect();
                    return Err(format!("no system `{name}` (systems: {})", names.join(", ")));
                }
                if enabled {
                    self.scripts.disabled.remove(name);
                } else {
                    self.scripts.disabled.insert(name.to_string());
                }
                done(format!("{name} {}", if enabled { "enabled" } else { "disabled" }))
            })(),
            "set_script_options" => (|| {
                if let Some(b) = a.bool("build_on_save")? {
                    self.scripts.auto_build = b;
                }
                if let Some(b) = a.bool("auto_reload")? {
                    self.scripts.auto_reload = b;
                }
                ok(json!({ "build_on_save": self.scripts.auto_build, "auto_reload": self.scripts.auto_reload }))
            })(),
            // -------------------------------------------------- play mode
            "play" => {
                if self.play != PlayMode::Playing {
                    self.play();
                }
                done(format!("Playing ({})", self.docs[self.active].name))
            }
            "pause" => match self.play {
                PlayMode::Playing => {
                    self.play();
                    done("Paused")
                }
                PlayMode::Paused => done("Already paused"),
                PlayMode::Edit => Err("not playing".into()),
            },
            "stop" => {
                if self.play == PlayMode::Edit {
                    Err("not playing".into())
                } else {
                    self.stop();
                    done("Stopped; scene restored")
                }
            }
            "step" => {
                return match (self.play, a.usize("frames")) {
                    (_, Err(e)) => Outcome::Now(Err(e)),
                    (PlayMode::Paused, Ok(n)) => {
                        self.mcp_steps += n.unwrap_or(1).clamp(1, 10_000) as u32;
                        Outcome::Later(WaitKind::Step)
                    }
                    _ => Outcome::Now(Err("pause the game first (play, then pause)".into())),
                };
            }
            "set_time_scale" => a.f64("scale").and_then(|s| {
                let s = s.ok_or("missing argument `scale`")?;
                self.time.time_scale = (s as f32).clamp(0.0, 4.0);
                done(format!("Time scale {}", self.time.time_scale))
            }),
            "send_input" => (|| {
                if self.play == PlayMode::Edit {
                    return Err("input only reaches the game while playing".into());
                }
                if a.bool("release_all")?.unwrap_or(false) {
                    self.input.clear();
                }
                for (list, down) in [(a.strings("press")?, true), (a.strings("release")?, false)] {
                    for k in list {
                        if let Some(key) = key_from_name(&k) {
                            self.input.set_key(key, down);
                        } else if let Some(b) = mouse_from_name(&k) {
                            self.input.set_mouse(b, down);
                        } else {
                            return Err(format!("unknown key `{k}`"));
                        }
                    }
                }
                if let Some([x, y]) = a.floats::<2>("mouse_position")? {
                    self.input.mouse_pos = Vec2::new(x, y);
                }
                if let Some([x, y]) = a.floats::<2>("mouse_delta")? {
                    self.input.mouse_delta = Vec2::new(x, y);
                }
                if let Some(s) = a.f64("scroll")? {
                    self.input.scroll = s as f32;
                }
                done("Input applied")
            })(),
            // -------------------------------------------------- console & stats
            "get_console_logs" => (|| {
                let min = match a.str("level")?.unwrap_or("info") {
                    "error" => log::Level::Error,
                    "warn" | "warning" => log::Level::Warn,
                    "info" => log::Level::Info,
                    "debug" => log::Level::Debug,
                    "trace" => log::Level::Trace,
                    l => return Err(format!("unknown level `{l}`")),
                };
                let source = a.str("source")?.unwrap_or("all");
                let contains = a.str("contains")?.map(str::to_lowercase);
                let limit = a.usize("limit")?.unwrap_or(100);
                let buf = self.log.0.lock().map_err(|_| "console unavailable")?;
                let lines: Vec<Json> = buf
                    .iter()
                    .filter(|l| l.level <= min)
                    .filter(|l| match source {
                        "scripts" => l.is_script(),
                        "engine" => !l.is_script(),
                        _ => true,
                    })
                    .filter(|l| contains.as_ref().is_none_or(|c| l.message.to_lowercase().contains(c.as_str())))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .take(limit)
                    .rev()
                    .map(|l| {
                        let mut j = json!({ "level": l.level.as_str().to_lowercase(), "source": l.source(), "message": l.message });
                        if l.repeat > 1 {
                            j["repeat"] = json!(l.repeat);
                        }
                        if let (Some(f), Some(n)) = (&l.file, l.line) {
                            j["at"] = json!(format!("{f}:{n}"));
                        }
                        j
                    })
                    .collect();
                ok(json!(lines))
            })(),
            "clear_console" => {
                if let Ok(mut b) = self.log.0.lock() {
                    b.clear();
                }
                done("Console cleared")
            }
            "get_performance_stats" => {
                let st = &self.stats;
                let rs = renderer.stats;
                let m = &self.monitor.sample;
                let scopes: Vec<Json> = dumb_core::profiler::last_frame()
                    .map(|f| f.scopes.iter().filter(|s| s.depth == 0 && s.thread == 0).map(|s| json!({ "name": s.name, "ms": (s.dur_us as f64 / 1000.0 * 1000.0).round() / 1000.0 })).collect())
                    .unwrap_or_default();
                ok(json!({
                    "fps": st.fps.round(),
                    "frame_ms": st.frame_ms,
                    "simulation_ms": st.sim_ms,
                    "extract_ms": st.extract_ms,
                    "render": { "views": rs.views, "draw_calls": rs.draw_calls, "objects": rs.instances, "triangles": rs.triangles, "culled": st.culled, "shadow_draws": rs.shadow_draws, "gpu_models": rs.gpu_models, "gpu_textures": rs.gpu_textures, "deferred_uploads": rs.deferred_uploads },
                    "entities": self.docs[self.active].world.entity_count(),
                    "physics": self.physics.as_ref().map(|p| json!({ "bodies": p.stats.bodies, "colliders": p.stats.colliders, "step_ms": p.stats.step_ms })),
                    "ai": { "agents": st.ai.agents, "thinking": st.ai.thinking, "deferred": st.ai.deferred, "sleeping": st.ai.sleeping },
                    "streaming": { "cells": st.stream.cells, "loaded": st.stream.loaded, "loading": st.stream.loading, "ms": st.stream.ms },
                    "system": { "cpu_process": m.process_cpu, "cpu_system": m.system_cpu, "cores": m.cores, "ram_process_mb": m.process_ram >> 20, "ram_used_mb": m.ram_used >> 20, "ram_total_mb": m.ram_total >> 20, "vram_used_mb": m.vram_used >> 20, "vram_budget_mb": m.vram_budget >> 20, "gpu_ms": m.gpu_ms },
                    "gpu": renderer.device_name(),
                    "scripts_ms": self.scripts.total_time_ms(),
                    "frame_scopes": scopes,
                }))
            }
            // -------------------------------------------------- view
            "capture_view" => {
                let view = match a.str("view") {
                    Ok(Some("scene")) => CenterTab::Scene,
                    Ok(Some("game")) => CenterTab::Game,
                    Ok(None) => self.center_tab,
                    Ok(Some(v)) => return Outcome::Now(Err(format!("unknown view `{v}` (scene, game)"))),
                    Err(e) => return Outcome::Now(Err(e)),
                };
                let max_width = match a.usize("max_width") {
                    Ok(w) => w.unwrap_or(1024) as u32,
                    Err(e) => return Outcome::Now(Err(e)),
                };
                self.center_tab = view;
                // One frame to lay out (and resize) the view, one to render it.
                return Outcome::Later(WaitKind::Capture { view, frames: 2, max_width, deadline: Instant::now() + std::time::Duration::from_secs(10) });
            }
            "get_scene_camera" => {
                let c = &self.docs[self.active].camera;
                ok(camera_json(c))
            }
            "set_scene_camera" => (|| {
                let (pos, look) = (a.vec3("position")?, a.vec3("look_at")?);
                let (pivot, dist, yaw, pitch) = (a.vec3("pivot")?, a.f64("distance")?, a.f64("yaw")?, a.f64("pitch")?);
                let c = &mut self.docs[self.active].camera;
                match (pos, look) {
                    (Some(p), Some(t)) => {
                        let f = (t - p).normalize_or_zero();
                        if f == Vec3::ZERO {
                            return Err("`position` and `look_at` must differ".into());
                        }
                        c.pivot = t;
                        c.distance = (t - p).length();
                        c.pitch = f.y.clamp(-1.0, 1.0).asin();
                        c.yaw = (-f.x).atan2(-f.z);
                    }
                    (Some(p), None) => c.pivot = p + c.forward() * c.distance,
                    (None, Some(t)) => c.pivot = t,
                    (None, None) => {}
                }
                if let Some(p) = pivot {
                    c.pivot = p;
                }
                if let Some(d) = dist {
                    c.distance = (d as f32).max(0.01);
                }
                if let Some(y) = yaw {
                    c.yaw = (y as f32).to_radians();
                }
                if let Some(p) = pitch {
                    c.pitch = (p as f32).to_radians().clamp(-1.55, 1.55);
                }
                let j = camera_json(c);
                self.center_tab = CenterTab::Scene;
                ok(j)
            })(),
            "set_gizmo" => (|| {
                match a.str("mode")? {
                    Some("translate") => self.gizmo.mode = GizmoMode::Translate,
                    Some("rotate") => self.gizmo.mode = GizmoMode::Rotate,
                    Some("scale") => self.gizmo.mode = GizmoMode::Scale,
                    Some(m) => return Err(format!("unknown mode `{m}`")),
                    None => {}
                }
                match a.str("space")? {
                    Some("world") => self.gizmo.space = GizmoSpace::World,
                    Some("local") => self.gizmo.space = GizmoSpace::Local,
                    Some(s) => return Err(format!("unknown space `{s}`")),
                    None => {}
                }
                if let Some(s) = a.bool("snap")? {
                    self.gizmo.snap = s;
                }
                ok(json!({ "mode": format!("{:?}", self.gizmo.mode).to_lowercase(), "space": format!("{:?}", self.gizmo.space).to_lowercase(), "snap": self.gizmo.snap }))
            })(),
            "open_window" => (|| {
                match a.req_str("window")? {
                    "scene" => self.center_tab = CenterTab::Scene,
                    "game" => self.center_tab = CenterTab::Game,
                    "assets" => self.bottom_tab = BottomTab::Assets,
                    "console" => self.bottom_tab = BottomTab::Console,
                    "scripts" => self.bottom_tab = BottomTab::Scripts,
                    "preferences" => self.prefs_window.open = true,
                    "build" => self.build_window.open = true,
                    "profiler" => self.profiler_window.open = true,
                    "web_browser" => match a.str("url")? {
                        Some(u) => self.web_browser.open_url(u),
                        None => self.web_browser.open = true,
                    },
                    "code_editor" => match a.str("path")? {
                        Some(p) => {
                            let abs = self.project_path(p)?;
                            if !abs.is_file() {
                                return Err(format!("{p} is not a file"));
                            }
                            self.code_editor.open_file(&abs, a.usize("line")?.map(|l| l as u32));
                        }
                        None => self.code_editor.open = true,
                    },
                    "material" => {
                        let id = self.asset(a, "asset")?;
                        self.actions.push(Action::OpenMaterial(id));
                        self.apply_actions(renderer);
                    }
                    "model" => {
                        let id = self.asset(a, "asset")?;
                        self.actions.push(Action::OpenAsset(id));
                        self.apply_actions(renderer);
                    }
                    "animation" => {
                        let id = self.asset(a, "asset")?;
                        self.actions.push(Action::OpenAnimationViewer(id));
                        self.apply_actions(renderer);
                    }
                    w => return Err(format!("unknown window `{w}`")),
                }
                done("Shown")
            })(),
            // -------------------------------------------------- build & export
            "build_export" => {
                return (|| {
                    {
                        let b = &mut self.project.settings.build;
                        if let Some(v) = a.bool("release")? {
                            b.release = v;
                        }
                        if let Some(v) = a.bool("include_scripts")? {
                            b.include_scripts = v;
                        }
                        if let Some(v) = a.str("out_dir")? {
                            b.out_dir = v.to_string();
                        }
                        if let Some(v) = a.bool("run_after")? {
                            b.run_after = v;
                        }
                    }
                    self.project.save().map_err(|e| format!("could not save project.ron: {e}"))?;
                    if self.play != PlayMode::Edit {
                        return Err("stop play mode before exporting".into());
                    }
                    self.save_all_scenes();
                    let out = self.build_window.start_export(&self.project)?;
                    self.build_window.open = true;
                    if a.bool("wait")?.unwrap_or(false) {
                        Ok(Outcome::Later(WaitKind::Export))
                    } else {
                        Ok(Outcome::Now(done(format!("Export started into {}; check get_export_status", out.display()))))
                    }
                })()
                .unwrap_or_else(|e| Outcome::Now(Err(e)));
            }
            "get_export_status" => a.usize("log_lines").and_then(|n| ok(self.build_window.status_json(n.unwrap_or(40)))),
            _ => Err(format!("tool `{tool}` needs a project to be open")),
        };
        Outcome::Now(r)
    }

    /// Name, transform and extra components for `create_entity` / `spawn_asset`.
    fn finish_new_entity(&mut self, e: Entity, a: &Args) -> ToolResult {
        let result = (|| {
            if let Some(n) = a.str("name")? {
                let w = &mut self.docs[self.active].world;
                match w.get_mut::<Name>(e) {
                    Some(name) => name.name = n.to_string(),
                    None => w.insert(e, Name { name: n.to_string() }),
                }
            }
            let (pos, rot, scale) = (a.vec3("position")?, a.vec3("rotation")?, a.vec3("scale")?);
            if let Some(t) = self.docs[self.active].world.get_mut::<Transform>(e) {
                if let Some(p) = pos {
                    t.translation = p;
                }
                if let Some(r) = rot {
                    t.set_euler_degrees(r);
                }
                if let Some(s) = scale {
                    t.scale = s;
                }
            }
            if let Some(comps) = a.object("components")? {
                for (name, value) in comps {
                    let id = self.component(name)?;
                    self.set_component_json(e, id, Some(value)).map_err(|err| format!("{name}: {err}"))?;
                }
            }
            Ok(())
        })();
        if let Err(err) = result {
            // Don't leave a half-made entity behind.
            let d = self.doc();
            d.world.despawn_recursive(e);
            d.selection.entities.retain(|x| *x != e);
            return Err(err);
        }
        self.docs[self.active].mark_changed();
        ok(self.entity_details(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        assert_eq!(key_from_name("space"), Some(Key::Space));
        assert_eq!(key_from_name("W"), Some(Key::W));
        assert_eq!(key_from_name("F12"), Some(Key::F12));
        assert_eq!(key_from_name("Num0"), Some(Key::Num0));
        assert_eq!(key_from_name("nope"), None);
        assert!(mouse_from_name("MouseLeft").is_some());
    }
}

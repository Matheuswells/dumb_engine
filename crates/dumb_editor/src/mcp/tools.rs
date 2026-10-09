//! The MCP tool catalog: names, descriptions and input schemas.
//! The editor side of each tool lives in `editor/mcp_tools.rs`.

use serde_json::{json, Map, Value as Json};
use std::sync::OnceLock;

fn entity() -> Json {
    json!({ "type": ["integer", "string"], "description": "Entity id (from list_entities) or a unique entity name" })
}

fn entities() -> Json {
    json!({ "type": "array", "items": entity(), "description": "Entity ids or unique names" })
}

fn asset(what: &str) -> Json {
    json!({ "type": "string", "description": format!("{what}: path under Assets/ (e.g. \"Models/Tree.glb\"), UUID, or built-in name (Cube, Sphere, Plane, Cylinder)") })
}

fn vec3(what: &str) -> Json {
    json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": what })
}

fn unsaved() -> Json {
    json!({ "type": "string", "enum": ["save", "discard"], "description": "What to do with unsaved scenes. Without it the call fails when scenes have unsaved changes." })
}

struct Def {
    name: &'static str,
    desc: &'static str,
    props: Json,
    required: &'static [&'static str],
    read_only: bool,
    destructive: bool,
}

fn def(name: &'static str, desc: &'static str, props: Json, required: &'static [&'static str]) -> Def {
    Def { name, desc, props, required, read_only: false, destructive: false }
}

fn read(name: &'static str, desc: &'static str, props: Json, required: &'static [&'static str]) -> Def {
    Def { read_only: true, ..def(name, desc, props, required) }
}

fn destructive(name: &'static str, desc: &'static str, props: Json, required: &'static [&'static str]) -> Def {
    Def { destructive: true, ..def(name, desc, props, required) }
}

fn all() -> Vec<Def> {
    vec![
        // ------------------------------------------------------------ editor & project
        read("get_editor_state", "Overview of the editor: open project, open scenes, the active scene, play mode, selection, script status, gizmo and frame stats. Start here.", json!({}), &[]),
        read("list_recent_projects", "Recently opened projects.", json!({}), &[]),
        def("open_project", "Open another project folder (the editor switches to it after this frame).", json!({ "path": { "type": "string", "description": "Project folder (contains project.ron and Assets/)" }, "unsaved": unsaved() }), &["path"]),
        def("create_project", "Create a new project folder with a default scene and open it.", json!({ "path": { "type": "string", "description": "New project folder" }, "name": { "type": "string", "description": "Project name (default: folder name)" }, "unsaved": unsaved() }), &["path"]),
        def("close_project", "Close the project and return to the start screen.", json!({ "unsaved": unsaved() }), &[]),
        read("get_project_settings", "The project's settings (project.ron): name, startup scene, physics, frame cap, window, build options, AI and streaming.", json!({}), &[]),
        def("set_project_settings", "Change project settings. Pass only the fields to change (nested objects merge). Saved to project.ron.", json!({ "settings": { "type": "object", "description": "Partial settings, same shape as get_project_settings" } }), &["settings"]),
        read("get_editor_settings", "Editor preferences: frame caps, vsync, theme, UI scale, viewport toggles (grid, wireframe, bounds, colliders, icons), camera, snapping, scripts, play mode and the MCP server.", json!({}), &[]),
        def("set_editor_settings", "Change editor preferences. Pass only the fields to change. Saved per user.", json!({ "settings": { "type": "object", "description": "Partial settings, same shape as get_editor_settings" } }), &["settings"]),
        // ------------------------------------------------------------ scenes
        read("list_scenes", "Open scene tabs (index, name, path, dirty, active) and every scene asset in the project.", json!({}), &[]),
        def("new_scene", "Open a new untitled scene with a sun, a camera and a ground plane.", json!({}), &[]),
        def("open_scene", "Open a scene asset in a tab (or switch to it if already open).", json!({ "scene": asset("Scene asset") }), &["scene"]),
        def("activate_scene", "Switch to an open scene tab.", json!({ "index": { "type": "integer", "description": "Tab index from list_scenes" } }), &["index"]),
        destructive("close_scene", "Close an open scene tab.", json!({ "index": { "type": "integer", "description": "Tab index (default: active)" }, "discard": { "type": "boolean", "description": "Close even with unsaved changes" } }), &[]),
        def("save_scene", "Save the active scene. New scenes are written to Scenes/<name>.scene.", json!({ "name": { "type": "string", "description": "Save under a new name (Save As)" } }), &[]),
        def("save_all_scenes", "Save every scene with unsaved changes.", json!({}), &[]),
        def("set_startup_scene", "Make a scene the one the game and editor open first.", json!({ "scene": asset("Scene asset (default: the active scene)") }), &[]),
        // ------------------------------------------------------------ entities
        read("list_entities", "Entities of the active scene as a hierarchy, with ids, names and component names.", json!({
            "name": { "type": "string", "description": "Only entities whose name contains this (case-insensitive); results are flat" },
            "component": { "type": "string", "description": "Only entities with this component; results are flat" },
            "root": entity(),
            "max_depth": { "type": "integer", "description": "Hierarchy depth to include (default: all)" },
            "limit": { "type": "integer", "description": "Most entities to return (default 500)" }
        }), &[]),
        read("get_entity", "Every component of an entity with its values, plus parent, children and world position.", json!({ "entity": entity() }), &["entity"]),
        def("create_entity", "Create an entity. `kind` adds the usual components for lights and cameras.", json!({
            "kind": { "type": "string", "enum": ["empty", "point_light", "directional_light", "camera"], "description": "Default: empty" },
            "name": { "type": "string" },
            "parent": entity(),
            "position": vec3("Local position"),
            "rotation": vec3("Local rotation as Euler angles in degrees"),
            "scale": vec3("Local scale"),
            "components": { "type": "object", "description": "Extra components to add: { \"ComponentName\": {fields...} }" }
        }), &[]),
        def("spawn_asset", "Place a model or prefab in the active scene.", json!({ "asset": asset("Model or prefab"), "position": vec3("World position (default: in front of the scene camera)"), "parent": entity(), "name": { "type": "string" } }), &["asset"]),
        destructive("delete_entities", "Delete entities and their children.", json!({ "entities": entities() }), &["entities"]),
        def("duplicate_entities", "Duplicate entities (with children). Returns the new ids.", json!({ "entities": entities() }), &["entities"]),
        def("rename_entity", "Rename an entity.", json!({ "entity": entity(), "name": { "type": "string" } }), &["entity", "name"]),
        def("set_parent", "Re-parent an entity, keeping its world transform. Use null parent for the root.", json!({ "entity": entity(), "parent": { "type": ["integer", "string", "null"], "description": "New parent, or null" } }), &["entity", "parent"]),
        def("set_transform", "Set an entity's local position, rotation (Euler degrees or quaternion) and/or scale.", json!({
            "entity": entity(),
            "position": vec3("Local position"),
            "rotation": vec3("Euler angles in degrees"),
            "rotation_quat": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4, "description": "Quaternion [x, y, z, w]" },
            "scale": vec3("Local scale"),
            "look_at": vec3("Rotate to face this world point")
        }), &["entity"]),
        def("set_component", "Add a component (engine or script) to an entity, or change fields of one it has. Only the fields you pass change.", json!({ "entity": entity(), "component": { "type": "string", "description": "Component name, e.g. Light, MeshRenderer, RigidBody or a script component" }, "value": { "type": "object", "description": "Fields to set (see list_component_types for the shape)" } }), &["entity", "component"]),
        destructive("remove_component", "Remove a component from an entity.", json!({ "entity": entity(), "component": { "type": "string" } }), &["entity", "component"]),
        read("list_component_types", "Component types that can be added (engine and script), optionally with their field schema and defaults.", json!({ "component": { "type": "string", "description": "Only this component, with schema and defaults" }, "schema": { "type": "boolean", "description": "Include field schemas for all (verbose)" } }), &[]),
        def("select_entities", "Set the editor selection (empty list clears it).", json!({ "entities": entities(), "add": { "type": "boolean", "description": "Add to the current selection" } }), &["entities"]),
        def("focus_entity", "Frame an entity in the scene view and reveal it in the hierarchy.", json!({ "entity": entity() }), &["entity"]),
        def("undo", "Undo the last change(s) in the active scene.", json!({ "steps": { "type": "integer", "description": "Default 1" } }), &[]),
        def("redo", "Redo undone change(s) in the active scene.", json!({ "steps": { "type": "integer", "description": "Default 1" } }), &[]),
        // ------------------------------------------------------------ prefabs & materials
        def("create_prefab", "Save an entity (with children) as a prefab asset and link the entity to it.", json!({ "entity": entity() }), &["entity"]),
        def("apply_prefab", "Write a prefab instance's changes back to its prefab and update the other instances.", json!({ "entity": entity() }), &["entity"]),
        def("assign_material", "Set the material of an entity's MeshRenderer.", json!({ "entity": entity(), "material": asset("Material") }), &["entity", "material"]),
        def("create_material", "Create a material asset (Materials/<name>.mat).", json!({ "name": { "type": "string" }, "values": { "type": "object", "description": "Initial fields, e.g. {\"albedo\": [1,0,0], \"metallic\": 1}" }, "folder": { "type": "string", "description": "Folder under Assets/ (default Materials)" } }), &["name"]),
        read("get_material", "A material's fields (albedo, textures, metallic, roughness, emission, alpha...).", json!({ "material": asset("Material") }), &["material"]),
        def("set_material", "Change fields of a material asset (.mat). Only the fields you pass change.", json!({ "material": asset("Material"), "values": { "type": "object" } }), &["material", "values"]),
        def("extract_material", "Copy a model's embedded material into an editable .mat asset.", json!({ "material": asset("Embedded material (sub-asset UUID)"), "name": { "type": "string" } }), &["material"]),
        // ------------------------------------------------------------ assets & files
        read("list_assets", "Assets in the project. Filter by folder, kind or text.", json!({
            "folder": { "type": "string", "description": "Folder under Assets/ (default: everything)" },
            "kind": { "type": "string", "enum": ["model", "texture", "material", "scene", "prefab", "audio", "script", "shader", "other"] },
            "query": { "type": "string", "description": "Words that must appear in the path; supports l:<label>" },
            "limit": { "type": "integer", "description": "Default 500" }
        }), &[]),
        read("list_folders", "Sub-folders of a folder under Assets/.", json!({ "folder": { "type": "string", "description": "Default: Assets/ root" } }), &[]),
        read("get_asset", "Details of an asset: kind, size, load state, labels, import settings, dependencies, references, and for models the meshes, materials, animations and bounds.", json!({ "asset": asset("Asset") }), &["asset"]),
        def("set_asset_meta", "Change an asset's .meta: import settings (scale, pivot, normals, LODs, compression...), preload, labels, animation events. Reimports when import settings change.", json!({ "asset": asset("Asset"), "meta": { "type": "object", "description": "Partial meta: {\"import\": {...}, \"preload\": bool, \"labels\": [...], \"animation_events\": [...]}" } }), &["asset", "meta"]),
        def("reimport_asset", "Reimport an asset from its source file.", json!({ "asset": asset("Asset") }), &["asset"]),
        def("import_file", "Copy an external file (model, texture, audio...) into the project and import it.", json!({ "source": { "type": "string", "description": "Absolute path of the file to import" }, "folder": { "type": "string", "description": "Destination folder under Assets/" } }), &["source"]),
        def("create_folder", "Create a folder under Assets/.", json!({ "path": { "type": "string", "description": "Folder path under Assets/, e.g. \"Models/Trees\"" } }), &["path"]),
        def("move_asset", "Move or rename an asset (its id and references are kept).", json!({ "asset": asset("Asset"), "to": { "type": "string", "description": "New path under Assets/, or just a new file name" } }), &["asset", "to"]),
        destructive("delete_asset", "Delete a file or folder under Assets/ (moved to Library/Trash).", json!({ "path": { "type": "string", "description": "Path under Assets/" } }), &["path"]),
        read("list_files", "Files and folders in a project directory (Assets, Scripts, ...).", json!({ "path": { "type": "string", "description": "Directory relative to the project root (default: root)" } }), &[]),
        read("read_file", "Read a text file in the project (scenes, prefabs, materials, scripts, shaders, project.ron...).", json!({ "path": { "type": "string", "description": "Path relative to the project root, e.g. \"Scripts/src/player.rs\" or \"Assets/Scenes/Main.scene\"" } }), &["path"]),
        def("write_file", "Write a text file in the project. Files under Assets/ are registered as assets; changed scripts are picked up by the script tools.", json!({ "path": { "type": "string", "description": "Path relative to the project root" }, "content": { "type": "string" } }), &["path", "content"]),
        // ------------------------------------------------------------ scripts
        read("get_script_status", "Script library status, compiler errors and warnings, registered components and systems (with timings), runtime script errors and the build log.", json!({ "log_lines": { "type": "integer", "description": "Build log lines to include (default 40)" } }), &[]),
        def("build_scripts", "Build the project's scripts crate with cargo and hot-reload it.", json!({ "wait": { "type": "boolean", "description": "Wait for the build to finish (default true)" }, "clean": { "type": "boolean", "description": "cargo clean first" } }), &[]),
        def("reload_scripts", "Reload the scripts library from disk (component data is kept).", json!({}), &[]),
        def("unload_scripts", "Unload the scripts library (script components are kept as data).", json!({}), &[]),
        def("create_script", "Create a script file from a template in the scripts crate (creating the crate if needed).", json!({ "name": { "type": "string", "description": "e.g. EnemyAI" }, "template": { "type": "string", "enum": ["behaviour", "component", "system", "empty"], "description": "Default: behaviour (component + system)" }, "build": { "type": "boolean", "description": "Start a build afterwards (default true)" } }), &["name"]),
        def("set_system_enabled", "Enable or disable a script system.", json!({ "system": { "type": "string" }, "enabled": { "type": "boolean" } }), &["system", "enabled"]),
        def("set_script_options", "Script build options.", json!({ "build_on_save": { "type": "boolean" }, "auto_reload": { "type": "boolean" } }), &[]),
        // ------------------------------------------------------------ play mode
        def("play", "Enter play mode (or resume when paused). Changes made while playing are undone on stop.", json!({}), &[]),
        def("pause", "Pause play mode.", json!({}), &[]),
        def("stop", "Leave play mode and restore the scene as it was before playing.", json!({}), &[]),
        def("step", "Advance a paused game by some frames.", json!({ "frames": { "type": "integer", "description": "Default 1" } }), &[]),
        def("set_time_scale", "Game time scale while playing (0 to 4).", json!({ "scale": { "type": "number" } }), &["scale"]),
        def("send_input", "Drive game input while playing: hold or release keys and mouse buttons, move the mouse. Keys stay down until released.", json!({
            "press": { "type": "array", "items": { "type": "string" }, "description": "Keys to hold: A-Z, Num0-Num9, Space, Enter, Escape, Tab, Backspace, Delete, Left, Right, Up, Down, LShift, RShift, LCtrl, RCtrl, LAlt, RAlt, F1-F12, MouseLeft, MouseRight, MouseMiddle" },
            "release": { "type": "array", "items": { "type": "string" }, "description": "Keys to let go" },
            "release_all": { "type": "boolean" },
            "mouse_position": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Pixels in the game view" },
            "mouse_delta": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 },
            "scroll": { "type": "number" }
        }), &[]),
        // ------------------------------------------------------------ console & stats
        read("get_console_logs", "Recent console output (engine and scripts).", json!({
            "level": { "type": "string", "enum": ["error", "warn", "info", "debug", "trace"], "description": "Lowest level to include (default info)" },
            "source": { "type": "string", "enum": ["all", "engine", "scripts"] },
            "contains": { "type": "string" },
            "limit": { "type": "integer", "description": "Most recent lines (default 100)" }
        }), &[]),
        def("clear_console", "Clear the console.", json!({}), &[]),
        read("get_performance_stats", "Frame time, FPS, simulation and extraction time, draw calls, triangles, culling, physics, AI and streaming stats, CPU/RAM/VRAM and profiler scopes.", json!({}), &[]),
        // ------------------------------------------------------------ view
        read("capture_view", "Screenshot of the scene or game view as a PNG image. Switches the center panel to that view. Egui overlays (gizmos, HUD, labels) are not included.", json!({ "view": { "type": "string", "enum": ["scene", "game"], "description": "Default: the view shown now" }, "max_width": { "type": "integer", "description": "Downscale to at most this width (default 1024)" } }), &[]),
        read("get_scene_camera", "The scene view camera: pivot, distance, yaw/pitch, position, fov.", json!({}), &[]),
        def("set_scene_camera", "Move the scene view camera. `position` + `look_at` place it directly; or set pivot/distance/yaw/pitch.", json!({
            "position": vec3("Camera position"),
            "look_at": vec3("Point to look at (becomes the pivot)"),
            "pivot": vec3("Orbit pivot"),
            "distance": { "type": "number" },
            "yaw": { "type": "number", "description": "Degrees" },
            "pitch": { "type": "number", "description": "Degrees" }
        }), &[]),
        def("set_gizmo", "Transform gizmo mode, space and snapping.", json!({ "mode": { "type": "string", "enum": ["translate", "rotate", "scale"] }, "space": { "type": "string", "enum": ["world", "local"] }, "snap": { "type": "boolean" } }), &[]),
        def("open_window", "Show an editor window or panel.", json!({
            "window": { "type": "string", "enum": ["scene", "game", "assets", "console", "scripts", "preferences", "build", "profiler", "web_browser", "code_editor", "material", "model", "animation"] },
            "asset": asset("For material/model/animation viewers"),
            "path": { "type": "string", "description": "For code_editor: file relative to the project root" },
            "line": { "type": "integer", "description": "For code_editor" },
            "url": { "type": "string", "description": "For web_browser" }
        }), &["window"]),
        // ------------------------------------------------------------ build & export
        def("build_export", "Package the project as a standalone game (File ▸ Build & Export). Unsaved scenes are saved first.", json!({
            "release": { "type": "boolean" },
            "include_scripts": { "type": "boolean" },
            "out_dir": { "type": "string", "description": "Output folder (default Builds/<name>)" },
            "run_after": { "type": "boolean", "description": "Start the game when done" },
            "wait": { "type": "boolean", "description": "Wait for the export to finish (default false)" }
        }), &[]),
        read("get_export_status", "Progress, result and log of the last Build & Export.", json!({ "log_lines": { "type": "integer", "description": "Default 40" } }), &[]),
    ]
}

static DEFS: OnceLock<Vec<Json>> = OnceLock::new();

/// Tool definitions for `tools/list`.
pub fn definitions() -> &'static [Json] {
    DEFS.get_or_init(|| {
        all()
            .into_iter()
            .map(|d| {
                let mut schema = Map::new();
                schema.insert("type".into(), json!("object"));
                schema.insert("properties".into(), d.props);
                if !d.required.is_empty() {
                    schema.insert("required".into(), json!(d.required));
                }
                json!({
                    "name": d.name,
                    "description": d.desc,
                    "inputSchema": schema,
                    "annotations": { "readOnlyHint": d.read_only, "destructiveHint": d.destructive, "openWorldHint": false },
                })
            })
            .collect()
    })
}

pub fn exists(name: &str) -> bool {
    definitions().iter().any(|d| d["name"] == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_schemas_valid() {
        let defs = definitions();
        let mut names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate tool names");
        for d in defs {
            let props = d["inputSchema"]["properties"].as_object().unwrap();
            for r in d["inputSchema"]["required"].as_array().into_iter().flatten() {
                assert!(props.contains_key(r.as_str().unwrap()), "{}: required {r} not in properties", d["name"]);
            }
        }
    }
}

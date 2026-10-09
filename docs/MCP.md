# AI agents (MCP)

The editor runs a [Model Context Protocol](https://modelcontextprotocol.io) server, so AI
agents such as Claude Code, Claude Desktop or any other MCP client can drive it. An agent can
build scenes, edit components and materials, manage assets, write and build scripts, play
the game, send it input, read the console and take screenshots. Every edit goes through the
same code as the UI, so you watch it happen live, it joins the undo history and it marks
scenes dirty.

## Connecting

The server starts with the editor and listens on **`http://127.0.0.1:47100/mcp`**
(Streamable HTTP). It only accepts connections from this computer, and browsers from other
sites are refused. **Edit ▸ Preferences ▸ AI agents (MCP)** turns it on or off, changes the
port and shows the setup command.

Claude Code:

```bash
claude mcp add --transport http dumb-engine http://127.0.0.1:47100/mcp
```

Or in a project's `.mcp.json`:

```json
{ "mcpServers": { "dumb-engine": { "type": "http", "url": "http://127.0.0.1:47100/mcp" } } }
```

Any client that speaks Streamable HTTP works the same way.

## Conventions

- **Entities**: numeric ids from `list_entities`, or a unique entity name.
- **Assets**: a path under `Assets/` (`"Materials/Gold.mat"`), a UUID, or a built-in model
  (`Cube`, `Sphere`, `Plane`, `Cylinder`).
- **Values**: plain JSON. Vectors, quaternions and colors are arrays (`[x, y, z]`,
  `[x, y, z, w]`, `[r, g, b, a]`, where alpha may be left out). Enums are variant names, and
  asset fields take paths. Objects may name only the fields to change. Unknown fields and
  variants are errors that list the valid ones.
- **Script components** work like engine ones: they come from reflection, so new fields show
  up as soon as the scripts are rebuilt.
- Edits mark scenes dirty. Call `save_scene` to write them. `undo` and `redo` work on agent
  edits too.
- Tools that would lose work (`open_project`, `close_project`, `close_scene`) refuse when there
  are unsaved changes, unless told to save or discard.

## Tools

| Area | Tools |
|---|---|
| Editor & project | `get_editor_state`, `list_recent_projects`, `open_project`, `create_project`, `close_project`, `get_project_settings`, `set_project_settings`, `get_editor_settings`, `set_editor_settings` |
| Scenes | `list_scenes`, `new_scene`, `open_scene`, `activate_scene`, `close_scene`, `save_scene`, `save_all_scenes`, `set_startup_scene` |
| Entities | `list_entities`, `get_entity`, `create_entity`, `spawn_asset`, `delete_entities`, `duplicate_entities`, `rename_entity`, `set_parent`, `set_transform`, `set_component`, `remove_component`, `list_component_types`, `select_entities`, `focus_entity`, `undo`, `redo` |
| Prefabs & materials | `create_prefab`, `apply_prefab`, `assign_material`, `create_material`, `get_material`, `set_material`, `extract_material` |
| Assets & files | `list_assets`, `list_folders`, `get_asset`, `set_asset_meta` (import settings, labels, animation events), `reimport_asset`, `import_file`, `create_folder`, `move_asset`, `delete_asset`, `list_files`, `read_file`, `write_file` |
| Scripts | `get_script_status`, `build_scripts`, `reload_scripts`, `unload_scripts`, `create_script`, `set_system_enabled`, `set_script_options` |
| Play mode | `play`, `pause`, `stop`, `step`, `set_time_scale`, `send_input` |
| Console & stats | `get_console_logs`, `clear_console`, `get_performance_stats` |
| View | `capture_view` (PNG of the scene or game view), `get_scene_camera`, `set_scene_camera`, `set_gizmo`, `open_window` |
| Build & export | `build_export`, `get_export_status` |

Each tool's description and input schema come from `tools/list`, and the catalog lives in
[`crates/dumb_editor/src/mcp/tools.rs`](../crates/dumb_editor/src/mcp/tools.rs).

Without an open project (start screen), only `get_editor_state`, `list_recent_projects`,
`open_project`, `create_project` and the editor settings tools work.

## How it works

- `mcp/server.rs`: a small HTTP server ([tiny_http](https://crates.io/crates/tiny_http)) on
  worker threads. It answers `initialize`, `ping` and `tools/list` directly. Each
  `tools/call` goes over a channel to the main thread.
- `editor/mcp_tools.rs`: the editor runs calls between frames, before `pre_frame`. Most calls
  reuse the editor's own actions (`Action::…`, `save_active`, `play`/`stop`…). Calls that take
  frames reply later: `build_scripts` (`wait`), `build_export` (`wait`), `step` and
  `capture_view`, which renders the view and then reads it back from the GPU with
  `Renderer::read_target`.
- `mcp/convert.rs`: JSON ⇄ reflection. Input is converted by walking the target's reflected
  shape, so field names, enum variants and asset paths are checked before anything changes.

`capture_view` returns the 3D render only. Egui overlays (gizmos, HUD, labels) are drawn on
top of it in the window and are not included.

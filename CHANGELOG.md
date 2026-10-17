# Changelog

## Unreleased

### Server
- **Headless server**: `dumb-server [project] [scene] [--hz N] [--ticks N]` runs a project's
  scripts, physics, AI and world streaming with no window or GPU, at a fixed tick rate (30 Hz by
  default), logging tick times every 10 s. Ctrl+C and SIGTERM stop it cleanly (scripts get
  their `on_unload`). Build it with `cargo build -p dumb_runtime --no-default-features --bin
  dumb-server`: without the new `render` feature, `dumb_runtime` doesn't pull in the renderer,
  winit, egui or the web view, so it builds and runs on machines without a desktop.
- **`StreamFocus` resource**: points that world streaming and AI distance LOD follow instead of
  the primary camera. With several points the nearest one counts, so a server can keep the world
  loaded around every player. `Streamer::update` and `ai::orchestrate` take a slice of points.
- `find_primary_camera` moved to `dumb_ecs` (`dumb_render` re-exports it).

### Simulation
- **Fixed-timestep loop.** New project settings `sim_hz` / `sim_max_steps` run the simulation
  (scripts, AI, streaming, animation) at a fixed tick rate independent of the frame rate;
  `sim_hz = 0` keeps one tick per frame. `deterministic` makes every tick exactly `1/hz` and
  derives `Time` from the tick counter, ignoring `time_scale`. In the editor and the player;
  Project Preferences ▸ Game tick rate.

## v0.3.3

Scripts must be rebuilt (ABI abi-10); the editor does it on its own.

### Scripting
- **Meshes made in code.** Scripts build a `RuntimeMesh` (positions, normals, UVs, and parts
  with their own materials, on any thread) and hand it to `ctx.meshes.set(id, mesh)`. The id
  works as a `MeshRenderer` model and the mesh is on the GPU by the next frame, with no file or
  import. Setting the id again replaces it, `ctx.meshes.remove(id)` frees it (CPU and GPU), and
  stopping play frees them all. `AssetId::from_name` gives stable ids for them.
- `Hud::screen_size`: the game view's size in HUD pixels, to fit UI to the screen.

### HUD
- Panels count as UI: `Hud::pointer_over_ui` is true over panels too, not only buttons and web
  pages, so scripts can scroll a panel with the wheel instead of zooming the camera.
- Presses on the HUD stay out of the game: clicking a panel or button and dragging no longer
  moves the game's camera. In the editor and the player.

### Editor
- **View modes** for the scene and game views, next to the Scene / Game tabs or in View:
  Rendered, No post-processing, Unlit (textures only), Mesh (clay, every facet visible),
  Normals and Wireframe. Debug modes skip lighting, shadows and custom surface shaders (a
  shader's `vertex()` still runs, so Mesh shows the geometry as drawn).
- The scene view shows the primary camera's ink outlines in Rendered mode, like the game.
- The Wireframe preference became the two view modes.

## v0.3.2

Scripts must be rebuilt (ABI abi-9).

- **Skybox per camera:** `Camera::skybox` takes any equirectangular (2:1) texture; empty keeps
  the built-in sky, and Solid color still turns it off. It also lights the scene and shows in
  reflections. The primary camera picks it.
- **`Registry::on_unload(fn)`:** runs before every script rebuild, reload and project close,
  so scripts can stop their threads (a thread still running unloaded code crashed the editor).
- HUD buttons work in the editor's game view again (pressing one took input away from the game
  before the click registered).
- No more stutter twice a second: the system monitor reads CPU/RAM on its own thread.

## v0.3.1

- Softer sun shadows (16 rotated Poisson taps) and shadow casters kept between the sun and a
  cascade (depth clamp), so a planet's horizon no longer leaves rings of light at sunset.

## v0.3.0

- User WGSL surface shaders: per-material shader assets compiled at runtime with hot reload,
  a shader viewer, WGSL in the code editor, and an ink-outline post effect.
- Vector fields fit the inspector column.

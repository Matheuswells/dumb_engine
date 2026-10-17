# Changelog

## Unreleased

### Audio
- **Spatial and streaming audio.** New `AudioSource` (clip, volume, pitch, looping, 3D on/off,
  min/max distance, stream, play on start) and `AudioListener` components. Sounds are positioned
  around the listener (or the primary camera) with distance attenuation, panning and Doppler.
  Short clips are decoded on a worker thread; `stream` decodes long files while they play.
  Scripts start and stop a sound by setting `playing`. Supports wav, ogg, flac and mp3. Plays in
  the editor's play mode (paused with it) and in the player. Linux output uses ALSA, loaded at
  runtime, so nothing extra is needed to build.

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

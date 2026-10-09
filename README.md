# Dumb Engine

Welcome to Dumb Engine, the game engine designed with minimal brainpower and maximum
enthusiasm. Built for when you don't need a multi-million dollar rendering pipeline, you
just need a box to move across the screen before you lose interest.

Under the hood, it is a 3D game engine and editor written in Rust on Vulkan. It is built for performance,
very large worlds, procedural generation, procedural animation and simulation. Game
code is plain Rust in a hot-reloadable DLL.

> Status: **MVP**. The full loop works: author in Blender → auto-import → place in the
> editor → script in Rust → play, edit live, hot-reload → save. See
> [docs/ROADMAP.md](docs/ROADMAP.md) for what comes next.

## ⚡ Features

* **Zero-Brain Architecture:** No complex setups. If a 5-year-old with a keyboard can't figure it out, we haven't dumbed it down enough.
* **Blazing Fast (Probably):** It's so light on features that your CPU won't even notice it's running.
* **Advanced Physics:** Objects fall down. Sometimes they bounce. Don't push your luck.
* **Photorealistic-ish Graphics:** Supports pixels, colored rectangles, and severe artistic compromises.
* **Memory Management:** If it leaks memory, just restart your computer.

## Quick start

```bash
cargo run -p dumb_editor -- project
```

The editor opens `project/` and its startup scene. The project's scripts live in
`project/Scripts` (see below). Press **🔨 Build scripts** once on first run.

To regenerate the demo content (this needs Blender for the robot):

```bash
blender --background --factory-startup --python tools/make_robot.py -- project/Assets/Characters/Player/Robot.blend
```

```bash
cargo run -p dumb_runtime --example make_demo -- project
```

Standalone player (no editor):

```bash
cargo run -p dumb_runtime --bin dumb-player -- project
```

Requirements: a Vulkan 1.3 GPU and driver, and a Rust toolchain. You do **not** need the
Vulkan SDK: shaders are WGSL, compiled to SPIR-V at build time by naga. Blender is optional.
It is found automatically (Steam or Program Files installs, `PATH`, or `DUMB_BLENDER`) and
is used to import `.blend`, `.fbx`, `.obj`, `.stl`, `.ply`, `.usd` and `.dae` files. Set
`DUMB_VALIDATION=1` to enable the Vulkan validation layer when it is installed.

## Writing game code

Every project has a scripts crate in `<project>/Scripts`. **Each `.rs` file in `Scripts/src/`
is one script.** A generated `build.rs` finds the files on its own, so you never edit `lib.rs`
or register modules by hand.

To create a script, use **Scripts ▸ New Script…**, the asset browser's **Create ▸ Script**,
or **➕ New Script** in the Scripts panel. Pick a name and a template:

| Template | Generates |
|---|---|
| Behaviour | a component with fields, plus a system that runs every frame |
| Component | a data-only component |
| System | a system with no component of its own |
| Empty | a helper module that other scripts can `use` |

The editor writes the file, builds it, hot-loads it, and opens it in VS Code (or your
default editor). A script looks like this:

```rust
use dumb_script::prelude::*;

#[derive(Component, Editor, Clone, Debug, Default)]
pub struct Player {
    #[editor(range = 0.0..=20.0, tooltip = "Meters per second")]
    pub speed: f32,
    #[editor(range = 0.0..=100.0)]
    pub health: f32,
    #[editor(asset = "model")]
    pub weapon: AssetId,
}

fn movement(ctx: &mut ScriptContext) {
    let dir = Vec3::new(ctx.input.axis(Key::A, Key::D), 0.0, ctx.input.axis(Key::W, Key::S));
    for (t, p) in ctx.world.query::<(&mut Transform, &Player)>() {
        t.translation += dir * p.speed * ctx.time.delta;
    }
}

// Called automatically for every file that defines it.
pub fn register(reg: &mut Registry) {
    reg.component::<Player>();
    reg.system("player::movement", movement);
}
```

The inspector shows `Player` automatically, and any reflected field can be edited while the
game runs. Rebuild with **Build scripts** or `cargo build` in `Scripts/`: the DLL reloads and
every component keeps its values. Fields are matched by name, so adding, removing or
reordering fields doesn't lose data. Moving a type to another file doesn't either.

**Scripts panel** (bottom dock, 📜 Scripts):
- **Files**: click to open, right-click to rename, delete (moved to `Library/Trash`) or show
  in Explorer. ⛔ marks files that have errors.
- **Systems**: a checkbox turns each system on or off while playing, with a live per-system
  cost in ms and panic markers.
- **Problems**: compiler errors and warnings. Click one to jump to the file and line in
  VS Code. The raw build output is a toggle away.
- **Build on save**, **Clean build**, **Reload / Unload**, and **Open in code editor**.

Scripts log with `trace!`, `debug!`, `info!`, `warn!` and `error!` (in the prelude). Messages show in the Console with their level and the script module that sent them.

Scripts also get `ctx.physics`: `raycast`, `overlap_sphere` and `collision_events()`. Physics components are plain data, so scripts move bodies by editing them: `rb.apply_impulse(..)`, `rb.add_force(..)`, `rb.linear_velocity = ..`, `cc.move_velocity(..)` and `cc.jump(..)`. See `project/Scripts/src/physics_toys.rs` (press F while playing to throw balls).

Right-click a script component in the inspector and choose **📝 Edit Script** to open its
source. The demo's scripts are in [project/Scripts/src](project/Scripts/src): player movement,
a spinner, procedural bobbing, value noise, and a procedural block field that spawns and
simulates up to 90,000 entities.

## Editor

| | |
|---|---|
| **Scene** | 3D viewport, move/rotate/scale gizmos (world/local, snapping), click picking, multiple scene tabs, undo/redo, Play/Pause/Step/Stop (Stop restores the exact pre-play scene), separate Game view |
| **Hierarchy** | Virtualized tree (fine with 100k entities), drag-and-drop reparenting, rename, duplicate, delete, create menu, prefabs (create / instantiate / apply) |
| **Inspector** | Generated from reflection for engine and script components, plus asset pickers, entity references, enums, lists, colors, ranges and tooltips |
| **Assets** | Folder tree, thumbnails (real 3D renders for models and materials), search (`t:model`, `l:label`), create, rename, move (drag onto a folder), delete (to `Library/Trash`), import (dialog or drag files onto the window), metadata, dependencies and references, reimport, hot reload |
| **Model viewer** | Nodes, meshes, materials, textures, skeleton/bones, animations, collision proxies (`UCX_`…), LOD groups (`_LOD0`…), bounds, wireframe, turntable |
| **Animation viewer** | Clip list, play/pause, timeline scrubbing, speed, loop, skeleton overlay, blend preview, animation events (saved in `.meta`), root-motion trail |
| **Material editor** | Sections for surface, normal (OpenGL/DirectX), metal/rough (packed, roughness or smoothness map), AO, emission, UV tiling/offset and transparency. Texture slots with thumbnails (drop textures on them). Live preview with sky, sun, exposure and point light. Edits save to `.mat` and update the scene immediately |
| **Physics** | Rapier: `RigidBody` (dynamic/static/kinematic), `Collider` (box, sphere, capsule, cylinder, convex hull, mesh, Blender `UCX_` proxies, auto-fit to the model), `CharacterController` (slopes, steps, ground snap). Fixed-rate simulation during Play, collider wireframes in the scene view |
| **Sky** | Built-in sky panorama (embedded in the engine): background, sky-tinted ambient light and roughness-aware reflections. Cameras choose Skybox or Solid color |
| **Preferences** | Edit ▸ Preferences. Editor: max FPS (or unlimited), background FPS, VSync, UI scale, theme, camera, snapping, play-mode and script options, custom code editor command. Project: startup scene, game max FPS, VSync, window size, fullscreen, gravity, physics rate |
| **Other** | Console (per-level filters, engine/script source filter, repeated lines collapsed), scripts panel (systems, build log, panics per system), project manager, stats overlay |

Viewport controls: RMB + WASD/QE to fly, MMB to pan, Alt+LMB to orbit, wheel to zoom,
F to focus. Gizmos: W/E/R, hold Ctrl to snap. Ctrl+P plays, Ctrl+S saves,
Ctrl+Z/Ctrl+Y undo and redo, Ctrl+D duplicates.

## Layout

```
crates/
  dumb_core     math (glam), ids, Entity, Color, Aabb/Frustum/Ray, Time, Input
  dumb_reflect  Reflect trait, Value tree, apply/to_value (no TypeId dependence)
  dumb_derive   #[derive(Editor)], #[derive(Component)]
  dumb_ecs      sparse-set ECS, queries, hierarchy, built-in components, scenes/prefabs
  dumb_asset    asset database, .meta, glTF + Blender importers, materials, primitives, watcher
  dumb_render   Vulkan 1.3 renderer (ash), PBR + skinning, egui backend, offscreen views
  dumb_script   plugin ABI, ScriptHost (load / hot reload / cargo build)
  dumb_runtime  game loop pieces, project settings, standalone player
  dumb_editor   the editor
project/Scripts         the demo project's scripts crate (one file per script)
project/                sample project (Assets/, project.ron)
tools/                  Blender demo generator, UI test helpers
```

More detail is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

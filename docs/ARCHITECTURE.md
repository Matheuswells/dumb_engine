# Architecture

```
                         ┌────────────── dumb_editor ───────────────┐
                         │ scene view · hierarchy · inspector ·     │
                         │ assets · model/anim/material viewers     │
                         └───────┬───────────────┬──────────────────┘
                                 │               │
  Scripts/*.dll (cdylib) ► dumb_script       dumb_runtime ──► dumb-player
        (cdylib)          ScriptHost        simulate(), Project
                                 │               │
             ┌───────────────────┼───────────────┼──────────────┐
             ▼                   ▼               ▼              ▼
         dumb_ecs ◄──────── dumb_asset ────► dumb_render (Vulkan)
             │                   │
             ▼                   ▼
       dumb_reflect ◄──── dumb_derive
             │
             ▼
         dumb_core
```

## Reflection: the core contract

*Anything exposed through reflection is editable at runtime without recompiling the engine.*

`Reflect` describes a value's shape with the `ReflectRef`/`ReflectMut` enums (struct, enum,
list, bool, ints, floats, string, vectors, quaternion, color, asset id, entity). It does
**not** use `TypeId`. Types compiled into a script DLL get different `TypeId`s from the
same types in the host, so `TypeId` cannot identify a type across that boundary.

- `to_value` / `apply` convert any reflected value to and from a `Value` tree. Scenes,
  prefabs, undo snapshots, play-mode snapshots, `.mat` files and the hot-reload state
  transfer all use this one path.
- `apply` matches struct fields **by name** and ignores unknown fields. Data therefore
  survives added, removed and reordered fields.
- `#[derive(Editor)]` generates the impl. Field attributes: `range`, `speed`, `readonly`,
  `hidden`, `tooltip`, `asset = "kind"`, `color`, `skip`.

## ECS

Sparse sets with type-erased `BlobVec` columns. A component type is a
`ComponentDescriptor`: name, layout, a drop function, a reflect-cast function and a
default constructor. Typed access (`world.get::<T>()`, `world.query::<(&mut A, &B)>()`)
looks up `TypeId` first and falls back to the stable type **name**. This is how a script
library sees the host's `Transform` and the host sees the library's `Player`.

- Queries drive iteration from the smallest required column. `With`/`Without`/`Option`
  filters are supported, and aliasing `&mut` access is rejected at query creation.
- Hierarchy uses a `Parent` component. `update_transforms` writes world matrices into a
  dense side table, so there is no per-entity `GlobalTransform` component.
- **Missing components**: when a type is unregistered (its script unloaded, or a build
  failed), its instances are stored as `Value`s and restored when the type comes back.
  The same mechanism lets a scene load before its scripts.
- `SceneData` serializes worlds to RON. `instantiate` remaps entity references.
  `restore_exact` keeps entity ids, which play-mode Stop and undo rely on so the editor
  selection stays valid.

## Scripting and hot reload

A game crate is a `cdylib` that exports `dumb_plugin_abi` (a version string: engine ABI +
rustc version) and `dumb_plugin_register(&mut Registry)`. Rust has no stable ABI, so the
host refuses a plugin built by a different compiler.

Reload sequence (`ScriptHost::load`):
1. Every world `unregister`s each plugin component. Its data moves into missing storage.
2. The old library is dropped. It was a *shadow copy* in `Library/ScriptCache`, so cargo
   was free to overwrite the original file.
3. A new shadow copy is loaded and the ABI string is checked.
4. The components are registered again, and missing data is applied back field by field.

Systems are `fn(&mut ScriptContext)`. Each one runs through a trampoline compiled
*inside* the plugin, which catches panics on the plugin's side of the boundary. The
host's std never unwinds a panic from another std. Plugin `log` calls are forwarded to
the host logger.

## Assets

`AssetDatabase` owns `Assets/`. Every file gets a `.meta` sidecar holding a stable
`AssetId`, import settings, labels and animation events. Renames and moves carry the
`.meta` along, so references never break. Sub-assets such as a model's embedded textures
and materials use deterministic UUIDv5 ids (`model.sub("material", i)`).

- Imports run on background threads, and the editor never blocks on them. Every import
  bumps the asset's `version`. The renderer compares versions to decide when to upload
  again, which is how hot reload reaches the GPU.
- **Blender is first-class.** `.blend` (and FBX/OBJ/STL/PLY/USD/DAE) files are converted by
  headless Blender to a cached GLB in `Library/Imported`, then imported through glTF.
  Saving in Blender triggers the file watcher (debounced) → reimport → editor update.
  Clips come from Blender actions and NLA tracks. `UCX_`/`UBX_`/`USP_`/`COL_` meshes become
  collision proxies, and `Name_LODn` meshes become LOD groups.
- Built-in primitives (cube, sphere, plane, cylinder) have fixed ids in
  `dumb_core::builtin`.

## Renderer

- Vulkan 1.3 through `ash`, using dynamic rendering and synchronization2.
  Memory comes from `gpu-allocator`.
- WGSL shaders are compiled to SPIR-V by `naga` in `build.rs`, so no SDK is needed.
- Every 3D view renders into its own offscreen target, and egui displays it as an image.
  This covers the scene view, the game view, each viewer window and each thumbnail. The
  swapchain itself only receives egui. That keeps docking and floating windows simple,
  and it is the same path a future multi-view or VR setup would use.
- Forward PBR (GGX, Smith, Schlick) with one sun, eight point lights, hemisphere ambient
  and ACES tonemapping. Materials use albedo, normal, metallic-roughness, AO and emission
  maps, with alpha mask or blend and an optional double-sided mode.
- GPU skinning reads joint matrices from a per-frame storage buffer (64k joints).
- Draws are sorted by material and model, blended draws go back to front, and frustum
  culling happens at extraction. Resources freed while frames are in flight wait in a
  deferred-destruction queue.

## Editor

Each frame runs this sequence: `pre_frame` (asset events, script reload, simulation) →
`ui` (egui panels, which record `Action`s) → `apply_actions` → `build_views` (scene, game,
viewers, up to 3 thumbnails) → `Renderer::draw_frame` → `post_frame` (undo bookkeeping).

Undo works by snapshot. A burst of changes becomes one undo step once a frame passes
with no change and no mouse button held, so a whole gizmo or slider drag is one step.
Undo is disabled during play, because Stop restores the pre-play snapshot anyway.

MCP tool calls ([MCP.md](MCP.md)) arrive on HTTP worker threads and run on the main thread
before `pre_frame`, through the same actions the UI uses.

## Performance notes (MVP state)

Measured on an RTX 3050 Laptop in a dev build with dependencies at opt-level 3:
- 10,000 script-driven entities: 1.2 ms simulation and about 8,000 draws at a
  vsync-capped 120 fps.
- 100,000+ entities show fine in the hierarchy, because it is virtualized.

The known next steps for massive worlds are listed in [ROADMAP.md](ROADMAP.md): GPU-driven
instanced rendering, chunked world streaming, floating origin, and a job system.

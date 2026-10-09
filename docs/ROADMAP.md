# Roadmap

The MVP covers the content pipeline and the editor loop. Next steps, ordered by how much
each one unlocks for the engine's focus areas (performance, massive worlds, procedural,
simulation):

## 1. Rendering at scale
- ✅ **Instancing**: draws are sorted and merged into one instanced draw per mesh + material +
  LOD, with per-instance transforms/tints in a storage buffer (the demo scene: ~10k objects in
  9 draw calls). Next: indirect draws filled by a GPU culling pass.
- GPU frustum and occlusion culling (Hi-Z) in a compute pass.
- ✅ Cascaded shadow maps for the sun: 4 cascades (2048²) fitted to the camera with texel snapping,
  normal-offset + slope bias, 3x3 PCF, cut-out shadows, per-object `cast_shadows`. Next:
  clustered lighting and point/spot shadows.
- ✅ Automatic LOD selection by screen size (authored `_LODn` groups and meshoptimizer-generated
  levels, per-model switch sizes and small-object culling, edited in the Model Viewer).
- MSAA or TAA, HDR target, IBL from `.hdr` environment maps.

## 2. Massive worlds
- ✅ World streaming: `StreamingCell` entities load their scene when the camera comes within
  `load_distance` (parsed on a worker thread, spawned in time slices within a per-frame budget)
  and unload past `unload_distance`.
- Floating origin, or 64-bit world positions with camera-relative rendering.
- Procedural terrain: heightfield chunks generated on worker threads, with GPU tessellation
  or clipmaps.

## 2.1 Massive npc ai support
- ✅ AI orchestrator: `AiAgent` think rates with distance LOD bands (near/mid/far/asleep), a
  per-frame think budget that defers the least overdue agents, and staggered phases so
  thousands of agents spread over frames. Scripts check `agent.think` and run their logic with
  `par_for_each` (the `Crowd` demo script: 5,000 NPCs at 100+ fps). Settings in Project
  Preferences ▸ AI.

## 2.2 massive loading while playing 
- ✅ Imports run on worker threads, streamed scenes parse off-thread and spawn in time slices,
  and GPU uploads have a per-frame byte budget (48 MB) so big assets spread over frames.

## 3. Simulation and core runtime
- ✅ Job system: `world.par_for_each::<Q, _>(...)` and `par_for_each_ref` run queries on all
  cores (rayon), also available to scripts.
- ✅ Physics (Rapier: rigid bodies, colliders, character controller, raycasts and overlap
  queries from scripts, collision events).
- Fixed-timestep simulation loop separate from rendering. Deterministic mode for
  networking and replays.
- ✅ `Commands` (deferred spawn/despawn/insert, usable from parallel queries), typed events
  (`world.send_event` / `world.events::<E>()`, double-buffered per frame) and resources
  (singleton components: `world.resource::<T>()`, `insert_resource`, `resource_or_default`).
- Audio (spatial, streaming) and networking (snapshot or rollback).

## 4. Animation
- State machines and blend trees as assets, plus animation events dispatched to scripts.
- Procedural animation: IK (two-bone, FABRIK), look-at, foot placement, spring bones.
- Root-motion extraction applied to `Transform`.
- Morph targets.

## 5. Editor
- Docking layout. Multi-object editing in the inspector.
- Prefab overrides (per-instance diffs) and nested prefabs.
- ✅ Packaging: File ▸ Build & Export makes a standalone game folder (player, scripts, assets,
  Blender conversions). Still to do: asset bundles / cooked binary formats.
- GPU picking (ID buffer) and selection outlines.
- Material graphs and a shader hot-reload path for user WGSL.
- a list of all changes made, user can undo the last or any of the actions 


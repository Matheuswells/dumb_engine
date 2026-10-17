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
- ✅ Audio (`dumb_audio`): `AudioSource` / `AudioListener` components, a software mixer with 3D
  positioning (distance attenuation, equal-power panning, Doppler), pitch, looping, a voice limit
  that steals the quietest voice, clips decoded on worker threads and long sounds streamed from
  disk (wav, ogg, flac, mp3). Output goes through ALSA loaded at runtime; with no device it stays
  silent but keeps time. Scripts play a sound by setting `playing`. Next: mixer buses/effects,
  Windows/macOS output backends. Networking: see 3.1.

## 3.1 Dedicated server and replication
Authoritative multiplayer for persistent shared worlds (MMO-scale sandboxes): one headless
server owns the simulation, clients render and send input.
- Headless server build: a `render` feature on `dumb_runtime` so the simulation, physics,
  scripts and scene/asset loading compile without `dumb_render`, winit, egui or wry, plus a
  `dumb-server` binary that runs a project with no window or GPU (assets load as data only).
  `simulate()` currently calls `dumb_render::find_primary_camera` for the streaming focus, so
  that moves behind a focus provider (per-client positions on the server), and `dumb-player`
  gets `required-features = ["render"]`.
- Authoritative fixed tick: the server runs `simulate()` at a fixed rate (e.g. 30 Hz) apart
  from any frame rate, stamps state with a tick number, and exposes `is_server()` /
  `is_client()` so scripts can gate logic. Builds on the fixed-timestep loop above.
- Transport: UDP with reliable-ordered and unreliable channels, sequence numbers, connect /
  disconnect / reconnect surfaced to scripts as typed events, and a debug network emulator
  (latency, jitter, loss, reordering) for testing.
- Replication: a `Replicated` component marks entities. Each tick the server diffs their
  reflected components (the same `Value` path as scenes) and sends delta snapshots with
  server→client entity id mapping (including entity references inside component values).
  Clients apply them and interpolate through a jitter buffer; client-side prediction with
  server reconciliation for the owned character.
- Interest management: a spatial hash grid resource (also usable for gameplay queries), a
  per-connection area of interest so clients only receive nearby entities (spawn/despawn on
  enter/leave), and a per-connection bandwidth budget prioritized by distance, like the
  `AiAgent` LOD bands.
- Client input and RPCs: clients send input commands, never replicated state. The server
  validates every command (the trust boundary), and scripts get typed client→server and
  server→client messages with per-entity ownership.
- Session security and persistence: connection handshake with authentication and a cookie
  challenge against UDP spoofing/amplification, per-connection rate limits, and periodic
  world save/restore on the server so the shared world survives restarts.

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
- ✅ User WGSL surface shaders: per-material shader assets compiled at runtime with hot reload, a shader viewer, WGSL in the code editor, and an ink-outline post effect. Next: material graphs and user post-process shaders.
- a list of all changes made, user can undo the last or any of the actions 


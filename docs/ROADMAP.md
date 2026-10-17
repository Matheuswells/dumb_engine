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
- ✅ Fixed-timestep simulation loop separate from rendering: `sim_hz` in the project settings
  (Preferences ▸ Game tick rate) runs scripts, AI, streaming and animation a whole number of
  ticks per frame (`FixedClock`, capped by `sim_max_steps`; 0 keeps one tick per frame). Deterministic
  mode: fixed `1/hz` tick, `Time` derived from the tick counter, `time_scale` ignored. Streaming
  budgets are wall-clock, so streamed content still arrives at frame-dependent times.
- ✅ `Commands` (deferred spawn/despawn/insert, usable from parallel queries), typed events
  (`world.send_event` / `world.events::<E>()`, double-buffered per frame) and resources
  (singleton components: `world.resource::<T>()`, `insert_resource`, `resource_or_default`).
- Audio (spatial, streaming). Networking: see 3.1.

## 3.1 Dedicated server and replication
Authoritative multiplayer for persistent shared worlds (MMO-scale sandboxes): one headless
server owns the simulation, clients render and send input.
- ✅ Headless server build: a `render` feature (on by default) on `dumb_runtime`, so the
  simulation, physics, scripts and scene/asset loading compile without `dumb_render`, winit,
  egui or wry (`cargo build -p dumb_runtime --no-default-features --bin dumb-server`). The
  `dumb-server` binary runs a project with no window or GPU at a fixed tick rate (`--hz`), loads
  models as data for colliders and animation, and stops cleanly on Ctrl+C / SIGTERM. Streaming
  and AI LOD follow the `StreamFocus` resource (several points, the nearest one wins: one per
  client) and fall back to the primary camera. Next: a server option in Build & Export.
- ✅ Authoritative fixed tick: `dumb-server` runs `simulate()` at a fixed rate (`--hz`, 30 by
  default) apart from any frame rate, and stamps state with a tick number (`NetSession.tick`).
  `world.is_server()` / `world.is_client()` let scripts gate logic.
- ✅ Transport: the `dumb_net` crate has UDP with reliable-ordered and unreliable channels,
  sequence numbers, connect / disconnect / reconnect, and a debug network emulator (latency,
  jitter, loss, reordering). The runtime surfaces them to scripts as the typed events
  `NetConnected` (with `reconnected`), `NetDisconnected` and `NetConnectFailed`; clients resume
  a timed-out session automatically.
- ✅ Replication: a `Replicated` component marks entities (`only` limits which components go
  out). Each tick `ServerNet` captures their reflected components (the same `Value` path as
  scenes) and sends each connection a delta against what it was last sent, with server→client
  entity id mapping (including entity references inside component values). `ClientNet`
  applies them through a jitter buffer; for the entity a client owns, `Transform` is left to
  client-side prediction and a `NetReconcile` event (server state + last applied command
  sequence) feeds `dumb_net::Predictor::reconcile`. Deltas ride the reliable-ordered channel.
- ✅ Interest management: `ServerNet` keeps a `SpatialGrid` of replicated entities
  (`ServerNet::grid`, for host-side gameplay queries; scripts reach the same data through
  `StreamFocus` and the per-connection area of interest), a per-connection area of interest
  (spawn/despawn on enter/leave, with hysteresis) and a per-connection bandwidth budget per
  tick, nearest entities first. The viewers also feed `StreamFocus`, so streaming and AI LOD
  follow the players.
- ✅ Client input and RPCs: scripts send `NetSend { message: NetMessage { name, entity,
  payload } }` (payload: any reflected value, `NetMessage::new` / `decode`), on a client as a
  command to the server, on the server to one connection or all. The server runs every command
  through the `CommandGate` (ownership of the target entity, per-tick rate limit, ordering,
  finite-float payload check, an optional game validator) before scripts see it as a
  `ClientCommand`; refused ones become `CommandRejected`. Clients get `ServerMessage`.
- ✅ Session security and persistence: handshake with a keyed cookie challenge against UDP
  spoofing/amplification, pluggable token authentication, per-connection packet rate limits
  (all in `dumb_net`), and `dumb-server --listen ADDR --save FILE` writes the replicated world
  atomically every `--save-every` ticks and on shutdown, and restores it (ownership cleared) at
  startup. `dumb-player --connect ADDR` joins a server.


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


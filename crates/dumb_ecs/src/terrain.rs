//! Procedural terrain: heightfield chunks generated on worker threads.
//!
//! Put a [`Terrain`] on an entity. While the game runs, the [`TerrainStreamer`] keeps a grid
//! of chunks around the camera: each chunk is a [`RuntimeMesh`] built off the main thread,
//! spawned as a root entity with a [`TerrainChunk`] marker, and rebuilt at a coarser
//! resolution as it gets farther away (distance LOD). Chunks are placed in true world space,
//! so the terrain is infinite and works with the floating origin ([`crate::origin`]): the
//! `Terrain` entity's own transform is ignored.
//!
//! Heights come from [`Terrain::height_at`], a pure function of the seed and the position,
//! so scripts can sample the ground (to place objects, say) and neighbouring chunks line up
//! exactly. Seams between chunks of different resolution are hidden by skirts.

use crate::components::{MeshRenderer, Name, Transform};
use crate::meshes::{MeshPart, MeshUploads, RuntimeMesh};
use crate::origin::WorldOrigin;
use crate::physics::{Collider, ColliderShape};
use crate::World;
use dumb_core::{AssetId, Entity, Vec3};
use dumb_derive::{Component, Editor};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

/// An endless procedural heightfield around the camera.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct Terrain {
    pub seed: u32,
    /// Side of one chunk (meters).
    #[editor(range = 8.0..=512.0)]
    pub chunk_size: f32,
    /// Quads along a chunk's side at full detail.
    #[editor(range = 4.0..=128.0)]
    pub resolution: u32,
    /// Height of the tallest features (meters).
    #[editor(range = 0.0..=2000.0)]
    pub height: f32,
    /// Base noise frequency (cycles per meter); each octave doubles it.
    #[editor(speed = 0.0005)]
    pub frequency: f32,
    #[editor(range = 1.0..=10.0)]
    pub octaves: u32,
    /// Chunks are kept this far from the camera (meters).
    #[editor(range = 16.0..=20000.0)]
    pub view_distance: f32,
    /// Distance where the first LOD change happens; each next level doubles it.
    #[editor(range = 8.0..=10000.0)]
    pub lod_distance: f32,
    /// Number of LOD levels; each halves the resolution.
    #[editor(range = 1.0..=6.0)]
    pub lod_count: u32,
    /// Material of the chunks (none = the default material).
    #[editor(asset = "material")]
    pub material: AssetId,
    /// Give full-detail chunks a mesh collider.
    pub collision: bool,
    pub cast_shadows: bool,
    /// Chunks currently alive, for display.
    #[editor(readonly)]
    pub chunks: u32,
}

impl Default for Terrain {
    fn default() -> Self {
        Terrain {
            seed: 1,
            chunk_size: 64.0,
            resolution: 32,
            height: 40.0,
            frequency: 0.004,
            octaves: 5,
            view_distance: 600.0,
            lod_distance: 150.0,
            lod_count: 4,
            material: AssetId::NONE,
            collision: true,
            cast_shadows: false,
            chunks: 0,
        }
    }
}

/// Marks the entities the streamer made.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct TerrainChunk {
    pub terrain: Entity,
    #[editor(readonly)]
    pub x: i32,
    #[editor(readonly)]
    pub z: i32,
    #[editor(readonly)]
    pub lod: u32,
}

impl Default for TerrainChunk {
    fn default() -> Self {
        TerrainChunk { terrain: Entity::NONE, x: 0, z: 0, lod: 0 }
    }
}

/// The inputs that change the shape of the ground (a change rebuilds every chunk).
#[derive(Clone, Copy, PartialEq, Debug)]
struct Shape {
    seed: u32,
    chunk_size: f32,
    resolution: u32,
    height: f32,
    frequency: f32,
    octaves: u32,
}

impl Terrain {
    fn shape(&self) -> Shape {
        Shape {
            seed: self.seed,
            chunk_size: self.chunk_size.max(1.0),
            resolution: self.resolution.clamp(2, 256),
            height: self.height,
            frequency: self.frequency,
            octaves: self.octaves.clamp(1, 12),
        }
    }

    /// Ground height at a true world position (fractal value noise, 0..=`height`).
    pub fn height_at(&self, x: f64, z: f64) -> f32 {
        self.shape().height_at(x, z)
    }

    /// The LOD level for a chunk this far from the camera.
    pub fn lod_for_distance(&self, d: f32) -> u32 {
        let levels = self.lod_count.max(1);
        if d < self.lod_distance || self.lod_distance <= 0.0 {
            return 0;
        }
        ((d / self.lod_distance).log2().floor() as u32 + 1).min(levels - 1)
    }
}

fn hash(seed: u32, x: i64, z: i64) -> f32 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B1_85EB_CA87) ^ (z as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ (seed as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^= h >> 33;
    (h >> 40) as f32 / (1u64 << 24) as f32
}

fn value_noise(seed: u32, x: f64, z: f64) -> f32 {
    let (x0, z0) = (x.floor(), z.floor());
    let (fx, fz) = ((x - x0) as f32, (z - z0) as f32);
    let (sx, sz) = (fx * fx * (3.0 - 2.0 * fx), fz * fz * (3.0 - 2.0 * fz));
    let (ix, iz) = (x0 as i64, z0 as i64);
    let a = hash(seed, ix, iz);
    let b = hash(seed, ix + 1, iz);
    let c = hash(seed, ix, iz + 1);
    let d = hash(seed, ix + 1, iz + 1);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    top + (bottom - top) * sz
}

impl Shape {
    fn height_at(&self, x: f64, z: f64) -> f32 {
        let (mut sum, mut amp, mut freq, mut norm) = (0.0f32, 1.0f32, self.frequency as f64, 0.0f32);
        for o in 0..self.octaves {
            sum += value_noise(self.seed.wrapping_add(o.wrapping_mul(0x85EB_CA6B)), x * freq, z * freq) * amp;
            norm += amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum / norm.max(1e-6) * self.height
    }
}

/// Build one chunk's mesh (chunk-local coordinates: x/z in 0..=`chunk_size`, y = height).
/// Runs on any thread.
fn build_chunk(shape: Shape, cx: i32, cz: i32, lod: u32) -> RuntimeMesh {
    let n = (shape.resolution >> lod).max(2) as usize;
    let size = shape.chunk_size;
    let step = size / n as f32;
    let (ox, oz) = (cx as f64 * size as f64, cz as f64 * size as f64);
    let row = n + 1;
    // Normals use a fixed sample distance so lighting matches across LOD levels.
    let e = (size / shape.resolution as f32).max(0.01) as f64;
    // Whole texture tiles per chunk so the texture repeats seamlessly across chunks.
    let tiles = (size / 8.0).round().max(1.0);
    let uv_of = |i: usize, j: usize| {
        let u = ((cx.rem_euclid(1024)) as f32 + i as f32 / n as f32) * tiles;
        let v = ((cz.rem_euclid(1024)) as f32 + j as f32 / n as f32) * tiles;
        [u, v]
    };
    let mut positions = Vec::with_capacity(row * row + 4 * row);
    let mut normals = Vec::with_capacity(positions.capacity());
    let mut uvs = Vec::with_capacity(positions.capacity());
    for j in 0..row {
        for i in 0..row {
            let (lx, lz) = (i as f32 * step, j as f32 * step);
            let (wx, wz) = (ox + lx as f64, oz + lz as f64);
            let h = shape.height_at(wx, wz);
            let dx = shape.height_at(wx + e, wz) - shape.height_at(wx - e, wz);
            let dz = shape.height_at(wx, wz + e) - shape.height_at(wx, wz - e);
            let nrm = Vec3::new(-dx, 2.0 * e as f32, -dz).normalize();
            positions.push([lx, h, lz]);
            normals.push(nrm.to_array());
            uvs.push(uv_of(i, j));
        }
    }
    let mut indices = Vec::with_capacity(n * n * 6 + 4 * n * 6);
    for j in 0..n {
        for i in 0..n {
            let a = (j * row + i) as u32;
            let b = ((j + 1) * row + i) as u32;
            let c = a + 1;
            let d = b + 1;
            indices.extend_from_slice(&[a, b, c, c, b, d]);
        }
    }
    // Skirts: a vertical flap down every border hides cracks between LOD levels.
    let depth = step * 2.0 + shape.height * 0.02;
    let mut skirt = |edge: Vec<usize>, flip: bool| {
        let base = positions.len() as u32;
        for &v in &edge {
            let mut p = positions[v];
            p[1] -= depth;
            positions.push(p);
            normals.push(normals[v]);
            uvs.push(uvs[v]);
        }
        for k in 0..edge.len() - 1 {
            let (t0, t1) = (edge[k] as u32, edge[k + 1] as u32);
            let (u0, u1) = (base + k as u32, base + k as u32 + 1);
            if flip {
                indices.extend_from_slice(&[t0, u0, t1, u0, u1, t1]);
            } else {
                indices.extend_from_slice(&[t0, t1, u0, u0, t1, u1]);
            }
        }
    };
    skirt((0..row).collect(), false); // z = 0
    skirt((0..row).map(|i| n * row + i).collect(), true); // z = max
    skirt((0..row).map(|j| j * row).collect(), true); // x = 0
    skirt((0..row).map(|j| j * row + n).collect(), false); // x = max
    RuntimeMesh { positions, normals, uvs, tangents: Vec::new(), parts: vec![MeshPart { indices, material: AssetId::NONE }] }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Key {
    terrain: Entity,
    x: i32,
    z: i32,
}

struct Chunk {
    entity: Option<Entity>,
    /// LOD of the mesh that is uploaded.
    lod: Option<u32>,
    /// Build in flight, and the LOD it builds.
    job: Option<(u32, Receiver<RuntimeMesh>)>,
}

/// What the streamer did, for the performance overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TerrainStats {
    pub chunks: u32,
    pub building: u32,
    /// Chunks created, rebuilt or removed this frame.
    pub churn: u32,
}

/// Keeps `Terrain` chunks around the camera. One per running game.
#[derive(Default)]
pub struct TerrainStreamer {
    chunks: HashMap<Key, Chunk>,
    shapes: HashMap<Entity, Shape>,
}

/// Most meshes handed to the renderer per frame, and builds in flight at once.
const MAX_APPLY_PER_FRAME: usize = 4;

fn chunk_mesh_id(k: Key) -> AssetId {
    AssetId::from_name(&format!("dumb/terrain/{:x}/{}/{}", k.terrain.to_bits(), k.x, k.z))
}

impl TerrainStreamer {
    /// Advance the streaming. `focus` is the camera's local position.
    pub fn update(&mut self, world: &mut World, meshes: &mut MeshUploads, focus: Option<Vec3>) -> TerrainStats {
        let mut stats = TerrainStats::default();
        let origin = world.resource::<WorldOrigin>().copied().unwrap_or_default();
        let terrains: Vec<(Entity, Terrain)> = world.query_ref::<(Entity, &Terrain)>().map(|(e, t)| (e, t.clone())).collect();

        // Terrains that went away or changed shape lose their chunks.
        let live: HashMap<Entity, Shape> = terrains.iter().map(|(e, t)| (*e, t.shape())).collect();
        let stale: Vec<Key> = self.chunks.keys().filter(|k| live.get(&k.terrain) != self.shapes.get(&k.terrain) || !live.contains_key(&k.terrain)).copied().collect();
        for k in stale {
            self.drop_chunk(world, meshes, k, &mut stats);
        }
        self.shapes = live;

        let max_jobs = std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1).max(1));
        let mut building = self.chunks.values().filter(|c| c.job.is_some()).count();

        for (te, terrain) in &terrains {
            let Some(focus) = focus else { continue };
            let shape = terrain.shape();
            let size = shape.chunk_size as f64;
            let (fx, fz) = (origin.x + focus.x as f64, origin.z + focus.z as f64);
            let view = terrain.view_distance.max(shape.chunk_size) as f64;
            let radius = (view / size).ceil() as i32 + 1;
            let (ccx, ccz) = ((fx / size).floor() as i32, (fz / size).floor() as i32);

            // Wanted chunks, nearest first.
            let mut wanted: Vec<(f32, Key, u32)> = Vec::new();
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    let (x, z) = (ccx + dx, ccz + dz);
                    let (mx, mz) = ((x as f64 + 0.5) * size, (z as f64 + 0.5) * size);
                    let d = ((mx - fx).powi(2) + (mz - fz).powi(2)).sqrt();
                    if d <= view + size * 0.71 {
                        wanted.push((d as f32, Key { terrain: *te, x, z }, terrain.lod_for_distance(d as f32 - shape.chunk_size * 0.5)));
                    }
                }
            }
            wanted.sort_by(|a, b| a.0.total_cmp(&b.0));

            // Drop chunks well outside the view (one chunk of hysteresis).
            let keep: std::collections::HashSet<Key> = wanted.iter().map(|w| w.1).collect();
            let far: Vec<Key> = self
                .chunks
                .keys()
                .filter(|k| k.terrain == *te && !keep.contains(k))
                .filter(|k| {
                    let (mx, mz) = ((k.x as f64 + 0.5) * size, (k.z as f64 + 0.5) * size);
                    ((mx - fx).powi(2) + (mz - fz).powi(2)).sqrt() > view + size * 2.0
                })
                .copied()
                .collect();
            for k in far {
                if self.chunks.get(&k).is_some_and(|c| c.job.is_some()) {
                    building -= 1;
                }
                self.drop_chunk(world, meshes, k, &mut stats);
            }

            // Start builds for missing / wrong-LOD chunks.
            for (_, key, lod) in &wanted {
                let chunk = self.chunks.entry(*key).or_insert(Chunk { entity: None, lod: None, job: None });
                let current = chunk.job.as_ref().map(|j| j.0).or(chunk.lod);
                if current != Some(*lod) && building < max_jobs {
                    let (tx, rx) = channel();
                    let (cx, cz, lod) = (key.x, key.z, *lod);
                    std::thread::spawn(move || {
                        let _ = tx.send(build_chunk(shape, cx, cz, lod));
                    });
                    // A replaced in-flight job is dropped; its thread's send just fails.
                    if chunk.job.replace((lod, rx)).is_none() {
                        building += 1;
                    }
                }
            }
        }

        // Finished builds become entities / replaced meshes.
        let keys: Vec<Key> = self.chunks.iter().filter(|(_, c)| c.job.is_some()).map(|(k, _)| *k).collect();
        let mut applied = 0;
        for key in keys {
            if applied >= MAX_APPLY_PER_FRAME {
                break;
            }
            let Some(terrain) = terrains.iter().find(|(e, _)| *e == key.terrain).map(|(_, t)| t) else { continue };
            let chunk = self.chunks.get_mut(&key).unwrap();
            let Some((lod, rx)) = chunk.job.take() else { continue };
            match rx.try_recv() {
                Ok(mut mesh) => {
                    applied += 1;
                    let id = chunk_mesh_id(key);
                    for part in &mut mesh.parts {
                        part.material = AssetId::NONE;
                    }
                    meshes.set(id, mesh);
                    let e = match chunk.entity.filter(|e| world.is_alive(*e)) {
                        Some(e) => e,
                        None => {
                            let e = world.spawn();
                            world.insert(e, Name { name: format!("Terrain chunk {} {}", key.x, key.z) });
                            world.insert(e, MeshRenderer { model: id, ..Default::default() });
                            e
                        }
                    };
                    chunk.entity = Some(e);
                    chunk.lod = Some(lod);
                    if let Some(mr) = world.get_mut::<MeshRenderer>(e) {
                        mr.material = terrain.material;
                        mr.cast_shadows = terrain.cast_shadows;
                    }
                    world.insert(e, TerrainChunk { terrain: key.terrain, x: key.x, z: key.z, lod });
                    // Only full-detail chunks collide; changing the collider rebuilds it.
                    if lod == 0 && terrain.collision {
                        if !world.has::<Collider>(e) {
                            world.insert(e, Collider { shape: ColliderShape::Mesh, auto_fit: false, ..Default::default() });
                        }
                    } else {
                        world.remove::<Collider>(e);
                    }
                    stats.churn += 1;
                }
                Err(TryRecvError::Empty) => chunk.job = Some((lod, rx)),
                Err(TryRecvError::Disconnected) => {}
            }
        }

        // Keep chunk positions exact in local space (the origin may have moved).
        for (key, chunk) in &self.chunks {
            let (Some(e), Some((_, t))) = (chunk.entity, terrains.iter().find(|(e, _)| *e == key.terrain)) else { continue };
            let size = t.shape().chunk_size as f64;
            let local = Vec3::new((key.x as f64 * size - origin.x) as f32, (0.0 - origin.y) as f32, (key.z as f64 * size - origin.z) as f32);
            match world.get_mut::<Transform>(e) {
                Some(tr) => {
                    if tr.translation != local {
                        tr.translation = local;
                    }
                }
                None => world.insert(e, Transform::from_translation(local)),
            }
        }

        stats.building = self.chunks.values().filter(|c| c.job.is_some()).count() as u32;
        for (te, _) in &terrains {
            let n = self.chunks.iter().filter(|(k, c)| k.terrain == *te && c.entity.is_some()).count() as u32;
            stats.chunks += n;
            if let Some(t) = world.get_mut::<Terrain>(*te) {
                t.chunks = n;
            }
        }
        stats
    }

    fn drop_chunk(&mut self, world: &mut World, meshes: &mut MeshUploads, key: Key, stats: &mut TerrainStats) {
        if let Some(chunk) = self.chunks.remove(&key) {
            if let Some(e) = chunk.entity {
                world.despawn(e);
                meshes.remove(chunk_mesh_id(key));
                stats.churn += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> Shape {
        Terrain::default().shape()
    }

    #[test]
    fn heights_are_deterministic_and_bounded() {
        let t = Terrain::default();
        assert_eq!(t.height_at(123.4, -987.6), t.height_at(123.4, -987.6));
        let other = Terrain { seed: 2, ..Default::default() };
        assert_ne!(t.height_at(123.4, -987.6), other.height_at(123.4, -987.6));
        for i in 0..200 {
            let h = t.height_at(i as f64 * 37.3 - 3000.0, i as f64 * -11.9);
            assert!((0.0..=t.height).contains(&h), "{h}");
        }
    }

    #[test]
    fn heights_work_far_from_the_origin() {
        let t = Terrain::default();
        let h = t.height_at(5.0e7, -5.0e7);
        assert!((0.0..=t.height).contains(&h));
    }

    #[test]
    fn neighbouring_chunks_share_their_border() {
        let s = shape();
        let (a, b) = (build_chunk(s, 3, -2, 0), build_chunk(s, 4, -2, 0));
        let n = s.resolution as usize;
        let row = n + 1;
        for j in 0..row {
            assert_eq!(a.positions[j * row + n][1], b.positions[j * row][1]);
        }
    }

    #[test]
    fn lod_reduces_triangles() {
        let s = shape();
        let tris: Vec<usize> = (0..4).map(|l| build_chunk(s, 0, 0, l).triangles()).collect();
        assert!(tris.windows(2).all(|w| w[0] > w[1]), "{tris:?}");
    }

    #[test]
    fn faces_point_the_right_way() {
        let s = shape();
        let m = build_chunk(s, 0, 0, 1);
        let n = (s.resolution >> 1) as usize;
        let top_tris = n * n * 2;
        let centre = Vec3::new(s.chunk_size * 0.5, 0.0, s.chunk_size * 0.5);
        for (t, tri) in m.parts[0].indices.chunks(3).enumerate() {
            let p: Vec<Vec3> = tri.iter().map(|i| Vec3::from(m.positions[*i as usize])).collect();
            let normal = (p[1] - p[0]).cross(p[2] - p[0]);
            if t < top_tris {
                assert!(normal.y > 0.0, "top triangle {t} faces down");
            } else {
                let out = (p[0] + p[1] + p[2]) / 3.0 - centre;
                assert!(normal.dot(Vec3::new(out.x, 0.0, out.z)) > 0.0, "skirt triangle {t} faces inward");
            }
        }
        assert_eq!(m.normals.len(), m.positions.len());
        assert_eq!(m.uvs.len(), m.positions.len());
        assert!(m.normals.iter().all(|n| (Vec3::from(*n).length() - 1.0).abs() < 1e-4));
    }

    #[test]
    fn lod_levels_by_distance() {
        let t = Terrain { lod_distance: 100.0, lod_count: 3, ..Default::default() };
        assert_eq!(t.lod_for_distance(50.0), 0);
        assert_eq!(t.lod_for_distance(150.0), 1);
        assert_eq!(t.lod_for_distance(250.0), 2);
        assert_eq!(t.lod_for_distance(9000.0), 2);
    }

    #[test]
    fn streams_chunks_around_the_focus_and_unloads_them() {
        let mut w = World::new();
        let te = w.spawn();
        w.insert(te, Terrain { view_distance: 100.0, chunk_size: 64.0, resolution: 8, ..Default::default() });
        let mut st = TerrainStreamer::default();
        let mut up = MeshUploads::default();
        let mut stats = TerrainStats::default();
        for _ in 0..400 {
            stats = st.update(&mut w, &mut up, Some(Vec3::new(10.0, 5.0, 10.0)));
            if stats.building == 0 && stats.chunks > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(stats.chunks >= 4, "{stats:?}");
        assert_eq!(w.resource::<Terrain>().unwrap().chunks, stats.chunks);
        let before = up.set.len();
        assert!(before >= 4);
        let chunk_entities = w.query_ref::<&TerrainChunk>().count();
        assert_eq!(chunk_entities as u32, stats.chunks);
        // Nearby chunk has a collider and sits at its true position.
        let (e, c) = w.query_ref::<(Entity, &TerrainChunk)>().find(|(_, c)| c.x == 0 && c.z == 0).map(|(e, c)| (e, c.clone())).unwrap();
        assert_eq!(c.lod, 0);
        assert!(w.has::<Collider>(e));
        assert_eq!(w.get::<Transform>(e).unwrap().translation, Vec3::ZERO);

        // Move the origin: the chunk keeps its true position.
        w.insert_resource(WorldOrigin { x: 64.0, y: 0.0, z: 0.0 });
        st.update(&mut w, &mut up, Some(Vec3::new(-54.0, 5.0, 10.0)));
        assert_eq!(w.get::<Transform>(e).unwrap().translation, Vec3::new(-64.0, 0.0, 0.0));

        // Far away: everything unloads.
        st.update(&mut w, &mut up, Some(Vec3::new(1.0e6, 0.0, 0.0)));
        assert!(w.query_ref::<&TerrainChunk>().all(|c| c.x > 1000));
        assert!(!up.remove.is_empty());
    }

    #[test]
    fn deleting_the_terrain_removes_its_chunks() {
        let mut w = World::new();
        let te = w.spawn();
        w.insert(te, Terrain { view_distance: 40.0, resolution: 4, ..Default::default() });
        let mut st = TerrainStreamer::default();
        let mut up = MeshUploads::default();
        for _ in 0..400 {
            let s = st.update(&mut w, &mut up, Some(Vec3::ZERO));
            if s.building == 0 && s.chunks > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(w.query_ref::<&TerrainChunk>().count() > 0);
        w.despawn(te);
        st.update(&mut w, &mut up, Some(Vec3::ZERO));
        assert_eq!(w.query_ref::<&TerrainChunk>().count(), 0);
    }
}

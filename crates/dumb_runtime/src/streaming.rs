//! World streaming: loads `StreamingCell` scenes around the camera while the game runs.
//!
//! Loading a cell has three stages, none of which may stall a frame:
//! 1. read + parse the scene file on a worker thread,
//! 2. spawn its entities a slice at a time within the frame budget,
//! 3. (their models and textures load asynchronously through the asset database).
//!
//! Unloading despawns a cell's entities, also within the budget.

use dumb_asset::AssetDatabase;
use dumb_core::{AssetId, Entity, Vec3};
use dumb_ecs::{IncrementalSpawn, SceneData, StreamingCell, World};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

enum Job {
    Parsing(AssetId, Receiver<Result<SceneData, String>>),
    Spawning(IncrementalSpawn),
    Loaded(Vec<Entity>),
    Unloading(Vec<Entity>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StreamStats {
    pub cells: u32,
    pub loaded: u32,
    pub loading: u32,
    /// Entities spawned + despawned this frame.
    pub churn: u32,
    pub ms: f32,
}

#[derive(Default)]
pub struct Streamer {
    jobs: HashMap<Entity, Job>,
    /// Parsed scenes, shared by cells that use the same scene asset.
    cache: HashMap<AssetId, Arc<SceneData>>,
}

impl Streamer {
    /// Advance streaming. `budget` caps the main-thread time spent spawning/despawning.
    pub fn update(&mut self, world: &mut World, db: &AssetDatabase, focus: Option<Vec3>, budget: Duration) -> StreamStats {
        let t0 = Instant::now();
        let mut stats = StreamStats::default();
        let Some(focus) = focus else { return stats };
        let cells: Vec<(Entity, StreamingCell, Vec3)> =
            world.query_ref::<(Entity, &StreamingCell)>().map(|(e, c)| (e, c.clone(), world.global_matrix(e).w_axis.truncate())).collect();
        stats.cells = cells.len() as u32;

        // Decide loads/unloads.
        for (e, cell, pos) in &cells {
            let d = pos.distance(focus);
            let active = self.jobs.contains_key(e) && !matches!(self.jobs.get(e), Some(Job::Unloading(_)));
            if !active && d <= cell.load_distance && !cell.scene.is_none() {
                if let Some(Job::Unloading(rest)) = self.jobs.remove(e) {
                    despawn_all(world, &rest);
                }
                let job = match self.cache.get(&cell.scene) {
                    Some(data) => Job::Spawning(IncrementalSpawn::new(data.clone(), world, Some(*e))),
                    None => match db.abs_path(cell.scene) {
                        Some(path) => {
                            let (tx, rx) = std::sync::mpsc::channel();
                            std::thread::spawn(move || {
                                let r = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| SceneData::from_ron(&s).map_err(|e| e.to_string()));
                                let _ = tx.send(r);
                            });
                            Job::Parsing(cell.scene, rx)
                        }
                        None => continue,
                    },
                };
                self.jobs.insert(*e, job);
            } else if active && d > cell.unload_distance.max(cell.load_distance) {
                let spawned = match self.jobs.remove(e) {
                    Some(Job::Spawning(s)) => s.spawned,
                    Some(Job::Loaded(list)) => list,
                    _ => Vec::new(),
                };
                self.jobs.insert(*e, Job::Unloading(spawned));
            }
        }
        // Cells that were deleted: drop their jobs (their children went with them).
        self.jobs.retain(|e, _| world.is_alive(*e));

        // Work through jobs within the budget.
        let ids: Vec<Entity> = self.jobs.keys().copied().collect();
        for e in ids {
            let left = budget.saturating_sub(t0.elapsed());
            let job = self.jobs.remove(&e).unwrap();
            let next = match job {
                Job::Parsing(scene, rx) => match rx.try_recv() {
                    Ok(Ok(data)) => {
                        let data = Arc::new(data);
                        self.cache.insert(scene, data.clone());
                        Some(Job::Spawning(IncrementalSpawn::new(data, world, Some(e))))
                    }
                    Ok(Err(err)) => {
                        log::error!("streaming: cell scene failed to load: {err}");
                        None
                    }
                    Err(TryRecvError::Empty) => Some(Job::Parsing(scene, rx)),
                    Err(TryRecvError::Disconnected) => None,
                },
                Job::Spawning(mut s) if !left.is_zero() => {
                    stats.churn += s.step(world, left) as u32;
                    if s.is_finished() {
                        Some(Job::Loaded(s.spawned))
                    } else {
                        Some(Job::Spawning(s))
                    }
                }
                Job::Unloading(mut list) if !left.is_zero() => {
                    let t = Instant::now();
                    while let Some(x) = list.pop() {
                        world.despawn(x);
                        stats.churn += 1;
                        if list.len() % 32 == 0 && t.elapsed() >= left {
                            break;
                        }
                    }
                    if list.is_empty() {
                        None
                    } else {
                        Some(Job::Unloading(list))
                    }
                }
                other => Some(other),
            };
            if let Some(n) = next {
                self.jobs.insert(e, n);
            }
        }

        // Reflect state on the components.
        for (e, c) in world.query::<(Entity, &mut StreamingCell)>() {
            let (state, n) = match self.jobs.get(&e) {
                None | Some(Job::Unloading(_)) => (0, 0),
                Some(Job::Parsing(..)) => (1, 0),
                Some(Job::Spawning(s)) => (1, s.done() as u32),
                Some(Job::Loaded(list)) => (2, list.len() as u32),
            };
            c.state = state;
            c.entities = n;
            match state {
                1 => stats.loading += 1,
                2 => stats.loaded += 1,
                _ => {}
            }
        }
        stats.ms = t0.elapsed().as_secs_f32() * 1000.0;
        stats
    }
}

fn despawn_all(world: &mut World, list: &[Entity]) {
    for e in list {
        world.despawn(*e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumb_ecs::{Name, Parent, Transform};

    #[test]
    fn cells_load_and_unload_around_the_camera() {
        let dir = std::env::temp_dir().join(format!("dumb_stream_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Assets")).unwrap();
        // A cell scene with 3000 entities.
        let mut src = World::new();
        for i in 0..3000 {
            src.spawn_named(&format!("rock {i}"));
        }
        std::fs::write(dir.join("Assets/Cell.scene"), SceneData::capture(&src).to_ron().unwrap()).unwrap();
        let db = AssetDatabase::open(&dir).unwrap();
        let scene = db.id_for_path("Cell.scene").unwrap();

        let mut w = World::new();
        let cell = w.spawn_named("cell");
        w.get_mut::<Transform>(cell).unwrap().translation = Vec3::new(100.0, 0.0, 0.0);
        w.insert(cell, StreamingCell { scene, load_distance: 50.0, unload_distance: 80.0, ..Default::default() });
        w.update_transforms();
        let mut s = Streamer::default();
        let budget = Duration::from_micros(500);

        // Far away: nothing happens.
        s.update(&mut w, &db, Some(Vec3::ZERO), budget);
        assert_eq!(w.entity_count(), 1);

        // Close: loads over several frames.
        let mut frames = 0;
        while w.get::<StreamingCell>(cell).unwrap().state != 2 {
            s.update(&mut w, &db, Some(Vec3::new(90.0, 0.0, 0.0)), budget);
            frames += 1;
            std::thread::sleep(Duration::from_millis(1));
            assert!(frames < 10_000, "never finished loading");
        }
        assert_eq!(w.entity_count(), 3001);
        assert!(frames > 2, "load was spread over frames ({frames})");
        let any = w.query_ref::<(&Name, &Parent)>().find(|(n, _)| n.name == "rock 7").map(|(_, p)| p.entity);
        assert_eq!(any, Some(cell));

        // Between load and unload distance: stays loaded.
        s.update(&mut w, &db, Some(Vec3::new(30.0, 0.0, 0.0)), budget);
        assert_eq!(w.get::<StreamingCell>(cell).unwrap().state, 2);

        // Far: unloads.
        for _ in 0..10_000 {
            s.update(&mut w, &db, Some(Vec3::new(-100.0, 0.0, 0.0)), budget);
            if w.entity_count() == 1 {
                break;
            }
        }
        assert_eq!(w.entity_count(), 1, "only the cell is left");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

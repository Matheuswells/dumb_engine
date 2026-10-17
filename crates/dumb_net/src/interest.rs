//! Interest management: a spatial hash grid (also usable for gameplay queries) and a
//! per-connection relevance set with spawn / despawn on enter / leave and a bandwidth budget
//! prioritized by distance.

use glam::Vec3;
use std::collections::{HashMap, HashSet};

/// Spatial hash grid over the XZ plane (Y is ignored for cell lookup, kept for exact distance).
#[derive(Default)]
pub struct SpatialGrid<K: Copy + Eq + std::hash::Hash> {
    cell: f32,
    cells: HashMap<(i32, i32), Vec<K>>,
    pos: HashMap<K, (Vec3, (i32, i32))>,
}

impl<K: Copy + Eq + std::hash::Hash> SpatialGrid<K> {
    pub fn new(cell_size: f32) -> Self {
        Self { cell: cell_size.max(0.001), cells: HashMap::new(), pos: HashMap::new() }
    }

    fn key(&self, p: Vec3) -> (i32, i32) {
        ((p.x / self.cell).floor() as i32, (p.z / self.cell).floor() as i32)
    }

    pub fn len(&self) -> usize {
        self.pos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pos.is_empty()
    }

    /// Inserts or moves an item.
    pub fn insert(&mut self, k: K, p: Vec3) {
        let c = self.key(p);
        if let Some((old, oc)) = self.pos.get_mut(&k) {
            *old = p;
            if *oc == c {
                return;
            }
            let oc_val = *oc;
            *oc = c;
            if let Some(v) = self.cells.get_mut(&oc_val) {
                v.retain(|x| *x != k);
                if v.is_empty() {
                    self.cells.remove(&oc_val);
                }
            }
        } else {
            self.pos.insert(k, (p, c));
        }
        self.cells.entry(c).or_default().push(k);
    }

    pub fn remove(&mut self, k: K) {
        if let Some((_, c)) = self.pos.remove(&k) {
            if let Some(v) = self.cells.get_mut(&c) {
                v.retain(|x| *x != k);
                if v.is_empty() {
                    self.cells.remove(&c);
                }
            }
        }
    }

    pub fn position(&self, k: K) -> Option<Vec3> {
        self.pos.get(&k).map(|p| p.0)
    }

    /// Items within `radius` of `center` (3D distance), with their squared distance.
    pub fn query_radius(&self, center: Vec3, radius: f32) -> Vec<(K, f32)> {
        let (x0, z0) = self.key(center - Vec3::splat(radius));
        let (x1, z1) = self.key(center + Vec3::splat(radius));
        let r2 = radius * radius;
        let mut out = Vec::new();
        for x in x0..=x1 {
            for z in z0..=z1 {
                for k in self.cells.get(&(x, z)).into_iter().flatten() {
                    let d2 = self.pos[k].0.distance_squared(center);
                    if d2 <= r2 {
                        out.push((*k, d2));
                    }
                }
            }
        }
        out
    }
}

/// What to send a connection this tick.
#[derive(Debug, Default, PartialEq)]
pub struct InterestUpdate<K> {
    /// Entered the area of interest: send a full spawn.
    pub spawn: Vec<K>,
    /// Left the area of interest: send a despawn.
    pub despawn: Vec<K>,
    /// Still relevant and selected within the budget: send a delta.
    pub update: Vec<K>,
}

/// One connection's area of interest.
#[derive(Default)]
pub struct Interest<K: Copy + Eq + std::hash::Hash> {
    known: HashSet<K>,
    /// Updates in a row an entity was due but didn't fit the budget (raises its priority).
    starved: HashMap<K, u32>,
}

impl<K: Copy + Eq + std::hash::Hash + Ord> Interest<K> {
    pub fn new() -> Self {
        Self { known: HashSet::new(), starved: HashMap::new() }
    }

    pub fn known(&self) -> &HashSet<K> {
        &self.known
    }

    /// Computes spawns/despawns for a viewer at `center` and picks which known entities get an
    /// update. `cost(k)` is the estimated bytes of an update; closer entities are served first
    /// until `budget` bytes are spent (spawns always go out and count toward the budget).
    /// `hysteresis` (>1) makes the leave radius larger than the enter radius to avoid flicker.
    pub fn update(
        &mut self,
        grid: &SpatialGrid<K>,
        center: Vec3,
        radius: f32,
        hysteresis: f32,
        budget: usize,
        cost: impl Fn(K) -> usize,
    ) -> InterestUpdate<K> {
        let near = grid.query_radius(center, radius);
        let keep: HashSet<K> = grid.query_radius(center, radius * hysteresis.max(1.0)).into_iter().map(|n| n.0).collect();
        let mut out = InterestUpdate { spawn: vec![], despawn: vec![], update: vec![] };
        let mut spent = 0usize;
        let mut ranked = near;
        ranked.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        for (k, _) in &ranked {
            if !self.known.contains(k) {
                out.spawn.push(*k);
                spent += cost(*k);
            }
        }
        for k in self.known.iter().copied().collect::<Vec<_>>() {
            if !keep.contains(&k) {
                self.known.remove(&k);
                self.starved.remove(&k);
                out.despawn.push(k);
            }
        }
        out.despawn.sort();
        for k in &out.spawn {
            self.known.insert(*k);
        }
        // Known entities still inside the keep radius, nearest first.
        // Distance is divided by how long an entity has been left out, so when the budget can't
        // cover everything, far entities still get their turn.
        let mut known: Vec<(K, f32)> = self
            .known
            .iter()
            .filter(|k| !out.spawn.contains(k))
            .filter_map(|k| {
                let age = self.starved.get(k).copied().unwrap_or(0) as f32;
                grid.position(*k).map(|p| (*k, (p.distance_squared(center) + 1.0) / (1.0 + age)))
            })
            .collect();
        known.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        for (k, _) in known {
            let c = cost(k);
            // An entry costlier than the whole budget still goes out when it is first in line,
            // otherwise it would never be sent. Others that don't fit are skipped, not blocking.
            if spent + c > budget && !(out.update.is_empty() && spent == 0) {
                if c > 0 {
                    *self.starved.entry(k).or_insert(0) += 1;
                }
                continue;
            }
            spent += c;
            self.starved.remove(&k);
            out.update.push(k);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_queries_and_moves() {
        let mut g = SpatialGrid::new(10.0);
        g.insert(1u32, Vec3::new(0.0, 0.0, 0.0));
        g.insert(2, Vec3::new(35.0, 0.0, 0.0));
        g.insert(3, Vec3::new(-5.0, 100.0, -5.0));
        let ids = |g: &SpatialGrid<u32>, r| {
            let mut v: Vec<u32> = g.query_radius(Vec3::ZERO, r).into_iter().map(|n| n.0).collect();
            v.sort();
            v
        };
        assert_eq!(ids(&g, 8.0), vec![1]);
        assert_eq!(ids(&g, 40.0), vec![1, 2]);
        g.insert(2, Vec3::new(3.0, 0.0, 0.0));
        assert_eq!(ids(&g, 8.0), vec![1, 2]);
        g.remove(1);
        assert_eq!(ids(&g, 8.0), vec![2]);
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn far_and_oversized_entities_are_not_starved() {
        let mut g = SpatialGrid::new(10.0);
        for i in 0..4u32 {
            g.insert(i, Vec3::new(i as f32, 0.0, 0.0));
        }
        let mut int = Interest::new();
        int.update(&g, Vec3::ZERO, 50.0, 1.5, 1000, |_| 10);
        // Room for one update per round: the farthest still gets served within a few rounds.
        let mut served = HashSet::new();
        for _ in 0..40 {
            served.extend(int.update(&g, Vec3::ZERO, 50.0, 1.5, 10, |_| 10).update);
        }
        assert_eq!(served.len(), 4);
        // An entity costlier than the whole budget goes out alone, and doesn't block others.
        let u = int.update(&g, Vec3::ZERO, 50.0, 1.5, 10, |k| if k == 0 { 99 } else { 10 });
        assert_eq!(u.update, vec![0]);
        let u = int.update(&g, Vec3::ZERO, 50.0, 1.5, 10, |k| if k == 0 { 99 } else { 10 });
        assert_eq!(u.update.len(), 1);
    }

    #[test]
    fn interest_spawn_despawn_and_budget() {
        let mut g = SpatialGrid::new(10.0);
        for i in 0..5u32 {
            g.insert(i, Vec3::new(i as f32 * 4.0, 0.0, 0.0));
        }
        let mut int = Interest::new();
        let u = int.update(&g, Vec3::ZERO, 10.0, 1.5, 1000, |_| 10);
        assert_eq!(u.spawn, vec![0, 1, 2]);
        assert!(u.despawn.is_empty());
        // Budget fits two updates: the nearest two.
        let u = int.update(&g, Vec3::ZERO, 10.0, 1.5, 20, |_| 10);
        assert_eq!(u.update, vec![0, 1]);
        assert!(u.spawn.is_empty());
        // Entity 2 at x=8 is within the hysteresis band (<=15) after the viewer moves: kept.
        // Entity 0 is past 15 m: despawned. Entity 3 (x=12) enters.
        let u = int.update(&g, Vec3::new(16.0, 0.0, 0.0), 10.0, 1.5, 1000, |_| 10);
        assert_eq!(u.despawn, vec![0]);
        assert_eq!(u.spawn, vec![4, 3]); // nearest first
    }
}

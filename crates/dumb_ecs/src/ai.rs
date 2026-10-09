//! AI orchestration for large numbers of NPCs.
//!
//! Every `AiAgent` wants to "think" (run its expensive decision logic) `think_hz` times per
//! second. The orchestrator decides, each frame, which agents actually think:
//!
//! - **Distance LOD**: far agents think less often (rate divided per LOD band), very far
//!   agents sleep.
//! - **Budget**: at most `max_thinks_per_frame` agents think in one frame. Agents that are due
//!   but over budget wait, and the most overdue go first next frame, so nobody starves.
//! - **Staggering**: agents start at different phases, so thousands of them spread evenly
//!   over frames instead of all thinking on the same one.
//!
//! Scripts check `agent.think` and use `agent.think_dt` (time since that agent last thought):
//!
//! ```ignore
//! ctx.world.par_for_each::<(&mut AiAgent, &mut Guard, &Transform), _>(|(ai, guard, t)| {
//!     if ai.think { guard.plan(t, ai.think_dt); }   // expensive, rate-limited
//!     guard.steer();                                // cheap, every frame
//! });
//! ```

use crate::components::Transform;
use crate::World;
use dumb_core::{Entity, Vec3};
use dumb_derive::{Component, Editor};

/// Marks an entity as an AI agent scheduled by the orchestrator.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct AiAgent {
    /// Thinks per second when close to the camera.
    #[editor(range = 0.1..=60.0)]
    pub think_hz: f32,
    /// Higher priority agents win when the frame budget is exceeded.
    #[editor(range = 0.0..=10.0)]
    pub priority: f32,
    /// Think less often when far from the camera.
    pub distance_lod: bool,
    /// Set by the orchestrator: this agent thinks this frame.
    #[editor(hidden)]
    pub think: bool,
    /// Seconds since this agent last thought (valid when `think` is set).
    #[editor(hidden)]
    pub think_dt: f32,
    /// Current distance band (0 = near ... 3 = asleep).
    #[editor(hidden)]
    pub lod: u32,
    #[editor(hidden)]
    pub since: f32,
    #[editor(hidden)]
    pub started: bool,
}

impl Default for AiAgent {
    fn default() -> Self {
        AiAgent { think_hz: 10.0, priority: 1.0, distance_lod: true, think: false, think_dt: 0.0, lod: 0, since: 0.0, started: false }
    }
}

/// Global orchestrator settings (from `project.ron`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AiSettings {
    /// Most agents allowed to think in one frame (0 = unlimited).
    pub max_thinks_per_frame: u32,
    /// Distance limits of the near, mid and far bands; beyond the last, agents sleep.
    pub lod_distances: [f32; 3],
    /// Think-rate divisor for each band (near, mid, far).
    pub lod_divisors: [f32; 3],
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings { max_thinks_per_frame: 500, lod_distances: [30.0, 80.0, 250.0], lod_divisors: [1.0, 4.0, 16.0] }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AiStats {
    pub agents: u32,
    pub thinking: u32,
    /// Due but postponed by the budget.
    pub deferred: u32,
    pub sleeping: u32,
    /// Agents per band (near, mid, far, asleep).
    pub per_lod: [u32; 4],
}

/// Deterministic per-entity phase in [0, 1) so agents spread over frames.
fn phase(e: Entity) -> f32 {
    let mut x = e.to_bits().wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 31;
    (x % 10_000) as f32 / 10_000.0
}

/// Schedule this frame's thinking. `focus` is usually the camera position (no LOD without it).
pub fn orchestrate(world: &mut World, dt: f32, focus: Option<Vec3>, s: &AiSettings) -> AiStats {
    let mut stats = AiStats::default();
    // (entity, urgency) of agents that are due.
    let mut due: Vec<(Entity, f32)> = Vec::new();
    // World positions first (the agent loop below borrows the world mutably).
    let positions: std::collections::HashMap<Entity, Vec3> = match focus {
        Some(_) => {
            let es: Vec<Entity> = world.query_ref::<(Entity, &AiAgent, &Transform)>().map(|(e, _, _)| e).collect();
            es.into_iter().map(|e| (e, world.global_matrix(e).w_axis.truncate())).collect()
        }
        None => Default::default(),
    };

    for (e, a) in world.query::<(Entity, &mut AiAgent)>() {
        stats.agents += 1;
        a.think = false;
        let lod = match (focus, a.distance_lod) {
            (Some(f), true) => {
                let d = positions.get(&e).map_or(0.0, |p| p.distance(f));
                s.lod_distances.iter().position(|lim| d <= *lim).unwrap_or(3)
            }
            _ => 0,
        };
        a.lod = lod as u32;
        stats.per_lod[lod] += 1;
        if lod >= 3 {
            stats.sleeping += 1;
            // Wake up promptly when coming back into range.
            a.since = a.since.max(0.0) + dt;
            continue;
        }
        let rate = a.think_hz.max(0.01) / s.lod_divisors[lod].max(1.0);
        let interval = 1.0 / rate;
        if !a.started {
            a.started = true;
            a.since = phase(e) * interval;
        }
        a.since += dt;
        if a.since >= interval {
            due.push((e, a.since / interval * (1.0 + a.priority.max(0.0))));
        }
    }

    let budget = if s.max_thinks_per_frame == 0 { due.len() } else { s.max_thinks_per_frame as usize };
    if due.len() > budget {
        due.select_nth_unstable_by(budget, |a, b| b.1.total_cmp(&a.1));
        stats.deferred = (due.len() - budget) as u32;
        due.truncate(budget);
    }
    for (e, _) in due {
        if let Some(a) = world.get_mut::<AiAgent>(e) {
            a.think = true;
            a.think_dt = a.since;
            a.since = 0.0;
            stats.thinking += 1;
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world_with(n: usize, hz: f32) -> (World, Vec<Entity>) {
        let mut w = World::new();
        let mut es = Vec::new();
        for i in 0..n {
            let e = w.spawn_named("npc");
            w.get_mut::<Transform>(e).unwrap().translation = Vec3::new(i as f32 * 0.01, 0.0, 0.0);
            w.insert(e, AiAgent { think_hz: hz, ..Default::default() });
            es.push(e);
        }
        (w, es)
    }

    #[test]
    fn staggers_and_keeps_the_rate() {
        // 1000 agents at 10 Hz, 60 fps: ~167 thinks per frame, evenly spread.
        let (mut w, _) = world_with(1000, 10.0);
        let s = AiSettings { max_thinks_per_frame: 0, ..Default::default() };
        let mut per_frame = Vec::new();
        for _ in 0..120 {
            per_frame.push(orchestrate(&mut w, 1.0 / 60.0, Some(Vec3::ZERO), &s).thinking);
        }
        let total: u32 = per_frame.iter().sum();
        assert!((19_000..=21_000).contains(&total), "2 s at 10 Hz for 1000 agents: {total}");
        let max = *per_frame[10..].iter().max().unwrap();
        assert!(max < 400, "spread over frames, got a spike of {max}");
    }

    #[test]
    fn budget_defers_without_starving() {
        let (mut w, es) = world_with(1000, 30.0);
        let s = AiSettings { max_thinks_per_frame: 100, ..Default::default() };
        let mut thought = std::collections::HashSet::new();
        for _ in 0..30 {
            let st = orchestrate(&mut w, 1.0 / 60.0, Some(Vec3::ZERO), &s);
            assert!(st.thinking <= 100);
            for e in &es {
                if w.get::<AiAgent>(*e).unwrap().think {
                    thought.insert(*e);
                }
            }
        }
        assert_eq!(thought.len(), 1000, "every agent got a turn");
    }

    #[test]
    fn far_agents_think_less_and_sleep() {
        let mut w = World::new();
        let near = w.spawn_named("near");
        let far = w.spawn_named("far");
        let gone = w.spawn_named("gone");
        w.get_mut::<Transform>(far).unwrap().translation = Vec3::new(200.0, 0.0, 0.0);
        w.get_mut::<Transform>(gone).unwrap().translation = Vec3::new(5000.0, 0.0, 0.0);
        for e in [near, far, gone] {
            w.insert(e, AiAgent { think_hz: 20.0, ..Default::default() });
        }
        w.update_transforms();
        let s = AiSettings { max_thinks_per_frame: 0, ..Default::default() };
        let mut counts = [0; 3];
        for _ in 0..600 {
            orchestrate(&mut w, 1.0 / 60.0, Some(Vec3::ZERO), &s);
            for (i, e) in [near, far, gone].iter().enumerate() {
                counts[i] += w.get::<AiAgent>(*e).unwrap().think as u32;
            }
        }
        assert!((195..=205).contains(&counts[0]), "near 20 Hz for 10 s: {}", counts[0]);
        assert!((10..=15).contains(&counts[1]), "far band /16: {}", counts[1]);
        assert_eq!(counts[2], 0, "asleep");
    }
}

//! Massive NPC crowd: thousands of agents scheduled by the AI orchestrator.
//!
//! Add a `Crowd` component to an entity and press Play. Each NPC has an `AiAgent`: the engine
//! decides which agents "think" each frame (distance LOD + per-frame budget, see Project
//! Preferences ▸ AI). Thinking picks a new wander target; moving happens every frame, in
//! parallel over all cores.

use crate::noise::hash;
use dumb_script::prelude::*;

/// Spawns `count` wandering NPCs around this entity when play starts.
#[derive(Component, Editor, Clone, Debug)]
pub struct Crowd {
    #[editor(range = 1..=20000, tooltip = "Number of NPCs")]
    pub count: u32,
    #[editor(range = 2.0..=500.0)]
    pub radius: f32,
    #[editor(range = 0.1..=30.0, tooltip = "Decisions per second per NPC (near the camera)")]
    pub think_hz: f32,
    #[editor(color)]
    pub color: Color,
    #[editor(readonly)]
    pub spawned: bool,
}

impl Default for Crowd {
    fn default() -> Self {
        Crowd { count: 2000, radius: 60.0, think_hz: 2.0, color: Color::rgb(0.9, 0.55, 0.25), spawned: false }
    }
}

/// One NPC's movement state.
#[derive(Component, Editor, Clone, Debug, Default)]
pub struct Npc {
    pub home: Vec3,
    pub radius: f32,
    pub target: Vec3,
    #[editor(range = 0.5..=8.0)]
    pub speed: f32,
    pub decisions: u32,
}

fn crowd(ctx: &mut ScriptContext) {
    let dt = ctx.time.delta;
    let t = ctx.time.elapsed as f32;

    // Spawn.
    let crowds: Vec<(Entity, Crowd)> = ctx.world.query::<(Entity, &Crowd)>().filter(|(_, c)| !c.spawned).map(|(e, c)| (e, c.clone())).collect();
    for (root, c) in crowds {
        let center = ctx.world.global_matrix(root).w_axis.truncate();
        for i in 0..c.count {
            let a = hash(i as i32, 1, 7) * std::f32::consts::TAU;
            let r = hash(i as i32, 2, 7).sqrt() * c.radius;
            let p = center + Vec3::new(a.cos() * r, 0.0, a.sin() * r);
            let e = ctx.world.spawn();
            ctx.world.insert(e, Name { name: format!("NPC {i}") });
            ctx.world.insert(e, Transform::from_translation(p + Vec3::Y * 0.6).with_scale(Vec3::new(0.5, 1.2, 0.5)));
            let shade = 0.75 + 0.5 * hash(i as i32, 3, 7);
            ctx.world.insert(e, MeshRenderer { model: builtin::CYLINDER, tint: Color::rgb(c.color.r * shade, c.color.g * shade, c.color.b * shade), ..Default::default() });
            ctx.world.insert(e, AiAgent { think_hz: c.think_hz, ..Default::default() });
            ctx.world.insert(e, Npc { home: center, radius: c.radius, target: p, speed: 1.5 + 2.0 * hash(i as i32, 4, 7), decisions: 0 });
        }
        if let Some(cr) = ctx.world.get_mut::<Crowd>(root) {
            cr.spawned = true;
        }
        info!("crowd: spawned {} NPCs", c.count);
    }

    // Think (rate-limited by the orchestrator) + move (every frame), in parallel.
    ctx.world.par_for_each::<(Entity, &AiAgent, &mut Npc, &mut Transform), _>(|(e, ai, npc, tr)| {
        if ai.think {
            // "Expensive" decision: pick a new destination around home.
            let k = (e.to_bits() as i32).wrapping_add(npc.decisions as i32 * 7919);
            let a = hash(k, 11, (t * 10.0) as u32) * std::f32::consts::TAU;
            let r = hash(k, 13, 3).sqrt() * npc.radius;
            npc.target = npc.home + Vec3::new(a.cos() * r, 0.0, a.sin() * r);
            npc.decisions += 1;
        }
        let to = Vec3::new(npc.target.x - tr.translation.x, 0.0, npc.target.z - tr.translation.z);
        let d = to.length();
        if d > 0.05 {
            let step = (npc.speed * dt).min(d);
            tr.translation += to / d * step;
            // Face the walking direction.
            tr.rotation = Quat::from_rotation_y((-to.z).atan2(to.x));
        }
    });
}

pub fn register(reg: &mut Registry) {
    reg.component::<Crowd>();
    reg.component::<Npc>();
    reg.system("crowd::think_and_move", crowd);
}

//! Physics examples: throwing balls (impulses), reacting to hits (collision events) and
//! probing the world (ray casts).
use dumb_script::prelude::*;

/// A thrown ball. Turns red the first time it hits something.
#[derive(Component, Editor, Clone, Debug, Default)]
pub struct Projectile {
    #[editor(readonly)]
    pub hits: u32,
    #[editor(readonly)]
    pub age: f32,
}

/// Press F to throw a ball from every `Player` in the direction it faces.
#[derive(Component, Editor, Clone, Debug)]
pub struct Thrower {
    #[editor(range = 0.0..=50.0)]
    pub throw_speed: f32,
    #[editor(range = 0.1..=2.0)]
    pub ball_size: f32,
    /// Balls disappear after this many seconds.
    #[editor(range = 1.0..=60.0)]
    pub lifetime: f32,
}

impl Default for Thrower {
    fn default() -> Self {
        Thrower { throw_speed: 14.0, ball_size: 0.35, lifetime: 12.0 }
    }
}

fn throw(ctx: &mut ScriptContext) {
    if !ctx.input.key_pressed(Key::F) {
        return;
    }
    let throwers: Vec<(Entity, Thrower, Mat4)> = ctx
        .world
        .query::<(Entity, &Thrower)>()
        .map(|(e, t)| (e, t.clone()))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|(e, t)| (e, t, ctx.world.global_matrix(e)))
        .collect();
    for (owner, thrower, m) in throwers {
        let (_, rot, pos) = m.to_scale_rotation_translation();
        let forward = rot * -Vec3::Z;
        let start = pos + Vec3::Y * 1.4 + forward * 0.8;
        // Aim a bit up when something is right in front (ray cast, ignoring the thrower).
        let blocked = ctx.physics.raycast(start, forward, 1.5, Some(owner)).is_some();
        let dir = (forward + Vec3::Y * if blocked { 0.6 } else { 0.15 }).normalize();

        let ball = ctx.world.spawn_named("Ball");
        let t = ctx.world.get_mut::<Transform>(ball).unwrap();
        t.translation = start;
        t.scale = Vec3::splat(thrower.ball_size);
        ctx.world.insert(ball, MeshRenderer { model: builtin::SPHERE, tint: Color::rgb(0.9, 0.9, 0.95), ..Default::default() });
        ctx.world.insert(ball, Collider { restitution: 0.6, density: 2.0, ..Collider::sphere(1.0) });
        // The velocity is applied when the body is created on the next physics step.
        ctx.world.insert(ball, RigidBody { linear_velocity: dir * thrower.throw_speed, ccd: true, ..RigidBody::dynamic() });
        ctx.world.insert(ball, Projectile::default());
        info!("threw a ball at {:.1} m/s{}", thrower.throw_speed, if blocked { " (aimed over an obstacle)" } else { "" });
    }
}

fn react_to_hits(ctx: &mut ScriptContext) {
    let events: Vec<CollisionEvent> = ctx.physics.collision_events().to_vec();
    for ev in events.iter().filter(|e| e.started) {
        for e in [ev.a, ev.b] {
            if let Some(p) = ctx.world.get_mut::<Projectile>(e) {
                p.hits += 1;
                debug!("ball {:?} hit #{}", e, p.hits);
                if let Some(mr) = ctx.world.get_mut::<MeshRenderer>(e) {
                    mr.tint = Color::rgb(1.0, 0.25, 0.15);
                }
            }
        }
    }
    // Expire old balls.
    let dt = ctx.time.delta;
    let lifetime = ctx.world.query::<&Thrower>().next().map_or(12.0, |t| t.lifetime);
    let expired: Vec<Entity> = ctx
        .world
        .query::<(Entity, &mut Projectile)>()
        .filter_map(|(e, p)| {
            p.age += dt;
            (p.age > lifetime).then_some(e)
        })
        .collect();
    for e in expired {
        ctx.world.despawn(e);
    }
    let alive = ctx.world.count::<Projectile>();
    if alive > 50 {
        warn!("{alive} balls alive, consider a shorter lifetime");
    }
}

pub fn register(reg: &mut Registry) {
    reg.component::<Projectile>();
    reg.component::<Thrower>();
    reg.system("physics_toys::throw", throw);
    reg.system("physics_toys::react_to_hits", react_to_hits);
}

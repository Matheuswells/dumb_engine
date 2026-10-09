use dumb_script::prelude::*;

/// The example from the engine requirements: shows up in the inspector as-is.
#[derive(Component, Editor, Clone, Debug)]
pub struct Player {
    #[editor(range = 0.0..=20.0, tooltip = "Meters per second")]
    pub speed: f32,
    #[editor(range = 0.0..=100.0)]
    pub health: f32,
    #[editor(range = 0.0..=50.0)]
    pub armor: f32,
    #[editor(asset = "model")]
    pub weapon: AssetId,
    #[editor(range = 0.0..=20.0)]
    pub jump_height: f32,
    #[editor(readonly)]
    pub vertical_velocity: f32,
}

impl Default for Player {
    fn default() -> Self {
        Player { speed: 5.0, health: 100.0, armor: 10.0, weapon: AssetId::NONE, jump_height: 1.5, vertical_velocity: 0.0 }
    }
}

/// WASD to move, Space to jump. Uses the physics `CharacterController` when the entity has one
/// (collides with walls, climbs steps), otherwise simple ground-plane movement.
fn movement(ctx: &mut ScriptContext) {
    let dt = ctx.time.delta;
    let input = ctx.input;
    let mv = Vec3::new(input.axis(Key::A, Key::D), 0.0, input.axis(Key::W, Key::S));
    let jump = input.key_pressed(Key::Space);
    for (t, p, cc) in ctx.world.query::<(&mut Transform, &mut Player, Option<&mut CharacterController>)>() {
        if let Some(cc) = cc {
            cc.move_velocity(mv.normalize_or_zero() * p.speed);
            if jump {
                cc.jump((2.0 * 9.81 * p.jump_height).sqrt());
            }
            if mv != Vec3::ZERO {
                let target = Quat::from_rotation_y(f32::atan2(-mv.x, -mv.z));
                t.rotation = t.rotation.slerp(target, (dt * 12.0).min(1.0));
            }
            p.vertical_velocity = cc.velocity.y;
            continue;
        }
        if mv != Vec3::ZERO {
            let dir = mv.normalize();
            t.translation += dir * p.speed * dt;
            let target = Quat::from_rotation_y(f32::atan2(-dir.x, -dir.z));
            t.rotation = t.rotation.slerp(target, (dt * 12.0).min(1.0));
        }
        // Simple gravity + jump against the ground plane.
        if jump && t.translation.y <= 0.001 {
            p.vertical_velocity = (2.0 * 9.81 * p.jump_height).sqrt();
        }
        p.vertical_velocity -= 9.81 * dt;
        t.translation.y = (t.translation.y + p.vertical_velocity * dt).max(0.0);
        if t.translation.y <= 0.0 {
            p.vertical_velocity = 0.0;
        }
    }
}

pub fn register(reg: &mut Registry) {
    reg.component::<Player>();
    reg.system("player::movement", movement);
}

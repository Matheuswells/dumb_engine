use dumb_script::prelude::*;

/// Procedural animation: sine bobbing with optional sway.
#[derive(Component, Editor, Clone, Debug)]
pub struct Bobber {
    #[editor(range = 0.0..=5.0)]
    pub amplitude: f32,
    #[editor(range = 0.0..=10.0)]
    pub frequency: f32,
    #[editor(range = 0.0..=45.0)]
    pub sway_degrees: f32,
    #[editor(hidden)]
    pub base_y: f32,
    #[editor(hidden)]
    pub initialized: bool,
}

impl Default for Bobber {
    fn default() -> Self {
        Bobber { amplitude: 0.25, frequency: 1.5, sway_degrees: 8.0, base_y: 0.0, initialized: false }
    }
}

fn bob(ctx: &mut ScriptContext) {
    let time = ctx.time.elapsed as f32;
    for (t, b) in ctx.world.query::<(&mut Transform, &mut Bobber)>() {
        if !b.initialized {
            b.base_y = t.translation.y;
            b.initialized = true;
        }
        let phase = time * b.frequency * std::f32::consts::TAU;
        t.translation.y = b.base_y + phase.sin() * b.amplitude;
        let sway = (phase * 0.5).sin() * b.sway_degrees.to_radians();
        let (y, _, _) = t.rotation.to_euler(EulerRot::YXZ);
        t.rotation = Quat::from_euler(EulerRot::YXZ, y, 0.0, sway);
    }
}

pub fn register(reg: &mut Registry) {
    reg.component::<Bobber>();
    reg.system("bobber::bob", bob);
}

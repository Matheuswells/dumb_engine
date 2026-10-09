use dumb_script::prelude::*;

/// Rotates the entity around its local Y axis.
#[derive(Component, Editor, Clone, Debug)]
pub struct Spinner {
    #[editor(range = -720.0..=720.0)]
    pub degrees_per_second: f32,
}

impl Default for Spinner {
    fn default() -> Self {
        Spinner { degrees_per_second: 90.0 }
    }
}

fn spin(ctx: &mut ScriptContext) {
    let dt = ctx.time.delta;
    for (t, s) in ctx.world.query::<(&mut Transform, &Spinner)>() {
        t.rotation = (t.rotation * Quat::from_rotation_y(s.degrees_per_second.to_radians() * dt)).normalize();
    }
}

pub fn register(reg: &mut Registry) {
    reg.component::<Spinner>();
    reg.system("spinner::spin", spin);
}

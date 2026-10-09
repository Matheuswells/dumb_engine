use crate::noise::{fbm, hash};
use dumb_script::prelude::*;

#[derive(Editor, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldShape {
    #[default]
    Terrain,
    Waves,
    Pillars,
}

/// Procedural generation: spawns `size * size` blocks as children when play starts.
#[derive(Component, Editor, Clone, Debug)]
pub struct ProceduralField {
    #[editor(range = 1..=300, tooltip = "Blocks per side (300 = 90,000 entities)")]
    pub size: u32,
    #[editor(range = 0.2..=4.0)]
    pub spacing: f32,
    #[editor(range = 0.0..=20.0)]
    pub height: f32,
    pub seed: u32,
    pub shape: FieldShape,
    /// Simulate the blocks every frame (wave simulation).
    pub animate: bool,
    #[editor(color)]
    pub low_color: Color,
    #[editor(color)]
    pub high_color: Color,
    #[editor(readonly)]
    pub generated: bool,
}

impl Default for ProceduralField {
    fn default() -> Self {
        ProceduralField {
            size: 64,
            spacing: 1.05,
            height: 6.0,
            seed: 1,
            shape: FieldShape::Terrain,
            animate: true,
            low_color: Color::rgb(0.15, 0.35, 0.8),
            high_color: Color::rgb(0.95, 0.9, 0.8),
            generated: false,
        }
    }
}

/// One generated block.
#[derive(Component, Editor, Clone, Debug, Default)]
pub struct FieldCell {
    pub grid: Vec2,
    pub base_height: f32,
}
fn field_height(shape: FieldShape, g: Vec2, size: f32, seed: u32, t: f32) -> f32 {
    match shape {
        FieldShape::Terrain => fbm(g / size * 4.0 + Vec2::splat(t * 0.05), seed),
        FieldShape::Waves => {
            let c = g - Vec2::splat(size * 0.5);
            ((c.length() * 0.35 - t * 2.0).sin() * 0.5 + 0.5) * (1.0 - (c.length() / size).min(1.0))
        }
        FieldShape::Pillars => {
            let n = hash(g.x as i32, g.y as i32, seed);
            if n > 0.92 { n } else { 0.05 * fbm(g * 0.2, seed) }
        }
    }
}

fn procedural_field(ctx: &mut ScriptContext) {
    let t = ctx.time.elapsed as f32;
    let fields: Vec<(Entity, ProceduralField)> =
        ctx.world.query::<(Entity, &ProceduralField)>().map(|(e, f)| (e, f.clone())).collect();
    let cube = builtin::CUBE;
    for (root, f) in fields {
        if !f.generated {
            let size = f.size.max(1);
            let half = (size as f32 - 1.0) * f.spacing * 0.5;
            for x in 0..size {
                for z in 0..size {
                    let g = Vec2::new(x as f32, z as f32);
                    let h = field_height(f.shape, g, size as f32, f.seed, 0.0);
                    let e = ctx.world.spawn();
                    ctx.world.insert(e, Name { name: format!("Cell {x},{z}") });
                    let height = (h * f.height).max(0.05);
                    ctx.world.insert(
                        e,
                        Transform::from_translation(Vec3::new(x as f32 * f.spacing - half, height * 0.5, z as f32 * f.spacing - half))
                            .with_scale(Vec3::new(f.spacing * 0.95, height, f.spacing * 0.95)),
                    );
                    let c = f.low_color.to_vec4().lerp(f.high_color.to_vec4(), h.clamp(0.0, 1.0));
                    ctx.world.insert(e, MeshRenderer { model: cube, tint: Color::rgba(c.x, c.y, c.z, 1.0), ..Default::default() });
                    ctx.world.insert(e, FieldCell { grid: g, base_height: height });
                    ctx.world.insert(e, Parent { entity: root });
                }
            }
            if let Some(pf) = ctx.world.get_mut::<ProceduralField>(root) {
                pf.generated = true;
            }
            info!("generated {} blocks", size * size);
        }
    }

    // Simulation pass: animate every cell (stress test for the ECS + renderer).
    let Some((_, f)) = ctx.world.query::<(Entity, &ProceduralField)>().map(|(e, f)| (e, f.clone())).next() else { return };
    if !f.animate || f.shape == FieldShape::Pillars {
        return;
    }
    let size = f.size as f32;
    // Every cell is independent, so the work is split across all cores.
    ctx.world.par_for_each::<(&mut Transform, &FieldCell, &mut MeshRenderer), _>(|(tr, cell, mr)| {
        let h = field_height(f.shape, cell.grid, size, f.seed, t);
        let height = (h * f.height).max(0.05);
        tr.scale.y = height;
        tr.translation.y = height * 0.5;
        let c = f.low_color.to_vec4().lerp(f.high_color.to_vec4(), h.clamp(0.0, 1.0));
        mr.tint = Color::rgba(c.x, c.y, c.z, 1.0);
    });
}

pub fn register(reg: &mut Registry) {
    reg.component::<ProceduralField>();
    reg.component::<FieldCell>();
    reg.system("procedural_field::generate_and_simulate", procedural_field);
}

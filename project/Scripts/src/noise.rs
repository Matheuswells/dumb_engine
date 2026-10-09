//! Helper module (no `register`): deterministic value noise used by other scripts.
use dumb_script::prelude::*;

/// Hash-based value noise, deterministic per seed.
pub fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263) ^ seed.wrapping_mul(2_246_822_519);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    (h ^ (h >> 16)) as f32 / u32::MAX as f32
}

pub fn value_noise(p: Vec2, seed: u32) -> f32 {
    let i = p.floor();
    let f = p - i;
    let u = f * f * (Vec2::splat(3.0) - 2.0 * f);
    let (x, y) = (i.x as i32, i.y as i32);
    let a = hash(x, y, seed);
    let b = hash(x + 1, y, seed);
    let c = hash(x, y + 1, seed);
    let d = hash(x + 1, y + 1, seed);
    a + (b - a) * u.x + (c - a) * u.y + (a - b - c + d) * u.x * u.y
}

/// Fractal noise: 5 octaves of value noise.
pub fn fbm(p: Vec2, seed: u32) -> f32 {
    let (mut sum, mut amp, mut freq) = (0.0, 0.5, 1.0);
    for o in 0..5 {
        sum += value_noise(p * freq, seed + o) * amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum
}

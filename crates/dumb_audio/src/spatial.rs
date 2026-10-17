//! 3D positioning: distance attenuation and stereo panning relative to a listener.

use std::f32::consts::FRAC_PI_4;

/// Where the listener is and which way it faces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Listener {
    pub position: [f32; 3],
    /// Unit vector to the listener's right.
    pub right: [f32; 3],
    pub velocity: [f32; 3],
}

impl Default for Listener {
    fn default() -> Self {
        Listener { position: [0.0; 3], right: [1.0, 0.0, 0.0], velocity: [0.0; 3] }
    }
}

/// Gain (0..=1) by distance: full inside `min`, silent at `max`, inverse-distance in between
/// (scaled so it reaches exactly zero at `max`).
pub fn attenuation(distance: f32, min: f32, max: f32) -> f32 {
    let min = min.max(1e-3);
    if distance <= min {
        return 1.0;
    }
    if distance >= max {
        return 0.0;
    }
    let inv = min / distance;
    let floor = min / max;
    ((inv - floor) / (1.0 - floor)).clamp(0.0, 1.0)
}

/// Equal-power pan: -1 is full left, +1 full right. Returns `(left, right)` gains.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * FRAC_PI_4;
    (a.cos(), a.sin())
}

/// What the mixer needs to know about an emitter after positioning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub gain: f32,
    pub pan: f32,
    /// Playback-rate multiplier from the Doppler shift.
    pub doppler: f32,
}

const SPEED_OF_SOUND: f32 = 343.0;

/// Position an emitter relative to the listener.
pub fn place(listener: &Listener, pos: [f32; 3], vel: [f32; 3], min: f32, max: f32, doppler_scale: f32) -> Placement {
    let d = [pos[0] - listener.position[0], pos[1] - listener.position[1], pos[2] - listener.position[2]];
    let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let gain = attenuation(dist, min, max);
    if dist < 1e-4 {
        return Placement { gain, pan: 0.0, doppler: 1.0 };
    }
    let dir = [d[0] / dist, d[1] / dist, d[2] / dist];
    let pan = dir[0] * listener.right[0] + dir[1] * listener.right[1] + dir[2] * listener.right[2];
    // Positive when listener and source close in on each other.
    let closing = |v: [f32; 3]| v[0] * dir[0] + v[1] * dir[1] + v[2] * dir[2];
    let vl = closing(listener.velocity).clamp(-SPEED_OF_SOUND * 0.5, SPEED_OF_SOUND * 0.5);
    let vs = closing(vel).clamp(-SPEED_OF_SOUND * 0.5, SPEED_OF_SOUND * 0.5);
    let shift = (SPEED_OF_SOUND + vl * doppler_scale) / (SPEED_OF_SOUND + vs * doppler_scale);
    Placement { gain, pan, doppler: shift }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attenuation_is_full_inside_min_and_silent_at_max() {
        assert_eq!(attenuation(0.5, 1.0, 20.0), 1.0);
        assert_eq!(attenuation(20.0, 1.0, 20.0), 0.0);
        assert_eq!(attenuation(500.0, 1.0, 20.0), 0.0);
        let a = attenuation(2.0, 1.0, 20.0);
        let b = attenuation(10.0, 1.0, 20.0);
        assert!(a > b && b > 0.0 && a < 1.0);
    }

    #[test]
    fn pan_is_equal_power() {
        for p in [-1.0, -0.3, 0.0, 0.7, 1.0] {
            let (l, r) = pan_gains(p);
            assert!((l * l + r * r - 1.0).abs() < 1e-5);
        }
        assert!(pan_gains(-1.0).1 < 1e-6);
        assert!(pan_gains(1.0).0 < 1e-6);
    }

    #[test]
    fn source_on_the_right_pans_right() {
        let l = Listener::default();
        let p = place(&l, [5.0, 0.0, 0.0], [0.0; 3], 1.0, 50.0, 1.0);
        assert!((p.pan - 1.0).abs() < 1e-5);
        let p = place(&l, [-5.0, 0.0, 0.0], [0.0; 3], 1.0, 50.0, 1.0);
        assert!((p.pan + 1.0).abs() < 1e-5);
        let p = place(&l, [0.0, 0.0, 5.0], [0.0; 3], 1.0, 50.0, 1.0);
        assert!(p.pan.abs() < 1e-5);
    }

    #[test]
    fn doppler_raises_pitch_when_approaching() {
        let l = Listener::default();
        let approaching = place(&l, [50.0, 0.0, 0.0], [-30.0, 0.0, 0.0], 1.0, 500.0, 1.0);
        let receding = place(&l, [50.0, 0.0, 0.0], [30.0, 0.0, 0.0], 1.0, 500.0, 1.0);
        assert!(approaching.doppler > 1.0 && receding.doppler < 1.0);
        let off = place(&l, [50.0, 0.0, 0.0], [-30.0, 0.0, 0.0], 1.0, 500.0, 0.0);
        assert_eq!(off.doppler, 1.0);
    }

    #[test]
    fn emitter_at_the_listener_is_centered() {
        let p = place(&Listener::default(), [0.0; 3], [0.0; 3], 1.0, 10.0, 1.0);
        assert_eq!((p.gain, p.pan, p.doppler), (1.0, 0.0, 1.0));
    }
}

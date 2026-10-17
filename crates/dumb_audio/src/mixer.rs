//! Software mixer: voices (clips or streams) positioned in 3D, resampled and summed to stereo.

use crate::decode::Clip;
use crate::spatial::{pan_gains, place, Listener};
use crate::stream::Stream;
use std::sync::Arc;

pub type VoiceId = u64;

/// What a voice plays.
pub enum Source {
    Clip(Arc<Clip>),
    Stream(Stream),
}

impl Source {
    fn rate(&self) -> u32 {
        match self {
            Source::Clip(c) => c.rate,
            Source::Stream(s) => s.rate,
        }
    }
}

/// Emitter placement for 3D sounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spatial {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    /// Full volume inside this distance.
    pub min_distance: f32,
    /// Silent beyond this distance.
    pub max_distance: f32,
}

/// Settings of one voice, changeable while it plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceParams {
    pub volume: f32,
    /// Playback speed multiplier (also changes pitch).
    pub pitch: f32,
    pub looping: bool,
    pub paused: bool,
    /// `None` plays in 2D, straight to the speakers.
    pub spatial: Option<Spatial>,
}

impl Default for VoiceParams {
    fn default() -> Self {
        VoiceParams { volume: 1.0, pitch: 1.0, looping: false, paused: false, spatial: None }
    }
}

struct Voice {
    id: VoiceId,
    source: Source,
    params: VoiceParams,
    /// Position in source frames.
    pos: f64,
    /// Gains applied at the end of the previous buffer, to ramp without clicks.
    gain: [f32; 2],
    done: bool,
}

pub struct Mixer {
    pub out_rate: u32,
    pub master_volume: f32,
    /// 0 turns the Doppler shift off.
    pub doppler_scale: f32,
    pub max_voices: usize,
    listener: Listener,
    voices: Vec<Voice>,
    next_id: VoiceId,
}

impl Mixer {
    pub fn new(out_rate: u32) -> Self {
        Mixer {
            out_rate,
            master_volume: 1.0,
            doppler_scale: 1.0,
            max_voices: 64,
            listener: Listener::default(),
            voices: Vec::new(),
            next_id: 1,
        }
    }

    pub fn set_listener(&mut self, l: Listener) {
        self.listener = l;
    }

    pub fn voice_count(&self) -> usize {
        self.voices.len()
    }

    pub fn is_playing(&self, id: VoiceId) -> bool {
        self.voices.iter().any(|v| v.id == id && !v.done)
    }

    /// Start a voice. When all voices are busy the quietest one is replaced if it is
    /// quieter than the new sound, otherwise the new sound is dropped (`None`).
    pub fn play(&mut self, source: Source, params: VoiceParams) -> Option<VoiceId> {
        if self.voices.len() >= self.max_voices {
            let loudness = |m: &Mixer, p: &VoiceParams| m.target_gains(p).iter().sum::<f32>();
            let new = loudness(self, &params);
            let (idx, quietest) = self
                .voices
                .iter()
                .enumerate()
                .map(|(i, v)| (i, loudness(self, &v.params)))
                .min_by(|a, b| a.1.total_cmp(&b.1))?;
            if quietest >= new {
                return None;
            }
            self.voices.swap_remove(idx);
        }
        let id = self.next_id;
        self.next_id += 1;
        let gain = self.target_gains(&params);
        // Start at the target so a new voice doesn't fade in from silence.
        self.voices.push(Voice { id, source, params, pos: 0.0, gain, done: false });
        Some(id)
    }

    pub fn set_params(&mut self, id: VoiceId, params: VoiceParams) {
        if let Some(v) = self.voices.iter_mut().find(|v| v.id == id) {
            v.params = params;
        }
    }

    pub fn stop(&mut self, id: VoiceId) {
        self.voices.retain(|v| v.id != id);
    }

    pub fn stop_all(&mut self) {
        self.voices.clear();
    }

    /// Left/right gains for a voice with the current listener.
    fn target_gains(&self, p: &VoiceParams) -> [f32; 2] {
        let vol = p.volume.max(0.0) * self.master_volume;
        match &p.spatial {
            None => [vol, vol],
            Some(s) => {
                let pl = place(&self.listener, s.position, s.velocity, s.min_distance, s.max_distance, self.doppler_scale);
                let (l, r) = pan_gains(pl.pan);
                [vol * pl.gain * l, vol * pl.gain * r]
            }
        }
    }

    fn step_of(&self, v: &Voice) -> f64 {
        let mut speed = v.params.pitch.max(0.01);
        if let Some(s) = &v.params.spatial {
            speed *= place(&self.listener, s.position, s.velocity, s.min_distance, s.max_distance, self.doppler_scale).doppler;
        }
        v.source.rate() as f64 / self.out_rate as f64 * speed as f64
    }

    /// Mix `out.len() / 2` stereo frames (interleaved) and drop finished voices.
    pub fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / 2;
        if frames == 0 {
            return;
        }
        let mut voices = std::mem::take(&mut self.voices);
        for v in voices.iter_mut() {
            if v.params.paused {
                continue;
            }
            let target = self.target_gains(&v.params);
            let step = self.step_of(v);
            mix_voice(v, out, frames, step, target);
            v.gain = target;
        }
        voices.retain(|v| !v.done);
        self.voices = voices;
    }
}

fn mix_voice(v: &mut Voice, out: &mut [f32], frames: usize, step: f64, target: [f32; 2]) {
    let looping = v.params.looping;
    let spatial = v.params.spatial.is_some();
    let start = v.gain;
    for n in 0..frames {
        let t = n as f32 / frames as f32;
        let gl = start[0] + (target[0] - start[0]) * t;
        let gr = start[1] + (target[1] - start[1]) * t;
        let i = v.pos as usize;
        let frac = (v.pos - i as f64) as f32;
        let pair = match &mut v.source {
            Source::Clip(c) => {
                let len = c.frames();
                if len == 0 || i >= len {
                    v.done = true;
                    return;
                }
                let next = if i + 1 < len { i + 1 } else if looping { 0 } else { i };
                let a = clip_frame(c, i);
                let b = clip_frame(c, next);
                Some([a[0] + (b[0] - a[0]) * frac, a[1] + (b[1] - a[1]) * frac])
            }
            Source::Stream(s) => {
                if s.finished(i) {
                    v.done = true;
                    return;
                }
                match (s.frame(i), s.frame(i + 1)) {
                    (Some(a), Some(b)) => Some([a[0] + (b[0] - a[0]) * frac, a[1] + (b[1] - a[1]) * frac]),
                    (Some(a), None) => Some(a),
                    // Underrun: hold the position until the decoder catches up.
                    _ => None,
                }
            }
        };
        let Some(s) = pair else { return };
        // A 3D sound is a point source, so it plays as mono through the pan.
        let (sl, sr) = if spatial { let m = (s[0] + s[1]) * 0.5; (m, m) } else { (s[0], s[1]) };
        out[n * 2] += sl * gl;
        out[n * 2 + 1] += sr * gr;
        v.pos += step;
        if looping {
            if let Source::Clip(c) = &v.source {
                let len = c.frames() as f64;
                if v.pos >= len {
                    v.pos %= len;
                }
            }
        }
    }
    if let Source::Stream(s) = &mut v.source {
        s.discard_before(v.pos as usize);
    }
}

fn clip_frame(c: &Clip, i: usize) -> [f32; 2] {
    if c.channels == 1 {
        let s = c.samples[i];
        [s, s]
    } else {
        [c.samples[i * 2], c.samples[i * 2 + 1]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(rate: u32, channels: usize, samples: Vec<f32>) -> Arc<Clip> {
        Arc::new(Clip { rate, channels, samples })
    }

    fn render(m: &mut Mixer, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        m.render(&mut out);
        out
    }

    #[test]
    fn plays_a_mono_clip_to_both_channels_then_finishes() {
        let mut m = Mixer::new(1000);
        let id = m.play(Source::Clip(clip(1000, 1, vec![0.5; 10])), VoiceParams::default()).unwrap();
        assert!(m.is_playing(id));
        let out = render(&mut m, 6);
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 1e-6));
        // 4 frames left, so the second buffer has 4 loud frames and then silence.
        let out = render(&mut m, 8);
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert_eq!(out[15], 0.0);
        assert!(!m.is_playing(id));
        assert_eq!(m.voice_count(), 0);
    }

    #[test]
    fn looping_never_finishes() {
        let mut m = Mixer::new(1000);
        let p = VoiceParams { looping: true, ..Default::default() };
        let id = m.play(Source::Clip(clip(1000, 1, vec![1.0; 4])), p).unwrap();
        let out = render(&mut m, 100);
        assert!(out.iter().all(|s| (*s - 1.0).abs() < 1e-6));
        assert!(m.is_playing(id));
    }

    #[test]
    fn pitch_and_sample_rate_change_playback_speed() {
        let mut m = Mixer::new(1000);
        // 2000 Hz clip on a 1000 Hz output plays at double speed: 20 frames last 10 output frames.
        let ramp: Vec<f32> = (0..20).map(|i| i as f32).collect();
        m.play(Source::Clip(clip(2000, 1, ramp)), VoiceParams::default());
        let out = render(&mut m, 4);
        assert_eq!([out[0], out[2], out[4], out[6]], [0.0, 2.0, 4.0, 6.0]);
        let mut m = Mixer::new(1000);
        let p = VoiceParams { pitch: 0.5, ..Default::default() };
        m.play(Source::Clip(clip(1000, 1, vec![0.0, 1.0, 2.0, 3.0])), p);
        let out = render(&mut m, 4);
        assert_eq!([out[0], out[2], out[4], out[6]], [0.0, 0.5, 1.0, 1.5]);
    }

    #[test]
    fn volume_master_and_pause() {
        let mut m = Mixer::new(1000);
        m.master_volume = 0.5;
        let p = VoiceParams { volume: 0.5, looping: true, ..Default::default() };
        let id = m.play(Source::Clip(clip(1000, 1, vec![1.0; 4])), p).unwrap();
        assert!((render(&mut m, 2)[0] - 0.25).abs() < 1e-6);
        m.set_params(id, VoiceParams { paused: true, ..p });
        assert!(render(&mut m, 2).iter().all(|s| *s == 0.0));
        assert!(m.is_playing(id));
        m.stop(id);
        assert!(!m.is_playing(id));
    }

    #[test]
    fn spatial_sound_pans_and_fades_with_distance() {
        let mut m = Mixer::new(1000);
        let at = |x: f32| VoiceParams {
            looping: true,
            spatial: Some(Spatial { position: [x, 0.0, 0.0], velocity: [0.0; 3], min_distance: 1.0, max_distance: 50.0 }),
            ..Default::default()
        };
        m.play(Source::Clip(clip(1000, 1, vec![1.0; 4])), at(10.0)).unwrap();
        let out = render(&mut m, 4);
        assert!(out[1] > out[0] * 10.0, "right ear louder: {out:?}");
        m.stop_all();
        m.play(Source::Clip(clip(1000, 1, vec![1.0; 4])), at(40.0)).unwrap();
        let far = render(&mut m, 4);
        assert!(far[1] < out[1]);
        m.stop_all();
        m.play(Source::Clip(clip(1000, 1, vec![1.0; 4])), at(80.0)).unwrap();
        assert!(render(&mut m, 4).iter().all(|s| s.abs() < 1e-6));
    }

    #[test]
    fn moving_the_listener_changes_the_mix_without_a_click() {
        let mut m = Mixer::new(1000);
        let p = VoiceParams {
            looping: true,
            spatial: Some(Spatial { position: [10.0, 0.0, 0.0], velocity: [0.0; 3], min_distance: 1.0, max_distance: 50.0 }),
            ..Default::default()
        };
        m.play(Source::Clip(clip(1000, 1, vec![1.0; 4])), p).unwrap();
        render(&mut m, 4);
        m.set_listener(Listener { position: [20.0, 0.0, 0.0], ..Default::default() });
        let out = render(&mut m, 100);
        // Now on the left, and the change is ramped across the buffer.
        assert!(out[198] > out[199]);
        assert!(out[0] - out[198] > 0.0 || out[1] - out[199] > 0.0);
        let step = (out[2] - out[0]).abs();
        assert!(step < 0.05, "ramped, got {step}");
    }

    #[test]
    fn full_mixer_steals_the_quietest_voice_only_for_louder_sounds() {
        let mut m = Mixer::new(1000);
        m.max_voices = 2;
        let c = clip(1000, 1, vec![1.0; 100]);
        let quiet = VoiceParams { volume: 0.1, ..Default::default() };
        let a = m.play(Source::Clip(c.clone()), quiet).unwrap();
        let b = m.play(Source::Clip(c.clone()), VoiceParams::default()).unwrap();
        // Even quieter than everything playing: rejected.
        assert!(m.play(Source::Clip(c.clone()), VoiceParams { volume: 0.01, ..Default::default() }).is_none());
        let n = m.play(Source::Clip(c), VoiceParams::default()).unwrap();
        assert!(!m.is_playing(a) && m.is_playing(b) && m.is_playing(n));
        assert_eq!(m.voice_count(), 2);
    }

    #[test]
    fn streams_a_wav_file() {
        let dir = std::env::temp_dir().join(format!("dumb_audio_stream_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.wav");
        let samples: Vec<i16> = vec![16384; 4000];
        write_test_wav(&path, &samples);
        let mut m = Mixer::new(8000);
        let s = Stream::open(&path, false).unwrap();
        let id = m.play(Source::Stream(s), VoiceParams::default()).unwrap();
        let mut heard = 0usize;
        for _ in 0..200 {
            let out = render(&mut m, 256);
            heard += out.chunks(2).filter(|f| (f[0] - 0.5).abs() < 1e-3).count();
            if !m.is_playing(id) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!m.is_playing(id), "stream should end");
        assert!(heard >= 3900, "heard {heard} frames");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn write_test_wav(path: &std::path::Path, samples: &[i16]) {
        let data_len = (samples.len() * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&8000u32.to_le_bytes());
        b.extend_from_slice(&16000u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }
}

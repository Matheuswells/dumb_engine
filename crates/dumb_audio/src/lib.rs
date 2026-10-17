//! Dumb Engine audio: spatial sound, streaming and a software mixer.
//!
//! - [`Clip`]s are short sounds decoded once ([`ClipCache`] does it on a worker thread).
//! - [`Stream`]s decode long sounds (music, ambience) ahead of the mixer on a worker thread.
//! - The [`Mixer`] positions voices around a [`Listener`] (distance attenuation, equal-power
//!   panning, Doppler) and sums them to stereo.
//! - [`Audio`] owns the mixer and, when a sound device is available, the output thread. Without
//!   one (a dedicated server, CI) it keeps time by mixing into a scratch buffer, so sounds
//!   still start and finish on schedule.

mod decode;
mod mixer;
mod output;
mod spatial;
mod stream;

pub use decode::{Clip, Decoder};
pub use mixer::{Mixer, Source, Spatial, VoiceId, VoiceParams};
pub use spatial::{attenuation, pan_gains, place, Listener, Placement};
pub use stream::Stream;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

const SAMPLE_RATE: u32 = 48_000;

/// Decodes clips on worker threads and caches them by path.
pub struct ClipCache {
    clips: HashMap<PathBuf, Option<Arc<Clip>>>,
    tx: Sender<(PathBuf, Result<Clip, String>)>,
    rx: Receiver<(PathBuf, Result<Clip, String>)>,
}

impl Default for ClipCache {
    fn default() -> Self {
        let (tx, rx) = channel();
        ClipCache { clips: HashMap::new(), tx, rx }
    }
}

impl ClipCache {
    /// The decoded clip, or `None` while it is still loading (or failed to load).
    pub fn get(&mut self, path: &Path) -> Option<Arc<Clip>> {
        while let Ok((p, res)) = self.rx.try_recv() {
            match res {
                Ok(c) => {
                    self.clips.insert(p, Some(Arc::new(c)));
                }
                Err(e) => {
                    log::warn!("audio: {e}");
                    self.clips.insert(p, None);
                }
            }
        }
        match self.clips.get(path) {
            Some(c) => c.clone(),
            None => {
                // Mark as pending so the load starts once.
                self.clips.insert(path.to_path_buf(), None);
                self.pending_load(path);
                None
            }
        }
    }

    fn pending_load(&mut self, path: &Path) {
        let tx = self.tx.clone();
        let p = path.to_path_buf();
        std::thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || {
                let res = Clip::load(&p);
                tx.send((p, res)).ok();
            })
            .ok();
    }

    /// Drop a cached clip so it reloads next time (hot reload).
    pub fn invalidate(&mut self, path: &Path) {
        self.clips.remove(path);
    }
}

/// The engine's audio system.
pub struct Audio {
    pub mixer: Arc<Mutex<Mixer>>,
    pub clips: ClipCache,
    device: Option<output::Device>,
}

impl Audio {
    /// Open the default sound device, falling back to silent timekeeping.
    pub fn new() -> Audio {
        let mixer = Arc::new(Mutex::new(Mixer::new(SAMPLE_RATE)));
        let device = match output::Device::open(mixer.clone(), SAMPLE_RATE) {
            Ok(d) => Some(d),
            Err(e) => {
                log::info!("audio output disabled: {e}");
                None
            }
        };
        Audio { mixer, clips: ClipCache::default(), device }
    }

    /// No device, ever: for servers and tests.
    pub fn silent() -> Audio {
        Audio { mixer: Arc::new(Mutex::new(Mixer::new(SAMPLE_RATE))), clips: ClipCache::default(), device: None }
    }

    pub fn has_device(&self) -> bool {
        self.device.is_some()
    }

    /// Call once per frame. Without a device this advances the mixer by `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        if self.device.is_some() {
            return;
        }
        let mut m = self.mixer.lock().unwrap();
        let mut frames = (dt.clamp(0.0, 0.25) * m.out_rate as f32) as usize;
        let mut scratch = [0.0f32; 2048];
        while frames > 0 {
            let n = frames.min(1024);
            m.render(&mut scratch[..n * 2]);
            frames -= n;
        }
    }
}

impl Default for Audio {
    fn default() -> Self {
        Audio::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_audio_keeps_time() {
        let mut a = Audio::silent();
        let clip = Arc::new(Clip { rate: SAMPLE_RATE, channels: 1, samples: vec![0.1; SAMPLE_RATE as usize / 2] });
        let id = a.mixer.lock().unwrap().play(Source::Clip(clip), VoiceParams::default()).unwrap();
        a.update(0.25);
        assert!(a.mixer.lock().unwrap().is_playing(id));
        a.update(0.25);
        a.update(0.05);
        assert!(!a.mixer.lock().unwrap().is_playing(id));
    }

    #[test]
    fn clip_cache_loads_in_the_background_and_reports_failures_once() {
        let mut c = ClipCache::default();
        let bad = Path::new("/no/such/file.wav");
        assert!(c.get(bad).is_none());
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            assert!(c.get(bad).is_none());
        }
        assert_eq!(c.clips.len(), 1);
    }
}

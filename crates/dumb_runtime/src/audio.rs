//! Plays `AudioSource` components through the mixer.

use dumb_audio::{Audio, Listener, Source, Spatial, Stream, VoiceId, VoiceParams};
use dumb_core::{Entity, Vec3};
use dumb_ecs::{AudioListener, AudioSource, World};
use dumb_asset::AssetDatabase;
use dumb_render::find_primary_camera;
use std::collections::HashMap;

/// Owns the sound device and keeps one voice per playing `AudioSource`.
pub struct AudioSystem {
    pub audio: Audio,
    voices: HashMap<u64, VoiceId>,
    last_listener: Option<Vec3>,
    /// Emitter positions last frame, for Doppler velocity.
    emitter_pos: HashMap<u64, Vec3>,
    paused: bool,
}

impl AudioSystem {
    pub fn new() -> Self {
        AudioSystem { audio: Audio::new(), voices: HashMap::new(), last_listener: None, emitter_pos: HashMap::new(), paused: false }
    }

    /// No sound device (servers, tests).
    pub fn silent() -> Self {
        AudioSystem { audio: Audio::silent(), voices: HashMap::new(), last_listener: None, emitter_pos: HashMap::new(), paused: false }
    }

    /// Pause or resume every sound (editor pause).
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Stop everything.
    pub fn stop_all(&mut self) {
        self.audio.mixer.lock().unwrap().stop_all();
        self.voices.clear();
        self.emitter_pos.clear();
    }

    pub fn voice_count(&self) -> usize {
        self.voices.len()
    }

    /// One frame: start/stop/update voices to match the world, then place the listener.
    pub fn update(&mut self, world: &mut World, db: &AssetDatabase, dt: f32) {
        let listener_e = world.query_ref::<(Entity, &AudioListener)>().next().map(|(e, _)| e).or_else(|| find_primary_camera(world));
        let master = listener_e.and_then(|e| world.get::<AudioListener>(e)).map(|l| l.volume).unwrap_or(1.0);
        let listener = listener_e.map(|e| {
            let m = world.global_matrix(e);
            let pos = m.w_axis.truncate();
            let vel = match self.last_listener {
                Some(prev) if dt > 1e-4 => (pos - prev) / dt,
                _ => Vec3::ZERO,
            };
            self.last_listener = Some(pos);
            Listener { position: pos.to_array(), right: m.x_axis.truncate().normalize_or(Vec3::X).to_array(), velocity: vel.to_array() }
        });

        let mut mixer = self.audio.mixer.lock().unwrap();
        mixer.master_volume = master;
        if let Some(l) = listener {
            mixer.set_listener(l);
        }
        drop(mixer);

        let mut seen: std::collections::HashSet<u64> = Default::default();
        let entities: Vec<Entity> = world.query_ref::<(Entity, &AudioSource)>().map(|(e, _)| e).collect();
        for e in entities {
            let m = world.global_matrix(e);
            let pos = m.w_axis.truncate();
            let Some(src) = world.get_mut::<AudioSource>(e) else { continue };
            if !src.started {
                src.started = true;
                if src.play_on_start {
                    src.playing = true;
                }
            }
            let key = e.to_bits();
            seen.insert(key);
            let vel = match self.emitter_pos.insert(key, pos) {
                Some(prev) if dt > 1e-4 => (pos - prev) / dt,
                _ => Vec3::ZERO,
            };
            let voice = self.voices.get(&key).copied();
            let mut mixer = self.audio.mixer.lock().unwrap();
            match voice {
                Some(id) if !mixer.is_playing(id) => {
                    // The one-shot ended.
                    self.voices.remove(&key);
                    if !src.looping {
                        src.playing = false;
                    }
                }
                Some(id) if !src.playing => {
                    mixer.stop(id);
                    self.voices.remove(&key);
                }
                Some(id) => {
                    let p = voice_params(src, pos, vel, self.paused);
                    mixer.set_params(id, p);
                }
                None if src.playing => {
                    drop(mixer);
                    let path = if src.clip == dumb_core::AssetId::NONE { None } else { db.abs_path(src.clip) };
                    let Some(path) = path else {
                        src.playing = false;
                        continue;
                    };
                    let source = if src.stream {
                        match Stream::open(&path, src.looping) {
                            Ok(s) => Some(Source::Stream(s)),
                            Err(e) => {
                                log::warn!("audio: {e}");
                                src.playing = false;
                                None
                            }
                        }
                    } else {
                        // Still decoding on a worker thread: try again next frame.
                        self.audio.clips.get(&path).map(Source::Clip)
                    };
                    if let Some(source) = source {
                        let p = voice_params(src, pos, Vec3::ZERO, self.paused);
                        if let Some(id) = self.audio.mixer.lock().unwrap().play(source, p) {
                            self.voices.insert(key, id);
                        } else {
                            src.playing = false;
                        }
                    }
                }
                None => {}
            }
        }
        // Entities that were despawned take their sounds with them.
        self.emitter_pos.retain(|k, _| seen.contains(k));
        let gone: Vec<u64> = self.voices.keys().copied().filter(|k| !seen.contains(k)).collect();
        if !gone.is_empty() {
            let mut mixer = self.audio.mixer.lock().unwrap();
            for k in gone {
                if let Some(id) = self.voices.remove(&k) {
                    mixer.stop(id);
                }
            }
        }
        self.audio.update(dt);
    }

}

fn voice_params(src: &AudioSource, pos: Vec3, vel: Vec3, paused: bool) -> VoiceParams {
    VoiceParams {
        volume: src.volume,
        pitch: src.pitch,
        looping: src.looping,
        paused,
        spatial: src.spatial.then(|| Spatial {
            position: pos.to_array(),
            velocity: vel.to_array(),
            min_distance: src.min_distance,
            max_distance: src.max_distance.max(src.min_distance + 0.01),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumb_core::Quat;
    use dumb_ecs::Transform;

    fn project_with_wav(tag: &str, seconds: f32) -> (std::path::PathBuf, AssetDatabase) {
        let dir = std::env::temp_dir().join(format!("dumb_runtime_audio_{tag}_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("Assets")).unwrap();
        let rate = 8000u32;
        let n = (rate as f32 * seconds) as usize;
        let data_len = (n * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for _ in 0..n {
            b.extend_from_slice(&8000i16.to_le_bytes());
        }
        std::fs::write(dir.join("Assets/beep.wav"), b).unwrap();
        let db = AssetDatabase::open(&dir).unwrap();
        (dir, db)
    }

    fn at(x: f32) -> Transform {
        Transform { translation: Vec3::new(x, 0.0, 0.0), rotation: Quat::IDENTITY, scale: Vec3::ONE }
    }

    fn run(sys: &mut AudioSystem, world: &mut World, db: &AssetDatabase, frames: usize, dt: f32) {
        for _ in 0..frames {
            world.update_transforms();
            sys.update(world, db, dt);
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
    }

    #[test]
    fn one_shot_plays_then_clears_playing() {
        let (dir, db) = project_with_wav("oneshot", 0.2);
        let clip = db.id_for_path("beep.wav").expect("wav indexed");
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, at(0.0));
        world.insert(e, AudioSource { clip, spatial: false, ..Default::default() });
        let mut sys = AudioSystem::silent();
        // Clip decodes on a worker thread; it starts playing as soon as that is done.
        for _ in 0..200 {
            run(&mut sys, &mut world, &db, 1, 0.001);
            if sys.voice_count() == 1 {
                break;
            }
        }
        assert_eq!(sys.voice_count(), 1, "voice started");
        assert!(world.get::<AudioSource>(e).unwrap().playing);
        run(&mut sys, &mut world, &db, 4, 0.1);
        let s = world.get::<AudioSource>(e).unwrap();
        assert!(!s.playing, "one-shot clears playing");
        assert_eq!(sys.voice_count(), 0);
        // Setting playing again restarts it.
        world.get_mut::<AudioSource>(e).unwrap().playing = true;
        run(&mut sys, &mut world, &db, 1, 0.001);
        assert_eq!(sys.voice_count(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn clearing_playing_or_despawning_stops_the_voice() {
        let (dir, db) = project_with_wav("stop", 5.0);
        let clip = db.id_for_path("beep.wav").unwrap();
        let mut world = World::new();
        let mut sys = AudioSystem::silent();
        let mk = |world: &mut World, x: f32| {
            let e = world.spawn();
            world.insert(e, at(x));
            world.insert(e, AudioSource { clip, looping: true, ..Default::default() });
            e
        };
        let (a, b) = (mk(&mut world, 1.0), mk(&mut world, 2.0));
        for _ in 0..200 {
            run(&mut sys, &mut world, &db, 1, 0.001);
            if sys.voice_count() == 2 {
                break;
            }
        }
        assert_eq!(sys.voice_count(), 2);
        world.get_mut::<AudioSource>(a).unwrap().playing = false;
        run(&mut sys, &mut world, &db, 1, 0.001);
        assert_eq!(sys.voice_count(), 1);
        world.despawn(b);
        run(&mut sys, &mut world, &db, 1, 0.001);
        assert_eq!(sys.voice_count(), 0);
        assert_eq!(sys.audio.mixer.lock().unwrap().voice_count(), 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_clip_does_not_play() {
        let (dir, db) = project_with_wav("missing", 0.1);
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, AudioSource { playing: true, ..Default::default() });
        let mut sys = AudioSystem::silent();
        run(&mut sys, &mut world, &db, 2, 0.01);
        assert_eq!(sys.voice_count(), 0);
        assert!(!world.get::<AudioSource>(e).unwrap().playing);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn listener_entity_sets_master_volume() {
        let (dir, db) = project_with_wav("listener", 0.1);
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, at(0.0));
        world.insert(e, AudioListener { volume: 0.25 });
        let mut sys = AudioSystem::silent();
        run(&mut sys, &mut world, &db, 1, 0.01);
        assert_eq!(sys.audio.mixer.lock().unwrap().master_volume, 0.25);
        std::fs::remove_dir_all(dir).ok();
    }
}

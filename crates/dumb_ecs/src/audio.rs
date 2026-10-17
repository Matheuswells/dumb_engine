//! Audio components. The runtime's `AudioSystem` turns them into mixer voices.
//!
//! Scripts play a sound by setting `playing = true` (and stop it with `false`); a one-shot
//! clears `playing` itself when it ends.

use dumb_core::AssetId;
use dumb_derive::{Component, Editor};

/// A sound attached to an entity.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct AudioSource {
    #[editor(asset = "audio")]
    pub clip: AssetId,
    #[editor(range = 0.0..=4.0, speed = 0.01)]
    pub volume: f32,
    /// Playback speed; also shifts the pitch.
    #[editor(range = 0.1..=4.0, speed = 0.01)]
    pub pitch: f32,
    pub looping: bool,
    /// Positioned at the entity (distance attenuation, panning, Doppler). Off plays in 2D.
    pub spatial: bool,
    /// Full volume inside this distance (meters).
    #[editor(range = 0.01..=1000.0)]
    pub min_distance: f32,
    /// Silent beyond this distance (meters).
    #[editor(range = 0.1..=10000.0)]
    pub max_distance: f32,
    /// Decode while playing instead of loading the whole file: for music and long ambience.
    pub stream: bool,
    /// Start when the entity first appears.
    pub play_on_start: bool,
    /// Set by scripts to start/stop; cleared by the audio system when a one-shot ends.
    pub playing: bool,
    #[editor(hidden)]
    pub started: bool,
}

impl Default for AudioSource {
    fn default() -> Self {
        AudioSource {
            clip: AssetId::NONE,
            volume: 1.0,
            pitch: 1.0,
            looping: false,
            spatial: true,
            min_distance: 1.0,
            max_distance: 50.0,
            stream: false,
            play_on_start: true,
            playing: false,
            started: false,
        }
    }
}

/// The ears. The first listener in the world is used; without one, the primary camera.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct AudioListener {
    /// Master volume.
    #[editor(range = 0.0..=2.0, speed = 0.01)]
    pub volume: f32,
}

impl Default for AudioListener {
    fn default() -> Self {
        AudioListener { volume: 1.0 }
    }
}

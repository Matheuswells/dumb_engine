//! Audio file decoding (wav, ogg/vorbis, flac, mp3) into interleaved `f32` samples.

use std::fs::File;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder as CodecDecoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// A fully decoded sound, for short effects.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub rate: u32,
    /// 1 or 2 (more channels are folded down while decoding).
    pub channels: usize,
    /// Interleaved samples.
    pub samples: Vec<f32>,
}

impl Clip {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    pub fn duration(&self) -> f32 {
        self.frames() as f32 / self.rate.max(1) as f32
    }

    /// Decode a whole file.
    pub fn load(path: &Path) -> Result<Clip, String> {
        let mut dec = Decoder::open(path)?;
        let mut samples = Vec::new();
        while dec.read_chunk(&mut samples)? {}
        Ok(Clip { rate: dec.rate, channels: dec.channels, samples })
    }
}

/// Incremental decoder: each `read_chunk` appends one packet worth of samples.
pub struct Decoder {
    format: Box<dyn FormatReader>,
    codec: Box<dyn CodecDecoder>,
    track: u32,
    pub rate: u32,
    pub channels: usize,
    src_channels: usize,
    buf: Option<SampleBuffer<f32>>,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Decoder, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| format!("{}: no audio track", path.display()))?;
        let rate = track.codec_params.sample_rate.ok_or_else(|| format!("{}: unknown sample rate", path.display()))?;
        let src_channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2).max(1);
        let id = track.id;
        let codec = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Decoder { format, codec, track: id, rate, channels: src_channels.min(2), src_channels, buf: None })
    }

    /// Append the next packet to `out`. Returns `false` at the end of the stream.
    pub fn read_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(SymError::ResetRequired) => return Ok(false),
                Err(e) => return Err(e.to_string()),
            };
            if packet.track_id() != self.track {
                continue;
            }
            match self.codec.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let buf = self.buf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
                    if buf.capacity() < decoded.capacity() * spec.channels.count() {
                        *buf = SampleBuffer::new(decoded.capacity() as u64, spec);
                    }
                    buf.copy_interleaved_ref(decoded);
                    fold(buf.samples(), self.src_channels, out);
                    return Ok(true);
                }
                // A corrupt packet is skipped, not fatal.
                Err(SymError::DecodeError(e)) => {
                    log::warn!("audio decode error: {e}");
                    continue;
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

/// Mono stays mono, stereo stays stereo, anything wider keeps its first two channels.
fn fold(src: &[f32], channels: usize, out: &mut Vec<f32>) {
    match channels {
        1 | 2 => out.extend_from_slice(src),
        n => {
            for frame in src.chunks_exact(n) {
                out.push(frame[0]);
                out.push(frame[1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_surround_to_stereo() {
        let mut out = Vec::new();
        fold(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], 4, &mut out);
        assert_eq!(out, vec![1.0, 2.0, 5.0, 6.0]);
    }

    /// Write a PCM16 wav in memory-free fashion for tests.
    pub fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
        let data_len = (samples.len() * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        b.extend_from_slice(&(channels * 2).to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn decodes_a_wav_file() {
        let dir = std::env::temp_dir().join(format!("dumb_audio_wav_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.wav");
        let samples: Vec<i16> = (0..400).map(|i| ((i % 100) as i16 - 50) * 300).collect();
        write_wav(&p, 8000, 2, &samples);
        let clip = Clip::load(&p).unwrap();
        assert_eq!((clip.rate, clip.channels, clip.frames()), (8000, 2, 200));
        assert!((clip.samples[1] - samples[1] as f32 / 32768.0).abs() < 1e-3);
        assert!((clip.duration() - 0.025).abs() < 1e-4);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(Clip::load(Path::new("/definitely/not/here.wav")).is_err());
    }
}

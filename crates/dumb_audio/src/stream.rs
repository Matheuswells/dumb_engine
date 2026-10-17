//! Streaming playback of long sounds: a worker thread decodes ahead, the mixer pulls frames.

use crate::decode::Decoder;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::sync::Arc;

/// Chunks decoded ahead of the mixer. Bounds memory to a few seconds of audio.
const AHEAD_CHUNKS: usize = 16;

pub struct Stream {
    pub rate: u32,
    pub channels: usize,
    rx: Receiver<Vec<f32>>,
    /// Interleaved samples starting at frame `base`.
    buf: VecDeque<f32>,
    base: usize,
    ended: bool,
    stop: Arc<AtomicBool>,
}

impl Stream {
    /// Open `path` and start decoding. With `looping` the file restarts when it ends.
    pub fn open(path: &Path, looping: bool) -> Result<Stream, String> {
        let mut dec = Decoder::open(path)?;
        let (rate, channels) = (dec.rate, dec.channels);
        let (tx, rx) = sync_channel::<Vec<f32>>(AHEAD_CHUNKS);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let path = path.to_path_buf();
        std::thread::Builder::new()
            .name("audio-stream".into())
            .spawn(move || {
                let mut produced_since_open = false;
                loop {
                    if flag.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut chunk = Vec::new();
                    match dec.read_chunk(&mut chunk) {
                        Ok(true) => {
                            produced_since_open |= !chunk.is_empty();
                            // Blocks while the mixer is far behind; fails once the stream is dropped.
                            let mut pending = Some(chunk);
                            while let Some(c) = pending.take() {
                                if flag.load(Ordering::Relaxed) {
                                    return;
                                }
                                match tx.try_send(c) {
                                    Ok(()) => {}
                                    Err(std::sync::mpsc::TrySendError::Full(c)) => {
                                        pending = Some(c);
                                        std::thread::sleep(std::time::Duration::from_millis(5));
                                    }
                                    Err(_) => return,
                                }
                            }
                        }
                        Ok(false) => {
                            // An empty file would loop forever without producing anything.
                            if !looping || !produced_since_open {
                                return;
                            }
                            produced_since_open = false;
                            match Decoder::open(&path) {
                                Ok(d) => dec = d,
                                Err(e) => {
                                    log::warn!("audio stream reopen failed: {e}");
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!("audio stream decode failed: {e}");
                            return;
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Stream { rate, channels, rx, buf: VecDeque::new(), base: 0, ended: false, stop })
    }

    fn pump(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(chunk) => self.buf.extend(chunk),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.ended = true;
                    return;
                }
            }
        }
    }

    /// Frame `i` as `[left, right]`. `None` when it isn't decoded yet (underrun) or the stream ended.
    pub fn frame(&mut self, i: usize) -> Option<[f32; 2]> {
        if i < self.base {
            return None;
        }
        let ch = self.channels;
        let idx = (i - self.base) * ch;
        if idx + ch > self.buf.len() {
            self.pump();
            if idx + ch > self.buf.len() {
                return None;
            }
        }
        let l = self.buf[idx];
        Some([l, if ch > 1 { self.buf[idx + 1] } else { l }])
    }

    /// Forget frames before `i`; they've been played.
    pub fn discard_before(&mut self, i: usize) {
        if i <= self.base {
            return;
        }
        let n = ((i - self.base) * self.channels).min(self.buf.len());
        self.buf.drain(..n);
        self.base += n / self.channels;
    }

    /// The decoder is done and every decoded frame was consumed.
    pub fn finished(&mut self, pos: usize) -> bool {
        self.pump();
        self.ended && (pos - self.base.min(pos)) * self.channels >= self.buf.len()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

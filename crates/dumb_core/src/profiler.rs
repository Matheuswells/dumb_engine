//! Lightweight frame profiler.
//!
//! ```ignore
//! profile_scope!("physics");          // timed until the end of the block
//! dumb_core::profiler::end_frame();    // once per frame, on the main thread
//! ```
//!
//! Scopes from any thread are recorded (with their thread) into the current frame. The last
//! [`HISTORY`] frames are kept for the profiler window and overlays. Recording costs one mutex
//! lock per scope, so use scopes for coarse work (systems, passes), not inner loops.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// Frames kept in the history.
pub const HISTORY: usize = 600;

#[derive(Clone, Debug)]
pub struct ScopeEvent {
    pub name: &'static str,
    /// Microseconds from the start of the frame.
    pub start_us: f32,
    pub dur_us: f32,
    /// Nesting depth on its thread (0 = top level).
    pub depth: u16,
    /// Small id per thread (0 = the thread that ends frames).
    pub thread: u16,
}

#[derive(Clone, Debug, Default)]
pub struct FrameRecord {
    pub index: u64,
    /// Wall time of the whole frame (ms).
    pub frame_ms: f32,
    /// GPU time of the frame, when the renderer reports it (ms).
    pub gpu_ms: Option<f32>,
    pub scopes: Vec<ScopeEvent>,
    /// Named counters reported this frame (draw calls, entities...).
    pub counters: Vec<(&'static str, f64)>,
}

impl FrameRecord {
    /// Total time of all top-level scopes with this name (ms).
    pub fn time_of(&self, name: &str) -> f32 {
        self.scopes.iter().filter(|s| s.name == name).map(|s| s.dur_us).sum::<f32>() / 1000.0
    }
}

struct State {
    frame_start: Instant,
    current: FrameRecord,
    history: VecDeque<FrameRecord>,
    threads: Vec<std::thread::ThreadId>,
    pending_gpu: Vec<(u64, f32)>,
}

static ENABLED: AtomicBool = AtomicBool::new(true);
static PAUSED: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<State>> = Mutex::new(None);

thread_local! {
    static DEPTH: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(|| State {
        frame_start: Instant::now(),
        current: FrameRecord::default(),
        history: VecDeque::with_capacity(HISTORY),
        threads: vec![std::thread::current().id()],
        pending_gpu: Vec::new(),
    });
    f(s)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Stop adding frames to the history (to inspect a spike).
pub fn set_paused(p: bool) {
    PAUSED.store(p, Ordering::Relaxed);
}

pub fn paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

/// Timer that records a scope when dropped. Use [`profile_scope!`](crate::profile_scope).
pub struct Scope {
    name: &'static str,
    start: Instant,
    depth: u16,
    active: bool,
}

impl Scope {
    pub fn new(name: &'static str) -> Self {
        let active = enabled();
        let depth = if active {
            DEPTH.with(|d| {
                let v = d.get();
                d.set(v + 1);
                v
            })
        } else {
            0
        };
        Scope { name, start: Instant::now(), depth, active }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        let end = Instant::now();
        let id = std::thread::current().id();
        with_state(|s| {
            let thread = match s.threads.iter().position(|t| *t == id) {
                Some(i) => i,
                None => {
                    s.threads.push(id);
                    s.threads.len() - 1
                }
            } as u16;
            let start_us = self.start.saturating_duration_since(s.frame_start).as_secs_f32() * 1e6;
            s.current.scopes.push(ScopeEvent { name: self.name, start_us, dur_us: (end - self.start).as_secs_f32() * 1e6, depth: self.depth, thread });
        });
    }
}

/// Time the rest of the enclosing block under `name`.
#[macro_export]
macro_rules! profile_scope {
    ($name:expr) => {
        let _profile_scope = $crate::profiler::Scope::new($name);
    };
}

/// A `&'static` copy of a dynamic scope name (script system names...). Each distinct name is
/// leaked once.
pub fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut v = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = v.iter().find(|n| **n == name) {
        return n;
    }
    let n: &'static str = Box::leak(name.to_string().into_boxed_str());
    v.push(n);
    n
}

/// Record a counter for the current frame (shown in the profiler).
pub fn counter(name: &'static str, value: f64) {
    if enabled() {
        with_state(|s| s.current.counters.push((name, value)));
    }
}

/// Report the GPU time of a frame (by index, arriving a few frames late).
pub fn report_gpu(frame_index: u64, ms: f32) {
    with_state(|s| {
        if let Some(f) = s.history.iter_mut().rev().find(|f| f.index == frame_index) {
            f.gpu_ms = Some(ms);
        } else {
            s.pending_gpu.push((frame_index, ms));
        }
    });
}

/// Close the current frame and start the next one. Returns the index of the new frame.
pub fn end_frame() -> u64 {
    with_state(|s| {
        let now = Instant::now();
        let mut done = std::mem::take(&mut s.current);
        done.frame_ms = (now - s.frame_start).as_secs_f32() * 1000.0;
        if let Some(i) = s.pending_gpu.iter().position(|(f, _)| *f == done.index) {
            done.gpu_ms = Some(s.pending_gpu.swap_remove(i).1);
        }
        s.pending_gpu.retain(|(f, _)| *f + 16 > done.index);
        let next = done.index + 1;
        if !paused() {
            if s.history.len() >= HISTORY {
                s.history.pop_front();
            }
            s.history.push_back(done);
        }
        s.current.index = next;
        s.frame_start = now;
        next
    })
}

/// Index of the frame being recorded.
pub fn current_frame() -> u64 {
    with_state(|s| s.current.index)
}

/// Copy of the recorded history (oldest first).
pub fn history() -> Vec<FrameRecord> {
    with_state(|s| s.history.iter().cloned().collect())
}

/// The last finished frame.
pub fn last_frame() -> Option<FrameRecord> {
    with_state(|s| s.history.back().cloned())
}

/// Average and maximum of `f` over the last `n` frames.
pub fn stats(n: usize, f: impl Fn(&FrameRecord) -> Option<f32>) -> (f32, f32) {
    with_state(|s| {
        let vals: Vec<f32> = s.history.iter().rev().take(n).filter_map(&f).collect();
        if vals.is_empty() {
            return (0.0, 0.0);
        }
        (vals.iter().sum::<f32>() / vals.len() as f32, vals.iter().cloned().fold(0.0, f32::max))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_nested_scopes_per_frame() {
        end_frame();
        {
            profile_scope!("outer");
            std::thread::sleep(std::time::Duration::from_millis(2));
            {
                profile_scope!("inner");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        std::thread::spawn(|| {
            profile_scope!("worker");
        })
        .join()
        .unwrap();
        counter("draws", 12.0);
        let idx = end_frame() - 1;
        report_gpu(idx, 3.5);
        let f = history().into_iter().find(|f| f.index == idx).unwrap();
        let outer = f.scopes.iter().find(|s| s.name == "outer").unwrap();
        let inner = f.scopes.iter().find(|s| s.name == "inner").unwrap();
        assert_eq!((outer.depth, inner.depth), (0, 1));
        assert!(outer.dur_us >= inner.dur_us && f.time_of("outer") >= 2.0);
        assert!(f.scopes.iter().any(|s| s.name == "worker" && s.thread != outer.thread));
        assert_eq!(f.counters, [("draws", 12.0)]);
        assert_eq!(f.gpu_ms, Some(3.5));
        assert!(f.frame_ms >= 3.0);
    }
}

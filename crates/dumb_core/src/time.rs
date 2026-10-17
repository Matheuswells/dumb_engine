/// Frame timing handed to systems and scripts.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Time {
    /// Seconds since the previous frame, already scaled by `time_scale`.
    pub delta: f32,
    /// Unscaled seconds since the previous frame.
    pub raw_delta: f32,
    /// Seconds since play started.
    pub elapsed: f64,
    pub frame: u64,
    pub time_scale: f32,
}

impl Time {
    pub fn new() -> Self {
        Time { time_scale: 1.0, ..Default::default() }
    }

    pub fn advance(&mut self, raw_delta: f32) {
        // Clamp so a debugger break or a long hitch does not explode the simulation.
        let raw = raw_delta.min(0.25);
        self.raw_delta = raw;
        self.delta = raw * self.time_scale;
        self.elapsed += self.delta as f64;
        self.frame += 1;
    }
}

/// Fixed-timestep accumulator: turns variable frame times into whole simulation ticks.
///
/// The simulation runs `hz` ticks per second no matter the frame rate. A slow frame runs
/// several ticks (capped by `max_steps`, so a hitch cannot spiral); a fast frame may run none.
/// In `deterministic` mode every tick has exactly `1/hz` seconds, ignores `time_scale`, and
/// `elapsed` is derived from the tick counter, so the same inputs per tick replay identically.
#[derive(Clone, Debug)]
pub struct FixedClock {
    pub hz: u32,
    pub max_steps: u32,
    pub deterministic: bool,
    accumulator: f64,
    tick: u64,
    time: Time,
}

impl FixedClock {
    pub fn new(hz: u32, max_steps: u32, deterministic: bool) -> Self {
        FixedClock { hz: hz.max(1), max_steps: max_steps.max(1), deterministic, accumulator: 0.0, tick: 0, time: Time::new() }
    }

    /// Continue from an existing `Time` (tick counter and elapsed) instead of zero.
    pub fn resume(&mut self, t: &Time) {
        self.tick = t.frame;
        self.time = *t;
    }

    /// Seconds per tick.
    pub fn dt(&self) -> f32 {
        1.0 / self.hz as f32
    }

    /// Ticks simulated so far.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// How far the next tick is along, 0..1 (for render interpolation).
    pub fn alpha(&self) -> f32 {
        (self.accumulator * self.hz as f64).clamp(0.0, 1.0) as f32
    }

    /// Feed a frame's wall-clock seconds; returns how many ticks to run now.
    pub fn accumulate(&mut self, raw_delta: f32) -> u32 {
        // Same hitch clamp as `Time::advance`.
        self.accumulator += raw_delta.clamp(0.0, 0.25) as f64;
        let dt = 1.0 / self.hz as f64;
        // The epsilon keeps an exact `1/hz` frame from falling just short of a tick.
        let n = (self.accumulator / dt + 1e-6) as u64;
        let run = n.min(self.max_steps as u64);
        // Ticks beyond the cap are dropped instead of carried as a backlog.
        self.accumulator -= n as f64 * dt;
        run as u32
    }

    /// Advance one tick and return the `Time` handed to systems for it.
    pub fn next_tick(&mut self, time_scale: f32) -> Time {
        let dt = self.dt();
        let scale = if self.deterministic { 1.0 } else { time_scale };
        self.tick += 1;
        self.time.time_scale = scale;
        self.time.raw_delta = dt;
        self.time.delta = dt * scale;
        self.time.frame = self.tick;
        self.time.elapsed = if self.deterministic { self.tick as f64 / self.hz as f64 } else { self.time.elapsed + self.time.delta as f64 };
        self.time
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_whole_ticks() {
        let mut c = FixedClock::new(50, 5, false);
        assert_eq!(c.accumulate(0.01), 0);
        assert_eq!(c.accumulate(0.01), 1);
        assert_eq!(c.accumulate(0.05), 2);
        assert_eq!(c.accumulate(c.dt()), 1);
    }

    #[test]
    fn caps_catch_up() {
        let mut c = FixedClock::new(100, 3, false);
        assert_eq!(c.accumulate(0.25), 3);
        // Backlog is dropped, not carried over.
        assert!(c.alpha() <= 1.0);
        assert_eq!(c.accumulate(0.0), 0);
    }

    #[test]
    fn deterministic_time_follows_ticks() {
        let mut c = FixedClock::new(20, 5, true);
        let mut t = Time::new();
        for _ in 0..40 {
            t = c.next_tick(0.5);
        }
        assert_eq!(t.frame, 40);
        assert_eq!(t.delta, 0.05);
        assert!((t.elapsed - 2.0).abs() < 1e-9);
    }
}

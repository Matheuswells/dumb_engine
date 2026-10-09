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

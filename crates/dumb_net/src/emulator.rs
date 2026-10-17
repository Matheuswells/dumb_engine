use crate::Datagram;
use std::net::SocketAddr;

/// Debug network conditions applied to outgoing datagrams.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EmulatorConfig {
    /// Base one-way delay (seconds).
    pub latency: f64,
    /// Extra random delay in `0..=jitter` seconds. Jitter larger than the gap between packets
    /// also reorders them.
    pub jitter: f64,
    /// Probability (0..1) that a datagram is dropped.
    pub loss: f64,
    /// Probability (0..1) that a datagram is delayed by an extra `reorder_delay`.
    pub reorder: f64,
    pub reorder_delay: f64,
    /// Probability (0..1) that a datagram is delivered twice.
    pub duplicate: f64,
}

/// Wraps a socket and delays / drops / reorders what it sends. Call [`Emulated::advance`] every
/// tick with the current time to release due datagrams.
pub struct Emulated<S: Datagram> {
    pub inner: S,
    pub config: EmulatorConfig,
    now: f64,
    pending: Vec<(f64, SocketAddr, Vec<u8>)>,
    rng: u64,
}

impl<S: Datagram> Emulated<S> {
    pub fn new(inner: S, config: EmulatorConfig, seed: u64) -> Self {
        Self { inner, config, now: 0.0, pending: Vec::new(), rng: seed | 1 }
    }

    fn rand(&mut self) -> f64 {
        // xorshift64*: deterministic so tests are reproducible.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn advance(&mut self, now: f64) {
        self.now = now;
        let mut i = 0;
        let mut due = Vec::new();
        while i < self.pending.len() {
            if self.pending[i].0 <= now {
                due.push(self.pending.remove(i));
            } else {
                i += 1;
            }
        }
        due.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, addr, data) in due {
            self.inner.send_to(addr, &data);
        }
    }

    fn enqueue(&mut self, addr: SocketAddr, data: &[u8]) {
        let c = self.config;
        let mut delay = c.latency + self.rand() * c.jitter;
        if self.rand() < c.reorder {
            delay += c.reorder_delay;
        }
        self.pending.push((self.now + delay, addr, data.to_vec()));
    }
}

impl<S: Datagram> Datagram for Emulated<S> {
    fn send_to(&mut self, addr: SocketAddr, data: &[u8]) {
        if self.rand() < self.config.loss {
            return;
        }
        self.enqueue(addr, data);
        if self.rand() < self.config.duplicate {
            self.enqueue(addr, data);
        }
        // Zero-delay traffic goes out immediately.
        let now = self.now;
        self.advance(now);
    }
    fn recv_from(&mut self) -> Option<(SocketAddr, Vec<u8>)> {
        self.inner.recv_from()
    }
}

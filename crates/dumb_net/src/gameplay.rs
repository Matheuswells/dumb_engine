//! Client-side prediction with server reconciliation, and the server-side trust boundary for
//! client commands (validation, per-tick limits, per-entity ownership).

use crate::{ConnectionId, NetId};
use std::collections::{HashMap, VecDeque};

/// Predicts the owned entity locally and replays unacknowledged inputs on top of each
/// authoritative state.
pub struct Predictor<I: Clone, S: Clone> {
    next_seq: u32,
    pending: VecDeque<(u32, I)>,
    pub state: S,
}

impl<I: Clone, S: Clone> Predictor<I, S> {
    pub fn new(state: S) -> Self {
        Self { next_seq: 1, pending: VecDeque::new(), state }
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Applies `input` locally right away and returns its sequence number for sending.
    pub fn apply(&mut self, input: I, step: impl Fn(&S, &I) -> S) -> u32 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.state = step(&self.state, &input);
        self.pending.push_back((seq, input));
        seq
    }

    /// The server state includes inputs up to `acked_seq`: rewind to it, replay the rest.
    pub fn reconcile(&mut self, server_state: S, acked_seq: u32, step: impl Fn(&S, &I) -> S) {
        while self.pending.front().is_some_and(|(s, _)| *s <= acked_seq) {
            self.pending.pop_front();
        }
        self.state = self.pending.iter().fold(server_state, |s, (_, i)| step(&s, i));
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    NotOwner,
    RateLimited,
    Invalid,
    Stale,
}

/// Validates every client command before the simulation sees it.
#[derive(Default)]
pub struct CommandGate {
    owners: HashMap<NetId, ConnectionId>,
    max_per_tick: u32,
    used: HashMap<ConnectionId, u32>,
    last_seq: HashMap<ConnectionId, u32>,
}

impl CommandGate {
    pub fn new(max_per_tick: u32) -> Self {
        Self { max_per_tick, ..Default::default() }
    }

    pub fn set_owner(&mut self, entity: NetId, owner: ConnectionId) {
        self.owners.insert(entity, owner);
    }

    pub fn clear_owner(&mut self, entity: NetId) {
        self.owners.remove(&entity);
    }

    pub fn drop_connection(&mut self, c: ConnectionId) {
        self.owners.retain(|_, o| *o != c);
        self.used.remove(&c);
        self.last_seq.remove(&c);
    }

    pub fn new_tick(&mut self) {
        self.used.clear();
    }

    /// Accepts a command from `from` targeting `entity` with client sequence `seq`. `valid` is
    /// the command-specific check (ranges, cooldowns, ...).
    pub fn check(&mut self, from: ConnectionId, entity: NetId, seq: u32, valid: impl FnOnce() -> bool) -> Result<(), Reject> {
        if self.owners.get(&entity) != Some(&from) {
            return Err(Reject::NotOwner);
        }
        let n = self.used.entry(from).or_insert(0);
        if *n >= self.max_per_tick {
            return Err(Reject::RateLimited);
        }
        *n += 1;
        if self.last_seq.get(&from).is_some_and(|l| seq <= *l) {
            return Err(Reject::Stale);
        }
        if !valid() {
            return Err(Reject::Invalid);
        }
        self.last_seq.insert(from, seq);
        Ok(())
    }

    pub fn last_seq(&self, c: ConnectionId) -> u32 {
        self.last_seq.get(&c).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prediction_converges_after_reconcile() {
        let step = |s: &f32, i: &f32| s + i;
        let mut p = Predictor::new(0.0f32);
        for _ in 0..4 {
            p.apply(1.0, step);
        }
        assert_eq!(p.state, 4.0);
        // Server processed 2 inputs but clamped movement (state 1.5): replay the other 2.
        p.reconcile(1.5, 2, step);
        assert_eq!(p.pending(), 2);
        assert_eq!(p.state, 3.5);
        p.reconcile(3.5, 4, step);
        assert_eq!((p.pending(), p.state), (0, 3.5));
    }

    #[test]
    fn gate_enforces_ownership_rate_order_and_validity() {
        let (a, b) = (ConnectionId(1), ConnectionId(2));
        let mut g = CommandGate::new(3);
        g.set_owner(10, a);
        assert_eq!(g.check(b, 10, 1, || true), Err(Reject::NotOwner));
        assert_eq!(g.check(a, 99, 1, || true), Err(Reject::NotOwner));
        assert_eq!(g.check(a, 10, 1, || false), Err(Reject::Invalid));
        assert_eq!(g.check(a, 10, 1, || true), Ok(()));
        assert_eq!(g.check(a, 10, 1, || true), Err(Reject::Stale));
        assert_eq!(g.check(a, 10, 2, || true), Err(Reject::RateLimited));
        g.new_tick();
        assert_eq!(g.check(a, 10, 2, || true), Ok(()));
        assert_eq!(g.last_seq(a), 2);
        g.drop_connection(a);
        assert_eq!(g.check(a, 10, 3, || true), Err(Reject::NotOwner));
    }
}

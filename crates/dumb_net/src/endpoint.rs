use crate::wire::{self, newer, Reader};
use crate::Datagram;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;

/// Delivery guarantee of a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Always delivered, exactly once, in send order (retransmitted until acknowledged).
    ReliableOrdered,
    /// Fire and forget; may be lost, but stale (out-of-order) messages are dropped.
    Unreliable,
}

/// Identifies one session; survives a client address change (reconnect).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub u64);

/// Typed network events, surfaced to scripts as engine events.
#[derive(Clone, Debug, PartialEq)]
pub enum NetEvent {
    Connected(ConnectionId),
    /// A known session came back (new socket address) within the grace period.
    Reconnected(ConnectionId),
    /// Peer left or timed out. `timed_out` is false for an explicit disconnect.
    Disconnected { id: ConnectionId, timed_out: bool },
    Message { id: ConnectionId, channel: Channel, data: Vec<u8> },
    /// Client only: the connection attempt timed out.
    ConnectFailed,
}

#[derive(Clone, Copy, Debug)]
pub struct NetConfig {
    /// Silence longer than this drops the connection (seconds).
    pub timeout: f64,
    /// Resend interval for unacknowledged reliable messages.
    pub resend_interval: f64,
    /// Idle keep-alive / ack interval.
    pub keepalive: f64,
    /// How long a timed-out session can still be resumed by a reconnect.
    pub reconnect_grace: f64,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self { timeout: 5.0, resend_interval: 0.1, keepalive: 0.5, reconnect_grace: 30.0 }
    }
}

/// Per-connection reliability state shared by client and server.
struct Link {
    next_rel: u16,
    unacked: VecDeque<(u16, Vec<u8>, f64)>,
    expect_rel: u16,
    rel_buf: HashMap<u16, Vec<u8>>,
    unrel_out: Vec<(u16, Vec<u8>)>,
    next_unrel: u16,
    last_unrel_in: Option<u16>,
    last_recv: f64,
    last_send: f64,
    need_ack: bool,
}

impl Link {
    fn new(now: f64) -> Self {
        Self {
            next_rel: 0,
            unacked: VecDeque::new(),
            expect_rel: 0,
            rel_buf: HashMap::new(),
            unrel_out: Vec::new(),
            next_unrel: 0,
            last_unrel_in: None,
            last_recv: now,
            last_send: now,
            need_ack: false,
        }
    }

    fn send(&mut self, channel: Channel, data: &[u8]) -> bool {
        if data.len() + 5 > wire::MAX_BODY - 2 {
            log::warn!("dumb_net: message of {} bytes exceeds the datagram budget", data.len());
            return false;
        }
        match channel {
            Channel::ReliableOrdered => {
                self.unacked.push_back((self.next_rel, data.to_vec(), f64::NEG_INFINITY));
                self.next_rel = self.next_rel.wrapping_add(1);
            }
            Channel::Unreliable => {
                self.unrel_out.push((self.next_unrel, data.to_vec()));
                self.next_unrel = self.next_unrel.wrapping_add(1);
            }
        }
        true
    }

    fn receive(&mut self, body: &[u8], now: f64) -> Vec<(Channel, Vec<u8>)> {
        self.last_recv = now;
        let mut out = Vec::new();
        let mut r = Reader(body);
        let Some(ack) = r.u16() else { return out };
        // `ack` is the peer's next expected reliable id: everything before it arrived.
        while let Some(&(id, _, _)) = self.unacked.front() {
            if newer(ack, id) {
                self.unacked.pop_front();
            } else {
                break;
            }
        }
        while !r.is_empty() {
            let (Some(ch), Some(id), Some(len)) = (r.u8(), r.u16(), r.u16()) else { break };
            let Some(data) = r.take(len as usize) else { break };
            match ch {
                0 => {
                    self.need_ack = true;
                    // Only ids at or after the cursor are new; older ones are duplicates.
                    if id == self.expect_rel || newer(id, self.expect_rel) {
                        self.rel_buf.entry(id).or_insert_with(|| data.to_vec());
                    }
                }
                1 => {
                    if self.last_unrel_in.is_none_or(|l| newer(id, l)) {
                        self.last_unrel_in = Some(id);
                        out.push((Channel::Unreliable, data.to_vec()));
                    }
                }
                _ => break,
            }
        }
        while let Some(d) = self.rel_buf.remove(&self.expect_rel) {
            out.push((Channel::ReliableOrdered, d));
            self.expect_rel = self.expect_rel.wrapping_add(1);
        }
        out
    }

    /// Builds the data body to send now, if there is anything (or a keep-alive is due).
    fn flush(&mut self, now: f64, cfg: &NetConfig) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        let mut body = self.expect_rel.to_le_bytes().to_vec();
        let mut has_payload = false;
        let put = |body: &mut Vec<u8>, packets: &mut Vec<Vec<u8>>, ch: u8, id: u16, data: &[u8], expect: u16| {
            if body.len() + 5 + data.len() > wire::MAX_BODY {
                packets.push(std::mem::replace(body, expect.to_le_bytes().to_vec()));
            }
            body.push(ch);
            body.extend_from_slice(&id.to_le_bytes());
            body.extend_from_slice(&(data.len() as u16).to_le_bytes());
            body.extend_from_slice(data);
        };
        let expect = self.expect_rel;
        for (id, data, last) in self.unacked.iter_mut() {
            if now - *last >= cfg.resend_interval {
                *last = now;
                put(&mut body, &mut packets, 0, *id, data, expect);
                has_payload = true;
            }
        }
        for (id, data) in self.unrel_out.drain(..) {
            put(&mut body, &mut packets, 1, id, &data, expect);
            has_payload = true;
        }
        if has_payload || self.need_ack || now - self.last_send >= cfg.keepalive {
            packets.push(body);
            self.last_send = now;
            self.need_ack = false;
        }
        packets
    }
}

fn data_packet(session: u64, body: &[u8]) -> Vec<u8> {
    let mut p = wire::header(wire::DATA, session);
    p.extend_from_slice(body);
    p
}

struct Conn {
    addr: SocketAddr,
    link: Link,
    /// `Some(t)`: timed out at `t`, resumable until the grace period ends.
    lingering: Option<f64>,
}

/// Authoritative endpoint: accepts clients and talks to many connections on one socket.
pub struct Server<S: Datagram> {
    socket: S,
    cfg: NetConfig,
    conns: HashMap<u64, Conn>,
    by_addr: HashMap<SocketAddr, u64>,
    next_session: u64,
    events: Vec<NetEvent>,
}

impl<S: Datagram> Server<S> {
    pub fn new(socket: S, cfg: NetConfig) -> Self {
        // Not a secret: handshake authentication and cookies are a separate layer.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Self { socket, cfg, conns: HashMap::new(), by_addr: HashMap::new(), next_session: seed | 1, events: Vec::new() }
    }

    pub fn socket_mut(&mut self) -> &mut S {
        &mut self.socket
    }

    pub fn connection_count(&self) -> usize {
        self.conns.values().filter(|c| c.lingering.is_none()).count()
    }

    pub fn send(&mut self, id: ConnectionId, channel: Channel, data: &[u8]) -> bool {
        match self.conns.get_mut(&id.0) {
            Some(c) if c.lingering.is_none() => c.link.send(channel, data),
            _ => false,
        }
    }

    pub fn disconnect(&mut self, id: ConnectionId) {
        if let Some(c) = self.conns.remove(&id.0) {
            self.by_addr.remove(&c.addr);
            self.socket.send_to(c.addr, &wire::header(wire::DISCONNECT, id.0));
        }
    }

    /// Pumps the socket and timers; returns the events produced since the last call.
    pub fn update(&mut self, now: f64) -> Vec<NetEvent> {
        while let Some((from, data)) = self.socket.recv_from() {
            let Some((kind, session, body)) = wire::parse_header(&data) else { continue };
            match kind {
                wire::CONNECT => self.on_connect(from, body, now),
                wire::DISCONNECT => {
                    if self.conns.get(&session).is_some_and(|c| c.addr == from) {
                        let c = self.conns.remove(&session).unwrap();
                        self.by_addr.remove(&c.addr);
                        self.events.push(NetEvent::Disconnected { id: ConnectionId(session), timed_out: false });
                    }
                }
                wire::DATA => {
                    // The sender address must match the session: other sources are ignored.
                    if let Some(c) = self.conns.get_mut(&session).filter(|c| c.addr == from && c.lingering.is_none()) {
                        for (channel, data) in c.link.receive(body, now) {
                            self.events.push(NetEvent::Message { id: ConnectionId(session), channel, data });
                        }
                    }
                }
                _ => {}
            }
        }
        let cfg = self.cfg;
        let mut dead = Vec::new();
        for (&session, c) in self.conns.iter_mut() {
            match c.lingering {
                Some(t) => {
                    if now - t >= cfg.reconnect_grace {
                        dead.push((session, false));
                    }
                }
                None => {
                    if now - c.link.last_recv >= cfg.timeout {
                        c.lingering = Some(now);
                        self.events.push(NetEvent::Disconnected { id: ConnectionId(session), timed_out: true });
                    } else {
                        for body in c.link.flush(now, &cfg) {
                            self.socket.send_to(c.addr, &data_packet(session, &body));
                        }
                    }
                }
            }
        }
        for (session, _) in dead {
            if let Some(c) = self.conns.remove(&session) {
                self.by_addr.remove(&c.addr);
            }
        }
        std::mem::take(&mut self.events)
    }

    fn on_connect(&mut self, from: SocketAddr, body: &[u8], now: f64) {
        // Retransmitted Connect from a known address: repeat the Accept.
        if let Some(&s) = self.by_addr.get(&from) {
            if self.conns.get(&s).is_some_and(|c| c.lingering.is_none()) {
                self.accept(from, s);
                return;
            }
        }
        let resume = body.get(..8).map(|b| u64::from_le_bytes(b.try_into().unwrap()));
        if let Some(s) = resume.filter(|s| self.conns.contains_key(s)) {
            let c = self.conns.get_mut(&s).unwrap();
            let old = c.addr;
            c.addr = from;
            c.link.last_recv = now;
            c.lingering = None;
            self.by_addr.remove(&old);
            self.by_addr.insert(from, s);
            self.accept(from, s);
            self.events.push(NetEvent::Reconnected(ConnectionId(s)));
            return;
        }
        let s = self.next_session;
        self.next_session = self.next_session.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) | 1;
        self.conns.insert(s, Conn { addr: from, link: Link::new(now), lingering: None });
        self.by_addr.insert(from, s);
        self.accept(from, s);
        self.events.push(NetEvent::Connected(ConnectionId(s)));
    }

    fn accept(&mut self, to: SocketAddr, session: u64) {
        self.socket.send_to(to, &wire::header(wire::ACCEPT, session));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientState {
    Disconnected,
    Connecting,
    Connected,
}

pub struct Client<S: Datagram> {
    socket: S,
    cfg: NetConfig,
    server: SocketAddr,
    state: ClientState,
    session: Option<u64>,
    resuming: bool,
    link: Link,
    attempt_started: f64,
    last_connect_sent: f64,
}

impl<S: Datagram> Client<S> {
    pub fn new(socket: S, server: SocketAddr, cfg: NetConfig) -> Self {
        Self {
            socket,
            cfg,
            server,
            state: ClientState::Disconnected,
            session: None,
            resuming: false,
            link: Link::new(0.0),
            attempt_started: 0.0,
            last_connect_sent: f64::NEG_INFINITY,
        }
    }

    pub fn state(&self) -> ClientState {
        self.state
    }

    pub fn id(&self) -> Option<ConnectionId> {
        self.session.map(ConnectionId)
    }

    pub fn socket_mut(&mut self) -> &mut S {
        &mut self.socket
    }

    /// Starts a fresh connection.
    pub fn connect(&mut self, now: f64) {
        self.session = None;
        self.resuming = false;
        self.begin(now);
    }

    /// Resumes the previous session (after a timeout or a socket change). Falls back to a
    /// fresh session if the server already forgot it.
    pub fn reconnect(&mut self, now: f64) {
        self.resuming = self.session.is_some();
        self.begin(now);
    }

    fn begin(&mut self, now: f64) {
        self.state = ClientState::Connecting;
        self.attempt_started = now;
        self.last_connect_sent = f64::NEG_INFINITY;
    }

    pub fn disconnect(&mut self) {
        if let (ClientState::Connected, Some(s)) = (self.state, self.session) {
            self.socket.send_to(self.server, &wire::header(wire::DISCONNECT, s));
        }
        self.state = ClientState::Disconnected;
        self.session = None;
    }

    pub fn send(&mut self, channel: Channel, data: &[u8]) -> bool {
        self.state == ClientState::Connected && self.link.send(channel, data)
    }

    pub fn update(&mut self, now: f64) -> Vec<NetEvent> {
        let mut events = Vec::new();
        while let Some((from, data)) = self.socket.recv_from() {
            if from != self.server {
                continue;
            }
            let Some((kind, session, body)) = wire::parse_header(&data) else { continue };
            match (kind, self.state) {
                (wire::ACCEPT, ClientState::Connecting) => {
                    let resumed = self.resuming && self.session == Some(session);
                    self.session = Some(session);
                    self.state = ClientState::Connected;
                    // A resumed session keeps its reliability counters on both sides.
                    if !resumed {
                        self.link = Link::new(now);
                    }
                    self.link.last_recv = now;
                    let id = ConnectionId(session);
                    events.push(if resumed { NetEvent::Reconnected(id) } else { NetEvent::Connected(id) });
                }
                (wire::DATA, ClientState::Connected) if self.session == Some(session) => {
                    for (channel, data) in self.link.receive(body, now) {
                        events.push(NetEvent::Message { id: ConnectionId(session), channel, data });
                    }
                }
                (wire::DISCONNECT, ClientState::Connected) if self.session == Some(session) => {
                    self.state = ClientState::Disconnected;
                    events.push(NetEvent::Disconnected { id: ConnectionId(session), timed_out: false });
                    self.session = None;
                }
                _ => {}
            }
        }
        match self.state {
            ClientState::Connecting => {
                if now - self.attempt_started >= self.cfg.timeout {
                    self.state = ClientState::Disconnected;
                    events.push(NetEvent::ConnectFailed);
                } else if now - self.last_connect_sent >= self.cfg.resend_interval.max(0.05) * 2.5 {
                    self.last_connect_sent = now;
                    let mut p = wire::header(wire::CONNECT, 0);
                    if let (true, Some(s)) = (self.resuming, self.session) {
                        p.extend_from_slice(&s.to_le_bytes());
                    }
                    self.socket.send_to(self.server, &p);
                }
            }
            ClientState::Connected => {
                let s = self.session.unwrap();
                if now - self.link.last_recv >= self.cfg.timeout {
                    // The session id is kept so `reconnect` can resume it.
                    self.state = ClientState::Disconnected;
                    events.push(NetEvent::Disconnected { id: ConnectionId(s), timed_out: true });
                } else {
                    for body in self.link.flush(now, &self.cfg) {
                        self.socket.send_to(self.server, &data_packet(s, &body));
                    }
                }
            }
            ClientState::Disconnected => {}
        }
        events
    }
}

#[cfg(test)]
mod tests;

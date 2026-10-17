use super::*;
use crate::{Emulated, EmulatorConfig, MemNet, MemSocket};

fn addr(p: u16) -> SocketAddr {
    format!("127.0.0.1:{p}").parse().unwrap()
}

struct Rig {
    server: Server<Emulated<MemSocket>>,
    client: Client<Emulated<MemSocket>>,
    now: f64,
    sev: Vec<NetEvent>,
    cev: Vec<NetEvent>,
}

impl Rig {
    fn new(cfg: EmulatorConfig, net_cfg: NetConfig) -> Self {
        let net = MemNet::default();
        let server = Server::new(Emulated::new(net.socket(addr(1)), cfg, 7), net_cfg);
        let client = Client::new(Emulated::new(net.socket(addr(2)), cfg, 9), addr(1), net_cfg);
        Rig { server, client, now: 0.0, sev: vec![], cev: vec![] }
    }
    fn run(&mut self, secs: f64) {
        let end = self.now + secs;
        while self.now < end {
            self.now += 0.01;
            self.server.socket_mut().advance(self.now);
            self.client.socket_mut().advance(self.now);
            self.sev.extend(self.server.update(self.now));
            self.cev.extend(self.client.update(self.now));
        }
    }
}

fn msgs(ev: &[NetEvent], ch: Channel) -> Vec<Vec<u8>> {
    ev.iter()
        .filter_map(|e| match e {
            NetEvent::Message { channel, data, .. } if *channel == ch => Some(data.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn connect_and_exchange() {
    let mut r = Rig::new(EmulatorConfig::default(), NetConfig::default());
    r.client.connect(0.0);
    r.run(0.5);
    assert_eq!(r.client.state(), ClientState::Connected);
    assert!(matches!(r.sev[0], NetEvent::Connected(_)));
    assert!(matches!(r.cev[0], NetEvent::Connected(_)));
    r.client.send(Channel::ReliableOrdered, b"hello");
    let id = r.client.id().unwrap();
    r.server.send(id, Channel::Unreliable, b"tick");
    r.run(0.5);
    assert_eq!(msgs(&r.sev, Channel::ReliableOrdered), vec![b"hello".to_vec()]);
    assert_eq!(msgs(&r.cev, Channel::Unreliable), vec![b"tick".to_vec()]);
}

#[test]
fn reliable_ordered_survives_loss_reorder_and_duplicates() {
    let cfg = EmulatorConfig { latency: 0.05, jitter: 0.08, loss: 0.3, reorder: 0.2, reorder_delay: 0.15, duplicate: 0.1 };
    let mut r = Rig::new(cfg, NetConfig { timeout: 30.0, ..Default::default() });
    r.client.connect(0.0);
    r.run(3.0);
    assert_eq!(r.client.state(), ClientState::Connected);
    for i in 0..200u32 {
        r.client.send(Channel::ReliableOrdered, &i.to_le_bytes());
        if i % 20 == 0 {
            r.run(0.05);
        }
    }
    r.run(20.0);
    let got: Vec<u32> = msgs(&r.sev, Channel::ReliableOrdered).iter().map(|d| u32::from_le_bytes(d[..4].try_into().unwrap())).collect();
    assert_eq!(got, (0..200).collect::<Vec<_>>());
}

#[test]
fn unreliable_drops_stale_messages() {
    let cfg = EmulatorConfig { latency: 0.02, jitter: 0.2, ..Default::default() };
    let mut r = Rig::new(cfg, NetConfig::default());
    r.client.connect(0.0);
    r.run(1.0);
    for i in 0..50u8 {
        r.client.send(Channel::Unreliable, &[i]);
        r.run(0.02);
    }
    r.run(2.0);
    let got = msgs(&r.sev, Channel::Unreliable);
    assert!(!got.is_empty());
    assert!(got.windows(2).all(|w| w[0][0] < w[1][0]), "stale message delivered: {got:?}");
}

#[test]
fn timeout_then_reconnect_resumes_session() {
    let net_cfg = NetConfig { timeout: 1.0, reconnect_grace: 10.0, ..Default::default() };
    let mut r = Rig::new(EmulatorConfig::default(), net_cfg);
    r.client.connect(0.0);
    r.run(0.5);
    let id = r.client.id().unwrap();
    // Client goes silent: swap in an unreachable state by simply not updating it.
    for _ in 0..200 {
        r.now += 0.01;
        r.sev.extend(r.server.update(r.now));
    }
    assert!(r.sev.contains(&NetEvent::Disconnected { id, timed_out: true }));
    r.client.reconnect(r.now);
    r.run(0.5);
    assert!(r.sev.contains(&NetEvent::Reconnected(id)));
    assert!(r.cev.contains(&NetEvent::Reconnected(id)));
    r.client.send(Channel::ReliableOrdered, b"back");
    r.run(0.5);
    assert_eq!(msgs(&r.sev, Channel::ReliableOrdered), vec![b"back".to_vec()]);
}

#[test]
fn explicit_disconnect_and_connect_failure() {
    let mut r = Rig::new(EmulatorConfig::default(), NetConfig::default());
    r.client.connect(0.0);
    r.run(0.5);
    let id = r.client.id().unwrap();
    r.client.disconnect();
    r.run(0.5);
    assert!(r.sev.contains(&NetEvent::Disconnected { id, timed_out: false }));
    assert_eq!(r.server.connection_count(), 0);

    // No server listening: the attempt times out.
    let net = MemNet::default();
    let mut c = Client::new(net.socket(addr(2)), addr(9), NetConfig { timeout: 1.0, ..Default::default() });
    c.connect(0.0);
    let mut ev = vec![];
    for i in 1..200 {
        ev.extend(c.update(i as f64 * 0.01));
    }
    assert_eq!(ev, vec![NetEvent::ConnectFailed]);
}

#[test]
fn ignores_foreign_sources_and_garbage() {
    let net = MemNet::default();
    let mut server = Server::new(net.socket(addr(1)), NetConfig::default());
    let mut intruder = net.socket(addr(3));
    intruder.send_to(addr(1), b"junk");
    let mut client = Client::new(net.socket(addr(2)), addr(1), NetConfig::default());
    client.connect(0.0);
    for i in 0..6 {
        let t = i as f64 * 0.01;
        client.update(t);
        server.update(t);
    }
    client.update(0.06);
    let id = client.id().unwrap();
    // Valid session id, wrong source address: dropped.
    let mut p = wire::header(wire::DATA, id.0);
    p.extend_from_slice(&0u16.to_le_bytes());
    p.extend_from_slice(&[0, 0, 0, 1, 0, b'x']);
    intruder.send_to(addr(1), &p);
    assert!(server.update(0.07).iter().all(|e| !matches!(e, NetEvent::Message { .. })));
}

#[test]
fn cookie_challenge_blocks_spoofed_connects() {
    let net = MemNet::default();
    let mut server = Server::new(net.socket(addr(1)), NetConfig::default());
    let mut spoofer = net.socket(addr(3));
    // A Connect with a made-up cookie gets only a small challenge and creates no state.
    let mut p = wire::header(wire::CONNECT, 0);
    p.extend_from_slice(&[0u8; 16]);
    spoofer.send_to(addr(1), &p);
    assert!(server.update(0.0).is_empty());
    assert_eq!(server.connection_count(), 0);
    let (_, reply) = spoofer.recv_from().unwrap();
    assert!(reply.len() < p.len());
    assert_eq!(wire::parse_header(&reply).unwrap().0, wire::CHALLENGE);
    assert!(spoofer.recv_from().is_none());
}

#[test]
fn authenticator_rejects_bad_tokens() {
    let mut r = Rig::new(EmulatorConfig::default(), NetConfig::default());
    r.server.set_authenticator(|t| t == b"let-me-in");
    r.client.set_auth_token(b"wrong");
    r.client.connect(0.0);
    r.run(1.0);
    assert_eq!(r.cev, vec![NetEvent::ConnectFailed]);
    assert_eq!(r.server.connection_count(), 0);
    r.client.set_auth_token(b"let-me-in");
    r.client.connect(r.now);
    r.run(1.0);
    assert_eq!(r.client.state(), ClientState::Connected);
}

#[test]
fn per_connection_rate_limit_drops_floods() {
    let net = MemNet::default();
    let cfg = NetConfig { max_packets_per_sec: 10.0, ..Default::default() };
    let mut server = Server::new(net.socket(addr(1)), cfg);
    let mut client = Client::new(net.socket(addr(2)), addr(1), cfg);
    client.connect(0.0);
    for i in 0..10 {
        client.update(i as f64 * 0.01);
        server.update(i as f64 * 0.01);
    }
    let id = client.id().unwrap();
    let mut flood = net.socket(addr(2)); // same address as the client
    for i in 0..100u16 {
        let mut p = wire::header(wire::DATA, id.0);
        p.extend_from_slice(&0u16.to_le_bytes());
        p.extend_from_slice(&[1, (i & 0xff) as u8, (i >> 8) as u8, 1, 0, 0]);
        flood.send_to(addr(1), &p);
    }
    let got = server.update(0.2).iter().filter(|e| matches!(e, NetEvent::Message { .. })).count();
    assert!(got <= 11, "{got} messages passed the limiter");
    assert!(got > 0);
}

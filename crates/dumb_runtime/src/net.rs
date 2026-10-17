//! Runtime side of multiplayer: the authoritative [`ServerNet`] and the replicating
//! [`ClientNet`], both driven once per simulation tick from the host loop (`dumb-server`,
//! `dumb-player --connect`). They sit between the world and `dumb_net`'s transport and
//! replication codec, and talk to scripts only through the typed events in `dumb_ecs::net`.
//!
//! Server tick: `pre_tick` (poll the socket, raise `NetConnected` / `ClientCommand` events) →
//! `simulate` → `post_tick` (snapshot the `Replicated` entities, send each connection a delta
//! for its area of interest, flush `NetSend` events, save the world now and then).
//!
//! Deltas travel on the reliable-ordered channel, so each one is computed against the previous
//! one sent to that connection and can never be lost or reordered; the transport's bandwidth
//! budget lives in the interest update (nearest entities first).

use dumb_core::{Entity, Vec3};
use dumb_ecs::net::{
    ClientCommand, CommandRejected, NetConnectFailed, NetConnected, NetDisconnected, NetMessage, NetReconcile, NetSend, NetSession,
    Replicated, ServerMessage,
};
use dumb_ecs::{SceneData, StreamFocus, Transform, World};
use dumb_net::{
    apply_delta, decode_delta, diff, encode_delta, encode_str, encode_value, load_snapshot, save_snapshot, ByteReader, Channel, Client,
    ClientState, CommandGate, ConnectionId, Datagram, EntityMap, Interest, JitterBuffer, NetConfig, NetEvent, NetId, Op, Reject, Server,
    Snapshot, SpatialGrid,
};
use dumb_reflect::{Typed, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;

const MSG_DELTA: u8 = 1;
const MSG_SERVER_MESSAGE: u8 = 2;
const MSG_COMMAND: u8 = 3;

/// Largest command payload accepted (bytes of wire encoding are bounded by the datagram; this
/// bounds the decoded tree).
const MAX_PAYLOAD_NODES: usize = 4096;

/// Snapshot of one entity: component type name → reflected value.
fn capture(world: &World, e: Entity) -> BTreeMap<String, Value> {
    let only: Vec<String> = world.get::<Replicated>(e).map(|r| r.only.clone()).unwrap_or_default();
    let mut out = BTreeMap::new();
    for id in world.components_of(e) {
        let Some(desc) = world.descriptor(id) else { continue };
        let keep = only.is_empty() || desc.name == Replicated::TYPE_NAME || only.contains(&desc.name);
        if keep {
            if let Some(r) = world.get_reflect(e, id) {
                out.insert(desc.name.clone(), dumb_reflect::to_value(r));
            }
        }
    }
    if only.is_empty() {
        for (name, v) in world.missing_components(e) {
            out.insert(name.clone(), v.clone());
        }
    }
    out
}

fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    match v {
        Value::Struct(f) => f.iter().find(|(n, _)| n == name).map(|(_, v)| v),
        _ => None,
    }
}

fn owner_of(comps: &BTreeMap<String, Value>) -> u64 {
    match comps.get(Replicated::TYPE_NAME).and_then(|r| field(r, "owner")) {
        Some(Value::Int(i)) => *i as u64,
        _ => 0,
    }
}

/// A payload is sane when its floats are finite and it is not absurdly large.
fn value_is_sane(v: &Value, budget: &mut usize) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    match v {
        Value::Float(f) => f.is_finite(),
        Value::Vec(f) => f.iter().all(|x| x.is_finite()),
        Value::List(l) => l.iter().all(|x| value_is_sane(x, budget)),
        Value::Struct(f) => f.iter().all(|(_, x)| value_is_sane(x, budget)),
        _ => true,
    }
}

fn encode_message(tag: u8, header: &[u8], m: &NetMessage) -> Vec<u8> {
    let mut b = vec![tag];
    b.extend_from_slice(header);
    encode_str(&mut b, &m.name);
    b.extend_from_slice(&m.entity.to_le_bytes());
    encode_value(&mut b, &m.payload);
    b
}

fn decode_message(r: &mut ByteReader) -> Option<NetMessage> {
    Some(NetMessage { name: r.str()?, entity: r.u64()?, payload: r.value()? })
}

/// Server settings.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Simulation rate, sent to clients so they can place snapshots in time.
    pub hz: u32,
    /// Entities farther than this from a connection's viewer are not sent to it (meters).
    pub interest_radius: f32,
    /// Leave radius = `interest_radius * hysteresis`.
    pub hysteresis: f32,
    /// Estimated bytes per connection per tick for entity updates (spawns always go out).
    pub bandwidth_per_tick: usize,
    pub grid_cell: f32,
    /// Commands per connection per tick before the rest are rejected.
    pub max_commands_per_tick: u32,
    /// World save file, and how often to write it (ticks; 0 = never).
    pub save_path: Option<PathBuf>,
    pub save_every: u64,
    pub net: NetConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            hz: 30,
            interest_radius: 150.0,
            hysteresis: 1.25,
            bandwidth_per_tick: 1200,
            grid_cell: 50.0,
            max_commands_per_tick: 8,
            save_path: None,
            save_every: 30 * 60,
            net: NetConfig::default(),
        }
    }
}

struct Conn {
    interest: Interest<NetId>,
    /// What this connection has been sent so far (the baseline of the next delta).
    sent: Snapshot,
    last_tick: u32,
    viewer: Vec3,
    /// Timed out and waiting for a reconnect: nothing is sent and it doesn't count as a viewer.
    lingering: bool,
}

/// The authoritative side.
pub struct ServerNet<S: Datagram> {
    server: Server<S>,
    pub cfg: ServerConfig,
    conns: HashMap<ConnectionId, Conn>,
    /// All replicated entities by position. Also usable for gameplay queries by hosts.
    pub grid: SpatialGrid<NetId>,
    gate: CommandGate,
    owned: HashMap<NetId, ConnectionId>,
    members: HashSet<NetId>,
    /// Events from the flush at the end of the last tick, handled at the start of the next.
    pending: Vec<NetEvent>,
    now: f64,
    validator: Option<Box<dyn Fn(&ClientCommand) -> bool>>,
    tick: u32,
}

impl<S: Datagram> ServerNet<S> {
    pub fn new(socket: S, cfg: ServerConfig, world: &mut World) -> Self {
        let mut session = world.resource::<NetSession>().cloned().unwrap_or_default();
        session.server = true;
        world.insert_resource(session);
        ServerNet {
            server: Server::new(socket, cfg.net),
            gate: CommandGate::new(cfg.max_commands_per_tick),
            grid: SpatialGrid::new(cfg.grid_cell),
            cfg,
            conns: HashMap::new(),
            owned: HashMap::new(),
            members: HashSet::new(),
            pending: Vec::new(),
            now: 0.0,
            validator: None,
            tick: 0,
        }
    }

    /// Credential check run on every connection request.
    pub fn set_authenticator(&mut self, f: impl Fn(&[u8]) -> bool + 'static) {
        self.server.set_authenticator(f);
    }

    /// Game-specific command check (ranges, cooldowns), run after the generic gate.
    pub fn set_validator(&mut self, f: impl Fn(&ClientCommand) -> bool + 'static) {
        self.validator = Some(Box::new(f));
    }

    pub fn connection_count(&self) -> usize {
        self.conns.len()
    }

    pub fn tick(&self) -> u32 {
        self.tick
    }

    pub fn socket_mut(&mut self) -> &mut S {
        self.server.socket_mut()
    }

    /// Replaces the world's replicated entities with the save at `cfg.save_path`, if any.
    /// Ownership is cleared (connection ids don't survive a restart). Returns the entities
    /// restored.
    pub fn restore(&mut self, world: &mut World) -> usize {
        let Some(path) = self.cfg.save_path.clone().filter(|p| p.exists()) else { return 0 };
        let (tick, snap) = match load_snapshot(&path) {
            Ok(s) => s,
            Err(e) => {
                log::error!("world save {} is unreadable ({e}); starting from the scene", path.display());
                return 0;
            }
        };
        let old: Vec<Entity> = world.query_ref::<(Entity, &Replicated)>().map(|(e, _)| e).collect();
        for e in old {
            if world.is_alive(e) {
                world.despawn(e);
            }
        }
        let entities = snap
            .into_iter()
            .map(|(id, comps)| {
                let mut components: Vec<(String, Value)> = comps.into_iter().collect();
                for (name, v) in &mut components {
                    if name == Replicated::TYPE_NAME {
                        if let Value::Struct(f) = v {
                            f.iter_mut().filter(|(n, _)| n == "owner").for_each(|(_, v)| *v = Value::Int(0));
                        }
                    }
                }
                dumb_ecs::EntityData { id, components }
            })
            .collect::<Vec<_>>();
        let n = entities.len();
        SceneData { entities }.instantiate(world);
        self.tick = tick;
        log::info!("restored {n} entities from {} (tick {tick})", path.display());
        n
    }

    /// Start of a tick: poll the network and raise events.
    pub fn pre_tick(&mut self, world: &mut World, now: f64) {
        self.gate.new_tick();
        self.now = now;
        let mut events = std::mem::take(&mut self.pending);
        events.extend(self.server.update(now));
        for ev in events {
            match ev {
                NetEvent::Connected(id) | NetEvent::Reconnected(id) => {
                    let reconnected = matches!(ev, NetEvent::Reconnected(_));
                    let c = self.conns.entry(id).or_insert_with(|| Conn { interest: Interest::new(), sent: Snapshot::new(), last_tick: 0, viewer: Vec3::ZERO, lingering: false });
                    c.lingering = false;
                    world.send_event(NetConnected { connection: id.0, reconnected });
                }
                NetEvent::Disconnected { id, timed_out } => {
                    // A timed-out session can still resume within the grace period: keep its
                    // state, drop it only on an explicit disconnect.
                    if timed_out {
                        if let Some(c) = self.conns.get_mut(&id) {
                            c.lingering = true;
                        }
                    } else {
                        self.conns.remove(&id);
                        self.gate.drop_connection(id);
                    }
                    world.send_event(NetDisconnected { connection: id.0, timed_out });
                }
                NetEvent::Message { id, data, .. } => self.on_message(world, id, &data),
                NetEvent::ConnectFailed => {}
            }
        }
    }

    fn on_message(&mut self, world: &mut World, from: ConnectionId, data: &[u8]) {
        let mut r = ByteReader::new(data);
        if r.u8() != Some(MSG_COMMAND) {
            return;
        }
        let (Some(seq), Some(msg)) = (r.u32(), decode_message(&mut r)) else {
            world.send_event(CommandRejected { from: from.0, name: String::new(), reason: "malformed".into() });
            return;
        };
        let cmd = ClientCommand { from: from.0, seq, message: msg };
        let sane = |c: &ClientCommand| value_is_sane(&c.message.payload, &mut MAX_PAYLOAD_NODES.clone());
        let res = self.gate.check(from, cmd.message.entity, seq, || sane(&cmd) && self.validator.as_ref().is_none_or(|v| v(&cmd)));
        match res {
            Ok(()) => world.send_event(cmd),
            Err(why) => {
                let reason = match why {
                    Reject::NotOwner => "not owner",
                    Reject::RateLimited => "rate limited",
                    Reject::Invalid => "invalid",
                    Reject::Stale => "stale",
                };
                world.send_event(CommandRejected { from: from.0, name: cmd.message.name, reason: reason.into() });
            }
        }
    }

    /// End of a tick: replicate, flush outgoing messages, save.
    pub fn post_tick(&mut self, world: &mut World) {
        self.tick = self.tick.wrapping_add(1);
        if let Some(s) = world.resource_mut::<NetSession>() {
            s.tick = self.tick;
        }

        // Current state of every replicated entity; positions feed the spatial grid.
        let ents: Vec<Entity> = world.query_ref::<(Entity, &Replicated)>().map(|(e, _)| e).collect();
        let mut current = Snapshot::new();
        let mut live: HashSet<NetId> = HashSet::new();
        let mut owners: HashMap<NetId, ConnectionId> = HashMap::new();
        let mut viewers: HashMap<ConnectionId, Vec3> = HashMap::new();
        for e in &ents {
            let id = e.to_bits();
            let comps = capture(world, *e);
            let pos = world.global_matrix(*e).w_axis.truncate();
            self.grid.insert(id, pos);
            live.insert(id);
            let owner = owner_of(&comps);
            if owner != 0 {
                owners.insert(id, ConnectionId(owner));
                viewers.entry(ConnectionId(owner)).or_insert(pos);
            }
            current.insert(id, comps);
        }
        let gone: Vec<NetId> = self.owned.keys().filter(|k| !owners.contains_key(k)).copied().collect();
        for k in gone {
            self.gate.clear_owner(k);
        }
        for (k, c) in &owners {
            self.gate.set_owner(*k, *c);
        }
        self.owned = owners;
        // Drop grid entries of despawned entities.
        for id in self.members.iter().copied().filter(|i| !live.contains(i)).collect::<Vec<_>>() {
            self.grid.remove(id);
        }
        self.members = live;

        // The streaming / AI focus follows the connected players.
        let mut points: Vec<Vec3> = Vec::new();
        for (id, c) in self.conns.iter_mut().filter(|(_, c)| !c.lingering) {
            if let Some(p) = viewers.get(id) {
                c.viewer = *p;
            }
            points.push(c.viewer);
        }
        if !points.is_empty() {
            world.insert_resource(StreamFocus { points });
        }

        // One delta per connection, against what it was last sent.
        let ids: Vec<ConnectionId> = self.conns.iter().filter(|(_, c)| !c.lingering).map(|(id, _)| *id).collect();
        for id in ids {
            let c = self.conns.get_mut(&id).unwrap();
            let cost = |k: NetId| 24 + current.get(&k).map_or(0, |m| m.len() * 40);
            let upd = c.interest.update(&self.grid, c.viewer, self.cfg.interest_radius, self.cfg.hysteresis, self.cfg.bandwidth_per_tick, cost);
            let fresh: HashSet<NetId> = upd.spawn.iter().chain(&upd.update).copied().collect();
            let mut target = Snapshot::new();
            for k in c.interest.known() {
                if let Some(comps) = if fresh.contains(k) { current.get(k) } else { c.sent.get(k).or_else(|| current.get(k)) } {
                    target.insert(*k, comps.clone());
                }
            }
            let delta = diff(&c.sent, &target, c.last_tick, self.tick);
            c.sent = target;
            c.last_tick = self.tick;
            let acked = self.gate.last_seq(id);
            if delta.ops.is_empty() && self.tick % self.cfg.hz.max(1) != 0 {
                continue; // nothing to say; a heartbeat delta goes out once a second
            }
            let mut b = vec![MSG_DELTA];
            b.extend_from_slice(&(self.cfg.hz as u16).to_le_bytes());
            b.extend_from_slice(&acked.to_le_bytes());
            b.extend_from_slice(&encode_delta(&delta));
            self.server.send(id, Channel::ReliableOrdered, &b);
        }

        // Messages scripts asked to send.
        let out: Vec<NetSend> = world.events::<NetSend>().cloned().collect();
        for m in out {
            let b = encode_message(MSG_SERVER_MESSAGE, &[], &m.message);
            let ch = if m.reliable { Channel::ReliableOrdered } else { Channel::Unreliable };
            let targets: Vec<ConnectionId> = if m.to == 0 { self.conns.iter().filter(|(_, c)| !c.lingering).map(|(id, _)| *id).collect() } else { vec![ConnectionId(m.to)] };
            for t in targets {
                self.server.send(t, ch, &b);
            }
        }

        if let Some(path) = self.cfg.save_path.clone() {
            if self.cfg.save_every > 0 && self.tick as u64 % self.cfg.save_every == 0 {
                self.save(&current, &path);
            }
        }
        // Send now rather than at the start of the next tick; replies are handled then.
        let events = self.server.update(self.now);
        self.pending.extend(events);
    }

    fn save(&self, state: &Snapshot, path: &std::path::Path) {
        if let Err(e) = save_snapshot(path, self.tick, state) {
            log::error!("world save failed: {e}");
        }
    }

    /// Writes the world now (also called on shutdown).
    pub fn save_now(&self, world: &World) {
        let Some(path) = &self.cfg.save_path else { return };
        let mut state = Snapshot::new();
        for (e, _) in world.query_ref::<(Entity, &Replicated)>() {
            state.insert(e.to_bits(), capture(world, e));
        }
        self.save(&state, path);
    }
}

/// The replicating side.
pub struct ClientNet<S: Datagram> {
    client: Client<S>,
    map: EntityMap,
    /// Newest state received, in server entity ids.
    latest: Snapshot,
    /// What has been applied to the world (interpolated), in server entity ids.
    applied: Snapshot,
    jitter: JitterBuffer,
    /// Server time minus local time, smoothed.
    offset: Option<f64>,
    hz: f64,
    next_seq: u32,
    /// Skip applying the owned entity's `Transform` from snapshots (the game predicts it and
    /// reacts to `NetReconcile`).
    pub predict_owned: bool,
    acked_seq: u32,
}

impl<S: Datagram> ClientNet<S> {
    /// `interp_delay`: seconds behind the newest snapshot the world is shown (100 ms is typical).
    pub fn new(socket: S, server: SocketAddr, cfg: NetConfig, interp_delay: f64, world: &mut World, now: f64) -> Self {
        let mut session = world.resource::<NetSession>().cloned().unwrap_or_default();
        session.client = true;
        world.insert_resource(session);
        let mut client = Client::new(socket, server, cfg);
        client.connect(now);
        ClientNet {
            client,
            map: EntityMap::default(),
            latest: Snapshot::new(),
            applied: Snapshot::new(),
            jitter: JitterBuffer::new(interp_delay),
            offset: None,
            hz: 30.0,
            next_seq: 1,
            predict_owned: true,
            acked_seq: 0,
        }
    }

    pub fn set_auth_token(&mut self, token: &[u8]) {
        self.client.set_auth_token(token);
    }

    pub fn state(&self) -> ClientState {
        self.client.state()
    }

    /// Highest command sequence the server reports having applied.
    pub fn acked_seq(&self) -> u32 {
        self.acked_seq
    }

    pub fn socket_mut(&mut self) -> &mut S {
        self.client.socket_mut()
    }

    /// Local entity for a server entity id, once it has been spawned here.
    pub fn local_entity(&self, server: NetId) -> Option<Entity> {
        self.map.local(server).map(Entity::from_bits)
    }

    /// Once per frame: receive, apply the interpolated state, send queued commands.
    pub fn update(&mut self, world: &mut World, now: f64) {
        // Queue this tick's commands first so the update below puts them on the wire.
        let me = world.resource::<NetSession>().map_or(0, |s| s.connection);
        let out: Vec<NetSend> = world.events::<NetSend>().cloned().collect();
        for m in out {
            let seq = if m.seq != 0 {
                self.next_seq = self.next_seq.max(m.seq.wrapping_add(1));
                m.seq
            } else {
                self.next_seq += 1;
                self.next_seq - 1
            };
            // Commands name the server's entity id for the local entity they are about.
            let mut msg = m.message;
            if let Some(server) = self.server_id(msg.entity) {
                msg.entity = server;
            }
            let b = encode_message(MSG_COMMAND, &seq.to_le_bytes(), &msg);
            let ch = if m.reliable { Channel::ReliableOrdered } else { Channel::Unreliable };
            if !self.client.send(ch, &b) {
                log::debug!("command `{}` dropped: not connected (connection {me})", msg.name);
            }
        }
        for ev in self.client.update(now) {
            match ev {
                NetEvent::Connected(id) | NetEvent::Reconnected(id) => {
                    if let Some(s) = world.resource_mut::<NetSession>() {
                        s.connection = id.0;
                    }
                    world.send_event(NetConnected { connection: id.0, reconnected: matches!(ev, NetEvent::Reconnected(_)) });
                }
                NetEvent::Disconnected { timed_out, .. } => {
                    world.send_event(NetDisconnected { connection: 0, timed_out });
                    // Try to resume the same session; the server falls back to a fresh one.
                    if timed_out {
                        self.client.reconnect(now);
                    }
                }
                NetEvent::ConnectFailed => world.send_event(NetConnectFailed),
                NetEvent::Message { data, .. } => self.on_message(world, now, &data),
            }
        }
        self.apply_interpolated(world, now);
    }

    fn server_id(&self, local: u64) -> Option<NetId> {
        self.latest.keys().copied().find(|s| self.map.local(*s) == Some(local))
    }

    fn on_message(&mut self, world: &mut World, now: f64, data: &[u8]) {
        let mut r = ByteReader::new(data);
        match r.u8() {
            Some(MSG_DELTA) => {
                let (Some(hz), Some(acked)) = (r.u16(), r.u32()) else { return };
                let Some(delta) = decode_delta(r.rest()) else { return };
                self.hz = hz.max(1) as f64;
                self.acked_seq = acked;
                apply_delta(&mut self.latest, &delta);
                let server_time = delta.tick as f64 / self.hz;
                let cand = server_time - now;
                self.offset = Some(match self.offset {
                    None => cand,
                    Some(o) => o + (cand - o) * 0.1,
                });
                self.jitter.push(server_time, self.latest.clone());
                if let Some(s) = world.resource_mut::<NetSession>() {
                    s.tick = delta.tick;
                }
                // The owned entity's authoritative state, for reconciliation.
                let me = world.resource::<NetSession>().map_or(0, |s| s.connection);
                for (sid, comps) in &self.latest {
                    if me != 0 && owner_of(comps) == me {
                        if let (Some(t), Some(local)) = (comps.get(Transform::TYPE_NAME), self.map.local(*sid)) {
                            world.send_event(NetReconcile { entity: local, acked_seq: acked, tick: delta.tick, transform: t.clone() });
                        }
                    }
                }
            }
            Some(MSG_SERVER_MESSAGE) => {
                if let Some(mut m) = decode_message(&mut r) {
                    if let Some(l) = self.map.local(m.entity) {
                        m.entity = l;
                    }
                    world.send_event(ServerMessage { message: m });
                }
            }
            _ => {}
        }
    }

    fn apply_interpolated(&mut self, world: &mut World, now: f64) {
        let Some(offset) = self.offset else { return };
        let Some(target) = self.jitter.sample(now + offset) else { return };
        let delta = diff(&self.applied, &target, 0, 0);
        if delta.ops.is_empty() {
            return;
        }
        // Local entities of despawned ids must be read before the map forgets them.
        let despawned: Vec<u64> = delta.ops.iter().filter_map(|o| if let Op::Despawn { entity } = o { self.map.local(*entity) } else { None }).collect();
        let local = {
            let mut spawn = || world.spawn().to_bits();
            self.map.localize(&delta, &mut spawn)
        };
        let me = world.resource::<NetSession>().map_or(0, |s| s.connection);
        let predicted: HashSet<u64> = if self.predict_owned && me != 0 {
            target.iter().filter(|(_, c)| owner_of(c) == me).filter_map(|(s, _)| self.map.local(*s)).collect()
        } else {
            HashSet::new()
        };
        for op in &local.ops {
            match op {
                Op::Set { entity, comp, value } => {
                    // The first Transform is the spawn position; after that the game predicts it.
                    if comp == Transform::TYPE_NAME && predicted.contains(entity) && world.has::<Transform>(Entity::from_bits(*entity)) {
                        continue;
                    }
                    world.insert_by_name(Entity::from_bits(*entity), comp, value);
                }
                Op::Patch { entity, comp, fields } => {
                    if comp == Transform::TYPE_NAME && predicted.contains(entity) {
                        continue;
                    }
                    let e = Entity::from_bits(*entity);
                    if let Some(id) = world.component_id(comp) {
                        if let Some(r) = world.get_reflect_mut(e, id) {
                            dumb_reflect::apply(r, &Value::Struct(fields.clone()));
                        }
                    }
                }
                Op::RemoveComp { entity, comp } => {
                    let e = Entity::from_bits(*entity);
                    match world.component_id(comp) {
                        Some(id) => {
                            world.remove_id(e, id);
                        }
                        None => world.remove_missing(e, comp),
                    }
                }
                Op::Despawn { .. } => {}
            }
        }
        for l in despawned {
            let e = Entity::from_bits(l);
            if world.is_alive(e) {
                world.despawn(e);
            }
        }
        self.applied = target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumb_net::MemNet;

    const DT: f64 = 1.0 / 30.0;

    struct Rig {
        sw: World,
        cw: World,
        server: ServerNet<dumb_net::MemSocket>,
        client: ClientNet<dumb_net::MemSocket>,
        t: f64,
    }

    impl Rig {
        fn new(cfg: ServerConfig) -> Rig {
            let net = MemNet::default();
            let saddr: SocketAddr = "10.0.0.1:7000".parse().unwrap();
            let caddr: SocketAddr = "10.0.0.2:7001".parse().unwrap();
            let mut sw = World::new();
            let mut cw = World::new();
            let server = ServerNet::new(net.socket(saddr), cfg, &mut sw);
            let client = ClientNet::new(net.socket(caddr), saddr, NetConfig::default(), 0.1, &mut cw, 0.0);
            Rig { sw, cw, server, client, t: 0.0 }
        }

        fn step(&mut self) {
            self.t += DT;
            self.server.pre_tick(&mut self.sw, self.t);
            self.sw.update_transforms();
            self.sw.update_events();
            self.server.post_tick(&mut self.sw);
            self.client.update(&mut self.cw, self.t);
            self.cw.update_events();
        }

        fn run(&mut self, secs: f64) {
            for _ in 0..(secs / DT) as usize {
                self.step();
            }
        }

        fn replicated_at(&mut self, x: f32, owner: u64) -> Entity {
            let e = self.sw.spawn();
            self.sw.insert(e, Transform { translation: Vec3::new(x, 0.0, 0.0), ..Default::default() });
            self.sw.insert(e, Replicated { owner, only: vec![] });
            e
        }
    }

    fn client_x(r: &Rig, server_e: Entity) -> Option<f32> {
        let l = r.client.local_entity(server_e.to_bits())?;
        r.cw.get::<Transform>(l).map(|t| t.translation.x)
    }

    #[test]
    fn server_state_reaches_the_client_and_is_interpolated() {
        let mut r = Rig::new(ServerConfig::default());
        let near = r.replicated_at(5.0, 0);
        r.run(1.0);
        assert!(r.sw.is_server() && r.cw.is_client() && !r.cw.is_server());
        assert_eq!(client_x(&r, near), Some(5.0));
        // The server moves it; the client follows (interpolated, so a little later).
        r.sw.get_mut::<Transform>(near).unwrap().translation.x = 9.0;
        r.run(1.0);
        assert_eq!(client_x(&r, near), Some(9.0));
        r.sw.despawn(near);
        r.run(1.0);
        assert!(r.cw.query_ref::<&Replicated>().next().is_none());
    }

    #[test]
    fn only_nearby_entities_are_replicated() {
        let mut r = Rig::new(ServerConfig { interest_radius: 50.0, ..Default::default() });
        let near = r.replicated_at(5.0, 0);
        let far = r.replicated_at(500.0, 0);
        r.run(1.0);
        assert_eq!(client_x(&r, near), Some(5.0));
        assert_eq!(client_x(&r, far), None);
        r.sw.get_mut::<Transform>(far).unwrap().translation.x = 20.0;
        r.run(1.0);
        assert_eq!(client_x(&r, far), Some(20.0));
    }

    #[test]
    fn events_commands_and_ownership() {
        let mut r = Rig::new(ServerConfig::default());
        r.run(0.5);
        let conn = r.cw.resource::<NetSession>().unwrap().connection;
        assert_ne!(conn, 0);
        let mine = r.replicated_at(0.0, conn);
        let other = r.replicated_at(1.0, 12345);
        r.run(0.5);
        let sent = |r: &mut Rig, e: Entity, seq: u32, v: f32| {
            let l = r.client.local_entity(e.to_bits()).unwrap();
            r.cw.send_event(NetSend { to: 0, message: NetMessage { name: "move".into(), entity: l.to_bits(), payload: Value::Vec(vec![v, 0.0, 0.0]) }, reliable: false, seq });
            r.cw.update_events();
            r.t += 0.001;
            r.client.update(&mut r.cw, r.t);
        };
        sent(&mut r, mine, 1, 1.0);
        sent(&mut r, other, 2, 1.0); // not owned
        sent(&mut r, mine, 3, f32::NAN); // hostile payload
        sent(&mut r, mine, 3, 1.0); // fine, but...
        r.server.pre_tick(&mut r.sw, r.t);
        r.sw.update_events();
        let ok: Vec<_> = r.sw.events::<ClientCommand>().map(|c| (c.seq, c.from)).collect();
        let bad: Vec<_> = r.sw.events::<CommandRejected>().map(|c| c.reason.clone()).collect();
        assert_eq!(ok, vec![(1, conn), (3, conn)]);
        assert_eq!(bad, vec!["not owner".to_string(), "invalid".to_string()]);
    }

    #[test]
    fn owned_entity_reconciles_instead_of_interpolating() {
        let mut r = Rig::new(ServerConfig::default());
        r.run(0.5);
        let conn = r.cw.resource::<NetSession>().unwrap().connection;
        let mine = r.replicated_at(3.0, conn);
        r.run(0.5);
        let l = r.client.local_entity(mine.to_bits()).unwrap();
        // The client predicts its own position; snapshots must not overwrite it.
        r.cw.get_mut::<Transform>(l).unwrap().translation.x = 7.0;
        r.sw.get_mut::<Transform>(mine).unwrap().translation.x = 4.0;
        let mut got = None;
        for _ in 0..10 {
            r.step();
            got = got.or_else(|| r.cw.events::<NetReconcile>().last().cloned());
        }
        assert_eq!(r.cw.get::<Transform>(l).unwrap().translation.x, 7.0);
        let rec = got.expect("reconcile event");
        assert_eq!(rec.entity, l.to_bits());
    }

    #[test]
    fn world_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("dumb_rt_save_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = ServerConfig { save_path: Some(dir.join("world.sav")), save_every: 5, ..Default::default() };
        let mut r = Rig::new(cfg.clone());
        let e = r.replicated_at(42.0, 99);
        r.run(0.5);
        let _ = e;
        // New process: a fresh world loads the save; the stale owner is cleared.
        let mut w = World::new();
        let stale = w.spawn();
        w.insert(stale, Replicated::default());
        let mut s = ServerNet::new(MemNet::default().socket("10.0.0.1:1".parse().unwrap()), cfg, &mut w);
        assert_eq!(s.restore(&mut w), 1);
        let (t, rep) = w.query_ref::<(&Transform, &Replicated)>().next().map(|(t, r)| (t.translation.x, r.owner)).unwrap();
        assert_eq!((t, rep), (42.0, 0));
        assert!(s.tick() > 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

//! What scripts see of networking: the `Replicated` marker, the `NetSession` resource
//! (`is_server()` / `is_client()`), and the typed events the transport raises. The transport and
//! the replication itself live in `dumb_net` and `dumb_runtime::net`.

use crate::World;
use dumb_derive::{Component, Editor};
use dumb_reflect::{Typed, Value};

/// Marks an entity the server replicates to clients.
#[derive(Component, Editor, Clone, Debug, Default, PartialEq)]
pub struct Replicated {
    /// Connection id (from `NetConnected`) that owns this entity: its client may send commands
    /// for it and predicts it locally. 0 = server-owned.
    pub owner: u64,
    /// Component type names to replicate. Empty = every component.
    pub only: Vec<String>,
}

/// Resource describing this process's networking role. Without one the game is standalone.
#[derive(Component, Editor, Clone, Debug, Default, PartialEq)]
pub struct NetSession {
    pub server: bool,
    pub client: bool,
    /// Server: the tick being simulated. Client: the newest server tick received.
    pub tick: u32,
    /// Client: this client's connection id once connected (0 before).
    pub connection: u64,
}

impl World {
    /// True on the authoritative server. Gate gameplay logic with it.
    pub fn is_server(&self) -> bool {
        self.resource::<NetSession>().is_some_and(|s| s.server)
    }

    /// True on a client connected to (or connecting to) a server.
    pub fn is_client(&self) -> bool {
        self.resource::<NetSession>().is_some_and(|s| s.client)
    }
}

/// Server: a client connected (or came back within the grace period, `reconnected`).
#[derive(Clone, Debug, PartialEq)]
pub struct NetConnected {
    pub connection: u64,
    pub reconnected: bool,
}

/// Server: a client left or timed out. Client: the server connection dropped (connection 0).
#[derive(Clone, Debug, PartialEq)]
pub struct NetDisconnected {
    pub connection: u64,
    pub timed_out: bool,
}

/// Client: connecting failed.
#[derive(Clone, Debug, PartialEq)]
pub struct NetConnectFailed;

/// A typed message: `name` picks the handler, `payload` is any reflected value (see
/// [`NetMessage::new`] / [`NetMessage::decode`]).
#[derive(Clone, Debug, PartialEq)]
pub struct NetMessage {
    pub name: String,
    /// The entity the message is about (a command targets an entity its sender owns).
    /// Server→client: the local entity; client→server: the server's entity.
    pub entity: u64,
    pub payload: Value,
}

impl NetMessage {
    pub fn new<T: dumb_reflect::Reflect>(name: &str, entity: u64, payload: &T) -> Self {
        NetMessage { name: name.into(), entity, payload: dumb_reflect::to_value(payload) }
    }

    pub fn decode<T: Typed>(&self) -> T {
        let mut t = T::default();
        dumb_reflect::apply(&mut t, &self.payload);
        t
    }
}

/// Send a message: `world.send_event(NetSend { to: 0, message })`. On a client it goes to the
/// server (as a command); on the server it goes to connection `to`, or to everyone when 0.
#[derive(Clone, Debug, PartialEq)]
pub struct NetSend {
    pub to: u64,
    pub message: NetMessage,
    pub reliable: bool,
    /// Client commands: the sequence number (strictly increasing; the server drops stale ones).
    /// 0 = let the runtime number it. Use the `Predictor`'s number for predicted commands.
    pub seq: u32,
}

/// Server: a validated client command (ownership, rate and ordering already checked).
#[derive(Clone, Debug, PartialEq)]
pub struct ClientCommand {
    pub from: u64,
    pub seq: u32,
    pub message: NetMessage,
}

/// Server: a command the gate refused (for logging, strikes or kicking).
#[derive(Clone, Debug, PartialEq)]
pub struct CommandRejected {
    pub from: u64,
    pub name: String,
    pub reason: String,
}

/// Client: a message from the server.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerMessage {
    pub message: NetMessage,
}

/// Client: the server's authoritative state for an entity this client owns, with the last
/// command sequence it has applied. Feed it to a `dumb_net::Predictor::reconcile`.
#[derive(Clone, Debug, PartialEq)]
pub struct NetReconcile {
    pub entity: u64,
    pub acked_seq: u32,
    pub tick: u32,
    /// The owned entity's replicated `Transform` value.
    pub transform: Value,
}

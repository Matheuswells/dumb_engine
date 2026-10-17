//! UDP transport for the dedicated server: reliable-ordered and unreliable channels, sequence
//! numbers, connect / disconnect / reconnect events and a debug network emulator.
//!
//! Everything is driven by an explicit clock (`now`, seconds) and a [`Datagram`] socket
//! abstraction, so the same code runs over a real [`UdpSocket`](std::net::UdpSocket), over the
//! in-memory [`MemNet`] used by tests, and behind the [`Emulated`] wrapper (latency, jitter,
//! loss, reordering). Authentication and cookie challenges build on top of this layer.

mod emulator;
mod gameplay;
mod interest;
mod replication;
mod endpoint;
mod socket;
mod wire;

pub use emulator::{Emulated, EmulatorConfig};
pub use gameplay::{CommandGate, Predictor, Reject};
pub use interest::{Interest, InterestUpdate, SpatialGrid};
pub use endpoint::{Channel, Client, ClientState, ConnectionId, NetConfig, NetEvent, Server};
pub use replication::{encode_str, encode_value, ByteReader, load_snapshot, save_snapshot, decode_delta, encode_delta, apply_delta, diff, Delta, EntityMap, JitterBuffer, NetId, Op, Snapshot};
pub use socket::{bind_udp, Datagram, MemNet, MemSocket};

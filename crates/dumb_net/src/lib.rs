//! UDP transport for the dedicated server: reliable-ordered and unreliable channels, sequence
//! numbers, connect / disconnect / reconnect events and a debug network emulator.
//!
//! Everything is driven by an explicit clock (`now`, seconds) and a [`Datagram`] socket
//! abstraction, so the same code runs over a real [`UdpSocket`](std::net::UdpSocket), over the
//! in-memory [`MemNet`] used by tests, and behind the [`Emulated`] wrapper (latency, jitter,
//! loss, reordering). Authentication and cookie challenges build on top of this layer.

mod emulator;
mod endpoint;
mod socket;
mod wire;

pub use emulator::{Emulated, EmulatorConfig};
pub use endpoint::{Channel, Client, ClientState, ConnectionId, NetConfig, NetEvent, Server};
pub use socket::{bind_udp, Datagram, MemNet, MemSocket};

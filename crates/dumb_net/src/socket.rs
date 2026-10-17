use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::rc::Rc;

/// Minimal non-blocking datagram socket.
pub trait Datagram {
    fn send_to(&mut self, addr: SocketAddr, data: &[u8]);
    fn recv_from(&mut self) -> Option<(SocketAddr, Vec<u8>)>;
}

impl Datagram for std::net::UdpSocket {
    fn send_to(&mut self, addr: SocketAddr, data: &[u8]) {
        let _ = std::net::UdpSocket::send_to(self, data, addr);
    }
    fn recv_from(&mut self) -> Option<(SocketAddr, Vec<u8>)> {
        let mut buf = [0u8; 2048];
        match std::net::UdpSocket::recv_from(self, &mut buf) {
            Ok((n, from)) => Some((from, buf[..n].to_vec())),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => None,
            // ICMP "connection reset" surfaces as an error on some platforms; ignore it.
            Err(_) => None,
        }
    }
}

/// Binds a non-blocking UDP socket.
pub fn bind_udp(addr: &str) -> io::Result<std::net::UdpSocket> {
    let s = std::net::UdpSocket::bind(addr)?;
    s.set_nonblocking(true)?;
    Ok(s)
}

type Queues = HashMap<SocketAddr, VecDeque<(SocketAddr, Vec<u8>)>>;

/// In-memory network connecting [`MemSocket`]s by address (for tests and local loopback).
#[derive(Clone, Default)]
pub struct MemNet(Rc<RefCell<Queues>>);

impl MemNet {
    pub fn socket(&self, addr: SocketAddr) -> MemSocket {
        self.0.borrow_mut().entry(addr).or_default();
        MemSocket { net: self.clone(), addr }
    }
}

pub struct MemSocket {
    net: MemNet,
    addr: SocketAddr,
}

impl MemSocket {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Datagram for MemSocket {
    fn send_to(&mut self, addr: SocketAddr, data: &[u8]) {
        if let Some(q) = self.net.0.borrow_mut().get_mut(&addr) {
            q.push_back((self.addr, data.to_vec()));
        }
    }
    fn recv_from(&mut self) -> Option<(SocketAddr, Vec<u8>)> {
        self.net.0.borrow_mut().get_mut(&self.addr)?.pop_front()
    }
}

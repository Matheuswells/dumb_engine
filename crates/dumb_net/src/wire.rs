//! Datagram layout: `magic u16 | kind u8 | session u64 | body`.

pub const MAGIC: u16 = 0xD0B1;
pub const HEADER: usize = 11;
/// Payload budget per datagram (stays under a typical 1200-byte safe MTU).
pub const MAX_BODY: usize = 1100;

pub const CONNECT: u8 = 1;
pub const ACCEPT: u8 = 2;
pub const DISCONNECT: u8 = 3;
pub const DATA: u8 = 4;
/// Server → client: here is your address cookie, resend Connect with it.
pub const CHALLENGE: u8 = 5;
/// Server → client: authentication failed.
pub const REJECT: u8 = 6;

pub fn header(kind: u8, session: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(HEADER + 64);
    v.extend_from_slice(&MAGIC.to_le_bytes());
    v.push(kind);
    v.extend_from_slice(&session.to_le_bytes());
    v
}

pub fn parse_header(d: &[u8]) -> Option<(u8, u64, &[u8])> {
    if d.len() < HEADER || u16::from_le_bytes([d[0], d[1]]) != MAGIC {
        return None;
    }
    Some((d[2], u64::from_le_bytes(d[3..11].try_into().ok()?), &d[HEADER..]))
}

/// Cursor over a body.
pub struct Reader<'a>(pub &'a [u8]);

impl<'a> Reader<'a> {
    pub fn u8(&mut self) -> Option<u8> {
        let (b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(*b)
    }
    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `a` is newer than `b` in wrapping u16 sequence space.
pub fn newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

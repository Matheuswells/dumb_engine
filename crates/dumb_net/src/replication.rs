//! Delta snapshots of replicated entities. A snapshot is the reflected [`Value`] of every
//! component of every `Replicated` entity (the same path scenes use); the server diffs the
//! current snapshot against what a client last acknowledged and sends only the changes.

use dumb_core::AssetId;
use dumb_reflect::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};

/// Server-side entity id, stable across the wire.
pub type NetId = u64;

/// All replicated state: entity → component type name → value.
pub type Snapshot = BTreeMap<NetId, BTreeMap<String, Value>>;

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// New component, or one whose shape changed: the full value.
    Set { entity: NetId, comp: String, value: Value },
    /// Struct component with only the changed fields.
    Patch { entity: NetId, comp: String, fields: Vec<(String, Value)> },
    RemoveComp { entity: NetId, comp: String },
    Despawn { entity: NetId },
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Delta {
    /// Server tick this delta brings the receiver up to.
    pub tick: u32,
    /// Tick of the snapshot this delta was computed against (0 = from nothing).
    pub baseline: u32,
    pub ops: Vec<Op>,
}

fn diff_value(old: &Value, new: &Value) -> Option<Op2> {
    if old == new {
        return None;
    }
    match (old, new) {
        (Value::Struct(a), Value::Struct(b)) if a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.0 == y.0) => {
            let fields: Vec<_> = a.iter().zip(b).filter(|(x, y)| x.1 != y.1).map(|(_, y)| y.clone()).collect();
            Some(Op2::Patch(fields))
        }
        _ => Some(Op2::Set),
    }
}

enum Op2 {
    Set,
    Patch(Vec<(String, Value)>),
}

/// Changes needed to turn `old` into `new`.
pub fn diff(old: &Snapshot, new: &Snapshot, baseline: u32, tick: u32) -> Delta {
    let mut ops = Vec::new();
    for (&entity, comps) in new {
        let prev = old.get(&entity);
        for (comp, value) in comps {
            match prev.and_then(|p| p.get(comp)) {
                None => ops.push(Op::Set { entity, comp: comp.clone(), value: value.clone() }),
                Some(o) => match diff_value(o, value) {
                    None => {}
                    Some(Op2::Set) => ops.push(Op::Set { entity, comp: comp.clone(), value: value.clone() }),
                    Some(Op2::Patch(fields)) => ops.push(Op::Patch { entity, comp: comp.clone(), fields }),
                },
            }
        }
        if let Some(p) = prev {
            for comp in p.keys().filter(|c| !comps.contains_key(*c)) {
                ops.push(Op::RemoveComp { entity, comp: comp.clone() });
            }
        }
    }
    for &entity in old.keys().filter(|e| !new.contains_key(*e)) {
        ops.push(Op::Despawn { entity });
    }
    Delta { tick, baseline, ops }
}

/// Applies a delta in place. Patches against a missing component are ignored (stale baseline).
pub fn apply_delta(snap: &mut Snapshot, d: &Delta) {
    for op in &d.ops {
        match op {
            Op::Set { entity, comp, value } => {
                snap.entry(*entity).or_default().insert(comp.clone(), value.clone());
            }
            Op::Patch { entity, comp, fields } => {
                if let Some(Value::Struct(cur)) = snap.get_mut(entity).and_then(|c| c.get_mut(comp)) {
                    for (name, v) in fields {
                        if let Some(slot) = cur.iter_mut().find(|f| f.0 == *name) {
                            slot.1 = v.clone();
                        }
                    }
                }
            }
            Op::RemoveComp { entity, comp } => {
                if let Some(c) = snap.get_mut(entity) {
                    c.remove(comp);
                }
            }
            Op::Despawn { entity } => {
                snap.remove(entity);
            }
        }
    }
}

// ---- entity id mapping -------------------------------------------------------------------

/// Server → client entity id mapping. Unknown ids (including forward references inside
/// component values) get a local entity from `spawn` the first time they are seen.
#[derive(Default)]
pub struct EntityMap {
    map: HashMap<NetId, u64>,
}

impl EntityMap {
    pub fn local(&self, server: NetId) -> Option<u64> {
        self.map.get(&server).copied()
    }

    pub fn forget(&mut self, server: NetId) -> Option<u64> {
        self.map.remove(&server)
    }

    pub fn resolve(&mut self, server: NetId, spawn: &mut impl FnMut() -> u64) -> u64 {
        *self.map.entry(server).or_insert_with(spawn)
    }

    /// Rewrites every entity id in `d` (op targets and `Value::Entity` references) to local ids.
    pub fn localize(&mut self, d: &Delta, spawn: &mut impl FnMut() -> u64) -> Delta {
        let mut out = d.clone();
        for op in &mut out.ops {
            match op {
                Op::Set { entity, value, .. } => {
                    self.remap(value, spawn);
                    *entity = self.resolve(*entity, spawn);
                }
                Op::Patch { entity, fields, .. } => {
                    for (_, v) in fields.iter_mut() {
                        self.remap(v, spawn);
                    }
                    *entity = self.resolve(*entity, spawn);
                }
                Op::RemoveComp { entity, .. } => *entity = self.resolve(*entity, spawn),
                Op::Despawn { entity } => {
                    *entity = self.map.remove(entity).unwrap_or(*entity);
                }
            }
        }
        out
    }

    fn remap(&mut self, v: &mut Value, spawn: &mut impl FnMut() -> u64) {
        match v {
            Value::Entity(e) => *e = self.resolve(*e, spawn),
            Value::List(l) => l.iter_mut().for_each(|x| self.remap(x, spawn)),
            Value::Struct(f) => f.iter_mut().for_each(|(_, x)| self.remap(x, spawn)),
            _ => {}
        }
    }
}

// ---- wire encoding -----------------------------------------------------------------------

fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u16).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

fn put_value(b: &mut Vec<u8>, v: &Value) {
    match v {
        Value::None => b.push(0),
        Value::Bool(x) => b.extend_from_slice(&[1, *x as u8]),
        Value::Int(x) => {
            b.push(2);
            b.extend_from_slice(&x.to_le_bytes());
        }
        Value::Float(x) => {
            b.push(3);
            b.extend_from_slice(&x.to_le_bytes());
        }
        Value::String(s) => {
            b.push(4);
            put_str(b, s);
        }
        Value::Vec(f) => {
            b.push(5);
            b.push(f.len() as u8);
            f.iter().for_each(|x| b.extend_from_slice(&x.to_le_bytes()));
        }
        Value::Asset(a) => {
            b.push(6);
            b.extend_from_slice(a.0.as_bytes());
        }
        Value::Entity(e) => {
            b.push(7);
            b.extend_from_slice(&e.to_le_bytes());
        }
        Value::Enum(s) => {
            b.push(8);
            put_str(b, s);
        }
        Value::List(l) => {
            b.push(9);
            b.extend_from_slice(&(l.len() as u32).to_le_bytes());
            l.iter().for_each(|x| put_value(b, x));
        }
        Value::Struct(f) => {
            b.push(10);
            b.extend_from_slice(&(f.len() as u16).to_le_bytes());
            for (n, x) in f {
                put_str(b, n);
                put_value(b, x);
            }
        }
    }
}

struct Rd<'a>(&'a [u8]);

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    fn value(&mut self, depth: u32) -> Option<Value> {
        if depth > 32 {
            return None; // hostile nesting
        }
        Some(match self.u8()? {
            0 => Value::None,
            1 => Value::Bool(self.u8()? != 0),
            2 => Value::Int(self.u64()? as i64),
            3 => Value::Float(f64::from_bits(self.u64()?)),
            4 => Value::String(self.str()?),
            5 => {
                let n = self.u8()? as usize;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(f32::from_bits(self.u32()?));
                }
                Value::Vec(v)
            }
            6 => Value::Asset(AssetId(uuid::Uuid::from_slice(self.take(16)?).ok()?)),
            7 => Value::Entity(self.u64()?),
            8 => Value::Enum(self.str()?),
            9 => {
                let n = self.u32()? as usize;
                // Every element takes at least one byte: bound the allocation by the input.
                if n > self.0.len() {
                    return None;
                }
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.value(depth + 1)?);
                }
                Value::List(v)
            }
            10 => {
                let n = self.u16()? as usize;
                let mut v = Vec::with_capacity(n.min(self.0.len()));
                for _ in 0..n {
                    v.push((self.str()?, self.value(depth + 1)?));
                }
                Value::Struct(v)
            }
            _ => return None,
        })
    }
}

pub fn encode_delta(d: &Delta) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&d.tick.to_le_bytes());
    b.extend_from_slice(&d.baseline.to_le_bytes());
    b.extend_from_slice(&(d.ops.len() as u32).to_le_bytes());
    for op in &d.ops {
        match op {
            Op::Set { entity, comp, value } => {
                b.push(0);
                b.extend_from_slice(&entity.to_le_bytes());
                put_str(&mut b, comp);
                put_value(&mut b, value);
            }
            Op::Patch { entity, comp, fields } => {
                b.push(1);
                b.extend_from_slice(&entity.to_le_bytes());
                put_str(&mut b, comp);
                b.extend_from_slice(&(fields.len() as u16).to_le_bytes());
                for (n, v) in fields {
                    put_str(&mut b, n);
                    put_value(&mut b, v);
                }
            }
            Op::RemoveComp { entity, comp } => {
                b.push(2);
                b.extend_from_slice(&entity.to_le_bytes());
                put_str(&mut b, comp);
            }
            Op::Despawn { entity } => {
                b.push(3);
                b.extend_from_slice(&entity.to_le_bytes());
            }
        }
    }
    b
}

/// Decodes untrusted bytes; `None` on any malformed input.
pub fn decode_delta(bytes: &[u8]) -> Option<Delta> {
    let mut r = Rd(bytes);
    let tick = r.u32()?;
    let baseline = r.u32()?;
    let n = r.u32()? as usize;
    if n > bytes.len() {
        return None;
    }
    let mut ops = Vec::with_capacity(n);
    for _ in 0..n {
        ops.push(match r.u8()? {
            0 => Op::Set { entity: r.u64()?, comp: r.str()?, value: r.value(0)? },
            1 => {
                let entity = r.u64()?;
                let comp = r.str()?;
                let k = r.u16()? as usize;
                let mut fields = Vec::new();
                for _ in 0..k {
                    fields.push((r.str()?, r.value(0)?));
                }
                Op::Patch { entity, comp, fields }
            }
            2 => Op::RemoveComp { entity: r.u64()?, comp: r.str()? },
            3 => Op::Despawn { entity: r.u64()? },
            _ => return None,
        });
    }
    Some(Delta { tick, baseline, ops })
}

// ---- interpolation -----------------------------------------------------------------------

fn lerp_value(a: &Value, b: &Value, t: f64) -> Value {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => Value::Float(x + (y - x) * t),
        (Value::Vec(x), Value::Vec(y)) if x.len() == y.len() => {
            Value::Vec(x.iter().zip(y).map(|(p, q)| p + (q - p) * t as f32).collect())
        }
        (Value::Struct(x), Value::Struct(y)) if x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.0 == q.0) => {
            Value::Struct(x.iter().zip(y).map(|(p, q)| (p.0.clone(), lerp_value(&p.1, &q.1, t))).collect())
        }
        // Discrete values switch at the later snapshot.
        _ => if t >= 1.0 { b.clone() } else { a.clone() },
    }
}

/// Client-side jitter buffer: snapshots are rendered `delay` seconds in the past so there are
/// almost always two to interpolate between.
pub struct JitterBuffer {
    pub delay: f64,
    buf: VecDeque<(f64, Snapshot)>,
}

impl JitterBuffer {
    pub fn new(delay: f64) -> Self {
        Self { delay, buf: VecDeque::new() }
    }

    /// Records a snapshot stamped with server time `time` (out-of-order stamps are dropped).
    pub fn push(&mut self, time: f64, snap: Snapshot) {
        if self.buf.back().is_some_and(|(t, _)| *t >= time) {
            return;
        }
        self.buf.push_back((time, snap));
        while self.buf.len() > 64 {
            self.buf.pop_front();
        }
    }

    /// State at server time `now - delay`. Holds the newest snapshot if the buffer ran dry.
    pub fn sample(&mut self, now: f64) -> Option<Snapshot> {
        let t = now - self.delay;
        // Keep one snapshot at or before `t` as the interpolation start.
        while self.buf.len() > 2 && self.buf[1].0 <= t {
            self.buf.pop_front();
        }
        let (t0, a) = self.buf.front()?;
        let Some((t1, b)) = self.buf.get(1) else { return Some(a.clone()) };
        if t <= *t0 {
            return Some(a.clone());
        }
        let k = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
        let mut out = b.clone();
        for (e, comps) in &mut out {
            for (name, v) in comps.iter_mut() {
                if let Some(prev) = a.get(e).and_then(|c| c.get(name)) {
                    *v = lerp_value(prev, v, k);
                }
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: f32) -> Value {
        Value::Struct(vec![("pos".into(), Value::Vec(vec![x, 0.0, 0.0])), ("hp".into(), Value::Int(10))])
    }

    fn snap(x: f32, target: u64) -> Snapshot {
        let mut s = Snapshot::new();
        s.entry(5).or_default().insert("Body".into(), pos(x));
        s.entry(5).or_default().insert("Target".into(), Value::Struct(vec![("who".into(), Value::Entity(target))]));
        s.entry(9).or_default().insert("Body".into(), pos(1.0));
        s
    }

    #[test]
    fn diff_is_minimal_and_roundtrips() {
        let a = snap(0.0, 9);
        let b = snap(2.0, 9);
        let d = diff(&a, &b, 1, 2);
        assert_eq!(d.ops, vec![Op::Patch { entity: 5, comp: "Body".into(), fields: vec![("pos".into(), Value::Vec(vec![2.0, 0.0, 0.0]))] }]);
        let d2 = decode_delta(&encode_delta(&d)).unwrap();
        assert_eq!(d, d2);
        let mut c = a.clone();
        apply_delta(&mut c, &d2);
        assert_eq!(c, b);
        assert!(diff(&b, &b, 2, 3).ops.is_empty());
    }

    #[test]
    fn despawn_remove_and_spawn() {
        let a = snap(0.0, 9);
        let mut b = a.clone();
        b.remove(&9);
        b.get_mut(&5).unwrap().remove("Target");
        b.entry(7).or_default().insert("Body".into(), pos(3.0));
        let d = diff(&a, &b, 1, 2);
        let mut c = a.clone();
        apply_delta(&mut c, &decode_delta(&encode_delta(&d)).unwrap());
        assert_eq!(c, b);
        let from_nothing = diff(&Snapshot::new(), &b, 0, 2);
        let mut e = Snapshot::new();
        apply_delta(&mut e, &from_nothing);
        assert_eq!(e, b);
    }

    #[test]
    fn entity_ids_are_mapped_including_references() {
        let d = diff(&Snapshot::new(), &snap(0.0, 9), 0, 1);
        let mut next = 100;
        let mut spawn = || {
            next += 1;
            next
        };
        let mut map = EntityMap::default();
        let local = map.localize(&d, &mut spawn);
        let mut s = Snapshot::new();
        apply_delta(&mut s, &local);
        // Entity 5 → 101?  The Target reference to 9 was resolved first or second, but must
        // agree with entity 9's own mapping.
        let l5 = map.local(5).unwrap();
        let l9 = map.local(9).unwrap();
        assert_ne!(l5, l9);
        assert_eq!(s[&l5]["Target"], Value::Struct(vec![("who".into(), Value::Entity(l9))]));
        assert!(s.contains_key(&l9));
        let despawn = Delta { tick: 2, baseline: 1, ops: vec![Op::Despawn { entity: 9 }] };
        let l = map.localize(&despawn, &mut spawn);
        assert_eq!(l.ops, vec![Op::Despawn { entity: l9 }]);
        assert!(map.local(9).is_none());
    }

    #[test]
    fn decoder_rejects_garbage() {
        assert!(decode_delta(&[]).is_none());
        assert!(decode_delta(&[0; 11]).is_none() || decode_delta(&[0; 11]).unwrap().ops.is_empty());
        let mut huge = vec![0u8; 8];
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_delta(&huge).is_none());
        let good = encode_delta(&diff(&Snapshot::new(), &snap(0.0, 9), 0, 1));
        for cut in 0..good.len() {
            let _ = decode_delta(&good[..cut]); // must not panic
        }
    }

    #[test]
    fn jitter_buffer_interpolates() {
        let mut j = JitterBuffer::new(0.1);
        j.push(1.0, snap(0.0, 9));
        j.push(1.1, snap(10.0, 9));
        let s = j.sample(1.15).unwrap(); // renders t = 1.05
        let Value::Struct(f) = &s[&5]["Body"] else { panic!() };
        assert_eq!(f[0].1, Value::Vec(vec![5.0, 0.0, 0.0]));
        // Stale stamp ignored; dry buffer holds the newest.
        j.push(1.05, snap(99.0, 9));
        let s = j.sample(5.0).unwrap();
        let Value::Struct(f) = &s[&5]["Body"] else { panic!() };
        assert_eq!(f[0].1, Value::Vec(vec![10.0, 0.0, 0.0]));
    }
}

// ---- persistence -------------------------------------------------------------------------

/// Writes a snapshot atomically (temp file + rename) so a crash never leaves a torn save.
pub fn save_snapshot(path: &std::path::Path, tick: u32, snap: &Snapshot) -> std::io::Result<()> {
    let bytes = encode_delta(&diff(&Snapshot::new(), snap, 0, tick));
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Loads a save written by [`save_snapshot`]; returns the tick and the world state.
pub fn load_snapshot(path: &std::path::Path) -> std::io::Result<(u32, Snapshot)> {
    let bytes = std::fs::read(path)?;
    let d = decode_delta(&bytes).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "corrupt world save"))?;
    let mut s = Snapshot::new();
    apply_delta(&mut s, &d);
    Ok((d.tick, s))
}

#[cfg(test)]
mod save_tests {
    use super::*;

    #[test]
    fn save_and_restore_roundtrip() {
        let dir = std::env::temp_dir().join(format!("dumb_net_save_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("world.sav");
        let mut s = Snapshot::new();
        s.entry(1).or_default().insert("Body".into(), Value::Struct(vec![("hp".into(), Value::Int(3))]));
        save_snapshot(&path, 42, &s).unwrap();
        assert_eq!(load_snapshot(&path).unwrap(), (42, s));
        std::fs::write(&path, b"junk").unwrap();
        assert!(load_snapshot(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

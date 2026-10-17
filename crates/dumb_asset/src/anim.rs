//! Animation beyond plain clip playback: state machines and blend trees (the `.animgraph`
//! asset), animation events, root motion, and procedural animation (IK, look-at, spring bones).
//!
//! Everything here is pure CPU math over `Trs` poses / model-space positions so it can run in
//! the headless server too and is unit-tested without a GPU.

use crate::model::{ModelData, Trs};
use dumb_core::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------------------------
// Pose blending
// ---------------------------------------------------------------------------------------------

/// Blend `b` over `a` by `w` (0 = a, 1 = b), in place on `a`.
pub fn blend_poses(a: &mut [Trs], b: &[Trs], w: f32) {
    if w <= 0.0 {
        return;
    }
    let w = w.min(1.0);
    for (p, o) in a.iter_mut().zip(b) {
        p.translation = p.translation.lerp(o.translation, w);
        let mut q = o.rotation;
        if p.rotation.dot(q) < 0.0 {
            q = -q;
        }
        p.rotation = p.rotation.slerp(q, w).normalize();
        p.scale = p.scale.lerp(o.scale, w);
    }
}

// ---------------------------------------------------------------------------------------------
// Animation graph asset
// ---------------------------------------------------------------------------------------------

/// A value in the graph: either a clip or a blend tree over child nodes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum BlendNode {
    /// Plays a clip by name, `speed` scales its playback.
    Clip { clip: String, #[serde(default = "one")] speed: f32 },
    /// 1D blend on a float parameter: children sit at `threshold`s (sorted ascending) and the two
    /// around the parameter value are mixed.
    Blend1D { param: String, children: Vec<(f32, BlendNode)> },
    /// Mix of exactly two children by a 0..1 float parameter.
    Mix { param: String, a: Box<BlendNode>, b: Box<BlendNode> },
    /// 2D directional blend: inverse-distance weights of the nearest children in (x, y) space.
    Blend2D { param_x: String, param_y: String, children: Vec<([f32; 2], BlendNode)> },
}

fn one() -> f32 {
    1.0
}

/// When a transition fires.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Condition {
    /// Float param compared with a value.
    Greater(String, f32),
    Less(String, f32),
    /// Bool param is true.
    Is(String),
    /// Bool param is false.
    Not(String),
    /// Trigger param was set (consumed by the transition).
    Trigger(String),
    /// The current state has played through once (non-looping or looped at least once).
    Finished,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Transition {
    /// Target state name.
    pub to: String,
    /// All conditions must hold (an empty list is an unconditional transition).
    #[serde(default)]
    pub when: Vec<Condition>,
    /// Cross-fade duration in seconds.
    #[serde(default = "default_fade")]
    pub duration: f32,
}

fn default_fade() -> f32 {
    0.2
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub name: String,
    pub motion: BlendNode,
    #[serde(default = "yes")]
    pub looping: bool,
    #[serde(default)]
    pub transitions: Vec<Transition>,
}

fn yes() -> bool {
    true
}

/// State machine whose states hold blend trees. Stored as a `.animgraph` JSON asset.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct AnimGraph {
    pub states: Vec<State>,
    /// Name of the initial state (first state when empty).
    #[serde(default)]
    pub entry: String,
}

impl AnimGraph {
    pub fn from_json(s: &str) -> Result<AnimGraph, String> {
        let g: AnimGraph = serde_json::from_str(s).map_err(|e| e.to_string())?;
        g.validate()?;
        Ok(g)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Checks that transitions and the entry point refer to existing states.
    pub fn validate(&self) -> Result<(), String> {
        if self.states.is_empty() {
            return Err("graph has no states".into());
        }
        if !self.entry.is_empty() && self.state_index(&self.entry).is_none() {
            return Err(format!("entry state '{}' does not exist", self.entry));
        }
        for s in &self.states {
            for t in &s.transitions {
                if self.state_index(&t.to).is_none() {
                    return Err(format!("state '{}' transitions to unknown state '{}'", s.name, t.to));
                }
            }
        }
        Ok(())
    }

    pub fn state_index(&self, name: &str) -> Option<usize> {
        self.states.iter().position(|s| s.name == name)
    }

    fn entry_index(&self) -> usize {
        self.state_index(&self.entry).unwrap_or(0)
    }
}

/// Parameters driving a graph (set from scripts).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnimParams {
    pub floats: HashMap<String, f32>,
    pub bools: HashMap<String, bool>,
    pub triggers: HashMap<String, bool>,
}

impl AnimParams {
    pub fn float(&self, k: &str) -> f32 {
        self.floats.get(k).copied().unwrap_or(0.0)
    }
    pub fn set_float(&mut self, k: &str, v: f32) {
        self.floats.insert(k.to_string(), v);
    }
    pub fn set_bool(&mut self, k: &str, v: bool) {
        self.bools.insert(k.to_string(), v);
    }
    pub fn set_trigger(&mut self, k: &str) {
        self.triggers.insert(k.to_string(), true);
    }
}

/// Runtime state of a graph on one entity.
#[derive(Clone, Debug, Default)]
pub struct GraphPlayer {
    pub current: usize,
    /// Normalized time (in cycles) of the current state; the integer part counts loops.
    pub cycles: f32,
    /// Previous state being faded out: (state, its cycles, fade progress 0..1, fade duration).
    pub fading: Option<(usize, f32, f32, f32)>,
    initialized: bool,
}

impl GraphPlayer {
    pub fn new(graph: &AnimGraph) -> GraphPlayer {
        GraphPlayer { current: graph.entry_index(), cycles: 0.0, fading: None, initialized: true }
    }

    /// Rebuild a player from saved component fields (state names instead of indices).
    pub fn restore(graph: &AnimGraph, state: &str, cycles: f32, fade: Option<(&str, f32, f32, f32)>) -> GraphPlayer {
        let current = graph.state_index(state).unwrap_or_else(|| graph.entry_index());
        let fading = fade.and_then(|(n, c, p, d)| graph.state_index(n).map(|i| (i, c, p, d)));
        GraphPlayer { current, cycles, fading, initialized: true }
    }

    pub fn current_name<'a>(&self, graph: &'a AnimGraph) -> &'a str {
        graph.states.get(self.current).map_or("", |s| s.name.as_str())
    }

    /// Advance time and evaluate transitions. Returns the clip-time delta used for events:
    /// (clip name, previous time, new time) of the current state's dominant clip.
    pub fn update(&mut self, graph: &AnimGraph, model: &ModelData, params: &mut AnimParams, dt: f32) {
        if !self.initialized || self.current >= graph.states.len() {
            *self = GraphPlayer::new(graph);
        }
        let dur = state_duration(&graph.states[self.current].motion, model, params).max(1e-3);
        self.cycles += dt / dur;
        if !graph.states[self.current].looping {
            self.cycles = self.cycles.min(1.0);
        }
        if let Some((s, c, p, d)) = self.fading.as_mut() {
            let sdur = graph.states.get(*s).map_or(1.0, |st| state_duration(&st.motion, model, params)).max(1e-3);
            *c += dt / sdur;
            *p += dt / d.max(1e-3);
            if *p >= 1.0 {
                self.fading = None;
            }
        }
        // First matching transition wins.
        let finished = self.cycles >= 1.0;
        let st = &graph.states[self.current];
        for t in &st.transitions {
            if t.when.iter().all(|c| holds(c, params, finished)) {
                for c in &t.when {
                    if let Condition::Trigger(k) = c {
                        params.triggers.insert(k.clone(), false);
                    }
                }
                let to = graph.state_index(&t.to).unwrap_or(self.current);
                if to != self.current {
                    self.fading = Some((self.current, self.cycles, 0.0, t.duration.max(0.0)));
                    if t.duration <= 0.0 {
                        self.fading = None;
                    }
                    self.current = to;
                    self.cycles = 0.0;
                }
                break;
            }
        }
    }

    /// Evaluate the pose for the current state (cross-faded with the previous one).
    pub fn pose(&self, graph: &AnimGraph, model: &ModelData, params: &AnimParams) -> Vec<Trs> {
        let Some(st) = graph.states.get(self.current) else { return model.rest_pose() };
        let mut pose = eval_state(st, self.cycles, model, params);
        if let Some((s, c, p, _)) = self.fading {
            if let Some(prev) = graph.states.get(s) {
                let other = eval_state(prev, c, model, params);
                // Fade from `other` to the current pose.
                let mut out = other;
                blend_poses(&mut out, &pose, p.clamp(0.0, 1.0));
                pose = out;
            }
        }
        pose
    }
}

fn holds(c: &Condition, p: &AnimParams, finished: bool) -> bool {
    match c {
        Condition::Greater(k, v) => p.float(k) > *v,
        Condition::Less(k, v) => p.float(k) < *v,
        Condition::Is(k) => p.bools.get(k).copied().unwrap_or(false),
        Condition::Not(k) => !p.bools.get(k).copied().unwrap_or(false),
        Condition::Trigger(k) => p.triggers.get(k).copied().unwrap_or(false),
        Condition::Finished => finished,
    }
}

/// Weighted leaf clips of a blend tree: (clip, speed, weight). Weights sum to 1.
pub fn leaf_weights(node: &BlendNode, params: &AnimParams) -> Vec<(String, f32, f32)> {
    let mut out = Vec::new();
    collect(node, params, 1.0, &mut out);
    out
}

fn collect(node: &BlendNode, params: &AnimParams, w: f32, out: &mut Vec<(String, f32, f32)>) {
    if w <= 0.0 {
        return;
    }
    match node {
        BlendNode::Clip { clip, speed } => out.push((clip.clone(), *speed, w)),
        BlendNode::Blend1D { param, children } => {
            if children.is_empty() {
                return;
            }
            let v = params.float(param);
            let i = children.partition_point(|(t, _)| *t <= v);
            if i == 0 {
                collect(&children[0].1, params, w, out);
            } else if i >= children.len() {
                collect(&children[children.len() - 1].1, params, w, out);
            } else {
                let (t0, a) = &children[i - 1];
                let (t1, b) = &children[i];
                let f = ((v - t0) / (t1 - t0).max(1e-6)).clamp(0.0, 1.0);
                collect(a, params, w * (1.0 - f), out);
                collect(b, params, w * f, out);
            }
        }
        BlendNode::Mix { param, a, b } => {
            let f = params.float(param).clamp(0.0, 1.0);
            collect(a, params, w * (1.0 - f), out);
            collect(b, params, w * f, out);
        }
        BlendNode::Blend2D { param_x, param_y, children } => {
            if children.is_empty() {
                return;
            }
            let p = [params.float(param_x), params.float(param_y)];
            let mut ws: Vec<f32> = Vec::with_capacity(children.len());
            for (pos, _) in children {
                let d = ((pos[0] - p[0]).powi(2) + (pos[1] - p[1]).powi(2)).sqrt();
                if d < 1e-5 {
                    // Exactly on a sample point.
                    ws = vec![0.0; children.len()];
                    let idx = children.iter().position(|(q, _)| q == pos).unwrap_or(0);
                    ws[idx] = 1.0;
                    break;
                }
                ws.push(1.0 / (d * d));
            }
            let sum: f32 = ws.iter().sum();
            for ((_, n), wi) in children.iter().zip(ws) {
                collect(n, params, w * wi / sum, out);
            }
        }
    }
}

/// Duration of a state's motion: the weight-averaged clip duration (so synced blends keep
/// their cycle aligned), divided by speed.
pub fn state_duration(node: &BlendNode, model: &ModelData, params: &AnimParams) -> f32 {
    let leaves = leaf_weights(node, params);
    let mut d = 0.0;
    let mut total = 0.0;
    for (clip, speed, w) in &leaves {
        let Some(ci) = model.find_clip(clip) else { continue };
        d += model.animations[ci].duration / speed.max(1e-3) * w;
        total += w;
    }
    if total > 0.0 {
        d / total
    } else {
        1.0
    }
}

fn eval_state(st: &State, cycles: f32, model: &ModelData, params: &AnimParams) -> Vec<Trs> {
    let phase = if st.looping { cycles.rem_euclid(1.0) } else { cycles.clamp(0.0, 1.0) };
    let leaves = leaf_weights(&st.motion, params);
    let mut pose = model.rest_pose();
    let mut acc = 0.0;
    for (clip, _speed, w) in leaves {
        let Some(ci) = model.find_clip(&clip) else { continue };
        let c = &model.animations[ci];
        let mut p = model.rest_pose();
        c.sample_into(phase * c.duration, &mut p);
        if acc == 0.0 {
            pose = p;
        } else {
            blend_poses(&mut pose, &p, w / (acc + w));
        }
        acc += w;
    }
    pose
}

// ---------------------------------------------------------------------------------------------
// Animation events
// ---------------------------------------------------------------------------------------------

/// Events in `(prev, now]` of a clip of length `duration`, handling a loop wrap-around.
pub fn events_between<'a>(events: &'a [crate::model::AnimationEvent], prev: f32, now: f32, duration: f32) -> Vec<&'a crate::model::AnimationEvent> {
    let mut out = Vec::new();
    if now >= prev {
        out.extend(events.iter().filter(|e| e.time > prev && e.time <= now));
    } else {
        // Wrapped around the end of the clip.
        out.extend(events.iter().filter(|e| e.time > prev && e.time <= duration));
        out.extend(events.iter().filter(|e| e.time >= 0.0 && e.time <= now));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Root motion
// ---------------------------------------------------------------------------------------------

/// Root-motion delta between two clip times, in the root node's parent space. The wrap case
/// (looping clips) accumulates end - prev plus now - start.
pub fn root_motion_delta(model: &ModelData, clip: usize, root: usize, prev: f32, now: f32) -> Trs {
    let Some(c) = model.animations.get(clip) else { return Trs::default() };
    let at = |t: f32| {
        let mut p = model.rest_pose();
        c.sample_into(t, &mut p);
        p[root.min(p.len().saturating_sub(1))]
    };
    let delta = |a: Trs, b: Trs| Trs {
        translation: b.translation - a.translation,
        rotation: (b.rotation * a.rotation.inverse()).normalize(),
        scale: Vec3::ONE,
    };
    if now >= prev {
        delta(at(prev), at(now))
    } else {
        let d1 = delta(at(prev), at(c.duration));
        let d2 = delta(at(0.0), at(now));
        Trs { translation: d1.translation + d2.translation, rotation: (d2.rotation * d1.rotation).normalize(), scale: Vec3::ONE }
    }
}

/// Strip the horizontal translation and yaw of the root node from a pose so the clip plays in
/// place (the extracted motion is applied to the entity's `Transform` instead).
pub fn strip_root_motion(pose: &mut [Trs], root: usize, rest: &Trs) {
    if let Some(p) = pose.get_mut(root) {
        p.translation.x = rest.translation.x;
        p.translation.z = rest.translation.z;
        let (yaw, pitch, roll) = p.rotation.to_euler(glam_yxz());
        let _ = yaw;
        p.rotation = Quat::from_euler(glam_yxz(), rest.rotation.to_euler(glam_yxz()).0, pitch, roll);
    }
}

fn glam_yxz() -> dumb_core::EulerRot {
    dumb_core::EulerRot::YXZ
}

// ---------------------------------------------------------------------------------------------
// Procedural animation
// ---------------------------------------------------------------------------------------------

/// Rotation taking direction `from` onto `to`.
fn rot_between(from: Vec3, to: Vec3) -> Quat {
    let (f, t) = (from.normalize_or_zero(), to.normalize_or_zero());
    if f == Vec3::ZERO || t == Vec3::ZERO {
        return Quat::IDENTITY;
    }
    Quat::from_rotation_arc(f, t)
}

/// Analytic two-bone IK (law of cosines) in model space. Given the root (shoulder/hip), mid
/// (elbow/knee) and end (wrist/ankle) positions, returns the new mid and end positions that put
/// the end on `target` (clamped to reach), bending towards `pole`.
pub fn two_bone_ik(root: Vec3, mid: Vec3, end: Vec3, target: Vec3, pole: Vec3) -> (Vec3, Vec3) {
    let l1 = (mid - root).length();
    let l2 = (end - mid).length();
    let to_target = target - root;
    let dist = to_target.length().clamp((l1 - l2).abs() + 1e-4, (l1 + l2 - 1e-4).max(1e-4));
    let dir = to_target.normalize_or(Vec3::X);
    // Distance along the target direction to the elbow's projection, and its height.
    let a = (l1 * l1 - l2 * l2 + dist * dist) / (2.0 * dist);
    let h = (l1 * l1 - a * a).max(0.0).sqrt();
    // Bend plane: pole direction made orthogonal to `dir`.
    let mut side = (pole - root) - dir * (pole - root).dot(dir);
    if side.length_squared() < 1e-8 {
        side = (mid - root) - dir * (mid - root).dot(dir);
    }
    if side.length_squared() < 1e-8 {
        side = dir.any_orthonormal_vector();
    }
    let side = side.normalize();
    let new_mid = root + dir * a + side * h;
    let new_end = root + dir * dist;
    (new_mid, new_end)
}

/// Apply a two-bone IK solve to a pose: `chain` = [root, mid, end] node indices (each the
/// child of the previous). `target`/`pole` are in model space. Rotations of the root and mid
/// nodes are adjusted so the chain reaches the target.
pub fn solve_two_bone(model: &ModelData, pose: &mut [Trs], chain: [usize; 3], target: Vec3, pole: Vec3, weight: f32) {
    let globals = model.global_matrices(pose);
    let pos = |i: usize| globals[i].w_axis.truncate();
    let (r, m, e) = (pos(chain[0]), pos(chain[1]), pos(chain[2]));
    let (nm, ne) = two_bone_ik(r, m, e, target, pole);
    let w = weight.clamp(0.0, 1.0);
    let (nm, ne) = (m.lerp(nm, w), e.lerp(ne, w));
    // Rotate root so root->mid points at new mid.
    let q_root = rot_between(m - r, nm - r);
    rotate_node_global(model, pose, chain[0], q_root);
    // After rotating the root, the mid moved to `nm`; rotate mid so mid->end points at new end.
    let globals = model.global_matrices(pose);
    let (m2, e2) = (globals[chain[1]].w_axis.truncate(), globals[chain[2]].w_axis.truncate());
    let q_mid = rot_between(e2 - m2, ne - nm);
    rotate_node_global(model, pose, chain[1], q_mid);
}

/// Pre-multiply a model-space rotation onto a node's local rotation.
fn rotate_node_global(model: &ModelData, pose: &mut [Trs], node: usize, q_global: Quat) {
    let globals = model.global_matrices(pose);
    let parent_rot = model.nodes[node]
        .parent
        .map(|p| globals[p].to_scale_rotation_translation().1)
        .unwrap_or(Quat::IDENTITY);
    // local' = parent^-1 * q * parent * local
    let q_local = parent_rot.inverse() * q_global * parent_rot;
    pose[node].rotation = (q_local * pose[node].rotation).normalize();
}

/// FABRIK for a chain of any length. `joints` are model-space positions root-first; bone lengths
/// are taken from the input. The root stays fixed. Returns the new positions.
pub fn fabrik(joints: &[Vec3], target: Vec3, iterations: usize, tolerance: f32) -> Vec<Vec3> {
    let n = joints.len();
    let mut p = joints.to_vec();
    if n < 2 {
        return p;
    }
    let lens: Vec<f32> = joints.windows(2).map(|w| (w[1] - w[0]).length()).collect();
    let total: f32 = lens.iter().sum();
    let root = p[0];
    if (target - root).length() >= total {
        // Out of reach: stretch straight towards the target.
        let dir = (target - root).normalize_or(Vec3::X);
        for i in 1..n {
            p[i] = p[i - 1] + dir * lens[i - 1];
        }
        return p;
    }
    for _ in 0..iterations.max(1) {
        if (p[n - 1] - target).length() <= tolerance {
            break;
        }
        // Backward: end effector to the target.
        p[n - 1] = target;
        for i in (0..n - 1).rev() {
            let d = (p[i] - p[i + 1]).normalize_or(Vec3::X);
            p[i] = p[i + 1] + d * lens[i];
        }
        // Forward: root back to its place.
        p[0] = root;
        for i in 1..n {
            let d = (p[i] - p[i - 1]).normalize_or(Vec3::X);
            p[i] = p[i - 1] + d * lens[i - 1];
        }
    }
    p
}

/// Apply FABRIK to a chain of nodes (root-first) in a pose, rotating each node so the chain
/// follows the solved positions.
pub fn solve_fabrik(model: &ModelData, pose: &mut [Trs], chain: &[usize], target: Vec3, iterations: usize, weight: f32) {
    if chain.len() < 2 {
        return;
    }
    let globals = model.global_matrices(pose);
    let cur: Vec<Vec3> = chain.iter().map(|i| globals[*i].w_axis.truncate()).collect();
    let solved = fabrik(&cur, target, iterations, 1e-3);
    let w = weight.clamp(0.0, 1.0);
    let mut cur_now = cur.clone();
    for i in 0..chain.len() - 1 {
        let want = cur[i].lerp(solved[i + 1], w);
        // Use the already-rotated parent position to aim this bone.
        let g = model.global_matrices(pose);
        let a = g[chain[i]].w_axis.truncate();
        let b = g[chain[i + 1]].w_axis.truncate();
        let _ = &mut cur_now;
        let q = rot_between(b - a, want - a);
        rotate_node_global(model, pose, chain[i], q);
    }
}

/// Rotate `node` so its local `forward` axis (model space after the node's parent transform)
/// points at `target` (model space). `weight` fades the effect; `max_angle` (radians) limits it.
pub fn look_at(model: &ModelData, pose: &mut [Trs], node: usize, forward: Vec3, target: Vec3, weight: f32, max_angle: f32) {
    let globals = model.global_matrices(pose);
    let (_, rot, pos) = globals[node].to_scale_rotation_translation();
    let cur = rot * forward;
    let want = (target - pos).normalize_or_zero();
    if want == Vec3::ZERO {
        return;
    }
    let mut q = rot_between(cur, want);
    let (axis, mut angle) = q.to_axis_angle();
    angle = angle.min(max_angle.max(0.0)) * weight.clamp(0.0, 1.0);
    q = Quat::from_axis_angle(axis, angle);
    rotate_node_global(model, pose, node, q);
}

/// Foot placement: given a foot's model-space position and the ground height under it, returns
/// the target for the ankle IK (ankle height above the sole preserved) and a pelvis drop so
/// that neither leg needs to over-extend.
pub fn foot_target(foot: Vec3, ground_y: f32, sole_offset: f32) -> Vec3 {
    Vec3::new(foot.x, ground_y + sole_offset, foot.z)
}

/// Pelvis offset that keeps the lowest foot reachable: move down by the largest drop.
pub fn pelvis_offset(foot_ys: &[f32], target_ys: &[f32]) -> f32 {
    foot_ys.iter().zip(target_ys).map(|(f, t)| t - f).fold(0.0, f32::min)
}

/// Spring-bone chain simulation (hair, tails, cloth strips): each bone's tip follows its rest
/// Spring-bone state for one node (hair, tails, capes): the tip follows its animated rest
/// position with inertia and damping and keeps a fixed bone length (verlet integration).
/// `simulate` advances the state (runtime); `apply` turns the node towards the tip (pure, used
/// when building poses for rendering).
#[derive(Clone, Debug)]
pub struct SpringBone {
    pub node: usize,
    pub stiffness: f32,
    pub damping: f32,
    pub gravity: Vec3,
    /// Verlet state: current and previous tip positions in model space.
    pub tip: Vec3,
    pub prev: Vec3,
    pub primed: bool,
}

impl SpringBone {
    pub fn new(node: usize, stiffness: f32, damping: f32, gravity: Vec3) -> SpringBone {
        SpringBone { node, stiffness, damping, gravity, tip: Vec3::ZERO, prev: Vec3::ZERO, primed: false }
    }

    fn head(&self, model: &ModelData, pose: &[Trs], bone_axis: Vec3) -> (Vec3, Vec3) {
        let globals = model.global_matrices(pose);
        let (_, rot, pos) = globals[self.node].to_scale_rotation_translation();
        (pos, rot * bone_axis.normalize_or(Vec3::Y))
    }

    /// Advance the simulation one step against the animated `pose` (without this node's spring
    /// rotation). `bone_axis` is the local direction to the tip, `bone_len` its distance.
    pub fn simulate(&mut self, model: &ModelData, pose: &[Trs], bone_axis: Vec3, bone_len: f32, dt: f32) {
        let (pos, rest_dir) = self.head(model, pose, bone_axis);
        let rest_tip = pos + rest_dir * bone_len;
        if !self.primed {
            self.tip = rest_tip;
            self.prev = rest_tip;
            self.primed = true;
        }
        let dt = dt.clamp(1e-4, 0.1);
        let vel = (self.tip - self.prev) * (1.0 - self.damping.clamp(0.0, 1.0));
        let spring = (rest_tip - self.tip) * self.stiffness * dt * dt * 60.0;
        let next = self.tip + vel + spring + self.gravity * dt * dt;
        self.prev = self.tip;
        // Constrain to the bone length around the (moving) head.
        let d = (next - pos).normalize_or(rest_dir);
        self.tip = pos + d * bone_len;
    }

    /// Rotate the node so its `bone_axis` points at the simulated tip.
    pub fn apply(&self, model: &ModelData, pose: &mut [Trs], bone_axis: Vec3) {
        if !self.primed {
            return;
        }
        let (pos, rest_dir) = self.head(model, pose, bone_axis);
        let want = (self.tip - pos).normalize_or(rest_dir);
        rotate_node_global(model, pose, self.node, rot_between(rest_dir, want));
    }

    /// `simulate` then `apply`.
    pub fn step(&mut self, model: &ModelData, pose: &mut [Trs], bone_axis: Vec3, bone_len: f32, dt: f32) {
        self.simulate(model, pose, bone_axis, bone_len, dt);
        self.apply(model, pose, bone_axis);
    }
}

/// Convenience: model-space position of a node in a pose.
pub fn node_position(model: &ModelData, pose: &[Trs], node: usize) -> Vec3 {
    let g: Vec<Mat4> = model.global_matrices(pose);
    g[node].w_axis.truncate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AnimationClip, Channel, ChannelTarget, Interpolation, Node};
    use dumb_core::Vec4;

    fn chain_model(n: usize) -> ModelData {
        let mut m = ModelData::default();
        for i in 0..n {
            m.nodes.push(Node {
                name: format!("b{i}"),
                parent: if i == 0 { None } else { Some(i - 1) },
                children: if i + 1 < n { vec![i + 1] } else { vec![] },
                local: Trs { translation: if i == 0 { Vec3::ZERO } else { Vec3::Y }, ..Default::default() },
                mesh: None,
                skin: None,
            });
        }
        m.roots = vec![0];
        m
    }

    fn clip(name: &str, node: usize, from: Vec3, to: Vec3, dur: f32) -> AnimationClip {
        AnimationClip {
            name: name.into(),
            duration: dur,
            channels: vec![Channel {
                node,
                target: ChannelTarget::Translation,
                interpolation: Interpolation::Linear,
                times: vec![0.0, dur],
                values: vec![from.extend(0.0), to.extend(0.0)],
            }],
        }
    }

    #[test]
    fn two_bone_reaches_target_and_keeps_lengths() {
        let (r, m, e) = (Vec3::ZERO, Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 2.0, 0.0));
        let target = Vec3::new(1.2, 0.8, 0.0);
        let (nm, ne) = two_bone_ik(r, m, e, target, Vec3::new(0.0, 0.0, 5.0));
        assert!((ne - target).length() < 1e-3);
        assert!(((nm - r).length() - 1.0).abs() < 1e-3);
        assert!(((ne - nm).length() - 1.0).abs() < 1e-3);
        assert!(nm.z > 0.0, "bends towards the pole");
    }

    #[test]
    fn two_bone_clamps_out_of_reach() {
        let (nm, ne) = two_bone_ik(Vec3::ZERO, Vec3::Y, Vec3::Y * 2.0, Vec3::new(10.0, 0.0, 0.0), Vec3::Z);
        assert!((ne.length() - 2.0).abs() < 1e-2);
        assert!(ne.x > 1.9);
        assert!(nm.is_finite());
    }

    #[test]
    fn solve_two_bone_moves_end_node() {
        let m = chain_model(3);
        let mut pose = m.rest_pose();
        let target = Vec3::new(1.0, 1.0, 0.0);
        solve_two_bone(&m, &mut pose, [0, 1, 2], target, Vec3::new(0.0, 0.0, 3.0), 1.0);
        assert!((node_position(&m, &pose, 2) - target).length() < 1e-2);
    }

    #[test]
    fn fabrik_reaches_and_fixes_root() {
        let joints = [Vec3::ZERO, Vec3::Y, Vec3::Y * 2.0, Vec3::Y * 3.0];
        let target = Vec3::new(1.5, 1.5, 0.5);
        let out = fabrik(&joints, target, 20, 1e-4);
        assert_eq!(out[0], Vec3::ZERO);
        assert!((out[3] - target).length() < 1e-2);
        for w in out.windows(2) {
            assert!(((w[1] - w[0]).length() - 1.0).abs() < 1e-3);
        }
    }

    #[test]
    fn fabrik_out_of_reach_stretches() {
        let joints = [Vec3::ZERO, Vec3::Y, Vec3::Y * 2.0];
        let out = fabrik(&joints, Vec3::new(10.0, 0.0, 0.0), 5, 1e-4);
        assert!((out[2] - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn look_at_aims_forward_axis() {
        let m = chain_model(2);
        let mut pose = m.rest_pose();
        let target = Vec3::new(5.0, 0.0, 0.0);
        look_at(&m, &mut pose, 0, Vec3::Y, target, 1.0, std::f32::consts::PI);
        let g = m.global_matrices(&pose);
        let dir = (g[0].to_scale_rotation_translation().1 * Vec3::Y).normalize();
        assert!((dir - Vec3::X).length() < 1e-3);
    }

    #[test]
    fn look_at_respects_max_angle_and_weight() {
        let m = chain_model(2);
        let mut pose = m.rest_pose();
        look_at(&m, &mut pose, 0, Vec3::Y, Vec3::X * 5.0, 1.0, 0.25);
        let (_, a) = pose[0].rotation.to_axis_angle();
        assert!((a - 0.25).abs() < 1e-3);
        let mut pose = m.rest_pose();
        look_at(&m, &mut pose, 0, Vec3::Y, Vec3::X * 5.0, 0.5, std::f32::consts::PI);
        let (_, a) = pose[0].rotation.to_axis_angle();
        assert!((a - std::f32::consts::FRAC_PI_4).abs() < 1e-3);
    }

    #[test]
    fn spring_bone_lags_then_settles() {
        let m = chain_model(2);
        let mut pose = m.rest_pose();
        let mut sb = SpringBone::new(0, 0.3, 0.1, Vec3::ZERO);
        sb.step(&m, &mut pose, Vec3::Y, 1.0, 1.0 / 60.0);
        // Move the parent rest pose sideways: the tip should lag behind the new rest direction.
        let mut moved = m.rest_pose();
        moved[0].rotation = Quat::from_rotation_z(-1.0);
        sb.step(&m, &mut moved, Vec3::Y, 1.0, 1.0 / 60.0);
        let (_, a) = moved[0].rotation.to_axis_angle();
        assert!(a < 1.0 - 0.1, "tip lags: simulated angle {a} should be below the rest angle 1.0");
        for _ in 0..600 {
            let mut p = m.rest_pose();
            p[0].rotation = Quat::from_rotation_z(-1.0);
            sb.step(&m, &mut p, Vec3::Y, 1.0, 1.0 / 60.0);
            moved = p;
        }
        let (_, a) = moved[0].rotation.to_axis_angle();
        assert!((a - 1.0).abs() < 0.05, "settles on rest, got {a}");
    }

    #[test]
    fn blend_tree_1d_weights() {
        let tree = BlendNode::Blend1D {
            param: "speed".into(),
            children: vec![
                (0.0, BlendNode::Clip { clip: "Idle".into(), speed: 1.0 }),
                (1.0, BlendNode::Clip { clip: "Walk".into(), speed: 1.0 }),
                (3.0, BlendNode::Clip { clip: "Run".into(), speed: 1.0 }),
            ],
        };
        let mut p = AnimParams::default();
        p.set_float("speed", 0.25);
        let w = leaf_weights(&tree, &p);
        assert_eq!(w.len(), 2);
        assert!((w[0].2 - 0.75).abs() < 1e-5 && w[0].0 == "Idle");
        p.set_float("speed", 2.0);
        let w = leaf_weights(&tree, &p);
        assert!(w.iter().any(|(c, _, x)| c == "Walk" && (x - 0.5).abs() < 1e-5));
        p.set_float("speed", 99.0);
        let w = leaf_weights(&tree, &p);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].0, "Run");
    }

    #[test]
    fn blend_tree_2d_weights_sum_to_one() {
        let tree = BlendNode::Blend2D {
            param_x: "x".into(),
            param_y: "y".into(),
            children: vec![
                ([0.0, 1.0], BlendNode::Clip { clip: "F".into(), speed: 1.0 }),
                ([1.0, 0.0], BlendNode::Clip { clip: "R".into(), speed: 1.0 }),
                ([-1.0, 0.0], BlendNode::Clip { clip: "L".into(), speed: 1.0 }),
            ],
        };
        let mut p = AnimParams::default();
        p.set_float("y", 1.0);
        let w = leaf_weights(&tree, &p);
        assert!((w.iter().map(|x| x.2).sum::<f32>() - 1.0).abs() < 1e-4);
        assert!(w.iter().find(|x| x.0 == "F").unwrap().2 > 0.99);
    }

    fn graph() -> AnimGraph {
        AnimGraph::from_json(
            r#"{ "states": [
                { "name": "idle", "motion": {"Clip": {"clip": "Idle"}},
                  "transitions": [ {"to": "walk", "when": [{"Greater": ["speed", 0.1]}], "duration": 0.5} ] },
                { "name": "walk", "motion": {"Clip": {"clip": "Walk"}},
                  "transitions": [ {"to": "idle", "when": [{"Less": ["speed", 0.1]}], "duration": 0.0},
                                   {"to": "jump", "when": [{"Trigger": "jump"}], "duration": 0.1} ] },
                { "name": "jump", "looping": false, "motion": {"Clip": {"clip": "Jump"}},
                  "transitions": [ {"to": "idle", "when": ["Finished"], "duration": 0.1} ] }
            ] }"#,
        )
        .unwrap()
    }

    fn graph_model() -> ModelData {
        let mut m = chain_model(1);
        m.animations.push(clip("Idle", 0, Vec3::ZERO, Vec3::ZERO, 1.0));
        m.animations.push(clip("Walk", 0, Vec3::ZERO, Vec3::X * 2.0, 1.0));
        m.animations.push(clip("Jump", 0, Vec3::ZERO, Vec3::Y * 2.0, 0.5));
        m
    }

    #[test]
    fn state_machine_transitions_and_triggers() {
        let g = graph();
        let m = graph_model();
        let mut params = AnimParams::default();
        let mut pl = GraphPlayer::new(&g);
        pl.update(&g, &m, &mut params, 0.1);
        assert_eq!(pl.current_name(&g), "idle");
        params.set_float("speed", 1.0);
        pl.update(&g, &m, &mut params, 0.1);
        assert_eq!(pl.current_name(&g), "walk");
        assert!(pl.fading.is_some());
        // Mid cross-fade the pose sits between idle and walk.
        let pose = pl.pose(&g, &m, &params);
        assert!(pose[0].translation.x >= 0.0);
        params.set_trigger("jump");
        pl.update(&g, &m, &mut params, 0.1);
        assert_eq!(pl.current_name(&g), "jump");
        assert!(!params.triggers["jump"], "trigger consumed");
        // Jump finishes (0.5 s) and returns to idle (stay idle: speed is 0).
        params.set_float("speed", 0.0);
        for _ in 0..6 {
            pl.update(&g, &m, &mut params, 0.1);
        }
        assert_eq!(pl.current_name(&g), "idle");
    }

    #[test]
    fn graph_validation_rejects_bad_targets() {
        let bad = r#"{ "states": [ { "name": "a", "motion": {"Clip": {"clip": "x"}}, "transitions": [{"to": "nope"}] } ] }"#;
        assert!(AnimGraph::from_json(bad).is_err());
        assert!(AnimGraph::from_json(r#"{ "states": [] }"#).is_err());
        let g = graph();
        assert_eq!(AnimGraph::from_json(&g.to_json()).unwrap(), g);
    }

    #[test]
    fn events_fire_once_and_across_wraps() {
        let ev = vec![
            crate::model::AnimationEvent { time: 0.2, name: "a".into() },
            crate::model::AnimationEvent { time: 0.9, name: "b".into() },
        ];
        let names = |v: Vec<&crate::model::AnimationEvent>| v.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(events_between(&ev, 0.1, 0.5, 1.0)), ["a"]);
        assert!(events_between(&ev, 0.2, 0.5, 1.0).is_empty(), "exclusive start");
        assert_eq!(names(events_between(&ev, 0.8, 0.3, 1.0)), ["b", "a"]);
    }

    #[test]
    fn root_motion_delta_and_wrap() {
        let m = graph_model();
        let d = root_motion_delta(&m, 1, 0, 0.25, 0.75);
        assert!((d.translation.x - 1.0).abs() < 1e-4);
        let w = root_motion_delta(&m, 1, 0, 0.75, 0.25);
        // 0.75->1.0 gives 0.5, 0.0->0.25 gives 0.5.
        assert!((w.translation.x - 1.0).abs() < 1e-4);
    }

    #[test]
    fn strip_root_motion_keeps_height() {
        let mut pose = vec![Trs { translation: Vec3::new(3.0, 1.5, -2.0), rotation: Quat::from_rotation_y(1.0), scale: Vec3::ONE }];
        let rest = Trs::default();
        strip_root_motion(&mut pose, 0, &rest);
        assert_eq!(pose[0].translation, Vec3::new(0.0, 1.5, 0.0));
        let (yaw, _, _) = pose[0].rotation.to_euler(dumb_core::EulerRot::YXZ);
        assert!(yaw.abs() < 1e-4);
    }

    #[test]
    fn foot_placement_helpers() {
        let t = foot_target(Vec3::new(1.0, 0.3, 2.0), 0.1, 0.05);
        assert_eq!(t, Vec3::new(1.0, 0.15, 2.0));
        assert!((pelvis_offset(&[0.3, 0.3], &[0.15, 0.4]) + 0.15).abs() < 1e-6);
        let _ = Vec4::ZERO;
    }
}

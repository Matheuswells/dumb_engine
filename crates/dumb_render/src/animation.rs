//! Builds the final skeleton pose of an entity: clip or animation graph, then root motion
//! stripping, IK, look-at and spring bones.

use dumb_asset::anim::{self, AnimGraph, AnimParams, GraphPlayer};
use dumb_asset::model::{ModelData, Trs};
use dumb_asset::AssetDatabase;
use dumb_core::{Entity, Quat, Vec3};
use dumb_ecs::{AnimGraphPlayer, Animator, IkConstraint, LookAtConstraint, MeshRenderer, SpringChain, Transform, World};

/// Index of the node named `name`, or the first root when `name` is empty.
pub fn find_node(model: &ModelData, name: &str) -> Option<usize> {
    if name.is_empty() {
        return model.roots.first().copied();
    }
    model.nodes.iter().position(|n| n.name == name)
}

/// Params as the graph sees them: every name is a float, and also a bool/trigger when > 0.5.
pub fn params_of(p: &AnimGraphPlayer) -> AnimParams {
    let mut out = AnimParams::default();
    for (n, v) in p.param_names.iter().zip(&p.param_values) {
        out.floats.insert(n.clone(), *v);
        out.bools.insert(n.clone(), *v > 0.5);
        out.triggers.insert(n.clone(), *v > 0.5);
    }
    out
}

pub fn player_of(p: &AnimGraphPlayer, graph: &AnimGraph) -> GraphPlayer {
    let fade = (p.fade_duration > 0.0 && !p.fade_from.is_empty()).then_some((p.fade_from.as_str(), p.fade_cycles, p.fade_progress, p.fade_duration));
    GraphPlayer::restore(graph, &p.state, p.cycles, fade)
}

/// Pose of the skeleton before procedural constraints.
fn base_pose(world: &World, db: &AssetDatabase, e: Entity, model: &ModelData) -> Option<Vec<Trs>> {
    if let Some(gp) = world.get::<AnimGraphPlayer>(e) {
        if let Some(graph) = db.anim_graph(gp.graph) {
            let player = player_of(gp, &graph);
            return Some(player.pose(&graph, model, &params_of(gp)));
        }
    }
    let a = world.get::<Animator>(e)?;
    let clip = model.find_clip(&a.clip)?;
    let blend = model.find_clip(&a.blend_clip).filter(|_| !a.blend_clip.is_empty()).map(|b| (b, a.time, a.blend));
    Some(model.sample_pose(Some((clip, a.time)), blend))
}

fn root_motion_node(world: &World, e: Entity, model: &ModelData) -> Option<usize> {
    let (on, name) = match (world.get::<AnimGraphPlayer>(e), world.get::<Animator>(e)) {
        (Some(g), _) if g.root_motion => (true, g.root_node.as_str()),
        (_, Some(a)) if a.root_motion => (true, a.root_node.as_str()),
        _ => (false, ""),
    };
    if on { find_node(model, name) } else { None }
}

fn model_space(world: &World, owner: Entity, target: Entity) -> Option<Vec3> {
    if target == Entity::NONE || !world.is_alive(target) {
        return None;
    }
    let inv = world.global_matrix(owner).inverse();
    Some(inv.transform_point3(world.global_matrix(target).w_axis.truncate()))
}

/// Direction and length of the bone from `node` to its first child.
pub fn spring_bone_axis(model: &ModelData, node: usize) -> Option<(Vec3, f32)> {
    let c = *model.nodes[node].children.first()?;
    let t = model.nodes[c].local.translation;
    let len = t.length();
    (len > 1e-5).then(|| (t / len, len))
}

/// Apply IK, look-at and spring constraints on top of `pose`.
pub fn apply_constraints(world: &World, e: Entity, model: &ModelData, pose: &mut [Trs]) {
    apply_ik_and_look_at(world, e, model, pose);
    apply_springs(world, e, model, pose);
}

fn apply_ik_and_look_at(world: &World, e: Entity, model: &ModelData, pose: &mut [Trs]) {
    if let Some(ik) = world.get::<IkConstraint>(e) {
        let chain: Option<Vec<usize>> = ik.chain.iter().map(|n| find_node(model, n)).collect();
        if let (Some(chain), Some(target)) = (chain, model_space(world, e, ik.target)) {
            if chain.len() == 3 {
                let pole = model_space(world, e, ik.pole).unwrap_or_else(|| anim::node_position(model, pose, chain[1]) + Vec3::Z);
                anim::solve_two_bone(model, pose, [chain[0], chain[1], chain[2]], target, pole, ik.weight);
            } else if chain.len() > 3 || chain.len() == 2 {
                anim::solve_fabrik(model, pose, &chain, target, ik.iterations as usize, ik.weight);
            }
        }
    }
    if let Some(l) = world.get::<LookAtConstraint>(e) {
        if let (Some(node), Some(target)) = (find_node(model, &l.node).filter(|_| !l.node.is_empty()), model_space(world, e, l.target)) {
            anim::look_at(model, pose, node, l.forward, target, l.weight, l.max_angle.to_radians());
        }
    }
}

fn apply_springs(world: &World, e: Entity, model: &ModelData, pose: &mut [Trs]) {
    if let Some(s) = world.get::<SpringChain>(e) {
        for (i, name) in s.nodes.iter().enumerate() {
            let Some(node) = find_node(model, name) else { continue };
            let Some((axis, _)) = spring_bone_axis(model, node) else { continue };
            let (Some(&tip), Some(&prev)) = (s.tips.get(i), s.prevs.get(i)) else { continue };
            let mut sb = anim::SpringBone::new(node, s.stiffness, s.damping, s.gravity);
            sb.tip = tip;
            sb.prev = prev;
            sb.primed = s.primed;
            sb.apply(model, pose, axis);
        }
    }
}

/// Final pose for an entity, or `None` when nothing animates it (draw the rest pose).
pub fn entity_pose(world: &World, db: &AssetDatabase, e: Entity, model: &ModelData) -> Option<Vec<Trs>> {
    let has_constraints = world.get::<IkConstraint>(e).is_some() || world.get::<LookAtConstraint>(e).is_some() || world.get::<SpringChain>(e).is_some();
    let mut pose = match base_pose(world, db, e, model) {
        Some(p) => p,
        None if has_constraints => model.rest_pose(),
        None => return None,
    };
    if let Some(root) = root_motion_node(world, e, model) {
        let rest = model.nodes[root].local;
        anim::strip_root_motion(&mut pose, root, &rest);
    }
    apply_constraints(world, e, model, &mut pose);
    Some(pose)
}

/// Advance the spring chain of `e` by `dt`. Returns the new (tips, prevs), to be written back to
/// the `SpringChain` component. Each bone is simulated against the animated pose with its
/// parents' spring rotation already applied.
pub fn simulate_springs(world: &World, db: &AssetDatabase, e: Entity, model: &ModelData, dt: f32) -> Option<(Vec<Vec3>, Vec<Vec3>)> {
    let s = world.get::<SpringChain>(e)?;
    let mut pose = base_pose(world, db, e, model).unwrap_or_else(|| model.rest_pose());
    if let Some(root) = root_motion_node(world, e, model) {
        anim::strip_root_motion(&mut pose, root, &model.nodes[root].local);
    }
    apply_ik_and_look_at(world, e, model, &mut pose);
    let mut tips = Vec::with_capacity(s.nodes.len());
    let mut prevs = Vec::with_capacity(s.nodes.len());
    for (i, name) in s.nodes.iter().enumerate() {
        let bone = find_node(model, name).and_then(|n| spring_bone_axis(model, n).map(|(a, l)| (n, a, l)));
        let Some((node, axis, len)) = bone else {
            tips.push(s.tips.get(i).copied().unwrap_or(Vec3::ZERO));
            prevs.push(s.prevs.get(i).copied().unwrap_or(Vec3::ZERO));
            continue;
        };
        let mut sb = anim::SpringBone::new(node, s.stiffness, s.damping, s.gravity);
        if let (Some(&t), Some(&p)) = (s.tips.get(i), s.prevs.get(i)) {
            sb.tip = t;
            sb.prev = p;
            sb.primed = s.primed;
        }
        sb.simulate(model, &pose, axis, len, dt);
        sb.apply(model, &mut pose, axis);
        tips.push(sb.tip);
        prevs.push(sb.prev);
    }
    Some((tips, prevs))
}

/// Events of `clip` authored in the model's import settings.
fn clip_events(db: &AssetDatabase, model: dumb_core::AssetId, clip: &str) -> Vec<dumb_asset::AnimationEvent> {
    db.entry(model)
        .and_then(|e| e.meta.animation_events.iter().find(|(c, _)| c == clip).map(|(_, v)| v.clone()))
        .unwrap_or_default()
}

/// Advance animation: `Animator` clips, animation graphs (state machines + blend trees) and
/// spring bones. Sends `AnimEvent`s for markers passed this step and applies root motion to the
/// entity's `Transform`.
pub fn update_animators(world: &mut World, db: &AssetDatabase, dt: f32) {
    use dumb_asset::anim;
    let mut events: Vec<dumb_ecs::AnimEvent> = Vec::new();
    // (entity, root node, clip, previous time, new time)
    let mut motions: Vec<(Entity, Option<usize>, usize, f32, f32)> = Vec::new();
    let fire = |e: Entity, model: &ModelData, id: dumb_core::AssetId, clip: usize, prev: f32, now: f32, out: &mut Vec<dumb_ecs::AnimEvent>| {
        let c = &model.animations[clip];
        for ev in anim::events_between(&clip_events(db, id, &c.name), prev, now, c.duration) {
            out.push(dumb_ecs::AnimEvent { entity: e, name: ev.name.clone() });
        }
    };

    for (e, a, mr) in world.query::<(Entity, &mut Animator, &MeshRenderer)>() {
        if !a.playing {
            continue;
        }
        let prev = a.time;
        a.time += dt * a.speed;
        let Some(model) = db.model_loaded(mr.model) else { continue };
        let Some(ci) = model.find_clip(&a.clip) else { continue };
        let dur = model.animations[ci].duration.max(1e-3);
        if a.looping {
            a.time = a.time.rem_euclid(dur);
        } else if a.time > dur {
            a.time = dur;
            a.playing = false;
        }
        if a.speed > 0.0 {
            // A fresh clip (time 0) includes markers at exactly 0.
            let from = if prev <= 0.0 { -1e-4 } else { prev };
            fire(e, &model, mr.model, ci, from, a.time, &mut events);
            if a.root_motion {
                let root = find_node(&model, &a.root_node);
                motions.push((e, root, ci, prev, a.time));
            }
        }
        a.prev_time = a.time;
    }

    for (e, gp, mr) in world.query::<(Entity, &mut dumb_ecs::AnimGraphPlayer, &MeshRenderer)>() {
        let Some(model) = db.model_loaded(mr.model) else { continue };
        let Some(graph) = db.anim_graph(gp.graph) else { continue };
        let mut player = player_of(gp, &graph);
        let mut params = params_of(gp);
        let dominant = |state: usize, cycles: f32, params: &anim::AnimParams| -> Option<(usize, f32)> {
            let st = graph.states.get(state)?;
            let (clip, _, _) = anim::leaf_weights(&st.motion, params).into_iter().max_by(|a, b| a.2.total_cmp(&b.2))?;
            let ci = model.find_clip(&clip)?;
            let phase = if st.looping { cycles.rem_euclid(1.0) } else { cycles.clamp(0.0, 1.0) };
            Some((ci, phase * model.animations[ci].duration))
        };
        let before = dominant(player.current, player.cycles, &params);
        let started = player.current;
        player.update(&graph, &model, &mut params, dt * gp.speed.max(0.0));
        let after = dominant(player.current, player.cycles, &params);
        if let Some((ci, now)) = after {
            // After a state change the new clip starts from the beginning.
            let prev = match before {
                Some((bc, t)) if bc == ci && started == player.current => t,
                _ => -1e-4,
            };
            fire(e, &model, mr.model, ci, prev, now, &mut events);
            if gp.root_motion {
                motions.push((e, find_node(&model, &gp.root_node), ci, prev.max(0.0), now));
            }
            gp.prev_time = now;
        }
        gp.state = player.current_name(&graph).to_string();
        gp.cycles = player.cycles;
        match player.fading {
            Some((s, c, p, d)) => {
                gp.fade_from = graph.states.get(s).map(|st| st.name.clone()).unwrap_or_default();
                gp.fade_cycles = c;
                gp.fade_progress = p;
                gp.fade_duration = d;
            }
            None => {
                gp.fade_from.clear();
                gp.fade_duration = 0.0;
            }
        }
        // Consumed triggers reset to 0.
        for (n, v) in gp.param_names.iter().zip(gp.param_values.iter_mut()) {
            if *v > 0.5 && params.triggers.get(n) == Some(&false) {
                *v = 0.0;
            }
        }
    }

    for (e, delta_root, ci, prev, now) in motions {
        let Some(mr) = world.get::<MeshRenderer>(e) else { continue };
        let Some(model) = db.model_loaded(mr.model) else { continue };
        let Some(root) = delta_root else { continue };
        let d = anim::root_motion_delta(&model, ci, root, prev, now);
        if let Some(t) = world.get_mut::<Transform>(e) {
            // Horizontal motion and yaw only: the rest stays in the pose.
            let step = Vec3::new(d.translation.x, 0.0, d.translation.z);
            t.translation += t.rotation * (step * t.scale);
            let (yaw, _, _) = d.rotation.to_euler(dumb_core::EulerRot::YXZ);
            t.rotation = (Quat::from_rotation_y(yaw) * t.rotation).normalize();
        }
    }

    let springs: Vec<Entity> = world.component_id_of::<dumb_ecs::SpringChain>().map(|id| world.entities_with(id).to_vec()).unwrap_or_default();
    for e in springs {
        let Some(mr) = world.get::<MeshRenderer>(e) else { continue };
        let Some(model) = db.model_loaded(mr.model) else { continue };
        if let Some((tips, prevs)) = simulate_springs(world, db, e, &model, dt) {
            if let Some(s) = world.get_mut::<dumb_ecs::SpringChain>(e) {
                s.tips = tips;
                s.prevs = prevs;
                s.primed = true;
            }
        }
    }

    for ev in events {
        world.send_event(ev);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use dumb_asset::model::Node;
    use dumb_ecs::{Name, Transform};

    fn chain_model(n: usize) -> ModelData {
        let mut m = ModelData::default();
        for i in 0..n {
            m.nodes.push(Node {
                name: format!("b{i}"),
                parent: i.checked_sub(1),
                children: if i + 1 < n { vec![i + 1] } else { vec![] },
                local: Trs { translation: if i == 0 { Vec3::ZERO } else { Vec3::Y }, ..Default::default() },
                mesh: None,
                skin: None,
            });
        }
        m.roots = vec![0];
        m
    }

    fn db() -> AssetDatabase {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dumb_anim_test_{}_{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        AssetDatabase::open(dir).unwrap()
    }

    #[test]
    fn ik_constraint_moves_chain_end_to_target_entity() {
        let model = chain_model(3);
        let mut w = World::new();
        let owner = w.spawn();
        w.insert(owner, Transform::default());
        let target = w.spawn();
        w.insert(target, Transform { translation: Vec3::new(1.0, 1.0, 0.0), ..Default::default() });
        w.update_transforms();
        w.insert(owner, IkConstraint { chain: vec!["b0".into(), "b1".into(), "b2".into()], target, ..Default::default() });
        let pose = entity_pose(&w, &db(), owner, &model).expect("constraint yields a pose");
        let end = anim::node_position(&model, &pose, 2);
        assert!((end - Vec3::new(1.0, 1.0, 0.0)).length() < 1e-2, "end at {end:?}");
    }

    #[test]
    fn no_animation_means_no_pose() {
        let model = chain_model(2);
        let mut w = World::new();
        let e = w.spawn();
        w.insert(e, Name::new("x"));
        assert!(entity_pose(&w, &db(), e, &model).is_none());
    }

    #[test]
    fn spring_chain_primes_and_lags() {
        let model = chain_model(2);
        let mut w = World::new();
        let e = w.spawn();
        w.insert(e, Transform::default());
        w.insert(e, SpringChain { nodes: vec!["b0".into()], gravity: Vec3::ZERO, ..Default::default() });
        let (tips, prevs) = simulate_springs(&w, &db(), e, &model, 1.0 / 60.0).unwrap();
        assert!((tips[0] - Vec3::Y).length() < 1e-4);
        {
            let s = w.get_mut::<SpringChain>(e).unwrap();
            s.tips = tips;
            s.prevs = prevs;
            s.primed = true;
        }
        // Gravity pulls the primed tip off the rest direction.
        w.get_mut::<SpringChain>(e).unwrap().gravity = Vec3::new(30.0, 0.0, 0.0);
        let (tips, _) = simulate_springs(&w, &db(), e, &model, 1.0 / 60.0).unwrap();
        assert!(tips[0].x > 0.0);
        assert!((tips[0].length() - 1.0).abs() < 1e-3, "bone length preserved");
    }

    #[test]
    fn graph_params_expose_bools_and_triggers() {
        let mut p = AnimGraphPlayer::default();
        p.set_float("speed", 2.0);
        p.set_bool("grounded", true);
        p.trigger("jump");
        let ap = params_of(&p);
        assert_eq!(ap.float("speed"), 2.0);
        assert_eq!(ap.bools["grounded"], true);
        assert_eq!(ap.triggers["jump"], true);
        assert_eq!(p.float("jump"), 1.0);
    }

    #[test]
    fn animgraph_asset_loads_and_rejects_invalid() {
        let db_ = {
            let dir = std::env::temp_dir().join(format!("dumb_anim_graph_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let mut d = AssetDatabase::open(&dir).unwrap();
            std::fs::write(dir.join("Assets/ok.animgraph"), r#"{ "states": [ { "name": "a", "motion": {"Clip": {"clip": "x"}} } ] }"#).unwrap();
            std::fs::write(dir.join("Assets/bad.animgraph"), "{ nope").unwrap();
            d.scan();
            d
        };
        let ok = db_.id_for_path("ok.animgraph").unwrap();
        assert_eq!(db_.anim_graph(ok).unwrap().states.len(), 1);
        let bad = db_.id_for_path("bad.animgraph").unwrap();
        assert!(db_.anim_graph(bad).is_none());
    }
}

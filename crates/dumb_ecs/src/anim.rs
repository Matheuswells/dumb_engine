//! Animation components: animation graphs, procedural constraints and the events they emit.
//!
//! All runtime state lives in plain reflected fields (hidden in the inspector) so it survives
//! scene saves, script reloads and replication; the math is in `dumb_asset::anim`.

use dumb_core::{AssetId, Entity, Vec3};
use dumb_derive::{Component, Editor};

/// Fired (via `world.send_event`) when playback passes an `AnimationEvent` marker authored in
/// the Model Viewer. Read with `world.events::<AnimEvent>()`.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimEvent {
    pub entity: Entity,
    pub name: String,
}

/// Drives an entity's `MeshRenderer` with an animation graph (`.animgraph`): a state machine
/// whose states hold blend trees. Scripts set parameters; transitions fire from them.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct AnimGraphPlayer {
    #[editor(asset = "animgraph")]
    pub graph: AssetId,
    #[editor(range = 0.0..=4.0)]
    pub speed: f32,
    /// Current state name (empty = the graph's entry state).
    #[editor(readonly)]
    pub state: String,
    #[editor(hidden)]
    pub cycles: f32,
    #[editor(hidden)]
    pub fade_from: String,
    #[editor(hidden)]
    pub fade_cycles: f32,
    #[editor(hidden)]
    pub fade_progress: f32,
    #[editor(hidden)]
    pub fade_duration: f32,
    /// Parameters: bools are 0/1, triggers are set to 1 and reset to 0 when consumed.
    #[editor(hidden)]
    pub param_names: Vec<String>,
    #[editor(hidden)]
    pub param_values: Vec<f32>,
    /// Apply the root node's motion to this entity's `Transform` and play the clip in place.
    pub root_motion: bool,
    /// Root node name for root motion (empty = first root node).
    pub root_node: String,
    #[editor(hidden)]
    pub prev_time: f32,
}

impl Default for AnimGraphPlayer {
    fn default() -> Self {
        AnimGraphPlayer {
            graph: AssetId::NONE,
            speed: 1.0,
            state: String::new(),
            cycles: 0.0,
            fade_from: String::new(),
            fade_cycles: 0.0,
            fade_progress: 0.0,
            fade_duration: 0.0,
            param_names: Vec::new(),
            param_values: Vec::new(),
            root_motion: false,
            root_node: String::new(),
            prev_time: 0.0,
        }
    }
}

impl AnimGraphPlayer {
    pub fn set_float(&mut self, name: &str, v: f32) {
        match self.param_names.iter().position(|n| n == name) {
            Some(i) => self.param_values[i] = v,
            None => {
                self.param_names.push(name.to_string());
                self.param_values.push(v);
            }
        }
    }

    pub fn float(&self, name: &str) -> f32 {
        self.param_names.iter().position(|n| n == name).map_or(0.0, |i| self.param_values[i])
    }

    pub fn set_bool(&mut self, name: &str, v: bool) {
        self.set_float(name, if v { 1.0 } else { 0.0 });
    }

    /// Fire a trigger parameter; the first transition that uses it consumes it.
    pub fn trigger(&mut self, name: &str) {
        self.set_float(name, 1.0);
    }
}

/// Inverse kinematics on a bone chain of the entity's model. Two bones use analytic two-bone IK
/// (arm/leg with a pole target); other lengths use FABRIK. Foot placement is this with a foot
/// chain and a `target` entity a script keeps on the ground (e.g. from a physics raycast).
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct IkConstraint {
    /// Node names root-first (2 bones = 3 names for two-bone IK).
    pub chain: Vec<String>,
    /// Entity whose world position the chain end reaches for.
    pub target: Entity,
    /// Entity the middle joint bends towards (two-bone IK only).
    pub pole: Entity,
    #[editor(range = 0.0..=1.0)]
    pub weight: f32,
    #[editor(range = 1.0..=32.0)]
    pub iterations: f32,
}

impl Default for IkConstraint {
    fn default() -> Self {
        IkConstraint { chain: Vec::new(), target: Entity::NONE, pole: Entity::NONE, weight: 1.0, iterations: 10.0 }
    }
}

/// Turns a bone so its forward axis points at a target entity (head/eyes/turrets).
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct LookAtConstraint {
    pub node: String,
    pub target: Entity,
    /// Local axis of the bone that should face the target.
    pub forward: Vec3,
    #[editor(range = 0.0..=1.0)]
    pub weight: f32,
    #[editor(range = 0.0..=180.0)]
    pub max_angle: f32,
}

impl Default for LookAtConstraint {
    fn default() -> Self {
        LookAtConstraint { node: String::new(), target: Entity::NONE, forward: Vec3::Z, weight: 1.0, max_angle: 70.0 }
    }
}

/// Spring bones (hair, tails, capes): each listed node swings after its animated pose with
/// inertia and damping. The bone points towards its first child node.
#[derive(Component, Editor, Clone, Debug, PartialEq)]
pub struct SpringChain {
    pub nodes: Vec<String>,
    #[editor(range = 0.0..=1.0)]
    pub stiffness: f32,
    #[editor(range = 0.0..=1.0)]
    pub damping: f32,
    pub gravity: Vec3,
    #[editor(hidden)]
    pub tips: Vec<Vec3>,
    #[editor(hidden)]
    pub prevs: Vec<Vec3>,
    #[editor(hidden)]
    pub primed: bool,
}

impl Default for SpringChain {
    fn default() -> Self {
        SpringChain {
            nodes: Vec::new(),
            stiffness: 0.2,
            damping: 0.1,
            gravity: Vec3::new(0.0, -2.0, 0.0),
            tips: Vec::new(),
            prevs: Vec::new(),
            primed: false,
        }
    }
}

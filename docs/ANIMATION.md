# Animation

Clip playback is the `Animator` component. Everything below builds on it.

## Animation graphs (`.animgraph`)

A JSON asset: a state machine whose states hold blend trees. Add `AnimGraphPlayer` to an entity
with a `MeshRenderer` and pick the graph. Double-clicking the asset opens it in the code editor.

```json
{ "entry": "move", "states": [
  { "name": "move",
    "motion": { "Blend1D": { "param": "speed", "children": [
      [0.0, { "Clip": { "clip": "Idle" } }], [1.0, { "Clip": { "clip": "Walk" } }], [3.0, { "Clip": { "clip": "Run" } }] ] } },
    "transitions": [ { "to": "jump", "when": [ { "Trigger": "jump" } ], "duration": 0.1 } ] },
  { "name": "jump", "looping": false, "motion": { "Clip": { "clip": "Jump" } },
    "transitions": [ { "to": "move", "when": [ "Finished" ] } ] } ] }
```

Blend nodes: `Clip`, `Blend1D` (thresholds), `Mix` (two children by a 0..1 param), `Blend2D`
(inverse-distance weights over `[x, y]` samples). Conditions: `Greater`, `Less`, `Is`, `Not`,
`Trigger`, `Finished`. Scripts drive it with `player.set_float("speed", v)`, `set_bool`,
`trigger("jump")`; read the current state from `player.state`.

## Events and root motion

Markers authored in the Model Viewer fire as `AnimEvent { entity, name }`
(`world.events::<AnimEvent>()`) for both `Animator` clips and graphs. `root_motion` on `Animator` /
`AnimGraphPlayer` moves the entity's `Transform` by the root node's horizontal motion and yaw and
plays the clip in place.

## Procedural animation

Applied after the clip/graph pose, in this order: `IkConstraint` (3 names = two-bone IK with an
optional pole entity, otherwise FABRIK), `LookAtConstraint`, `SpringChain`. Targets are entities.
Foot placement is an `IkConstraint` on a leg chain whose target entity a script keeps on the
ground (for example from a physics raycast).

## Morph targets

glTF morph targets are imported. `MorphWeights.set(index, weight)` blends them; the renderer draws
a per-entity copy of the model rebuilt only when the weights change.

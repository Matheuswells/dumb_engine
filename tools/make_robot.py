# Builds a small rigged, animated robot and saves it as a .blend.
# Run: blender --background --factory-startup --python tools/make_robot.py -- <out.blend>
import bpy, sys, math

out = sys.argv[sys.argv.index("--") + 1]
bpy.ops.wm.read_factory_settings(use_empty=True)
scene = bpy.context.scene
scene.render.fps = 30


def material(name, rgb, metallic=0.0, rough=0.5, emit=None):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    bsdf = m.node_tree.nodes["Principled BSDF"]
    bsdf.inputs["Base Color"].default_value = (*rgb, 1.0)
    bsdf.inputs["Metallic"].default_value = metallic
    bsdf.inputs["Roughness"].default_value = rough
    if emit:
        bsdf.inputs["Emission Color"].default_value = (*emit, 1.0)
        bsdf.inputs["Emission Strength"].default_value = 4.0
    return m


body_mat = material("RobotBody", (0.85, 0.45, 0.12), metallic=0.6, rough=0.35)
dark_mat = material("RobotJoints", (0.08, 0.08, 0.09), metallic=0.8, rough=0.4)
eye_mat = material("RobotEyes", (0.1, 0.8, 1.0), emit=(0.2, 0.9, 1.0))

# --- armature -------------------------------------------------------------
bpy.ops.object.armature_add(location=(0, 0, 0))
arm = bpy.context.object
arm.name = "RobotRig"
arm.data.name = "RobotRig"
bpy.ops.object.mode_set(mode="EDIT")
eb = arm.data.edit_bones
root = eb[0]
root.name = "root"
root.head = (0, 0, 0.0)
root.tail = (0, 0, 0.3)


def bone(name, head, tail, parent):
    b = eb.new(name)
    b.head, b.tail = head, tail
    b.parent = eb[parent]
    return b


bone("hips", (0, 0, 0.95), (0, 0, 1.15), "root")
bone("spine", (0, 0, 1.15), (0, 0, 1.6), "hips")
bone("head", (0, 0, 1.6), (0, 0, 1.95), "spine")
for side, x in (("L", 1), ("R", -1)):
    bone(f"upper_arm.{side}", (0.32 * x, 0, 1.52), (0.32 * x, 0, 1.22), "spine")
    bone(f"forearm.{side}", (0.32 * x, 0, 1.22), (0.32 * x, 0, 0.92), f"upper_arm.{side}")
    bone(f"thigh.{side}", (0.13 * x, 0, 0.95), (0.13 * x, 0, 0.5), "hips")
    bone(f"shin.{side}", (0.13 * x, 0, 0.5), (0.13 * x, 0, 0.08), f"thigh.{side}")
bpy.ops.object.mode_set(mode="OBJECT")

# --- mesh parts, each rigidly bound to one bone ---------------------------
parts = []


def part(kind, name, loc, scale, bone_name, mat):
    if kind == "cube":
        bpy.ops.mesh.primitive_cube_add(size=1, location=loc)
    elif kind == "cyl":
        bpy.ops.mesh.primitive_cylinder_add(vertices=16, radius=0.5, depth=1, location=loc)
    else:
        bpy.ops.mesh.primitive_uv_sphere_add(segments=16, ring_count=8, radius=0.5, location=loc)
    o = bpy.context.object
    o.name = name
    o.scale = scale
    bpy.ops.object.transform_apply(scale=True)
    o.data.materials.append(mat)
    vg = o.vertex_groups.new(name=bone_name)
    vg.add([v.index for v in o.data.vertices], 1.0, "REPLACE")
    parts.append(o)


part("cube", "Torso", (0, 0, 1.35), (0.5, 0.3, 0.5), "spine", body_mat)
part("cube", "Pelvis", (0, 0, 1.02), (0.36, 0.24, 0.16), "hips", dark_mat)
part("cube", "Head", (0, 0, 1.78), (0.34, 0.3, 0.3), "head", body_mat)
part("sphere", "Eye.L", (0.08, -0.15, 1.8), (0.07, 0.04, 0.07), "head", eye_mat)
part("sphere", "Eye.R", (-0.08, -0.15, 1.8), (0.07, 0.04, 0.07), "head", eye_mat)
for side, x in (("L", 1), ("R", -1)):
    part("cyl", f"UpperArm.{side}", (0.32 * x, 0, 1.37), (0.12, 0.12, 0.3), f"upper_arm.{side}", body_mat)
    part("cyl", f"Forearm.{side}", (0.32 * x, 0, 1.07), (0.1, 0.1, 0.3), f"forearm.{side}", dark_mat)
    part("cyl", f"Thigh.{side}", (0.13 * x, 0, 0.72), (0.15, 0.15, 0.45), f"thigh.{side}", body_mat)
    part("cyl", f"Shin.{side}", (0.13 * x, 0, 0.29), (0.12, 0.12, 0.42), f"shin.{side}", dark_mat)

bpy.ops.object.select_all(action="DESELECT")
for o in parts:
    o.select_set(True)
bpy.context.view_layer.objects.active = parts[0]
bpy.ops.object.join()
mesh = bpy.context.object
mesh.name = "Robot"
mod = mesh.modifiers.new("Armature", "ARMATURE")
mod.object = arm
mesh.parent = arm

# Simple collision proxy (picked up by the engine as a collision node).
bpy.ops.mesh.primitive_cube_add(size=1, location=(0, 0, 1.0))
col = bpy.context.object
col.name = "UCX_Robot"
col.scale = (0.7, 0.45, 2.0)
bpy.ops.object.transform_apply(scale=True)

# --- animations -----------------------------------------------------------
bpy.context.view_layer.objects.active = arm
bpy.ops.object.mode_set(mode="POSE")
pb = arm.pose.bones
for b in pb:
    b.rotation_mode = "XYZ"


def key(frame, rots, hips_z=0.0, root_y=None):
    scene.frame_set(frame)
    for b in pb:
        b.rotation_euler = rots.get(b.name, (0, 0, 0))
        b.keyframe_insert("rotation_euler", frame=frame)
    pb["hips"].location = (0, hips_z, 0)
    pb["hips"].keyframe_insert("location", frame=frame)
    if root_y is not None:
        # Bone-local Z of an up-pointing bone is world -Y (Blender forward).
        pb["root"].location = (0, 0, -root_y)
        pb["root"].keyframe_insert("location", frame=frame)


def new_action(name):
    act = bpy.data.actions.new(name)
    arm.animation_data_create()
    arm.animation_data.action = act
    return act


def push_strip(act):
    track = arm.animation_data.nla_tracks.new()
    track.name = act.name
    track.strips.new(act.name, int(act.frame_range[0]), act)
    arm.animation_data.action = None


s = 0.6
walk = new_action("Walk")
for f, ph in ((1, 1), (16, -1), (31, 1)):
    key(f, {
        "thigh.L": (s * ph, 0, 0), "thigh.R": (-s * ph, 0, 0),
        "shin.L": (-0.5 * max(0, -ph), 0, 0), "shin.R": (-0.5 * max(0, ph), 0, 0),
        "upper_arm.L": (-0.5 * ph, 0, 0), "upper_arm.R": (0.5 * ph, 0, 0),
        "forearm.L": (-0.4, 0, 0), "forearm.R": (-0.4, 0, 0),
    }, root_y=-(f - 1) / 30.0 * 1.2)
for f in (8, 23):
    scene.frame_set(f)
    pb["hips"].location = (0, 0.05, 0)
    pb["hips"].keyframe_insert("location", frame=f)
push_strip(walk)

wave = new_action("Wave")
for f, a in ((1, 0.0), (10, 1.0), (20, 0.6), (30, 1.0), (40, 0.6), (50, 0.0)):
    key(f, {
        "upper_arm.R": (0, 0, -2.6 * min(a * 2, 1)), "forearm.R": (0, 0, -0.5 * a),
        "head": (0, 0, 0.3 * a), "spine": (0, 0.1 * a, 0),
    })
push_strip(wave)

idle = new_action("Idle")
for f, a in ((1, 0.0), (30, 1.0), (60, 0.0)):
    key(f, {"spine": (0.05 * a, 0, 0), "head": (-0.08 * a, 0.1 * a, 0), "upper_arm.L": (0, 0, 0.1 * a), "upper_arm.R": (0, 0, -0.1 * a)}, hips_z=-0.02 * a)
push_strip(idle)

bpy.ops.object.mode_set(mode="OBJECT")
bpy.ops.wm.save_as_mainfile(filepath=out)
print("ROBOT_SAVED", out)

// Debug lines: grid, bounds, skeletons, gizmo helpers.

struct PointLight {
    pos_range: vec4<f32>,
    color_intensity: vec4<f32>,
}

struct View {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    ambient_sky: vec4<f32>,
    ambient_ground: vec4<f32>,
    params: vec4<f32>,
    points: array<PointLight, 8>,
}

@group(0) @binding(0) var<uniform> view: View;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) color: vec4<f32>) -> VOut {
    var out: VOut;
    out.clip = view.view_proj * vec4<f32>(pos, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    return in.color;
}

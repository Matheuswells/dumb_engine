// Equirectangular skybox drawn with one fullscreen triangle behind everything.

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
    // x = point light count, y = time, z = exposure, w = sky mip count (0 = no sky)
    params: vec4<f32>,
    points: array<PointLight, 8>,
}

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(2) var sky_tex: texture_2d<f32>;
@group(0) @binding(3) var sky_samp: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    let ndc = uv * 2.0 - vec2<f32>(1.0);
    var out: VOut;
    // Far plane (depth 1) so it never covers geometry.
    out.clip = vec4<f32>(ndc, 1.0, 1.0);
    out.ndc = ndc;
    return out;
}

const PI: f32 = 3.14159265;

fn equirect_uv(d: vec3<f32>) -> vec2<f32> {
    let n = normalize(d);
    return vec2<f32>(0.5 + atan2(n.x, -n.z) / (2.0 * PI), acos(clamp(n.y, -1.0, 1.0)) / PI);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let far = view.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir = far.xyz / far.w - view.camera_pos.xyz;
    let c = textureSampleLevel(sky_tex, sky_samp, equirect_uv(dir), 0.0).rgb;
    return vec4<f32>(pow(c * view.params.z, vec3<f32>(1.0 / 2.2)), 1.0);
}

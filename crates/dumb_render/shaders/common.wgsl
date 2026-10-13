// Shared by the built-in mesh shader, user surface shaders and post effects: view and material
// bindings, vertex input, instancing, PBR lighting and sun shadows.

struct PointLight {
    pos_range: vec4<f32>,
    color_intensity: vec4<f32>,
}

struct View {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // xyz = direction the light travels, w = intensity
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    ambient_sky: vec4<f32>,
    ambient_ground: vec4<f32>,
    // x = point light count, y = time, z = exposure, w = sky mip count (0 = no sky)
    params: vec4<f32>,
    points: array<PointLight, 8>,
    shadow_mats: array<mat4x4<f32>, 4>,
    // view-depth where each cascade ends
    shadow_splits: vec4<f32>,
    // world size of a shadow texel per cascade
    shadow_texel: vec4<f32>,
    // x = enabled, y = strength
    shadow_params: vec4<f32>,
    cam_forward: vec4<f32>,
    // target size in pixels (xy) and its inverse (zw)
    screen: vec4<f32>,
}

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage, read> joints: array<mat4x4<f32>>;
@group(0) @binding(2) var sky_tex: texture_2d<f32>;
@group(0) @binding(3) var sky_samp: sampler;
@group(0) @binding(5) var shadow_tex: texture_depth_2d_array;
@group(0) @binding(6) var shadow_samp: sampler_comparison;

const PI: f32 = 3.14159265;

fn equirect_uv(d: vec3<f32>) -> vec2<f32> {
    let n = normalize(d);
    return vec2<f32>(0.5 + atan2(n.x, -n.z) / (2.0 * PI), acos(clamp(n.y, -1.0, 1.0)) / PI);
}

@group(1) @binding(0) var t_albedo: texture_2d<f32>;
@group(1) @binding(1) var t_normal: texture_2d<f32>;
@group(1) @binding(2) var t_mr: texture_2d<f32>;
@group(1) @binding(3) var t_ao: texture_2d<f32>;
@group(1) @binding(4) var t_emission: texture_2d<f32>;
@group(1) @binding(5) var t_metal: texture_2d<f32>;
@group(1) @binding(6) var samp: sampler;

struct Push {
    albedo: vec4<f32>,
    // rgb * w
    emission: vec4<f32>,
    // metallic, roughness, normal_scale, ao_strength
    pbr: vec4<f32>,
    // uv tiling (xy), uv offset (zw)
    uv: vec4<f32>,
    // material flags, alpha_cutoff bits, shadow cascade, unused
    misc: vec4<u32>,
    // free parameters for custom shaders (material "Shader params")
    params0: vec4<f32>,
    params1: vec4<f32>,
}
var<push_constant> pc: Push;

// Per-object data, indexed by instance_index (one instanced draw per mesh+material batch).
struct Instance {
    // Rows 0..2 of the affine model matrix (row 3 is 0,0,0,1).
    model_r0: vec4<f32>,
    model_r1: vec4<f32>,
    model_r2: vec4<f32>,
    tint: vec4<f32>,
    // joint_offset, instance flags (skinned, selected), unused, unused
    misc: vec4<u32>,
}
@group(0) @binding(4) var<storage, read> instances: array<Instance>;

const FLAG_SKINNED: u32 = 1u;
const FLAG_SELECTED: u32 = 2u;
const FLAG_ALPHA_MASK: u32 = 4u;
const FLAG_UNLIT: u32 = 8u;
const FLAG_NORMAL_DX: u32 = 16u;
const FLAG_ROUGHNESS_MAP: u32 = 32u;
const FLAG_SMOOTHNESS_MAP: u32 = 64u;

fn model_matrix(inst: Instance) -> mat4x4<f32> {
    let a = inst.model_r0;
    let b = inst.model_r1;
    let c = inst.model_r2;
    return mat4x4<f32>(
        vec4<f32>(a.x, b.x, c.x, 0.0),
        vec4<f32>(a.y, b.y, c.y, 0.0),
        vec4<f32>(a.z, b.z, c.z, 0.0),
        vec4<f32>(a.w, b.w, c.w, 1.0),
    );
}

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
    @location(4) joint_ids: vec4<u32>,
    @location(5) weights: vec4<f32>,
}

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
    @location(4) tint: vec4<f32>,
    @location(5) @interpolate(flat) inst_flags: u32,
    // The object's origin in world space.
    @location(6) @interpolate(flat) object_pos: vec3<f32>,
}

// Model matrix of an instance, including skinning.
fn world_matrix(inst: Instance, v: VIn) -> mat4x4<f32> {
    let model = model_matrix(inst);
    if ((inst.misc.y & FLAG_SKINNED) != 0u) {
        let o = inst.misc.x;
        let skin = joints[o + v.joint_ids.x] * v.weights.x
                 + joints[o + v.joint_ids.y] * v.weights.y
                 + joints[o + v.joint_ids.z] * v.weights.z
                 + joints[o + v.joint_ids.w] * v.weights.w;
        return model * skin;
    }
    return model;
}

fn d_ggx(n_h: f32, a: f32) -> f32 {
    let a2 = a * a;
    let d = n_h * n_h * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d + 1e-7);
}

fn g_smith(n_v: f32, n_l: f32, rough: f32) -> f32 {
    let k = (rough + 1.0) * (rough + 1.0) / 8.0;
    let gv = n_v / (n_v * (1.0 - k) + k);
    let gl = n_l / (n_l * (1.0 - k) + k);
    return gv * gl;
}

fn fresnel(cos_t: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - cos_t, 5.0);
}

fn brdf(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, albedo: vec3<f32>, metal: f32, rough: f32) -> vec3<f32> {
    let h = normalize(v + l);
    let n_l = max(dot(n, l), 0.0);
    let n_v = max(dot(n, v), 1e-4);
    let n_h = max(dot(n, h), 0.0);
    let f0 = mix(vec3<f32>(0.04), albedo, metal);
    let f = fresnel(max(dot(h, v), 0.0), f0);
    let spec = d_ggx(n_h, rough * rough) * g_smith(n_v, n_l, rough) * f / (4.0 * n_v * n_l + 1e-4);
    let kd = (vec3<f32>(1.0) - f) * (1.0 - metal);
    return (kd * albedo / PI + spec) * n_l;
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Linear HDR color -> display color (exposure, ACES, gamma).
fn tonemap(c: vec3<f32>) -> vec3<f32> {
    return pow(aces(c * view.params.z), vec3<f32>(1.0 / 2.2));
}

// Hemisphere ambient light for normal `n`.
fn ambient(n: vec3<f32>) -> vec3<f32> {
    return mix(view.ambient_ground.rgb, view.ambient_sky.rgb, n.y * 0.5 + 0.5);
}

// Standard lighting: sun (with shadows), point lights, ambient and sky reflections.
// `geo_n` is the geometric normal (used for the shadow lookup), `n` the shading normal.
fn pbr_lighting(world_pos: vec3<f32>, geo_n: vec3<f32>, n: vec3<f32>, base: vec3<f32>, metal: f32, rough: f32, ao: f32) -> vec3<f32> {
    let v = normalize(view.camera_pos.xyz - world_pos);
    let l = normalize(-view.light_dir.xyz);
    let shadow = sun_shadow(world_pos, geo_n);
    var color = brdf(n, v, l, base, metal, rough) * view.light_color.rgb * view.light_dir.w * shadow;

    let count = u32(view.params.x);
    for (var i = 0u; i < count; i++) {
        let p = view.points[i];
        let to_l = p.pos_range.xyz - world_pos;
        let dist = length(to_l);
        let falloff = clamp(1.0 - pow(dist / p.pos_range.w, 4.0), 0.0, 1.0);
        let atten = falloff * falloff / (dist * dist + 1.0);
        color += brdf(n, v, to_l / dist, base, metal, rough) * p.color_intensity.rgb * p.color_intensity.w * atten;
    }

    let hemi = ambient(n);
    let f0 = mix(vec3<f32>(0.04), base, metal);
    let fr = fresnel(max(dot(n, v), 0.0), f0);
    var spec_amb = fr * hemi * (1.0 - rough) * 0.5;
    if (view.params.w > 0.0) {
        // Reflections from the sky: blurrier mips for rougher surfaces.
        let r = reflect(-v, n);
        let env = textureSampleLevel(sky_tex, sky_samp, equirect_uv(r), rough * view.params.w).rgb;
        // Rough surfaces reflect a blurred sky but much more weakly.
        let gloss = 1.0 - rough;
        spec_amb = fr * env * (gloss * gloss * 0.9 + 0.05);
    }
    color += (hemi * base * (1.0 - metal) + spec_amb) * ao;
    return color;
}

const SHADOW_POISSON = array<vec2<f32>, 16>(
    vec2<f32>(-0.94201624, -0.39906216), vec2<f32>(0.94558609, -0.76890725),
    vec2<f32>(-0.09418410, -0.92938870), vec2<f32>(0.34495938, 0.29387760),
    vec2<f32>(-0.91588581, 0.45771432), vec2<f32>(-0.81544232, -0.87912464),
    vec2<f32>(-0.38277543, 0.27676845), vec2<f32>(0.97484398, 0.75648379),
    vec2<f32>(0.44323325, -0.97511554), vec2<f32>(0.53742981, -0.47373420),
    vec2<f32>(-0.26496911, -0.41893023), vec2<f32>(0.79197514, 0.19090188),
    vec2<f32>(-0.24188840, 0.99706507), vec2<f32>(-0.81409955, 0.91437590),
    vec2<f32>(0.19984126, 0.78641367), vec2<f32>(0.14383161, -0.14100790),
);

// 0 = fully shadowed, 1 = lit.
fn sun_shadow(world_pos: vec3<f32>, n: vec3<f32>) -> f32 {
    if (view.shadow_params.x < 0.5) {
        return 1.0;
    }
    let depth = dot(world_pos - view.camera_pos.xyz, view.cam_forward.xyz);
    if (depth > view.shadow_splits.w) {
        return 1.0;
    }
    var c = 0u;
    if (depth > view.shadow_splits.x) { c = 1u; }
    if (depth > view.shadow_splits.y) { c = 2u; }
    if (depth > view.shadow_splits.z) { c = 3u; }
    let texel = view.shadow_texel[c];
    // Normal offset: push the lookup out of the surface to avoid acne.
    let l = normalize(-view.light_dir.xyz);
    let slope = 1.0 - clamp(dot(n, l), 0.0, 1.0);
    let p = world_pos + n * texel * (1.0 + 2.0 * slope);
    let lp = view.shadow_mats[c] * vec4<f32>(p, 1.0);
    let ndc = lp.xyz / lp.w;
    let uv = ndc.xy * 0.5 + vec2<f32>(0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        return 1.0;
    }
    // 16 Poisson-disc taps (on top of the hardware's 2x2 compare filtering), the disc rotated
    // per point so the penumbra is smooth instead of stepping along shadow-map texels.
    let size = 1.0 / f32(textureDimensions(shadow_tex).x);
    let angle = 6.2831853 * fract(sin(dot(world_pos, vec3<f32>(12.9898, 78.233, 37.719))) * 43758.5453);
    let rot = mat2x2<f32>(cos(angle), sin(angle), -sin(angle), cos(angle));
    var taps = SHADOW_POISSON;
    var lit = 0.0;
    for (var i = 0; i < 16; i++) {
        lit += textureSampleCompareLevel(shadow_tex, shadow_samp, uv + rot * taps[i] * (1.75 * size), i32(c), ndc.z);
    }
    lit = lit / 16.0;
    // Fade out at the end of the shadow distance.
    let fade = clamp((view.shadow_splits.w - depth) / (view.shadow_splits.w * 0.1), 0.0, 1.0);
    return mix(1.0, lit, view.shadow_params.y * fade);
}

// ---- shadow pass (shared: custom shaders cast shadows with their undisplaced geometry)

struct ShadowOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// Depth-only pass into cascade `pc.misc.z`.
@vertex
fn vs_shadow(v: VIn, @builtin(instance_index) ii: u32) -> ShadowOut {
    let inst = instances[ii];
    let wp = world_matrix(inst, v) * vec4<f32>(v.pos, 1.0);
    var out: ShadowOut;
    out.clip = view.shadow_mats[pc.misc.z] * wp;
    out.uv = v.uv * pc.uv.xy + pc.uv.zw;
    return out;
}

@fragment
fn fs_shadow(in: ShadowOut) {
    // Cut-out materials (leaves, fences) cast cut-out shadows.
    if ((pc.misc.x & FLAG_ALPHA_MASK) != 0u) {
        let a = textureSample(t_albedo, samp, in.uv).a * pc.albedo.a;
        if (a < bitcast<f32>(pc.misc.y)) {
            discard;
        }
    }
}

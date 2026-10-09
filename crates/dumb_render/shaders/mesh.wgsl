// Forward PBR (metallic-roughness) with GPU skinning.

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
}

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage, read> joints: array<mat4x4<f32>>;
@group(0) @binding(2) var sky_tex: texture_2d<f32>;
@group(0) @binding(3) var sky_samp: sampler;
@group(0) @binding(5) var shadow_tex: texture_depth_2d_array;
@group(0) @binding(6) var shadow_samp: sampler_comparison;

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
    // material flags, alpha_cutoff bits, unused, unused
    misc: vec4<u32>,
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

@vertex
fn vs_main(v: VIn, @builtin(instance_index) ii: u32) -> VOut {
    let inst = instances[ii];
    let m = world_matrix(inst, v);
    let wp = m * vec4<f32>(v.pos, 1.0);
    let nm = mat3x3<f32>(m[0].xyz, m[1].xyz, m[2].xyz);
    var out: VOut;
    out.clip = view.view_proj * wp;
    out.world_pos = wp.xyz;
    out.normal = normalize(nm * v.normal);
    out.tangent = vec4<f32>(normalize(nm * v.tangent.xyz + vec3<f32>(1e-6, 0.0, 0.0)), v.tangent.w);
    out.uv = v.uv * pc.uv.xy + pc.uv.zw;
    out.tint = inst.tint;
    out.inst_flags = inst.misc.y;
    return out;
}

const PI: f32 = 3.14159265;

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

@fragment
fn fs_main(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let albedo_tex = textureSample(t_albedo, samp, in.uv);
    let normal_tex = textureSample(t_normal, samp, in.uv).xyz;
    let mr_tex = textureSample(t_mr, samp, in.uv);
    let ao_tex = textureSample(t_ao, samp, in.uv).r;
    let emission_tex = textureSample(t_emission, samp, in.uv).rgb;
    let metal_tex = textureSample(t_metal, samp, in.uv).r;

    let base = albedo_tex * pc.albedo * in.tint;
    let flags = pc.misc.x;
    if ((flags & FLAG_ALPHA_MASK) != 0u && base.a < bitcast<f32>(pc.misc.y)) {
        discard;
    }

    var n = normalize(in.normal);
    if (!front) {
        n = -n;
    }
    let t_raw = in.tangent.xyz - n * dot(n, in.tangent.xyz);
    if (dot(t_raw, t_raw) > 1e-8) {
        let t = normalize(t_raw);
        let b = cross(n, t) * in.tangent.w;
        var tn = normal_tex * 2.0 - vec3<f32>(1.0);
        if ((flags & FLAG_NORMAL_DX) != 0u) {
            tn.y = -tn.y;
        }
        n = normalize(t * (tn.x * pc.pbr.z) + b * (tn.y * pc.pbr.z) + n * tn.z);
    }

    var metal = clamp(pc.pbr.x * mr_tex.b, 0.0, 1.0);
    var rough_in = pc.pbr.y * mr_tex.g;
    if ((flags & FLAG_ROUGHNESS_MAP) != 0u) {
        metal = pc.pbr.x * metal_tex;
        rough_in = pc.pbr.y * mr_tex.r;
    } else if ((flags & FLAG_SMOOTHNESS_MAP) != 0u) {
        metal = pc.pbr.x * metal_tex;
        rough_in = pc.pbr.y * (1.0 - mr_tex.r);
    }
    let rough = clamp(rough_in, 0.045, 1.0);
    let ao = mix(1.0, ao_tex, pc.pbr.w);
    let v = normalize(view.camera_pos.xyz - in.world_pos);

    var color = vec3<f32>(0.0);
    if ((flags & FLAG_UNLIT) != 0u) {
        color = base.rgb;
    } else {
        let l = normalize(-view.light_dir.xyz);
        let shadow = sun_shadow(in.world_pos, normalize(in.normal));
        color += brdf(n, v, l, base.rgb, metal, rough) * view.light_color.rgb * view.light_dir.w * shadow;

        let count = u32(view.params.x);
        for (var i = 0u; i < count; i++) {
            let p = view.points[i];
            let to_l = p.pos_range.xyz - in.world_pos;
            let dist = length(to_l);
            let falloff = clamp(1.0 - pow(dist / p.pos_range.w, 4.0), 0.0, 1.0);
            let atten = falloff * falloff / (dist * dist + 1.0);
            color += brdf(n, v, to_l / dist, base.rgb, metal, rough) * p.color_intensity.rgb * p.color_intensity.w * atten;
        }

        let hemi = mix(view.ambient_ground.rgb, view.ambient_sky.rgb, n.y * 0.5 + 0.5);
        let f0 = mix(vec3<f32>(0.04), base.rgb, metal);
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
        color += (hemi * base.rgb * (1.0 - metal) + spec_amb) * ao;
    }
    color += emission_tex * pc.emission.rgb * pc.emission.w;

    color = aces(color * view.params.z);
    color = pow(color, vec3<f32>(1.0 / 2.2));

    if ((in.inst_flags & FLAG_SELECTED) != 0u) {
        let rim = pow(1.0 - max(dot(n, v), 0.0), 2.0);
        color = mix(color, vec3<f32>(1.0, 0.55, 0.1), 0.15 + 0.6 * rim);
    }
    return vec4<f32>(color, base.a);
}

// ---- shadows

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
    // 3x3 PCF on top of the hardware's 2x2 compare filtering.
    let size = 1.0 / f32(textureDimensions(shadow_tex).x);
    var lit = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let o = vec2<f32>(f32(x), f32(y)) * size;
            lit += textureSampleCompareLevel(shadow_tex, shadow_samp, uv + o, i32(c), ndc.z);
        }
    }
    lit = lit / 9.0;
    // Fade out at the end of the shadow distance.
    let fade = clamp((view.shadow_splits.w - depth) / (view.shadow_splits.w * 0.1), 0.0, 1.0);
    return mix(1.0, lit, view.shadow_params.y * fade);
}

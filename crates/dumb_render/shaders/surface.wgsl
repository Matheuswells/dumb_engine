// Entry points for user surface shaders. A shader asset (.wgsl) only defines
//
//     fn surface(s: Surface) -> vec4<f32>      the pixel's final color (required)
//     fn vertex(v: Vertex) -> Vertex           moves vertices in world space (optional)
//
// and is compiled before this file and common.wgsl, which provide the bindings, lighting and
// the helpers below. Errors in user code keep the user file's line numbers.

// What `surface` gets for each pixel.
struct Surface {
    world_pos: vec3<f32>,
    // World normal, facing the camera on double-sided faces.
    normal: vec3<f32>,
    // From the surface toward the camera.
    view_dir: vec3<f32>,
    uv: vec2<f32>,
    // Material color × albedo texture × object tint.
    albedo: vec4<f32>,
    // Pixel coordinates in the view.
    pixel: vec2<f32>,
    // The object's origin in world space (e.g. a planet's center).
    object_pos: vec3<f32>,
    // Seconds since play started.
    time: f32,
}

// What `vertex` gets and returns.
struct Vertex {
    world_pos: vec3<f32>,
    normal: vec3<f32>,
    uv: vec2<f32>,
    // The object's origin in world space.
    object_pos: vec3<f32>,
}

// ---- helpers for shaders

fn time() -> f32 { return view.params.y; }
// The material's "Shader params" (two vec4s, edited in the material editor).
fn params0() -> vec4<f32> { return pc.params0; }
fn params1() -> vec4<f32> { return pc.params1; }
fn material_metallic() -> f32 { return pc.pbr.x; }
fn material_roughness() -> f32 { return pc.pbr.y; }
fn camera_pos() -> vec3<f32> { return view.camera_pos.xyz; }
// Direction toward the sun, and its color × intensity.
fn sun_direction() -> vec3<f32> { return normalize(-view.light_dir.xyz); }
fn sun_color() -> vec3<f32> { return view.light_color.rgb * view.light_dir.w; }
// 0 in the sun's shadow, 1 in sunlight.
fn shadow(s: Surface) -> f32 { return sun_shadow(s.world_pos, s.normal); }
// The engine's standard PBR lighting (sun, shadows, point lights, ambient, sky reflections),
// in linear HDR: pass it through `tonemap` before returning it.
fn lit(s: Surface, albedo: vec3<f32>, metallic: f32, roughness: f32) -> vec3<f32> {
    return pbr_lighting(s.world_pos, s.normal, s.normal, albedo, metallic, clamp(roughness, 0.045, 1.0), 1.0);
}
// The sky (or ambient color without a skybox) in direction `dir`, linear.
fn sky(dir: vec3<f32>, blur: f32) -> vec3<f32> {
    if (view.params.w > 0.0) {
        return textureSampleLevel(sky_tex, sky_samp, equirect_uv(dir), blur * view.params.w).rgb;
    }
    return ambient(dir);
}
// The material's textures.
fn albedo_texture(uv: vec2<f32>) -> vec4<f32> { return textureSample(t_albedo, samp, uv); }
fn emission_texture(uv: vec2<f32>) -> vec3<f32> { return textureSample(t_emission, samp, uv).rgb; }

// ---- entry points

@vertex
fn vs_main(v: VIn, @builtin(instance_index) ii: u32) -> VOut {
    let inst = instances[ii];
    let m = world_matrix(inst, v);
    let wp = m * vec4<f32>(v.pos, 1.0);
    let nm = mat3x3<f32>(m[0].xyz, m[1].xyz, m[2].xyz);
    var vd: Vertex;
    vd.world_pos = wp.xyz;
    vd.normal = normalize(nm * v.normal);
    vd.uv = v.uv * pc.uv.xy + pc.uv.zw;
    vd.object_pos = m[3].xyz;
    vd = vertex(vd);
    var out: VOut;
    out.clip = view.view_proj * vec4<f32>(vd.world_pos, 1.0);
    out.world_pos = vd.world_pos;
    out.normal = normalize(vd.normal);
    out.tangent = vec4<f32>(normalize(nm * v.tangent.xyz + vec3<f32>(1e-6, 0.0, 0.0)), v.tangent.w);
    out.uv = vd.uv;
    out.tint = inst.tint;
    out.inst_flags = inst.misc.y;
    out.object_pos = m[3].xyz;
    return out;
}

@fragment
fn fs_main(in: VOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let dx = dpdx(in.world_pos);
    let dy = dpdy(in.world_pos);
    let albedo = textureSample(t_albedo, samp, in.uv) * pc.albedo * in.tint;
    if ((pc.misc.x & FLAG_ALPHA_MASK) != 0u && albedo.a < bitcast<f32>(pc.misc.y)) {
        discard;
    }
    var n = normalize(in.normal);
    if (!front) {
        n = -n;
    }
    // Editor view modes skip the custom shading.
    let mode_color = view_mode_color(in.world_pos, dx, dy, n, albedo);
    if (mode_color.a >= 0.0) {
        return mode_color;
    }
    var s: Surface;
    s.world_pos = in.world_pos;
    s.normal = n;
    s.view_dir = normalize(view.camera_pos.xyz - in.world_pos);
    s.uv = in.uv;
    s.albedo = albedo;
    s.pixel = in.clip.xy;
    s.object_pos = in.object_pos;
    s.time = view.params.y;
    var color = surface(s);
    if ((in.inst_flags & FLAG_SELECTED) != 0u) {
        let rim = pow(1.0 - max(dot(n, s.view_dir), 0.0), 2.0);
        color = vec4<f32>(mix(color.rgb, vec3<f32>(1.0, 0.55, 0.1), 0.15 + 0.6 * rim), color.a);
    }
    return color;
}

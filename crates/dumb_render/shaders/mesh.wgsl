// Forward PBR (metallic-roughness) with GPU skinning. Compiled after common.wgsl.

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
    out.object_pos = m[3].xyz;
    return out;
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
    let geo_n = n;
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

    var color = vec3<f32>(0.0);
    if ((flags & FLAG_UNLIT) != 0u) {
        color = base.rgb;
    } else {
        color = pbr_lighting(in.world_pos, normalize(in.normal), n, base.rgb, metal, rough, ao);
    }
    color += emission_tex * pc.emission.rgb * pc.emission.w;
    color = tonemap(color);

    if ((in.inst_flags & FLAG_SELECTED) != 0u) {
        let v = normalize(view.camera_pos.xyz - in.world_pos);
        let rim = pow(1.0 - max(dot(n, v), 0.0), 2.0);
        color = mix(color, vec3<f32>(1.0, 0.55, 0.1), 0.15 + 0.6 * rim);
    }
    return vec4<f32>(color, base.a);
}

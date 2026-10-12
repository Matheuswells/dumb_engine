// Ink outlines, a post effect drawn over the finished image (comic / Borderlands style):
// dark lines where the depth jumps (silhouettes) and where surfaces fold sharply (creases).
// Compiled after common.wgsl. Parameters come in the push constants:
//   params0 = line color (rgb), line width in pixels (w)
//   params1 = silhouette sensitivity (x), crease sensitivity (y), fade-out distance (z)

@group(2) @binding(0) var depth_tex: texture_depth_2d;

struct InkOut {
    @builtin(position) clip: vec4<f32>,
}

@vertex
fn vs_ink(@builtin(vertex_index) i: u32) -> InkOut {
    // One triangle covering the screen.
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: InkOut;
    out.clip = vec4<f32>(uv * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
    return out;
}

// Distance from the camera to what's drawn at pixel `p` (very far for the sky).
fn distance_at(p: vec2<i32>) -> f32 {
    let size = vec2<i32>(view.screen.xy);
    let q = clamp(p, vec2<i32>(0), size - vec2<i32>(1));
    let d = textureLoad(depth_tex, q, 0);
    if (d >= 1.0) {
        return 1e9;
    }
    let uv = (vec2<f32>(q) + vec2<f32>(0.5)) * view.screen.zw;
    let ndc = vec4<f32>(uv * 2.0 - vec2<f32>(1.0), d, 1.0);
    let w = view.inv_view_proj * ndc;
    return length(w.xyz / w.w - view.camera_pos.xyz);
}

@fragment
fn fs_ink(in: InkOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(in.clip.xy);
    let r = max(i32(round(pc.params0.w * 0.5)), 1);
    let c = distance_at(p);
    if (c > 1e8) {
        return vec4<f32>(0.0);
    }
    let l = distance_at(p + vec2<i32>(-r, 0));
    let rr = distance_at(p + vec2<i32>(r, 0));
    let u = distance_at(p + vec2<i32>(0, -r));
    let d = distance_at(p + vec2<i32>(0, r));
    // Silhouette: a neighbor much farther away (the line goes on the near object).
    let far = max(max(l, rr), max(u, d));
    let silhouette = smoothstep(pc.params1.x, pc.params1.x * 2.0, (min(far, c * 4.0) - c) / c);
    // Crease: the surface bends (second difference of the distance), ignoring silhouettes.
    var crease = 0.0;
    if (far < c * 1.5) {
        let bend = max(abs(l + rr - 2.0 * c), abs(u + d - 2.0 * c)) / c;
        crease = smoothstep(pc.params1.y, pc.params1.y * 2.0, bend);
    }
    var a = max(silhouette, crease);
    // Thin out with distance so far terrain doesn't turn black.
    if (pc.params1.z > 0.0) {
        a *= 1.0 - smoothstep(pc.params1.z * 0.5, pc.params1.z, c);
    }
    return vec4<f32>(pc.params0.rgb * a, a);
}

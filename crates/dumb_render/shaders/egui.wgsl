// egui: premultiplied-alpha, gamma-space colors into a UNORM target.

struct Push {
    screen_size: vec2<f32>,
}
var<push_constant> pc: Push;

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> VOut {
    var out: VOut;
    out.clip = vec4<f32>(2.0 * pos / pc.screen_size - vec2<f32>(1.0), 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    return in.color * textureSample(tex, samp, in.uv);
}

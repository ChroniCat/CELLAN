// Vertex shader bindings

struct VertexOutput {
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>, // premultiplied sRGB gamma 0-1
    @builtin(position) position: vec4<f32>,
};

struct Locals {
    screen_size: vec2<f32>,
    dithering: u32, // 1 if dithering is enabled, 0 otherwise
    _padding: u32,
    cleartype_gamma: f32,
    cleartype_contrast: f32,
    _padding2: vec2<u32>,
};
@group(0) @binding(0) var<uniform> r_locals: Locals;

// Naga 24.0 (wgpu 24.x) represents dual-source fragment outputs via `@second_blend_source`
// on `@location(0)` without an `enable dual_source_blending;` directive.
struct ClearTypeOutput {
    @location(0) color: vec4<f32>,
    @location(0) @second_blend_source mask: vec4<f32>,
};

fn interleaved_gradient_noise(n: vec2<f32>) -> f32 {
    let f = 0.06711056 * n.x + 0.00583715 * n.y;
    return fract(52.9829189 * fract(f));
}

fn dither_interleaved(rgb: vec3<f32>, levels: f32, frag_coord: vec4<f32>) -> vec3<f32> {
    var noise = interleaved_gradient_noise(frag_coord.xy);
    noise = (noise - 0.5) * 0.95;
    return rgb + noise / (levels - 1.0);
}

// 0-1 linear  from  0-1 sRGB gamma
fn linear_from_gamma_rgb(srgb: vec3<f32>) -> vec3<f32> {
    let cutoff = srgb < vec3<f32>(0.04045);
    let lower = srgb / vec3<f32>(12.92);
    let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

// 0-1 sRGB gamma  from  0-1 linear
fn gamma_from_linear_rgb(rgb: vec3<f32>) -> vec3<f32> {
    let cutoff = rgb < vec3<f32>(0.0031308);
    let lower = rgb * vec3<f32>(12.92);
    let higher = vec3<f32>(1.055) * pow(rgb, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(higher, lower, cutoff);
}

// 0-1 sRGBA gamma  from  0-1 linear
fn gamma_from_linear_rgba(linear_rgba: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(gamma_from_linear_rgb(linear_rgba.rgb), linear_rgba.a);
}

// [u8; 4] SRGB as u32 -> [r, g, b, a] in 0..1
fn unpack_color(color: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(color & 255u),
        f32((color >> 8u) & 255u),
        f32((color >> 16u) & 255u),
        f32((color >> 24u) & 255u),
    ) / 255.0;
}

fn position_from_screen(screen_pos: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(
        2.0 * screen_pos.x / r_locals.screen_size.x - 1.0,
        1.0 - 2.0 * screen_pos.y / r_locals.screen_size.y,
        0.0,
        1.0,
    );
}

@vertex
fn vs_main(
    @location(0) a_pos: vec2<f32>,
    @location(1) a_tex_coord: vec2<f32>,
    @location(2) a_color: u32,
) -> VertexOutput {
    var out: VertexOutput;
    out.tex_coord = a_tex_coord;
    out.color = unpack_color(a_color);
    out.position = position_from_screen(a_pos);
    return out;
}

@group(1) @binding(0) var r_tex_color: texture_2d<f32>;
@group(1) @binding(1) var r_tex_sampler: sampler;

fn compute_cleartype_eff(v: vec4<f32>, tex_rgb: vec3<f32>) -> vec3<f32> {
    let gamma = max(r_locals.cleartype_gamma, 1.0);
    let enhanced_contrast = max(r_locals.cleartype_contrast, 0.0);

    let fg = clamp(v.rgb / max(v.a, 1e-5), vec3<f32>(0.0), vec3<f32>(1.0));
    let lum = dot(fg, vec3<f32>(0.299, 0.587, 0.114));

    // Direct2D / DirectWrite linear-light dark-on-light contrast + sRGB transmittance gamma:
    // preserves 0->0 and 1->1 exactly while preventing subpixel edge fringes from bloating.
    let k_dark = enhanced_contrast * 0.85;
    let a_ec = clamp(
        tex_rgb * (k_dark + 1.0) / (tex_rgb * k_dark + vec3<f32>(1.0)),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    let inv_a = max(vec3<f32>(1.0) - a_ec, vec3<f32>(1e-6));
    let eff_dark = select(
        vec3<f32>(1.0) - pow(inv_a, vec3<f32>(1.0 / gamma)),
        vec3<f32>(1.0),
        a_ec >= vec3<f32>(1.0),
    );
    let safe_tex = max(tex_rgb, vec3<f32>(1e-6));
    let eff_light = select(
        pow(safe_tex, vec3<f32>(1.0 / gamma)),
        vec3<f32>(0.0),
        tex_rgb <= vec3<f32>(0.0),
    );
    return mix(eff_dark, eff_light, lum);
}

@fragment
fn fs_cleartype_gamma_framebuffer(in: VertexOutput) -> ClearTypeOutput {
    let tex_linear = textureSample(r_tex_color, r_tex_sampler, in.tex_coord);
    let tex = gamma_from_linear_rgba(tex_linear);
    let v = in.color;

    let eff = compute_cleartype_eff(v, tex.rgb);
    let max_eff = max(eff.r, max(eff.g, eff.b));

    var out_rgb = v.rgb * eff;
    if r_locals.dithering == 1u {
        let dithered = clamp(dither_interleaved(out_rgb, 256.0, in.position), vec3<f32>(0.0), vec3<f32>(1.0));
        out_rgb = select(out_rgb, dithered, eff > vec3<f32>(0.0));
    }

    var out: ClearTypeOutput;
    out.color = vec4<f32>(out_rgb, v.a * max_eff);
    out.mask = vec4<f32>(v.a * eff, v.a * max_eff);
    return out;
}

@fragment
fn fs_cleartype_linear_framebuffer(in: VertexOutput) -> ClearTypeOutput {
    let tex_linear = textureSample(r_tex_color, r_tex_sampler, in.tex_coord);
    let tex = gamma_from_linear_rgba(tex_linear);
    let v = in.color;

    let enhanced_contrast = max(r_locals.cleartype_contrast, 0.0);
    let k_lin = enhanced_contrast * 0.5;
    let eff = clamp(
        tex.rgb * (k_lin + 1.0) / (tex.rgb * k_lin + vec3<f32>(1.0)),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    let max_eff = max(eff.r, max(eff.g, eff.b));

    var out_gamma_rgb = v.rgb * eff;
    if r_locals.dithering == 1u {
        let dithered = clamp(dither_interleaved(out_gamma_rgb, 256.0, in.position), vec3<f32>(0.0), vec3<f32>(1.0));
        out_gamma_rgb = select(out_gamma_rgb, dithered, eff > vec3<f32>(0.0));
    }

    let fg = clamp(v.rgb / max(v.a, 1e-5), vec3<f32>(0.0), vec3<f32>(1.0));
    let fg_linear = linear_from_gamma_rgb(fg);
    let shape_linear = linear_from_gamma_rgb(out_gamma_rgb);
    let is_solid = all(tex.rgb >= vec3<f32>(0.999));
    let out_linear_rgb = select(fg_linear * (v.a * eff), shape_linear, is_solid);

    var out: ClearTypeOutput;
    out.color = vec4<f32>(out_linear_rgb, v.a * max_eff);
    out.mask = vec4<f32>(v.a * eff, v.a * max_eff);
    return out;
}

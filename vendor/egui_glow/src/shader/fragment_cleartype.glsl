#ifdef GL_ES
    precision mediump float;
#endif

uniform sampler2D u_sampler;
uniform vec2 u_cleartype_params; // (gamma, enhanced_contrast)

in vec4 v_rgba_in_gamma;
in vec2 v_tc;

layout(location = 0, index = 0) out vec4 f_color;
layout(location = 0, index = 1) out vec4 f_mask;

float interleaved_gradient_noise(vec2 n) {
    float f = 0.06711056 * n.x + 0.00583715 * n.y;
    return fract(52.9829189 * fract(f));
}

vec3 dither_interleaved(vec3 rgb, float levels) {
    float noise = interleaved_gradient_noise(gl_FragCoord.xy);
    noise = (noise - 0.5) * 0.95;
    return rgb + noise / (levels - 1.0);
}

// 0-1 sRGB gamma  from  0-1 linear
vec3 srgb_gamma_from_linear(vec3 rgb) {
    bvec3 cutoff = lessThan(rgb, vec3(0.0031308));
    vec3 lower = rgb * vec3(12.92);
    vec3 higher = vec3(1.055) * pow(rgb, vec3(1.0 / 2.4)) - vec3(0.055);
    return mix(higher, lower, vec3(cutoff));
}

// 0-1 sRGBA gamma  from  0-1 linear
vec4 srgba_gamma_from_linear(vec4 rgba) {
    return vec4(srgb_gamma_from_linear(rgba.rgb), rgba.a);
}

void main() {
#if SRGB_TEXTURES
    vec4 tex = srgba_gamma_from_linear(texture(u_sampler, v_tc));
#else
    vec4 tex = texture(u_sampler, v_tc);
#endif

    vec4 v = v_rgba_in_gamma;
    float gamma = max(u_cleartype_params.x, 1.0);
    float enhanced_contrast = max(u_cleartype_params.y, 0.0);

    vec3 fg = clamp(v.rgb / max(v.a, 1e-5), vec3(0.0), vec3(1.0));
    float lum = dot(fg, vec3(0.299, 0.587, 0.114));

    float k_dark = enhanced_contrast * 0.85;
    vec3 a_ec = clamp(
        tex.rgb * (k_dark + 1.0) / (tex.rgb * k_dark + vec3(1.0)),
        vec3(0.0),
        vec3(1.0)
    );
    vec3 inv_a = max(vec3(1.0) - a_ec, vec3(1e-6));
    vec3 eff_dark = mix(
        vec3(1.0) - pow(inv_a, vec3(1.0 / gamma)),
        vec3(1.0),
        vec3(greaterThanEqual(a_ec, vec3(1.0)))
    );
    vec3 safe_tex = max(tex.rgb, vec3(1e-6));
    vec3 eff_light = mix(
        pow(safe_tex, vec3(1.0 / gamma)),
        vec3(0.0),
        vec3(lessThanEqual(tex.rgb, vec3(0.0)))
    );
    vec3 eff = mix(eff_dark, eff_light, lum);
    float max_eff = max(eff.r, max(eff.g, eff.b));

    vec3 out_rgb = v.rgb * eff;
#if DITHERING
    vec3 dithered = clamp(dither_interleaved(out_rgb, 256.0), vec3(0.0), vec3(1.0));
    out_rgb = mix(out_rgb, dithered, vec3(greaterThan(eff, vec3(0.0))));
#endif

    f_color = vec4(out_rgb, v.a * max_eff);
    f_mask = vec4(v.a * eff, v.a * max_eff);
}

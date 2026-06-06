// Uniform packing from Renderer::write_uniforms (src/overlay/renderer.rs):
// - header = (elapsed_time, processing_elapsed_or_zero, fade_alpha, _pad)
// - audio  = (voice_pulse, peak, surface_width, surface_height)
// - motion = (accumulated_fbm_rotation, accumulated_fbm_translation, _pad, _pad)
struct Uniforms {
    header: vec4<f32>,
    audio: vec4<f32>,
    motion: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

const PI: f32 = 3.14159265359;
const TAU: f32 = 6.28318530718;
const ORB_RADIUS: f32 = 0.4;
const WARP: f32 = 5.0;
const BANDS: f32 = 8.0;
const BAND_LINE_WIDTH: f32 = 0.085;
const BAND_STRENGTH: f32 = 0.82;
const BAND_LINE_STRENGTH: f32 = 0.34;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );

    var output: VertexOutput;
    output.position = vec4<f32>(positions[index], 0.0, 1.0);
    output.uv = positions[index] * 0.5 + vec2<f32>(0.5, 0.5);
    return output;
}

// Convert screen uv into ellipse-friendly coordinates that compensate for aspect ratio.
fn orb_space(uv: vec2<f32>) -> vec2<f32> {
    let size = max(uniforms.audio.zw, vec2<f32>(1.0, 1.0));
    let aspect = size.x / size.y;
    let centered = uv - vec2<f32>(0.5, 0.5);
    return vec2<f32>(centered.x * aspect, centered.y);
}

// Hash/value-noise/FBM helpers for the recording interior texture.
// The shader samples FBM in a curved "surface" space instead of flat uv space
// so the texture reads more like something wrapped over a volume.
fn hash21(p: vec2<f32>) -> f32 {
    let q = vec2<f32>(
        dot(p, vec2<f32>(127.1, 311.7)),
        dot(p, vec2<f32>(269.5, 183.3)),
    );
    return fract(sin(dot(q, vec2<f32>(1.0, 1.0))) * 43758.5453);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);

    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));

    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    var value = 0.0;
    var amplitude = 0.5;
    var point = p;
    for (var i = 0; i < 5; i = i + 1) {
        value += value_noise(point) * amplitude;
        point = point * 2.03 + vec2<f32>(17.1, 9.2);
        amplitude *= 0.5;
    }
    return value;
}

fn fbm_vec2(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        fbm(p + vec2<f32>(0.0, 0.0)),
        fbm(p + vec2<f32>(5.2, 1.3)),
    );
}

fn rot(a: f32) -> mat2x2<f32> {
    let s = sin(a);
    let c = cos(a);
    return mat2x2<f32>(
        vec2<f32>(c, s),
        vec2<f32>(-s, c),
    );
}

fn rainbow_band_color(t: f32) -> vec3<f32> {
    let phase = fract(t);
    let c0 = vec3<f32>(1.0, 0.04, 0.02);
    let c1 = vec3<f32>(1.0, 0.52, 0.02);
    let c2 = vec3<f32>(1.0, 0.90, 0.08);
    let c3 = vec3<f32>(0.06, 0.92, 0.12);
    let c4 = vec3<f32>(0.04, 0.82, 0.88);
    let c5 = vec3<f32>(0.08, 0.24, 1.0);
    let c6 = vec3<f32>(0.58, 0.10, 0.96);

    let segment = phase * 7.0;
    let f = fract(segment);
    let blend = smoothstep(0.30, 0.70, f);

    if (segment < 1.0) {
        return mix(c0, c1, blend);
    }
    if (segment < 2.0) {
        return mix(c1, c2, blend);
    }
    if (segment < 3.0) {
        return mix(c2, c3, blend);
    }
    if (segment < 4.0) {
        return mix(c3, c4, blend);
    }
    if (segment < 5.0) {
        return mix(c4, c5, blend);
    }
    if (segment < 6.0) {
        return mix(c5, c6, blend);
    }
    return mix(c6, c0, blend);
}

fn ease_in_out_01(t: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0);
    return x * x * (3.0 - 2.0 * x);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let processing_elapsed = uniforms.header.y;
    let fade = clamp(uniforms.header.z, 0.0, 1.0);
    let uv = input.uv;
    let p = orb_space(uv);
    let is_processing = processing_elapsed > 0.0;
    let pulse = clamp(uniforms.audio.x, 0.0, 1.0);
    let peak = clamp(uniforms.audio.y, 0.0, 1.0);

    var alpha = 0.0;
    var color = vec3<f32>(0.02, 0.06, 0.16);

    let drive = WARP * clamp(0.75 * pulse + 0.25 * peak, 0.0, 1.0);
    let accent = WARP * clamp(0.45 * pulse + 0.55 * peak, 0.0, 1.0);
    let recording_radius_scale =
        1.0
        + 0.25 * clamp(0.70 * pulse + 0.30 * peak, 0.0, 1.0);
    let radius = select(ORB_RADIUS, ORB_RADIUS * recording_radius_scale, !is_processing);
    let processing_half_cycle = 2.0;
    let processing_cycle = processing_half_cycle * 2.0;
    let processing_phase = processing_elapsed - floor(processing_elapsed / processing_cycle) * processing_cycle;
    let processing_t = select(
        processing_phase / processing_half_cycle,
        (processing_phase - processing_half_cycle) / processing_half_cycle,
        processing_phase >= processing_half_cycle,
    );
    let processing_eased = ease_in_out_01(processing_t);
    let processing_sweep = TAU * 2.0;
    let processing_rotation = select(
        processing_sweep * processing_eased,
        processing_sweep * (1.0 - processing_eased),
        processing_phase >= processing_half_cycle,
    );
    let orb_p = select(p, rot(processing_rotation) * p, is_processing);

    let sphere = orb_p / max(radius, 1e-4);
    let rr = dot(sphere, sphere);

    if (rr < 1.0) {
        let z = sqrt(max(1.0 - rr, 0.0));
        let n = normalize(vec3<f32>(sphere, z));

        let sph_uv = vec2<f32>(
            atan2(n.z, n.x) / TAU + 0.5,
            asin(clamp(n.y, -1.0, 1.0)) / PI + 0.5,
        );

        let monotonic_rotation = uniforms.motion.x + select(0.0, processing_elapsed, is_processing);
        let monotonic_translation = uniforms.motion.y + select(0.0, processing_elapsed, is_processing);
        let monotonic_rot = rot(monotonic_rotation * 5.0);
        let monotonic_translate = vec2<f32>(
            monotonic_translation * 5.0,
            -monotonic_translation * 5.0,
        );

        var q = monotonic_rot * ((sph_uv - 0.5) * vec2<f32>(6.0, 3.6)) + monotonic_translate;

        let td1 = vec2<f32>(
            cos(monotonic_rotation * 0.73 + monotonic_translation * 0.21),
            sin(monotonic_rotation * 0.73 + monotonic_translation * 0.21),
        );
        let td2 = vec2<f32>(
            cos(monotonic_rotation * 0.51 - monotonic_translation * 0.34 + 1.7),
            sin(monotonic_rotation * 0.51 - monotonic_translation * 0.34 + 1.7),
        );
        let td3 = vec2<f32>(
            cos(monotonic_rotation * 0.92 + monotonic_translation * 0.27 + 3.1),
            sin(monotonic_rotation * 0.92 + monotonic_translation * 0.27 + 3.1),
        );
        let td4 = vec2<f32>(
            cos(monotonic_rotation * 1.11 - monotonic_translation * 0.18 + 5.0),
            sin(monotonic_rotation * 1.11 - monotonic_translation * 0.18 + 5.0),
        );

        let ang1 = monotonic_rotation * 0.41 + monotonic_translation * 0.08;
        let ang2 = -monotonic_rotation * 0.63 + monotonic_translation * 0.12;
        let ang3 = monotonic_rotation * 0.87 - monotonic_translation * 0.10;
        let ang4 = -monotonic_rotation * 1.08 - monotonic_translation * 0.06;

        let warp_amt1 = 0.20 * drive;
        let warp_amt2 = 0.32 * drive + 0.08 * accent;
        let warp_amt3 = 0.26 * drive + 0.16 * accent;
        let warp_amt4 = 0.18 * drive + 0.10 * accent;

        let w1 = fbm_vec2(rot(ang1) * (q * 0.90) + td1 * 1.8 + vec2<f32>(0.0, 3.1));
        q += (w1 - 0.5) * warp_amt1;

        let w2 = fbm_vec2(rot(ang2) * (q * 1.65) + td2 * 2.1 + vec2<f32>(4.2, -2.7));
        q += (w2 - 0.5) * warp_amt2;

        let w3 = fbm_vec2(rot(ang3) * (q * 2.50) + td3 * 2.6 + vec2<f32>(-3.4, 5.6));
        q += (w3 - 0.5) * warp_amt3;

        let w4 = fbm_vec2(rot(ang4) * (q * 3.50) + td4 * 3.0 + vec2<f32>(7.4, -6.1));
        q += (w4 - 0.5) * warp_amt4;

        let f0 = fbm(q);
        let f1 = fbm(q * 1.9 + vec2<f32>(4.2, -1.7));
        let f2 = fbm(q * 0.7 - vec2<f32>(2.3, 1.1));
        let tex_raw = clamp(f0 * 0.62 + f1 * 0.28 + f2 * 0.20, 0.0, 1.0);
        let band_mix = clamp(BAND_STRENGTH * clamp(0.30 + drive, 0.0, 1.0) + 0.10, 0.0, 1.0);
        let tex_bands = tex_raw * BANDS;
        let band_index = floor(tex_bands);
        let band_frac = fract(tex_bands);
        let stepped = band_index / max(BANDS - 1.0, 1.0);
        let tex = clamp((mix(tex_raw, stepped, band_mix) - 0.5) * 1.28 + 0.5, 0.0, 1.0);

        let edge_dist = min(band_frac, 1.0 - band_frac);
        var contour = 1.0 - smoothstep(0.0, BAND_LINE_WIDTH, edge_dist);
        contour *= BAND_LINE_STRENGTH * clamp(0.55 + 0.95 * drive, 0.0, 1.0);

        let center_light = pow(max(n.z, 0.0), 0.85);
        let shade = 0.68 + 0.26 * center_light;
        let rim = smoothstep(0.55, 1.0, rr);
        let fresnel = pow(1.0 - max(n.z, 0.0), 2.1);

        let rainbow_band_t = stepped + band_frac * 0.08 + monotonic_rotation * 0.006;
        let rainbow_cycle_t = monotonic_translation * 1.20;
        var tex_color = rainbow_band_color(rainbow_band_t + rainbow_cycle_t);
        tex_color += vec3<f32>(1.0, 1.0, 1.0) * contour * 0.38;

        let body =
            tex_color * shade
            + tex_color * tex * (0.10 + 0.08 * peak)
            + vec3<f32>(1.0, 1.0, 1.0) * rim * (0.10 + 0.06 * pulse)
            + vec3<f32>(1.0, 1.0, 1.0) * fresnel * (0.08 + 0.05 * peak);

        color = body;
        alpha = 1.0;
    }

    alpha = clamp(alpha * fade, 0.0, 1.0);

    return vec4<f32>(color * alpha, alpha);
}

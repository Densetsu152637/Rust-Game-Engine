// engine::hdr - HDR display output transform. Mirrors src/hdr.rs; the double-brace NAME
// placeholders are filled from the Rust constants by hdr::wgsl_source().
// Input is display-linear Rec.709 relative to paper white (1.0 = paper
// white). engine_hdr_encode rolls the colour off to the display peak, then
// returns the surface signal: encoding 0 is scRGB (linear Rec.709, 1.0 = 80
// nits, Rgba16Float), encoding 1 is PQ (ST 2084 on Rec.2020, Rgb10a2Unorm).

const ENGINE_SCRGB_UNIT_NITS: f32 = {{SCRGB_UNIT_NITS}};
const ENGINE_PQ_MAX_NITS: f32 = {{PQ_MAX_NITS}};
const ENGINE_PQ_M1: f32 = {{PQ_M1}};
const ENGINE_PQ_M2: f32 = {{PQ_M2}};
const ENGINE_PQ_C1: f32 = {{PQ_C1}};
const ENGINE_PQ_C2: f32 = {{PQ_C2}};
const ENGINE_PQ_C3: f32 = {{PQ_C3}};
const ENGINE_MAX_INPUT: f32 = {{MAX_INPUT}};

fn engine_pq_oetf(nits: vec3<f32>) -> vec3<f32> {
    let y = pow(clamp(nits / ENGINE_PQ_MAX_NITS, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(ENGINE_PQ_M1));
    return pow((ENGINE_PQ_C1 + ENGINE_PQ_C2 * y) / (1.0 + ENGINE_PQ_C3 * y), vec3<f32>(ENGINE_PQ_M2));
}

fn engine_rec709_to_rec2020(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(vec3<f32>({{REC2020_R0}}, {{REC2020_R1}}, {{REC2020_R2}}), c),
        dot(vec3<f32>({{REC2020_G0}}, {{REC2020_G1}}, {{REC2020_G2}}), c),
        dot(vec3<f32>({{REC2020_B0}}, {{REC2020_B1}}, {{REC2020_B2}}), c));
}

fn engine_hdr_rolloff_start(headroom: f32) -> f32 {
    return min(0.5 * headroom, 1.0);
}

fn engine_hdr_rolloff_value(value: f32, headroom: f32) -> f32 {
    let start = engine_hdr_rolloff_start(headroom);
    if (value <= start) {
        return value;
    }
    let range = max(headroom - start, 1e-6);
    return start + range * (1.0 - exp(-(value - start) / range));
}

fn engine_hdr_rolloff(c: vec3<f32>, headroom: f32) -> vec3<f32> {
    let peak = max(c.r, max(c.g, c.b));
    if (peak <= engine_hdr_rolloff_start(headroom)) {
        return c;
    }
    return c * (engine_hdr_rolloff_value(peak, headroom) / peak);
}

fn engine_hdr_encode(encoding: u32, rgb: vec3<f32>, paper_white_nits: f32, peak_nits: f32) -> vec3<f32> {
    // select() maps NaN (which compares false) to zero.
    let clean = clamp(select(vec3<f32>(0.0), rgb, rgb == rgb), vec3<f32>(0.0), vec3<f32>(ENGINE_MAX_INPUT));
    let rolled = engine_hdr_rolloff(clean, peak_nits / paper_white_nits);
    if (encoding == 1u) {
        return engine_pq_oetf(engine_rec709_to_rec2020(rolled) * paper_white_nits);
    }
    return rolled * (paper_white_nits / ENGINE_SCRGB_UNIT_NITS);
}

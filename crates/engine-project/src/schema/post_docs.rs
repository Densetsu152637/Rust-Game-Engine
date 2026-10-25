//! Schema documentation of the post-processing components (`crate::post`).
use super::docs::ComponentDoc;
use serde_json::{Map, Value, json};

fn closed(properties: Value) -> Value {
    json!({"type": "object", "properties": properties, "required": [],
        "additionalProperties": false})
}

fn version() -> Value {
    json!({"type": "integer", "const": 1, "default": 1,
        "description": "Component schema version. Only 1 exists; may be omitted."})
}

fn num(min: f64, max: f64, default: f64, description: &str) -> Value {
    json!({"type": "number", "minimum": min, "maximum": max, "default": default,
        "description": description})
}

fn flag(default: bool, description: &str) -> Value {
    json!({"type": "boolean", "default": default, "description": description})
}

fn rgb(min: f64, max: f64, default: f64, description: &str) -> Value {
    json!({"type": "array", "items": {"type": "number", "minimum": min, "maximum": max},
        "minItems": 3, "maxItems": 3, "default": [default, default, default],
        "description": description})
}

fn tonemap() -> Value {
    json!({"type": "string", "enum": ["none", "aces", "agx", "agx_punchy", "neutral"],
        "default": "none",
        "description": "Display transform. none: linear clamp to 0..1 (today's default \
            appearance); aces: Narkowicz ACES fit; agx / agx_punchy: AgX-style sigmoid on \
            Rec.2020 primaries (punchy adds power 1.35 and saturation 1.4); neutral: Khronos \
            PBR Neutral. Applied with the sRGB encode of the target."})
}

fn exposure() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "mode": {"type": "string", "enum": ["off", "manual", "auto"], "default": "off",
            "description": "off: no post exposure (only Camera3D.lighting.exposure, which \
                scales photometric lights). manual: scale the HDR scene by \
                2^(compensation - ev100) after lighting. auto: histogram of scene luminance, \
                adapted over time to middle grey 0.18, plus compensation. Post exposure \
                multiplies with lighting.exposure, it does not replace it."},
        "ev100": num(-30.0, 30.0, 0.0, "Manual: scale is 2^-ev100."),
        "compensation": num(-16.0, 16.0, 0.0, "Stops added in manual and auto modes."),
        "min_ev": num(-16.0, 16.0, -8.0, "Auto: lowest log2 luminance in the histogram."),
        "max_ev": num(-16.0, 16.0, 6.0, "Auto: highest log2 luminance (min_ev + 0.5 or more)."),
        "low_percentile": num(0.0, 1.0, 0.1, "Auto: darkest fraction of pixels ignored."),
        "high_percentile": num(0.0, 1.0, 0.9, "Auto: pixels above this fraction are ignored."),
        "speed_up": num(0.0, 1000.0, 3.0, "Auto: adaptation rate (1/s) when the scene gets \
            darker; the remaining gap shrinks by e^(-speed * seconds)."),
        "speed_down": num(0.0, 1000.0, 1.0, "Auto: adaptation rate (1/s) when the scene \
            gets brighter."),
    }) else {
        unreachable!("an object literal")
    };
    map
}

fn bloom() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "enabled": flag(false, "Run the bloom chain."),
        "threshold": num(0.0, 1000.0, 1.0, "Exposed luminance where bloom starts."),
        "knee": num(0.0, 1.0, 0.5, "Soft knee width relative to the threshold."),
        "intensity": num(0.0, 100.0, 0.3, "Weight of the blurred result added to the scene."),
        "scatter": num(0.0, 1.0, 0.7, "How much each coarser mip contributes (wider halo)."),
        "tint": rgb(0.0, 100.0, 1.0, "Linear RGB multiplier of the bloom."),
    }) else {
        unreachable!("an object literal")
    };
    map
}

fn grading() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "enabled": flag(false, "Apply colour grading in HDR before tonemapping."),
        "temperature": num(-1.0, 1.0, 0.0, "White balance: -1 cool .. 1 warm."),
        "tint": num(-1.0, 1.0, 0.0, "White balance: -1 green .. 1 magenta."),
        "exposure_offset": num(-16.0, 16.0, 0.0, "Stops added before the other steps."),
        "contrast": num(0.0, 4.0, 1.0, "Power about the mid-grey pivot 0.18; 1 is neutral."),
        "saturation": num(0.0, 4.0, 1.0, "0 is grey, 1 neutral."),
        "lift": rgb(-1.0, 1.0, 0.0, "Added to the colour."),
        "gamma": rgb(0.1, 4.0, 1.0, "Power 1/gamma."),
        "gain": rgb(0.0, 4.0, 1.0, "Multiplier."),
        "hue_shift_degrees": num(-360.0, 360.0, 0.0, "Rotation about the grey axis."),
        "lut": {"type": ["string", "null"], "minLength": 1, "default": null,
            "description": "Asset ID of a `.cube` 3D LUT (asset kind lut), applied in a \
                reversible x/(1+x) compressed domain; an identity LUT is a no-op. A file that \
                fails validation is reported in the post status and not applied."},
        "lut_intensity": num(0.0, 1.0, 1.0, "Blend of the LUT result."),
    }) else {
        unreachable!("an object literal")
    };
    map
}

fn vignette() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "enabled": flag(false, "Darken toward the corners (after tonemapping)."),
        "intensity": num(0.0, 1.0, 0.4, "Blend toward `color` at the farthest corner."),
        "smoothness": num(0.01, 1.0, 0.5, "Falloff width as a fraction of the corner distance."),
        "center": json!({"type": "array", "items": {"type": "number", "minimum": 0, "maximum": 1},
            "minItems": 2, "maxItems": 2, "default": [0.5, 0.5]}),
        "color": rgb(0.0, 1.0, 0.0, "Linear RGB the edges blend toward."),
    }) else {
        unreachable!("an object literal")
    };
    map
}

fn grain() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "enabled": flag(false, "Add film grain (after tonemapping)."),
        "intensity": num(0.0, 1.0, 0.1, "Peak amplitude of the zero-mean noise."),
        "response": num(0.0, 1.0, 0.8, "How much darker pixels get more grain."),
    }) else {
        unreachable!("an object literal")
    };
    map
}

fn group(properties: Map<String, Value>, description: &str) -> Value {
    let mut schema = closed(Value::Object(properties));
    schema["description"] = json!(description);
    schema
}

/// The same group with every field optional ("not overridden").
fn overrides(properties: Map<String, Value>, description: &str) -> Value {
    let stripped = properties
        .into_iter()
        .map(|(name, mut schema)| {
            if let Some(object) = schema.as_object_mut() {
                object.remove("default");
            }
            (name, schema)
        })
        .collect();
    group(stripped, description)
}

pub(super) fn post_docs() -> Vec<ComponentDoc> {
    vec![
        ComponentDoc {
            name: "PostProcess",
            description: "HDR post-processing of a camera, on the Camera3D entity: tonemapping, \
                exposure (manual or histogram auto-exposure), bloom, HDR colour grading with an \
                optional 3D LUT, vignette and film grain. Without it (and without an \
                influencing PostProcessVolume) the camera renders with the default linear-clamp \
                output. PostProcessVolume entities override it by region.",
            requires: &[],
            schema: closed(json!({"version": version(), "tonemap": tonemap(),
                "exposure": group(exposure(), "Exposure."),
                "bloom": group(bloom(), "Bloom."),
                "grading": group(grading(), "HDR colour grading."),
                "vignette": group(vignette(), "Vignette."),
                "grain": group(grain(), "Film grain.")})),
            example: json!({"tonemap": "aces", "bloom": {"enabled": true, "intensity": 0.2}}),
        },
        ComponentDoc {
            name: "PostProcessVolume",
            description: "Overrides PostProcess parameters by region. Every parameter is \
                optional; unset parameters are not overridden. Volumes are evaluated at the \
                camera position in ascending priority (ties by entity ID) on top of the \
                camera's PostProcess; each is blended in by weight and a distance factor \
                (1 inside the shape, fading to 0 over blend_distance metres outside). \
                Continuous values interpolate; booleans, enums and asset ids switch at an \
                influence of 0.5. Non-global shapes require Transform and are scaled by it.",
            requires: &["Transform"],
            schema: closed(json!({
                "version": version(),
                "enabled": flag(true, "Disabled volumes are ignored."),
                "shape": {"oneOf": [
                    {"type": "object", "properties": {"type": {"const": "global"}},
                        "required": ["type"], "additionalProperties": false},
                    {"type": "object", "properties": {"type": {"const": "box"},
                        "size": {"type": "array", "items": {"type": "number",
                            "exclusiveMinimum": 0, "maximum": 1_000_000},
                            "minItems": 3, "maxItems": 3,
                            "description": "Full extents in local metres."}},
                        "required": ["type", "size"], "additionalProperties": false},
                    {"type": "object", "properties": {"type": {"const": "sphere"},
                        "radius": {"type": "number", "exclusiveMinimum": 0,
                            "maximum": 1_000_000}},
                        "required": ["type", "radius"], "additionalProperties": false}],
                    "default": {"type": "global"}},
                "priority": {"type": "integer", "minimum": -1000, "maximum": 1000, "default": 0,
                    "description": "Higher priorities are applied later and win."},
                "blend_distance": num(0.0, 100_000.0, 0.0, "Metres outside the shape over \
                    which the influence fades to zero (0: hard edge)."),
                "weight": num(0.0, 1.0, 1.0, "Overall strength."),
                "tonemap": {"type": "string", "enum": ["none", "aces", "agx", "agx_punchy", "neutral"],
                    "description": "Override of PostProcess.tonemap; omit to keep it."},
                "exposure": overrides(exposure(), "Exposure overrides."),
                "bloom": overrides(bloom(), "Bloom overrides."),
                "grading": overrides(grading(), "Grading overrides."),
                "vignette": overrides(vignette(), "Vignette overrides."),
                "grain": overrides(grain(), "Grain overrides.")})),
            example: json!({"shape": {"type": "box", "size": [6, 3, 6]}, "blend_distance": 2,
                "tonemap": "agx", "vignette": {"enabled": true, "intensity": 0.5}}),
        },
    ]
}

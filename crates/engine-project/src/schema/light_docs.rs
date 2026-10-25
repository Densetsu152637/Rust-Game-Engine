//! Schema documentation of the light components (`crate::lights`).
use super::docs::ComponentDoc;
use serde_json::{Value, json};

fn closed(properties: Value) -> Value {
    json!({"type": "object", "properties": properties, "required": [],
        "additionalProperties": false})
}

fn version() -> Value {
    json!({"type": "integer", "const": 1, "default": 1,
        "description": "Component schema version. Only 1 exists; may be omitted."})
}

fn color() -> Value {
    json!({"type": "array", "items": {"type": "number", "minimum": 0, "maximum": 1},
        "minItems": 3, "maxItems": 3, "default": [1, 1, 1],
        "description": "Linear RGB colour, each 0..1."})
}

fn enabled() -> Value {
    json!({"type": "boolean", "default": true,
        "description": "Disabled lights are not extracted or rendered."})
}

fn lumens() -> Value {
    json!({"type": "number", "minimum": 0, "maximum": 100_000_000, "default": 800,
        "description": "Luminous power in lumens (800 lm is a 60 W incandescent bulb). \
            The renderer multiplies photometric values by Camera3D.lighting.exposure."})
}

fn range() -> Value {
    json!({"type": "number", "exclusiveMinimum": 0, "maximum": 100_000, "default": 10,
        "description": "Metres where the smooth window (1 - (d/range)^4)^2 ends the \
            inverse-square falloff; the light contributes nothing beyond it and is \
            culled per view and cluster by this sphere."})
}

/// Properties shared by both shadow objects.
fn shadow_common(resolution: u32) -> serde_json::Map<String, Value> {
    let Value::Object(map) = json!({
        "enabled": {"type": "boolean", "default": false},
        "resolution": {"type": "integer", "enum": [64, 128, 256, 512, 1024, 2048, 4096],
            "default": resolution,
            "description": "Texels per shadow view edge in the shadow atlas (per cascade, \
                per cube face, or the spot view)."},
        "depth_bias": {"type": "number", "minimum": 0, "maximum": 16, "default": 1,
            "description": "Receiver offset toward the light, in shadow texels at the receiver."},
        "normal_bias": {"type": "number", "minimum": 0, "maximum": 16, "default": 1.5,
            "description": "Receiver offset along its normal, in shadow texels."},
        "filter": {"type": "string", "enum": ["hard", "pcf", "pcss"], "default": "pcf",
            "description": "hard: one bilinear comparison; pcf: (2 pcf_radius + 1)^2 \
                comparisons; pcss: blocker search then a filter whose radius grows with \
                the blocker-receiver distance and the light's size."},
        "pcf_radius": {"type": "integer", "minimum": 0, "maximum": 4, "default": 1},
        "pcss_max_radius": {"type": "integer", "minimum": 1, "maximum": 32, "default": 16,
            "description": "Largest PCSS filter radius, in texels."}
    }) else {
        unreachable!("an object literal")
    };
    map
}

fn local_shadows() -> Value {
    let mut properties = shadow_common(512);
    properties.insert(
        "source_radius".into(),
        json!({"type": "number", "minimum": 0.001, "maximum": 10, "default": 0.05,
            "description": "Emitter radius in metres used by PCSS penumbrae."}),
    );
    properties.insert(
        "near".into(),
        json!({"type": "number", "minimum": 0.001, "maximum": 100, "default": 0.05,
            "description": "Shadow projection near plane in metres."}),
    );
    closed(Value::Object(properties))
}

fn directional_shadows() -> Value {
    let mut properties = shadow_common(2048);
    for (name, schema) in [
        (
            "angular_diameter_radians",
            json!({"type": "number", "minimum": 0.0001, "maximum": 0.2, "default": 0.0093,
                "description": "Apparent source size for PCSS (the sun is about 0.0093)."}),
        ),
        (
            "cascades",
            json!({"type": "integer", "minimum": 1, "maximum": 4, "default": 4,
                "description": "Cascaded shadow maps fitted to the camera frustum."}),
        ),
        (
            "split_lambda",
            json!({"type": "number", "minimum": 0, "maximum": 1, "default": 0.75,
                "description": "Practical split scheme: 0 uniform, 1 logarithmic splits."}),
        ),
        (
            "max_distance",
            json!({"type": "number", "minimum": 0.01, "maximum": 100_000, "default": 100,
                "description": "Camera view depth (metres) the cascades cover; beyond it \
                    surfaces are unshadowed. Clamped to the camera's far plane."}),
        ),
        (
            "cascade_blend",
            json!({"type": "number", "minimum": 0, "maximum": 0.5, "default": 0.1,
                "description": "Fraction at the end of each cascade blended into the next \
                    (the last fades out)."}),
        ),
        (
            "caster_extension",
            json!({"type": "number", "minimum": 0, "maximum": 100_000, "default": 100,
                "description": "Metres beyond each cascade, toward the light, whose casters \
                    still cast shadows into it."}),
        ),
    ] {
        properties.insert(name.into(), schema);
    }
    closed(Value::Object(properties))
}

/// Documentation of `DirectionalLight`, `PointLight`, `SpotLight` and `AreaLight`.
pub(super) fn light_docs() -> Vec<ComponentDoc> {
    vec![
        ComponentDoc {
            name: "DirectionalLight",
            description: "An infinitely distant light (sun, moon) travelling along the entity's \
                local -Z axis (world rotation; identity without Transform). Illuminance in lux. \
                Any number may be active; shadows use 1..4 camera-fitted, texel-snapped cascades \
                in the shadow atlas.",
            requires: &[],
            schema: closed(
                json!({"version": version(), "enabled": enabled(), "color": color(),
                "lux": {"type": "number", "minimum": 0, "maximum": 200_000, "default": 3,
                    "description": "Illuminance in lux on a surface facing the light (direct \
                        sunlight is about 100000; pair it with a matching exposure)."},
                "shadows": directional_shadows()}),
            ),
            example: json!({"lux": 3, "shadows": {"enabled": true, "cascades": 3}}),
        },
        ComponentDoc {
            name: "PointLight",
            description: "An isotropic light at the entity's world position: intensity lumens / \
                4 pi candela with windowed inverse-square falloff to `range`. Shadows render six \
                cube faces into the shadow atlas. Requires Transform.",
            requires: &["Transform"],
            schema: closed(
                json!({"version": version(), "enabled": enabled(), "color": color(),
                "lumens": lumens(), "range": range(), "shadows": local_shadows()}),
            ),
            example: json!({"lumens": 800, "range": 8}),
        },
        ComponentDoc {
            name: "SpotLight",
            description: "A cone light at the entity's world position along its local -Z axis: \
                full intensity (lumens / 4 pi cd, independent of the angles) inside \
                `inner_angle_radians`, smooth falloff to zero at `outer_angle_radians`. Shadows \
                render one perspective view (at most 160 degrees). Requires Transform.",
            requires: &["Transform"],
            schema: closed(
                json!({"version": version(), "enabled": enabled(), "color": color(),
                "lumens": lumens(), "range": range(),
                "inner_angle_radians": {"type": "number", "minimum": 0, "default": 0.4,
                    "description": "Half angle of the fully lit cone; below outer_angle_radians."},
                "outer_angle_radians": {"type": "number", "exclusiveMinimum": 0,
                    "maximum": std::f32::consts::FRAC_PI_2, "default": 0.55,
                    "description": "Half angle where the light reaches zero, at most pi/2."},
                "shadows": local_shadows()}),
            ),
            example: json!({"lumens": 1200, "range": 12, "inner_angle_radians": 0.3,
                "outer_angle_radians": 0.5}),
        },
        ComponentDoc {
            name: "AreaLight",
            description: "A rectangle of uniform luminance (lumens / (pi * width * height) per \
                side) centred on the entity in its local XY plane, emitting along local -Z (and \
                +Z when two_sided). Diffuse lighting is the exact polygon integral; specular uses \
                a representative point. Shadows are six cube faces from the centre (an \
                approximation for large lights). Requires Transform.",
            requires: &["Transform"],
            schema: closed(
                json!({"version": version(), "enabled": enabled(), "color": color(),
                "lumens": lumens(),
                "range": {"type": "number", "exclusiveMinimum": 0, "maximum": 100_000,
                    "default": 10, "description": "Metres from the centre where the falloff \
                        window reaches zero; must exceed half the rectangle's diagonal."},
                "width": {"type": "number", "exclusiveMinimum": 0, "maximum": 1000, "default": 1,
                    "description": "Size along local X in metres."},
                "height": {"type": "number", "exclusiveMinimum": 0, "maximum": 1000, "default": 1,
                    "description": "Size along local Y in metres."},
                "two_sided": {"type": "boolean", "default": false},
                "shadows": local_shadows()}),
            ),
            example: json!({"lumens": 2000, "width": 2, "height": 0.5, "range": 6}),
        },
    ]
}

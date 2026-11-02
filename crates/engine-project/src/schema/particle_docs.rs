//! Schema documentation of the `ParticleEmitter` component (`crate::particles`).
use super::docs::ComponentDoc;
use serde_json::{Value, json};

fn pair(description: &str, default: [f64; 2], max: f64) -> Value {
    json!({"type": "array", "items": {"type": "number", "minimum": 0, "maximum": max},
        "minItems": 2, "maxItems": 2, "default": default, "description": description})
}

fn vector(description: &str, default: [f64; 3]) -> Value {
    json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3,
        "default": default, "description": description})
}

fn rgba(description: &str, default: [f64; 4]) -> Value {
    json!({"type": "array", "items": {"type": "number", "minimum": 0}, "minItems": 4,
        "maxItems": 4, "default": default,
        "description": format!("{description} Linear RGBA: rgb 0..1000 (HDR scene units), \
            alpha 0..1.")})
}

/// Documentation of `ParticleEmitter`.
pub(super) fn particle_docs() -> Vec<ComponentDoc> {
    vec![ComponentDoc {
        name: "ParticleEmitter",
        description: "A fixed-capacity GPU particle emitter at the entity's world position, \
            launching along its local +Y axis rotated into world space. The renderer simulates \
            every enabled emitter on the GPU exactly once per scene tick (never per view or per \
            presented frame) and draws camera-facing billboards depth-tested against the scene \
            after transparent meshes. Spawns beyond `capacity` are dropped and counted. At most \
            16 enabled emitters and 262144 particles in total; disabling or removing the \
            component frees its pool. Requires Transform. See docs/rendering-particles.md.",
        requires: &["Transform"],
        schema: json!({
            "type": "object",
            "properties": {
                "version": {"type": "integer", "const": 1, "default": 1,
                    "description": "Component schema version. Only 1 exists; may be omitted."},
                "enabled": {"type": "boolean", "default": true,
                    "description": "Disabled emitters are not extracted; their pool is freed."},
                "capacity": {"type": "integer", "minimum": 1, "maximum": 65_536,
                    "default": 1024,
                    "description": "Pool size, fixed for the life of the emitter."},
                "spawn_rate": {"type": "number", "minimum": 0, "maximum": 1_000_000,
                    "default": 100.0, "description": "Particles per second; fractions carry over."},
                "lifetime": {"type": "array", "items": {"type": "number", "exclusiveMinimum": 0,
                    "maximum": 3600}, "minItems": 2, "maxItems": 2, "default": [1.0, 2.0],
                    "description": "Lifetime range [min, max] in seconds, min <= max."},
                "speed": pair("Launch speed range [min, max] in metres per second, min <= max.",
                    [1.0, 3.0], 10_000.0),
                "spread": {"type": "number", "minimum": 0, "maximum": 1.5, "default": 0.35,
                    "description": "Half angle of the square-section launch cone in radians."},
                "spawn_extent": {"type": "number", "minimum": 0, "maximum": 100_000,
                    "default": 0.0, "description": "Half size of the spawn box in metres."},
                "direction": vector("Launch axis in the entity's local space; nonzero.",
                    [0.0, 1.0, 0.0]),
                "gravity": vector("World-space acceleration in metres per second squared.",
                    [0.0, -9.81, 0.0]),
                "drag": {"type": "number", "minimum": 0, "maximum": 1000, "default": 0.0,
                    "description": "Linear drag per second."},
                "size": pair("Billboard half size [birth, death] in metres, each 0..1000.",
                    [0.1, 0.05], 1000.0),
                "start_color": rgba("Colour at birth.", [1.0, 1.0, 1.0, 1.0]),
                "end_color": rgba("Colour at death.", [1.0, 1.0, 1.0, 0.0]),
                "blend": {"type": "string", "enum": ["alpha", "additive"], "default": "alpha",
                    "description": "alpha: premultiplied over in spawn order, no depth sort. \
                        additive: order independent."},
                "seed": {"type": "integer", "minimum": 0, "maximum": 4_294_967_295u32,
                    "default": 1, "description": "Seed of the deterministic random streams."}
            },
            "required": [],
            "additionalProperties": false
        }),
        example: json!({"capacity": 512, "spawn_rate": 60, "lifetime": [0.8, 1.5],
            "speed": [2, 4], "blend": "additive"}),
    }]
}

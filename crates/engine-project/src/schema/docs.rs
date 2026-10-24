//! Shared built-in authoring component documentation.
use serde_json::{Value, json};
/// The component keys of an entity's `components` object, in schema order.
pub const COMPONENT_NAMES: [&str; 7] = [
    "EventBindings",
    "Transform",
    "Script",
    "MeshRenderer",
    "SpriteRenderer",
    "Camera2D",
    "Collider2D",
];

/// Documentation for one authored component type.
pub struct ComponentDoc {
    pub name: &'static str,
    pub description: &'static str,
    /// Components that must also be present on the entity.
    pub requires: &'static [&'static str],
    /// JSON Schema of the component value.
    pub schema: Value,
    /// A valid minimal value.
    pub example: Value,
}

fn version() -> Value {
    json!({"type": "integer", "const": 1, "default": 1,
        "description": "Component schema version. Only 1 exists; may be omitted."})
}

fn vector(len: usize, description: &str, default: Option<Value>) -> Value {
    let mut schema = json!({"type": "array", "items": {"type": "number"},
        "minItems": len, "maxItems": len, "description": description});
    if let Some(default) = default {
        schema["default"] = default;
    }
    schema
}

fn rectangle(description: &str) -> Value {
    json!({"type": "array", "items": {"type": "integer", "minimum": 0},
        "minItems": 4, "maxItems": 4,
        "description": format!("{description} Pixel rectangle [x, y, width, height]; \
            width and height > 0 and x+width, y+height <= 8192.")})
}

fn closed(properties: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required,
        "additionalProperties": false})
}

/// All authored component documents.
pub fn component_docs() -> Vec<ComponentDoc> {
    vec![
        ComponentDoc {
            name: "EventBindings",
            description: "Named native callbacks invoked by bounded next-tick events.",
            requires: &[],
            schema: closed(
                json!({"bindings":{"type":"array","maxItems":64,"items":{"type":"object","properties":{"event":{"type":"string"},"handler":{"type":"string"}},"required":["event","handler"],"additionalProperties":false}}}),
                &["bindings"],
            ),
            example: json!({"bindings":[]}),
        },
        ComponentDoc {
            name: "Transform",
            description: "Local translation, quaternion rotation and scale (x right, y up, z depth).",
            requires: &[],
            schema: closed(
                json!({"version": version(),
                    "position": vector(3, "Local position [x, y, z]; all finite.", None), "rotation": vector(4, "Quaternion XYZW.", Some(json!([0,0,0,1]))), "scale": vector(3, "Local nonzero scale.", Some(json!([1,1,1])))}),
                &["position"],
            ),
            example: json!({"position": [0, 0, 0]}),
        },
        ComponentDoc {
            name: "Script",
            description: "Attaches a native Rust script. The asset must be a catalog asset of \
                kind `script` whose native factory is registered by the project's gameplay crate.",
            requires: &[],
            schema: closed(
                json!({"version": version(),
                    "asset": {"type": "string", "description":
                        "Asset ID of kind `script` (see asset_list kind=script)."}}),
                &["asset"],
            ),
            example: json!({"asset": "asset:player-script"}),
        },
        ComponentDoc {
            name: "MeshRenderer",
            description: "Draws a triangle mesh, optionally textured.",
            requires: &[],
            schema: closed(
                json!({"version": version(),
                    "mesh": {"type": "string", "description": "Asset ID of kind `mesh`."},
                    "texture": {"type": "string", "description":
                        "Optional asset ID of kind `texture`."}}),
                &["mesh"],
            ),
            example: json!({"mesh": "asset:player-mesh", "texture": "asset:player-texture"}),
        },
        ComponentDoc {
            name: "SpriteRenderer",
            description: "Draws a 2D sprite quad from a texture (whole texture, a source \
                rectangle, or an animation of frames). Requires Transform.",
            requires: &["Transform"],
            schema: closed(
                json!({"version": version(),
                    "texture": {"type": "string", "description": "Asset ID of kind `texture`."},
                    "size": {"type": "array", "items": {"type": "number", "exclusiveMinimum": 0},
                        "minItems": 2, "maxItems": 2, "default": [1, 1],
                        "description": "Quad size [width, height] in world units; > 0."},
                    "pivot": {"type": "array", "items": {"type": "number", "minimum": 0, "maximum": 1},
                        "minItems": 2, "maxItems": 2, "default": [0.5, 0.5],
                        "description": "Pivot [x, y] in 0..1 within the quad."},
                    "tint": {"type": "array", "items": {"type": "number", "minimum": 0, "maximum": 1},
                        "minItems": 4, "maxItems": 4, "default": [1, 1, 1, 1],
                        "description": "RGBA multiplier, each 0..1."},
                    "source": rectangle("Optional source rectangle in the texture; defaults to the whole texture."),
                    "frames": {"type": "array", "items": rectangle("One animation frame."),
                        "maxItems": 4096, "default": [],
                        "description": "Animation frames; when non-empty they replace `source`."},
                    "framesPerSecond": {"type": "number", "minimum": 0, "maximum": 1000, "default": 0,
                        "description": "Animation speed; 0 shows the first frame."},
                    "loop": {"type": "boolean", "default": true,
                        "description": "Whether the animation repeats."},
                    "layer": {"type": "integer", "default": 0,
                        "description": "Draw order; higher layers draw on top."},
                    "visible": {"type": "boolean", "default": true}}),
                &["texture"],
            ),
            example: json!({"texture": "asset:player-texture", "size": [1, 1]}),
        },
        ComponentDoc {
            name: "Camera2D",
            description: "The scene's 2D camera. At most one per scene. `follow` makes the \
                camera track another entity (which must have a Transform).",
            requires: &[],
            schema: closed(
                json!({"version": version(),
                    "logicalSize": {"type": "array", "items": {"type": "integer", "minimum": 1, "maximum": 8192},
                        "minItems": 2, "maxItems": 2, "default": [320, 180],
                        "description": "Logical resolution [width, height] in pixels."},
                    "pixelsPerUnit": {"type": "number", "exclusiveMinimum": 0, "default": 16,
                        "description": "Pixels per world unit."},
                    "center": vector(2, "Camera centre [x, y] in world units (ignored while following).",
                        Some(json!([0, 0]))),
                    "follow": {"type": "string", "description":
                        "Optional entity ID to follow; that entity must have a Transform."},
                    "pixelSnap": {"type": "boolean", "default": true,
                        "description": "Snap sprites to whole pixels."}}),
                &[],
            ),
            example: json!({"logicalSize": [320, 180], "pixelsPerUnit": 16}),
        },
        ComponentDoc {
            name: "Collider2D",
            description: "Axis-aligned 2D box collider; a trigger reports overlaps instead of \
                blocking. Requires Transform.",
            requires: &["Transform"],
            schema: closed(
                json!({"version": version(),
                    "size": {"type": "array", "items": {"type": "number", "exclusiveMinimum": 0, "maximum": 1_000_000},
                        "minItems": 2, "maxItems": 2, "description": "Box size [width, height]."},
                    "offset": {"type": "array", "items": {"type": "number", "minimum": -1_000_000, "maximum": 1_000_000},
                        "minItems": 2, "maxItems": 2, "default": [0, 0],
                        "description": "Box offset [x, y] from the entity position."},
                    "trigger": {"type": "boolean", "default": false},
                    "layer": {"type": "integer", "minimum": 1, "maximum": 4_294_967_295u64, "default": 1,
                        "description": "Collision layer bitmask; must be non-zero."},
                    "mask": {"type": "integer", "minimum": 1, "maximum": 4_294_967_295u64,
                        "default": 4_294_967_295u64,
                        "description": "Layers this collider interacts with (bitmask); non-zero."}}),
                &["size"],
            ),
            example: json!({"size": [1, 1]}),
        },
    ]
}

pub const SCENE_RULES: [&str; 5] = [
    "Entity `id` is the stable authored identifier: unique within the scene, 1-256 characters, no control characters. It cannot be changed after creation.",
    "A scene may contain at most one Camera2D.",
    "SpriteRenderer and Collider2D require a Transform on the same entity.",
    "Camera2D.follow must name an entity that has a Transform.",
    "Asset references (Script.asset, MeshRenderer.mesh/texture, SpriteRenderer.texture) must be catalog asset IDs of the right kind.",
];

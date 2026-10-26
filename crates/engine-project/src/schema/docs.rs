//! Shared built-in authoring component documentation.
use serde_json::{Value, json};
/// The component keys of an entity's `components` object, in schema order.
pub const COMPONENT_NAMES: [&str; 16] = [
    "Camera3D",
    "MeshMaterial",
    "EventBindings",
    "Transform",
    "Script",
    "MeshRenderer",
    "SpriteRenderer",
    "Camera2D",
    "Collider2D",
    "DirectionalLight",
    "PointLight",
    "SpotLight",
    "AreaLight",
    "PostProcess",
    "PostProcessVolume",
    "RayTracing",
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

fn lighting_schema() -> Value {
    closed(
        json!({
            "ambient":{"type":"number","minimum":0,"maximum":1,"default":0.08,
                "description":"Ambient radiance added to lit materials (already exposed)."},
            "exposure":{"type":"number","minimum":1e-8,"maximum":1000,"default":1,
                "description":"Linear factor from photometric light units (lux, lumens) to output: a 1 lux directional light on a white diffuse surface gives exposure/pi. Physically based scenes use 1/(1.2*2^EV100), about 2.5e-5 for sunlight at EV100 15. Lights are DirectionalLight, PointLight, SpotLight and AreaLight entities."}
        }),
        &[],
    )
}

fn upscaler_schema() -> Value {
    closed(
        json!({
            "mode":{"type":"string","enum":["none","dlss_sr"],"default":"none",
                "description":"Temporal upscaler for the 3D scene. dlss_sr requests NVIDIA DLSS Super Resolution (Vulkan, desktop `dlss` build feature, official NGX runtime, supported NVIDIA GPU); otherwise the scene renders natively and the run report states the fallback reason."},
            "quality":{"type":"string","enum":["ultra_performance","performance","balanced","quality","dlaa"],"default":"quality",
                "description":"DLSS preset. dlaa renders at output resolution (anti-aliasing only); the others let NGX choose a smaller render size."}
        }),
        &[],
    )
}

fn frame_generation_schema() -> Value {
    closed(
        json!({
            "mode":{"type":"string","enum":["off","dlss_fg"],"default":"off",
                "description":"dlss_fg requests NVIDIA DLSS Frame Generation (Vulkan, desktop `dlss` build feature, official SR and FG NGX runtimes, supported NVIDIA GPU, no native render passes); otherwise only real frames are presented and the run report states the fallback reason."},
            "multiplier":{"type":"integer","minimum":2,"maximum":4,"default":2,
                "description":"Presented frames per rendered frame (2 inserts one generated frame). Also limited at runtime by NGX's reported maximum."}
        }),
        &[],
    )
}

/// All authored component documents.
pub fn component_docs() -> Vec<ComponentDoc> {
    let mut docs = builtin_docs();
    docs.extend(super::light_docs::light_docs());
    docs.extend(super::post_docs::post_docs());
    docs.push(super::post_docs::ray_tracing_doc());
    docs
}

fn builtin_docs() -> Vec<ComponentDoc> {
    vec![
        ComponentDoc {
            name: "Camera3D",
            description: "Perspective or orthographic camera, right-handed Y-up and negative-Z forward. Active camera selection uses stable entity ID order. `upscaler` requests DLSS Super Resolution and `frame_generation` DLSS Frame Generation; unsupported requests render natively with a reported reason. `render_features` enables or disables render features in this camera view.",
            requires: &["Transform"],
            schema: closed(
                json!({"version":version(),"active":{"type":"boolean","default":true},"lighting":lighting_schema(),"upscaler":upscaler_schema(),"frame_generation":frame_generation_schema(),"render_features":{"type":"object","maxProperties":64,"propertyNames":{"pattern":"^[a-z0-9._-]{1,64}$"},"additionalProperties":{"type":"boolean"},"default":{},
                    "description":"Per-camera render feature overrides by stable feature name (for example engine.opaque, engine.shadows, engine.sprites, the UI features ui.sdf_text, ui.world and ui.color_filter of engine-ui-render, or a native feature such as world.terrain): false disables the feature in this camera view, true enables a feature that is off by default. Required features (engine.output) cannot be disabled. Unknown names are reported in the run report's render_pipeline.unknown_overrides."},"projection":{"oneOf":[
                    closed(json!({"kind":{"const":"perspective"},"fov_y_radians":{"type":"number","exclusiveMinimum":0,"exclusiveMaximum":std::f32::consts::PI},"near":{"type":"number","exclusiveMinimum":0},"far":{"type":"number","exclusiveMinimum":0}}), &["kind","fov_y_radians","near","far"]),
                    closed(json!({"kind":{"const":"orthographic"},"height":{"type":"number","exclusiveMinimum":0},"near":{"type":"number","exclusiveMinimum":0},"far":{"type":"number","exclusiveMinimum":0}}), &["kind","height","near","far"])
                ]}}),
                &[],
            ),
            example: json!({"projection":{"kind":"perspective","fov_y_radians":std::f32::consts::FRAC_PI_3,"near":0.1,"far":1000.0}}),
        },
        ComponentDoc {
            name: "MeshMaterial",
            description: "Material for MeshRenderer or SpriteRenderer: custom WGSL shader or material graph, texture, tint and 3D transparency/lighting controls.",
            requires: &[],
            schema: closed(
                json!({"version":version(),"shader":{"type":["string","null"],"description":"Catalog shader asset ID or graph asset ID whose document kind is material."},"texture":{"type":["string","null"],"description":"Catalog texture asset ID, or stream:<name> for a texture stream registered natively at runtime (video, camera feeds)."},"tint":{"type":"array","items":{"type":"number","minimum":0,"maximum":1},"minItems":4,"maxItems":4,"default":[1,1,1,1]},"unlit":{"type":"boolean","default":false},"transparent":{"type":"boolean","default":false},"metallic":{"type":"number","minimum":0,"maximum":1,"default":0},"roughness":{"type":"number","minimum":0,"maximum":1,"default":0.5},"cast_shadows":{"type":"boolean","default":true},"receive_shadows":{"type":"boolean","default":true},"normal_scale":{"type":"number","minimum":0,"maximum":10,"default":1},"normal_texture":{"type":["string","null"],"minLength":1},"metallic_roughness_texture":{"type":["string","null"],"minLength":1}}),
                &[],
            ),
            example: json!({"tint":[0.4,0.8,1.0,1.0],"unlit":true}),
        },
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

pub const SCENE_RULES: [&str; 7] = [
    "Entity `id` is the stable authored identifier: unique within the scene, 1-256 characters, no control characters. It cannot be changed after creation.",
    "A scene may contain at most one Camera2D.",
    "SpriteRenderer, Collider2D, Camera3D, PointLight, SpotLight and AreaLight require a Transform on the same entity.",
    "Camera2D.follow must name an entity that has a Transform.",
    "Asset references (Script.asset, MeshRenderer.mesh/texture, SpriteRenderer.texture, MeshMaterial.shader/texture/normal_texture/metallic_roughness_texture) must be catalog asset IDs of the right kind; MeshMaterial.shader also accepts a graph asset with document kind material.",
    "A scene may contain at most one `engine.AcousticEnvironment`; `engine.AudioEmitter.clip` must be an `audio` asset, `engine.AcousticGeometry.mesh` and `engine.AcousticVolume.mesh` `mesh` assets, and `engine.AcousticEnvironment.geometry.asset` an `acoustics` asset.",
    "A scene may contain at most one `engine.NetworkSession`; `engine.NetworkReplication` requires `engine.NetworkReplica` on the same entity, and `engine.NetworkHitboxes` and `engine.NetworkVoice` require a Transform.",
];

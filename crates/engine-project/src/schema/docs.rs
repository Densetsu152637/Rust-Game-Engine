//! Shared built-in authoring component documentation.
use serde_json::{Value, json};
/// The component keys of an entity's `components` object, in schema order.
pub const COMPONENT_NAMES: [&str; 19] = [
    "Camera3D",
    "MeshMaterial",
    "EventBindings",
    "Transform",
    "Script",
    "MeshRenderer",
    "MeshLods",
    "HiZOccluder",
    "MeshLightmap",
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
                "description":"Linear factor from photometric light units (lux, lumens) to output: a 1 lux directional light on a white diffuse surface gives exposure/pi. Physically based scenes use 1/(1.2*2^EV100), about 2.5e-5 for sunlight at EV100 15. Lights are DirectionalLight, PointLight, SpotLight and AreaLight entities."},
            "indirect":{"type":["string","null"],"minLength":1,
                "description":"Optional catalog indirect asset ID; externally prepared IBL and probe data. Absent uses constant ambient."}
        }),
        &[],
    )
}

fn upscaler_schema() -> Value {
    closed(
        json!({
            "mode":{"type":"string","enum":["none","dlss_sr","native_temporal","auto_temporal"],"default":"none",
                "description":"Temporal upscaler for the 3D scene; one owner supplies the jitter and one Upscale winner runs per view. dlss_sr requests NVIDIA DLSS Super Resolution (Vulkan, desktop `dlss` build feature, official NGX runtime, supported NVIDIA GPU); otherwise the scene renders natively and the run report states the fallback reason (it never becomes the native resolve). native_temporal requests the engine's own vendor-neutral WGSL temporal resolve (jitter, motion vectors, history, neighbourhood clipping, disocclusion rejection; not FSR, XeSS or DLSS). auto_temporal runs DLSS when it can and the native resolve otherwise. A running temporal resolve skips spatial_aa for that frame and the run report says so."},
            "quality":{"type":"string","enum":["ultra_performance","performance","balanced","quality","dlaa"],"default":"quality",
                "description":"Preset. dlaa renders at output resolution (anti-aliasing only); the others render smaller: NGX chooses the DLSS size, the native resolve uses per-axis scales 1/3, 0.5, 0.58 and 2/3 for ultra_performance, performance, balanced and quality."}
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

fn depth_of_field_schema() -> Value {
    json!({"oneOf":[closed(json!({
        "focus_distance":{"type":"number","exclusiveMinimum":0,"default":5.0,"description":"Positive view-space focus distance."},
        "focus_range":{"type":"number","exclusiveMinimum":0,"default":3.0,"description":"View-space distance from focus to maximum blur."},
        "max_radius_pixels":{"type":"number","minimum":0,"maximum":24,"default":8.0,"description":"Maximum output-pixel blur radius; zero skips the pass."},
        "edge_tolerance":{"type":"number","minimum":0,"default":0.5,"description":"Maximum view-space depth separation for a gather sample."}
    }), &[]), {"type":"null"}],"description":"Optional depth of field; absent or null disables it. Camera3D.render_features.effects.dof=false takes precedence."})
}

fn motion_blur_schema() -> Value {
    json!({"oneOf":[closed(json!({
        "shutter_scale":{"type":"number","minimum":0,"maximum":2,"default":0.5,"description":"Fraction of the current-to-previous motion path; zero skips the pass."},
        "max_radius_pixels":{"type":"number","minimum":0,"maximum":24,"default":16.0,"description":"Maximum output-pixel blur radius."},
        "edge_tolerance":{"type":"number","minimum":0,"default":0.5,"description":"Maximum view-space depth separation along the path."}
    }), &[]), {"type":"null"}],"description":"Optional motion blur; absent or null disables it. Camera3D.render_features.effects.motion_blur=false takes precedence."})
}

fn spatial_aa_schema() -> Value {
    json!({"oneOf":[closed(json!({
        "edge_threshold":{"type":"number","minimum":0,"maximum":1,"default":0.12,"description":"Relative local HDR-luma contrast threshold."},
        "strength":{"type":"number","minimum":0,"maximum":1,"default":0.75,"description":"Edge-directed blend fraction; zero skips the pass."}
    }), &[]), {"type":"null"}],"description":"Optional current-frame spatial AA; absent or null disables it. Camera3D.render_features.effects.spatial_aa=false takes precedence."})
}

fn screen_space_reflections_schema() -> Value {
    json!({"oneOf":[closed(json!({
        "max_steps":{"type":"integer","minimum":1,"maximum":256,"default":64,"description":"Samples along each projected ray; the stride grows when the ray covers more pixels."},
        "max_distance":{"type":"number","exclusiveMinimum":0,"maximum":10000,"default":30.0,"description":"Longest ray in view-space units; hits fade over its last quarter."},
        "thickness":{"type":"number","exclusiveMinimum":0,"maximum":100,"default":0.5,"description":"View-space depth a ray may lie behind a surface and still hit it."},
        "roughness_cutoff":{"type":"number","exclusiveMinimum":0,"maximum":1,"default":0.4,"description":"Perceptual roughness above which a built-in lit surface is not reflected; the blend fades between half the cutoff and the cutoff."},
        "edge_fade":{"type":"number","minimum":0,"maximum":0.5,"default":0.1,"description":"Screen-edge fade width of the hit position as a screen fraction."},
        "intensity":{"type":"number","minimum":0,"maximum":4,"default":1.0,"description":"Multiplier of the traced radiance; zero skips the pass."}
    }), &[]), {"type":"null"}],"description":"Optional screen-space reflections of built-in lit materials over the opaque scene; absent or null disables them. Camera3D.render_features.engine.ssr=false takes precedence, and an enabled engine.rt.reflections replaces them for the camera. Custom-shader and unlit materials are not reflective."})
}

fn atmosphere_schema() -> Value {
    let coefficient = |default: f64, description: &str| json!({"type":"number","minimum":0,"maximum":0.01,"default":default,"description":description});
    let channels = |defaults: [f64; 3], maximum: f64, description: &str| json!({"type":"array","items":{"type":"number","minimum":0,"maximum":maximum},"minItems":3,"maxItems":3,"default":defaults,"description":description});
    let height = |default: f64, description: &str| json!({"type":"number","minimum":1,"maximum":1.0e6,"default":default,"description":description});
    json!({"oneOf":[closed(json!({
        "planet_radius":{"type":"number","minimum":1.0e3,"maximum":1.0e8,"default":6_360_000.0,"description":"Planet radius in metres. The surface is world y = 0."},
        "atmosphere_radius":{"type":"number","exclusiveMinimum":0,"default":6_460_000.0,"description":"Radius of the top of the atmosphere in metres; 100 m to 1e7 m above planet_radius."},
        "rayleigh_scattering":channels([5.802e-6, 13.558e-6, 33.1e-6], 0.01, "Rayleigh scattering at the ground per colour channel, 1/m."),
        "rayleigh_scale_height":height(8000.0, "Exponential scale height of Rayleigh density in metres."),
        "mie_scattering":coefficient(3.996e-6, "Mie scattering at the ground, 1/m."),
        "mie_absorption":coefficient(0.444e-6, "Mie absorption at the ground, 1/m."),
        "mie_scale_height":height(1200.0, "Exponential scale height of Mie density in metres."),
        "mie_anisotropy":{"type":"number","minimum":-0.99,"maximum":0.99,"default":0.8,"description":"Cornette-Shanks asymmetry; positive scatters forward."},
        "ozone_absorption":channels([0.650e-6, 1.881e-6, 0.085e-6], 0.01, "Ozone absorption at the centre of its layer per colour channel, 1/m."),
        "ozone_center_height":{"type":"number","minimum":0,"maximum":1.0e7,"default":25_000.0,"description":"Altitude of the ozone layer centre in metres."},
        "ozone_width":{"type":"number","minimum":1,"maximum":1.0e7,"default":15_000.0,"description":"Distance in metres from the layer centre to where ozone density reaches zero."},
        "ground_albedo":channels([0.3, 0.3, 0.3], 1.0, "Lambertian albedo of the planet seen below the horizon."),
        "sun_angular_diameter":{"type":"number","minimum":0,"maximum":0.5,"default":0.0093,"description":"Sun disk angular diameter in radians; zero hides the disk."},
        "intensity":{"type":"number","minimum":0,"maximum":1000,"default":1.0,"description":"Multiplier on the sun irradiance (lux times lighting exposure) seen by the atmosphere; zero skips both passes."},
        "aerial_perspective_scale":{"type":"number","minimum":0,"maximum":1000,"default":1.0,"description":"Multiplier on geometry view distance for aerial perspective on opaque meshes; one is physical, zero disables it."}
    }), &[]), {"type":"null"}],"description":"Optional single-scattering atmosphere (sky, planet, sun disk, aerial perspective); absent or null disables it. The sun is the first directional light; with none the atmosphere draws nothing. Camera3D.render_features.engine.atmosphere=false and engine.atmosphere.aerial=false disable the passes individually."})
}

fn decals_schema() -> Value {
    json!({"type":"array","maxItems":64,"default":[],"items":closed(json!({
        "position":{"type":"array","items":{"type":"number","minimum":-100000,"maximum":100000},"minItems":3,"maxItems":3,"default":[0,0,0],"description":"World position of the box centre."},
        "rotation":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4,"default":[0,0,0,1],"description":"Orientation quaternion [x, y, z, w]; finite and nonzero. The decal faces local +Z and projects toward -Z."},
        "half_extents":{"type":"array","items":{"type":"number","exclusiveMinimum":0,"maximum":100000},"minItems":3,"maxItems":3,"default":[0.5,0.5,0.5],"description":"Half size of the projection box along local X, Y and Z."},
        "albedo":{"type":"array","items":{"type":"number","minimum":0,"maximum":1},"minItems":3,"maxItems":3,"default":[1,1,1],"description":"Linear albedo."},
        "opacity":{"type":"number","minimum":0,"maximum":1,"default":1.0,"description":"Opacity of the albedo response."},
        "blend":{"enum":["multiply","paint"],"default":"multiply","description":"multiply: lit * lerp(1, albedo, opacity). paint: lerp(lit, albedo * luma(lit), opacity)."},
        "emissive":{"type":"array","items":{"type":"number","minimum":0,"maximum":1000},"minItems":3,"maxItems":3,"default":[0,0,0],"description":"Radiance added where the decal covers the surface; not scaled by opacity."},
        "max_angle":{"type":"number","exclusiveMinimum":0,"maximum":std::f64::consts::FRAC_PI_2,"default":1.2217,"description":"Largest radians between the surface normal and the decal axis that still receives the decal."},
        "angle_fade":{"type":"number","minimum":0,"default":0.2618,"description":"Fade width in radians below max_angle; at most max_angle."},
        "edge_fade":{"type":"number","minimum":0,"maximum":1,"default":0.1,"description":"Fade width at each box face as a fraction of the half extent."}
    }), &[]),"description":"Optional projected decals in world space, drawn in list order. Absent or empty disables them. Camera3D.render_features.engine.decals=false takes precedence. Decals tint the lit scene colour of opaque built-in meshes; they do not change normals or roughness (docs/rendering-decals.md)."})
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
            description: "Perspective or orthographic camera, right-handed Y-up and negative-Z forward. Active camera selection uses stable entity ID order. `upscaler` requests DLSS Super Resolution or the native temporal resolve and `frame_generation` DLSS Frame Generation; unsupported requests render natively with a reported reason. Optional `depth_of_field`, `motion_blur` and `spatial_aa` enable bounded native post effects unless `render_features` explicitly disables them. Optional `screen_space_reflections` enables bounded screen-space reflections of built-in lit materials (docs/rendering-ssr.md). Optional `atmosphere` adds a physically based sky, planet, sun disk and aerial perspective driven by the first directional light (docs/rendering-atmosphere.md). Optional `decals` lists up to 64 world-space projected decals (docs/rendering-decals.md).",
            requires: &["Transform"],
            schema: closed(
                json!({"version":version(),"active":{"type":"boolean","default":true},"lighting":lighting_schema(),"upscaler":upscaler_schema(),"frame_generation":frame_generation_schema(),"depth_of_field":depth_of_field_schema(),"motion_blur":motion_blur_schema(),"spatial_aa":spatial_aa_schema(),"screen_space_reflections":screen_space_reflections_schema(),"atmosphere":atmosphere_schema(),"decals":decals_schema(),"render_features":{"type":"object","maxProperties":64,"propertyNames":{"pattern":"^[a-z0-9._-]{1,64}$"},"additionalProperties":{"type":"boolean"},"default":{},
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
            name: "MeshLods",
            description: "Additional mesh assets, finest to coarsest, selected per camera with screen-height thresholds and hysteresis. MeshRenderer.mesh is level zero; shadows retain its base geometry.",
            requires: &["MeshRenderer"],
            schema: closed(
                json!({"version":version(),
                    "meshes":{"type":"array","maxItems":7,"items":{"type":"string","minLength":1},"default":[]},
                    "thresholds":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"number","exclusiveMinimum":0,"maximum":1},"default":[1.0]},
                    "hysteresis":{"type":"number","minimum":0,"exclusiveMaximum":1,"default":0.1}}),
                &[],
            ),
            example: json!({"meshes":["asset:player-mesh-low"],"thresholds":[0.4,0.05],"hysteresis":0.1}),
        },
        ComponentDoc {
            name: "HiZOccluder",
            description: "Explicit permission to consider this MeshRenderer as a current-frame Hi-Z depth writer. Native opacity and geometry budget checks may still reject it. Camera3D render_features must enable engine.hiz.visibility.",
            requires: &["MeshRenderer"],
            schema: closed(
                json!({"version":version(),"enabled":{"type":"boolean","default":true}}),
                &[],
            ),
            example: json!({"enabled":true}),
        },
        ComponentDoc {
            name: "MeshLightmap",
            description: "Samples a supplied lightmap of the active Camera3D's indirect asset with the mesh's second UV set (glTF TEXCOORD_1): uv = uv2 * scale + offset, so entities sharing one mesh can address different atlas regions. mode `replace` makes the lightmap the diffuse indirect term, `add` adds it to the probe or ambient diffuse; specular IBL is unchanged. The mesh and every MeshLods level need UV2; custom MeshMaterial.shader is rejected. Supplied data only: nothing is baked.",
            requires: &["MeshRenderer"],
            schema: closed(
                json!({"version":version(),
                    "lightmap":{"type":"integer","minimum":0,"maximum":7,"default":0,"description":"Index into the indirect asset's `lightmaps`."},
                    "scale":{"type":"array","items":{"type":"number","minimum":0.0000152590219,"maximum":1},"minItems":2,"maxItems":2,"default":[1,1]},
                    "offset":{"type":"array","items":{"type":"number","minimum":0,"maximum":1},"minItems":2,"maxItems":2,"default":[0,0],"description":"offset + scale must not exceed 1 on either axis."},
                    "mode":{"enum":["replace","add"],"default":"replace"}}),
                &[],
            ),
            example: json!({"lightmap":0,"scale":[0.5,0.5],"offset":[0.5,0],"mode":"replace"}),
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
    "Asset references (Script.asset, MeshRenderer.mesh/texture, MeshLods.meshes, SpriteRenderer.texture, MeshMaterial.shader/texture/normal_texture/metallic_roughness_texture, Camera3D.lighting.indirect) must be catalog asset IDs of the right kind; MeshMaterial.shader also accepts a graph asset with document kind material.",
    "A scene may contain at most one `engine.AcousticEnvironment`; `engine.AudioEmitter` selects exactly one nonempty `clip` (`audio` asset) or `graph` (`graph` asset of kind `audio`) at runtime. File clips use bounded resident decoding by default or incremental decoding with `streamed: true`; graphs use bounded procedural streaming with atomic initial `parameters`. Nonresident sources reject `looped: true`; empty default emitter references remain structurally valid for authoring. `engine.AcousticGeometry.mesh` and `engine.AcousticVolume.mesh` must reference `mesh` assets, and `engine.AcousticEnvironment.geometry.asset` an `acoustics` asset.",
    "A scene may contain at most one `engine.NetworkSession`; `engine.NetworkReplication` requires `engine.NetworkReplica` on the same entity, and `engine.NetworkHitboxes` and `engine.NetworkVoice` require a Transform.",
];

//! Catalog descriptors for the acoustic and audio components. The schema subset
//! of the component catalog has no nullable types, so optional properties are
//! simply absent when unset.
use super::components::*;
use crate::ComponentDescriptor;
use serde_json::{Value, json};

fn number(min: f64, max: f64, description: &str) -> Value {
    json!({"type":"number","minimum":min,"maximum":max,"description":description})
}
fn integer(min: u64, max: u64, description: &str) -> Value {
    json!({"type":"integer","minimum":min,"maximum":max,"description":description})
}
fn boolean(description: &str) -> Value {
    json!({"type":"boolean","description":description})
}
fn text(max: u64, description: &str) -> Value {
    json!({"type":"string","maxLength":max,"description":description})
}
fn choice(values: &[&str], description: &str) -> Value {
    json!({"type":"string","enum":values,"description":description})
}
fn bands(min: f64, max: f64, description: &str) -> Value {
    json!({"type":"array","items":{"type":"number","minimum":min,"maximum":max},"minItems":4,"maxItems":4,"description":description})
}
fn object(properties: Value, description: &str) -> Value {
    json!({"type":"object","properties":properties,"additionalProperties":false,"description":description})
}

fn proxy_schema() -> Value {
    let preset = |what: &str| choice(&Preset::NAMES, what);
    object(
        json!({
            "plane": choice(&["side_view","top_down"], "side_view maps 2D (x, y) to acoustic (x, y) with depth along z (platformers); top_down maps (x, y) to (x, height, -y)."),
            "units_per_meter": number(0.001, 1000.0, "World units per acoustic metre; set it so a character maps to about 1.8 m."),
            "depth_m": number(0.1, 1000.0, "Side view: extrusion depth in metres."),
            "enclose_depth": boolean("Side view: close the slice with front and back planes."),
            "wall_height_m": number(0.1, 1000.0, "Top-down: footprint extrusion height; side view: unused."),
            "ear_height_m": number(0.0, 1000.0, "Top-down: source and listener height."),
            "floor": boolean("Top-down: add a floor."),
            "ceiling": boolean("Top-down: add a ceiling unless open_sky."),
            "open_sky": boolean("Remove the top face so rays escape and the reverb falls."),
            "enclosure": choice(&["none","auto","bounds"], "none, auto (level bounds plus margin_m) or an explicit rectangle in bounds."),
            "margin_m": number(0.0, 1000.0, "Margin around the auto enclosure."),
            "bounds": json!({"type":"array","items":{"type":"number","minimum":-1_000_000,"maximum":1_000_000},"minItems":4,"maxItems":4,"description":"[min_x, min_y, max_x, max_y] in world units for enclosure bounds."}),
            "colliders": boolean("Non-trigger Collider2D entities are footprints."),
            "physics_bodies": boolean("Non-sensor engine.PhysicsBody2D entities are footprints."),
            "material": preset("Material of footprints without engine.AcousticMaterial."),
            "floor_material": preset("Enclosure floor material."),
            "ceiling_material": preset("Enclosure ceiling material."),
            "side_material": preset("Enclosure side/depth plane material.")
        }),
        "2D to 3D proxy world synthesized from Collider2D, PhysicsBody2D and proxy2d volumes. Absent means a 3D scene.",
    )
}

fn environment_schema() -> Value {
    object(
        json!({
            "enabled": boolean("Master switch; a disabled environment reports state disabled."),
            "backend": choice(&["auto","cpu","gpu_compute","gpu_ray_query"], "Requested propagation tier. A request is not a claim: the status reports the active tier and the fallback reason."),
            "renderer": choice(&["binaural","stereo"], "binaural is the dataset-free structural head model (headphones); stereo is equal-power panning."),
            "reverb": choice(&["off","parametric","convolution"], "Late reverb renderer."),
            "reverb_source": choice(&["traced","volumes","off"], "traced fits the reverb to traced energy; volumes uses analytic room volumes."),
            "update_hz": number(1.0, 120.0, "Occlusion and early reflection update rate."),
            "reverb_update_hz": number(0.1, 60.0, "Late field update rate."),
            "speed_of_sound": number(300.0, 400.0, "Metres per second."),
            "air": object(json!({
                "enabled": boolean("false disables air absorption."),
                "temperature_c": number(-20.0, 50.0, "ISO 9613-1 temperature."),
                "humidity_percent": number(0.0, 100.0, "ISO 9613-1 relative humidity."),
                "absorption_db_per_m": bands(0.0, 10.0, "Explicit per-band dB/m (125, 500, 2000, 8000 Hz); overrides the estimate.")
            }), "Air absorption."),
            "quality": object(json!({
                "occlusion_rays": integer(0, 256, "Rays per occlusion test."),
                "reflection_order": integer(0, 3, "Image-source order (at most 3 on CPU)."),
                "max_reflections": integer(0, 16, "Strongest reflection taps kept per source."),
                "er_rays": integer(0, 4096, "Rays used to find reflecting planes."),
                "reverb_rays": integer(64, 1_048_576, "Reverb rays per update (cpu backend: at most 65536)."),
                "max_bounces": integer(1, 256, "Bounces per ray."),
                "histogram_length_s": number(0.1, 4.0, "Energy histogram length."),
                "histogram_bin_ms": number(0.5, 10.0, "Histogram bin width."),
                "late_start_ms": number(10.0, 500.0, "Late field starts here."),
                "receiver_radius_m": number(0.1, 10.0, "Receiver sphere radius."),
                "time_budget_ms": number(0.5, 100.0, "Per-update time budget in threaded mode.")
            }), "Propagation quality and cost."),
            "seed": integer(0, u64::MAX >> 11, "Deterministic tracing seed."),
            "geometry": object(json!({
                "render_meshes": boolean("MeshRenderer meshes contribute acoustic geometry."),
                "physics_bodies": boolean("Collider-only, non-sensor PhysicsBody3D boxes contribute acoustic geometry."),
                "asset": text(128, "Baked acoustics asset used instead of live static collection."),
                "default_material": choice(&Preset::NAMES, "Material of geometry without engine.AcousticMaterial."),
                "mask": integer(1, 255, "Instance mask for geometry without engine.AcousticGeometry.")
            }), "Geometry sources."),
            "proxy2d": proxy_schema()
        }),
        "Scene-wide acoustics configuration. At most one per scene; no Transform required.",
    )
}

fn descriptor(
    name: &str,
    defaults: impl serde::Serialize,
    schema: Value,
    requires: &[&str],
    data_refs: &[&str],
) -> ComponentDescriptor {
    ComponentDescriptor {
        name: name.into(),
        version: 1,
        schema,
        defaults: serde_json::to_value(defaults).expect("finite builtin defaults"),
        requires: requires.iter().map(|s| (*s).into()).collect(),
        entity_references: data_refs.iter().map(|s| (*s).into()).collect(),
        data_only: false,
    }
}

/// Descriptors chained into [`crate::ComponentCatalog::new`].
pub fn descriptors() -> Vec<ComponentDescriptor> {
    let transform = ["Transform"];
    vec![
        descriptor(
            ENVIRONMENT,
            AcousticEnvironment::default(),
            environment_schema(),
            &[],
            &[],
        ),
        descriptor(
            MATERIAL,
            AcousticMaterialSpec::default(),
            object(
                json!({
                    "preset": choice(&Preset::NAMES, "Published material row; each row cites its source in engine-acoustics."),
                    "absorption": bands(0.0, 1.0, "Per-band energy absorption override (125, 500, 2000, 8000 Hz)."),
                    "scattering": number(0.0, 1.0, "Scattering coefficient override."),
                    "transmission": bands(0.0, 1.0, "Per-band transmission override; must not exceed absorption.")
                }),
                "Acoustic surface of this entity's geometry, volume or 2D footprint.",
            ),
            &transform,
            &[],
        ),
        descriptor(
            GEOMETRY,
            AcousticGeometry::default(),
            object(
                json!({
                    "mode": choice(&["static","dynamic","exclude"], "static never moves after build; dynamic updates its transform every tick; exclude removes the entity's mesh."),
                    "mesh": text(128, "Simplified acoustic mesh asset replacing the render mesh."),
                    "mask": integer(1, 255, "Instance mask.")
                }),
                "Controls whether and how this entity's mesh contributes to the acoustic world.",
            ),
            &transform,
            &[],
        ),
        descriptor(
            VOLUME,
            AcousticVolume::default(),
            object(
                json!({
                    "shape": choice(&["box","mesh"], "Box (half_extents) or mesh asset."),
                    "half_extents": json!({"type":"array","items":{"type":"number","minimum":0.001,"maximum":100000},"minItems":3,"maxItems":3,"description":"Box half extents in metres (proxy2d: the first two are the footprint)."}),
                    "mesh": text(128, "Mesh asset for shape mesh."),
                    "kind": choice(&["solid","room"], "room creates an inward shell with optional open faces and an analytic RT60."),
                    "open_faces": json!({"type":"array","maxItems":6,"items":{"type":"string","enum":["-x","+x","-y","+y","-z","+z"]},"description":"Room faces left open."}),
                    "space": choice(&["world","proxy2d"], "proxy2d volumes are 2D footprints of the proxy world."),
                    "mobility": choice(&["static","dynamic"], "dynamic volumes follow their transform every tick.")
                }),
                "Acoustic-only solid or room.",
            ),
            &transform,
            &[],
        ),
        descriptor(
            SOURCE,
            AcousticSource::default(),
            object(
                json!({
                    "enabled": boolean("Enable acoustic propagation for voices this entity owns."),
                    "occlusion": boolean("Trace occlusion and transmission."),
                    "reflections": boolean("Trace early reflections."),
                    "reverb_send": number(0.0, 1.0, "Scalar reverb send."),
                    "doppler": number(0.0, 4.0, "Doppler scale."),
                    "propagation_delay": boolean("Delay line providing latency and Doppler."),
                    "air_absorption": boolean("Per-band air absorption."),
                    "radius_m": number(0.0, 10.0, "Source radius for volumetric occlusion."),
                    "priority": number(0.0, 1000.0, "Ranking weight when more sources than the trace budget exist."),
                    "hero": boolean("Per-pair convolution for this source (at most 4).")
                }),
                "Acoustic options for voices owned by this entity; wins over native PlayOptions.acoustics while an environment is enabled.",
            ),
            &transform,
            &[],
        ),
        descriptor(
            EMITTER,
            AudioEmitter::default(),
            object(
                json!({
                    "clip": text(256, "Asset ID of kind audio."),
                    "streamed": boolean("Decode the audio asset incrementally into bounded PCM."),
                    "graph": text(256, "Graph asset of kind audio, exclusive with clip."),
                    "parameters": json!({"type":"object","additionalProperties":true,"description":"Up to 64 finite Float graph parameter overrides."}),
                    "queue_frames": integer(2, 65536, "Bounded nonresident PCM queue capacity."),
                    "duration_frames": integer(0, 192000_u64*86400, "Optional generated-frame duration for procedural audio."),
                    "autoplay": boolean("Start when the scene starts."),
                    "looped": boolean("Loop the clip."),
                    "bus": choice(&["music","effects","dialogue"], "Mixer bus."),
                    "gain": number(0.0, 1.0, "Linear gain."),
                    "pitch": number(0.125, 8.0, "Playback rate multiplier."),
                    "spatial": object(json!({
                        "min_distance": number(0.0001, 1e6, "Distance below which gain is 1."),
                        "max_distance": number(0.0001, 1e6, "Distance beyond which the sound is silent."),
                        "rolloff": number(0.0, 100.0, "Rolloff factor."),
                        "attenuation": choice(&["none","linear","inverse"], "Distance law.")
                    }), "Distance model; the voice follows the entity transform. Omit for a non-positional clip."),
                    "play_on": text(128, "Event name that restarts playback (audio::notify, including trigger:<entity>:<phase>)."),
                    "stop_on": text(128, "Event name that stops playback.")
                }),
                "Plays an audio clip from this entity.",
            ),
            &transform,
            &[],
        ),
        descriptor(
            LISTENER,
            AudioListener::default(),
            object(
                json!({"active": boolean("The entity's world pose is the listener while active.")}),
                "The audio listener. The first active listener in authored-ID order wins.",
            ),
            &transform,
            &[],
        ),
    ]
}

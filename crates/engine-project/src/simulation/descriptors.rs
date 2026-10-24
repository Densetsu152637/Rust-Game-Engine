use super::*;
use serde_json::{Value, json};
fn vector(n: usize) -> Value {
    json!({"type":"array","items":{"type":"number"},"minItems":n,"maxItems":n})
}
fn descriptor<T: serde::Serialize>(
    name: &str,
    defaults: T,
    properties: Value,
) -> crate::ComponentDescriptor {
    crate::ComponentDescriptor {
        name: name.into(),
        version: 1,
        schema: json!({"type":"object","properties":properties,"additionalProperties":false}),
        defaults: serde_json::to_value(defaults).expect("finite builtin defaults"),
        requires: vec!["Transform".into()],
        entity_references: vec![],
        data_only: false,
    }
}
pub fn component_descriptors() -> Vec<crate::ComponentDescriptor> {
    let mut result = vec![];
    result.push(descriptor("engine.SkinBinding",SkinBinding::default(),json!({"model":{"type":"string","maxLength":128},"mesh_node":{"type":"integer","minimum":0,"maximum":4095},"primitive":{"type":"integer","minimum":0,"maximum":65535},"bindings":{"type":"array","maxItems":4096,"items":{"type":"string","maxLength":128}}})));
    result.extend(physics::component_descriptors());
    result.push(descriptor("engine.AnimationPlayer",AnimationPlayer::default(),json!({"clip":{"type":"string","maxLength":128},"looping":{"type":"boolean"},"speed":{"type":"number","minimum":0,"maximum":100},"bindings":{"type":"array","maxItems":4096,"items":{"type":"string","maxLength":128}}})));
    result.push(descriptor("engine.NavigationAgent",NavigationAgent::default(),json!({"speed":{"type":"number","minimum":0,"maximum":1000000},"waypoints":{"type":"array","maxItems":65536,"items":vector(3)},"navmesh":{"type":"string","maxLength":128},"target":vector(3),"repath":{"type":"integer","minimum":0},"query":{"type":"object","additionalProperties":false,"properties":{"max_distance":{"type":"number","minimum":0,"maximum":1000},"max_polygons":{"type":"integer","minimum":1,"maximum":65536},"max_expansions":{"type":"integer","minimum":0,"maximum":65536},"max_points":{"type":"integer","minimum":2,"maximum":65536}}}})));
    result
}

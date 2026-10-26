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
    result.push(descriptor("engine.AnimationGraphPlayer",AnimationGraphPlayer::default(),json!({"graph":{"type":"string","maxLength":128},"rig":{"type":"string","maxLength":128},"bindings":{"type":"array","maxItems":4096,"items":{"type":"string","maxLength":128}},"parameters":{"type":"object"}})));
    result.extend(physics::component_descriptors());
    result.push(descriptor("engine.AnimationPlayer",AnimationPlayer::default(),json!({"clip":{"type":"string","maxLength":128},"looping":{"type":"boolean"},"speed":{"type":"number","minimum":0,"maximum":100},"bindings":{"type":"array","maxItems":4096,"items":{"type":"string","maxLength":128}}})));
    result.push(descriptor("engine.NavigationAgent",NavigationAgent::default(),json!({"speed":{"type":"number","minimum":0,"maximum":1000000},"waypoints":{"type":"array","maxItems":65536,"items":vector(3)},"navmesh":{"type":"string","maxLength":128},"target":vector(3),"repath":{"type":"integer","minimum":0},"query":{"type":"object","additionalProperties":false,"properties":{"max_distance":{"type":"number","minimum":0,"maximum":1000},"max_polygons":{"type":"integer","minimum":1,"maximum":65536},"max_expansions":{"type":"integer","minimum":0,"maximum":65536},"max_points":{"type":"integer","minimum":2,"maximum":65536}}}})));
    let nav = json!({"type":"string","maxLength":128});
    let mut agent = descriptor(
        "engine.CrowdAgent",
        CrowdAgent::default(),
        json!({"navmesh":nav,"target":vector(3),"radius":{"type":"number","minimum":0.01,"maximum":100},"max_speed":{"type":"number","minimum":0,"maximum":1000},"arrival_radius":{"type":"number","minimum":0.001,"maximum":100},"formation":{"type":"string","maxLength":128},"repath":{"type":"integer","minimum":0}}),
    );
    agent.entity_references = vec!["/formation".into()];
    result.push(agent);
    result.push(descriptor("engine.NavigationObstacle",NavigationObstacle::default(),json!({"navmesh":nav,"shape":{"type":"string","enum":["box","cylinder"]},"half_extents":vector(3),"radius":{"type":"number","minimum":0,"maximum":10000},"half_height":{"type":"number","minimum":0,"maximum":10000},"enabled":{"type":"boolean"}})));
    result.push(descriptor("engine.OffMeshLink",OffMeshLink::default(),json!({"navmesh":nav,"end":vector(3),"cost":{"type":"number","minimum":0,"maximum":1000000},"bidirectional":{"type":"boolean"},"enabled":{"type":"boolean"},"snap_distance":{"type":"number","minimum":0,"maximum":100}})));
    let mut formation = descriptor(
        "engine.Formation",
        Formation::default(),
        json!({"navmesh":nav,"slots":{"type":"array","maxItems":256,"items":{"type":"array","items":{"type":"number"},"minItems":2,"maxItems":2}},"leader":{"type":"string","maxLength":128},"yaw":{"type":"number"},"follow_heading":{"type":"boolean"}}),
    );
    formation.entity_references = vec!["/leader".into()];
    result.push(formation);
    result
}

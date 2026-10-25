//! Schema descriptors for `engine.PhysicsBody2D`, `engine.PhysicsBody3D` and
//! `engine.PhysicsSettings`.
use super::{PhysicsBody2D, PhysicsBody3D, PhysicsSettings};
use serde_json::{Value, json};

fn vector(n: usize) -> Value {
    json!({"type":"array","items":{"type":"number"},"minItems":n,"maxItems":n})
}
fn list(max: usize, items: Value) -> Value {
    json!({"type":"array","maxItems":max,"items":items})
}
fn name() -> Value {
    json!({"type":"string","minLength":1,"maxLength":engine_physics::MAX_LAYER_NAME})
}
fn combine() -> Value {
    json!({"type":"string","enum":["average","min","max","multiply"],
        "description":"Coefficient combine rule; on disagreement max > multiply > min > average."})
}
/// Descriptor for a built-in component; `requires` lists required components.
fn descriptor<T: serde::Serialize>(
    name: &str,
    defaults: T,
    properties: Value,
    requires: &[&str],
) -> crate::ComponentDescriptor {
    crate::ComponentDescriptor {
        name: name.into(),
        version: 1,
        schema: json!({"type":"object","properties":properties,"additionalProperties":false}),
        defaults: serde_json::to_value(defaults).expect("finite builtin defaults"),
        requires: requires.iter().map(|r| (*r).into()).collect(),
        entity_references: vec![],
        data_only: false,
    }
}
fn body(n: usize) -> Value {
    let mut shapes = vec!["box", "ball", "capsule"];
    if n == 3 {
        shapes.push("cylinder");
    }
    let part_shapes = [shapes.as_slice(), &["convex_hull"]].concat();
    shapes.extend(["convex_hull", "trimesh", "heightfield", "compound"]);
    let rotation = if n == 2 {
        json!({"type":"number"})
    } else {
        vector(4)
    };
    let points = list(engine_physics::MAX_HULL_POINTS, vector(n));
    let part = json!({"type":"object","additionalProperties":false,"properties":{
        "shape":{"type":"string","enum":part_shapes},"position":vector(n),"rotation":rotation,
        "half_extents":vector(n),"radius":{"type":"number","minimum":0.00001,"maximum":10000},
        "half_height":{"type":"number","minimum":0,"maximum":10000},"points":points
    }});
    let mut properties = json!({
        "motion":{"type":"string","enum":["fixed","dynamic","kinematic"]},
        "shape":{"type":"string","enum":shapes,
            "description":"trimesh and heightfield require fixed or kinematic motion."},
        "half_extents":vector(n),"radius":{"type":"number","minimum":0.00001,"maximum":10000},
        "half_height":{"type":"number","minimum":0,"maximum":10000},
        "points":points,
        "vertices":list(engine_physics::MAX_MESH_VERTICES, vector(n)),
        "indices":list(engine_physics::MAX_MESH_TRIANGLES, json!({"type":"array","minItems":3,"maxItems":3,
            "items":{"type":"integer","minimum":0,"maximum":engine_physics::MAX_MESH_VERTICES - 1}})),
        "heights":list(engine_physics::MAX_HEIGHTFIELD_SAMPLES, json!({"type":"number"})),
        "heightfield_scale":vector(n),
        "parts":list(engine_physics::MAX_COMPOUND_PARTS, part),
        "velocity":vector(n),"angular_velocity":if n==2 {json!({"type":"number"})} else {vector(3)},
        "ccd":{"type":"boolean"},"material":{"type":"object","additionalProperties":false,"properties":{
            "density":{"type":"number","minimum":0.000001,"maximum":10000},"friction":{"type":"number","minimum":0,"maximum":10000},
            "restitution":{"type":"number","minimum":0,"maximum":1},"sensor":{"type":"boolean"},
            "friction_combine":combine(),"restitution_combine":combine(),
            "layer":{"type":"integer","minimum":0,"maximum":4294967295u64},"mask":{"type":"integer","minimum":0,"maximum":4294967295u64},
            "preset":{"type":"string","maxLength":engine_physics::MAX_LAYER_NAME,
                "description":"Named PhysicsSettings material replacing density, friction, restitution and combine rules."},
            "layer_name":{"type":"string","maxLength":engine_physics::MAX_LAYER_NAME,
                "description":"Named PhysicsSettings layer replacing layer and mask."}
        }}
    });
    if n == 3 {
        let count =
            json!({"type":"integer","minimum":0,"maximum":engine_physics::MAX_HEIGHTFIELD_SAMPLES});
        properties["heightfield_rows"] = count.clone();
        properties["heightfield_columns"] = count;
    }
    properties
}
pub(super) fn component_descriptors() -> Vec<crate::ComponentDescriptor> {
    let material = json!({"type":"object","additionalProperties":false,"properties":{
        "name":name(),"density":{"type":"number","minimum":0.000001,"maximum":10000},
        "friction":{"type":"number","minimum":0,"maximum":10000},
        "restitution":{"type":"number","minimum":0,"maximum":1},
        "friction_combine":combine(),"restitution_combine":combine()
    }});
    let settings = json!({
        "gravity_2d":vector(2),"gravity_3d":vector(3),
        "solver_iterations":{"type":"integer","minimum":1,"maximum":engine_physics::MAX_SOLVER_ITERATIONS},
        "layers":list(engine_physics::MAX_LAYERS, name()),
        "disabled_pairs":list(super::MAX_DISABLED_PAIRS, json!({"type":"array","minItems":2,"maxItems":2,"items":name()})),
        "materials":list(super::MAX_MATERIALS, material),
        "fixed_timestep":{"type":"number","minimum":0.001,"maximum":0.1,
            "description":"Fixed step in seconds. Frame time accumulates and only whole steps run; unset keeps variable substepping."},
        "max_substeps":{"type":"integer","minimum":1,"maximum":super::MAX_FIXED_SUBSTEPS,
            "description":"Most fixed steps per frame; excess time is dropped and reported (physics.step.clamped)."},
        "interpolation":{"type":"string","enum":["none","interpolate"],
            "description":"interpolate writes dynamic Transforms blended between the last two fixed steps; requires fixed_timestep."},
        "debug":{"type":"boolean","description":"Enables the physics debug view data (native debug_view)."},
        "debug_aabbs":{"type":"boolean"},
        "debug_max_items":{"type":"integer","minimum":1,"maximum":engine_physics::dim3::MAX_DEBUG_ITEMS}
    });
    vec![
        descriptor(
            "engine.PhysicsBody2D",
            PhysicsBody2D::default(),
            body(2),
            &["Transform"],
        ),
        descriptor(
            "engine.PhysicsBody3D",
            PhysicsBody3D::default(),
            body(3),
            &["Transform"],
        ),
        descriptor(
            "engine.PhysicsSettings",
            PhysicsSettings::default(),
            settings,
            &[],
        ),
    ]
}

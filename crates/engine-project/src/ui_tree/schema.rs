use super::*;
use crate::catalog::ComponentDescriptor;
use serde_json::{Value, json};

/// Authoring metadata for MCP, scene documents, and the shared editor inspector.
/// Tagged variants have a common field schema; UiRoot::validate checks their
/// exact variant shape, range relationships and hierarchy at the typed boundary.
pub fn descriptor() -> ComponentDescriptor {
    let length = json!({"type":"object","additionalProperties":false,"required":["unit"],"properties":{
        "unit":{"type":"string","enum":["pixels","percent","fill"]},
        "value":{"type":"number","minimum":0,"maximum":16384}
    }});
    let color = array(json!({"type":"number","minimum":0,"maximum":1}), 4);
    let text = json!({"type":"string","maxLength":4096});
    let action = json!({"type":"string","maxLength":128});
    let style = json!({"type":"object","additionalProperties":false,"properties":{
        "width":length,"height":length,
        "offset":array(json!({"type":"number","minimum":-16384,"maximum":16384}),2),
        "layout":{"type":"string","enum":["absolute","row","column"]},
        "padding":{"type":"number","minimum":0,"maximum":16384},
        "gap":{"type":"number","minimum":0,"maximum":16384},
        "clip":{"type":"boolean"},"background":color,"foreground":color,"accent":color,
        "font":action,"fontScale":{"type":"number","minimum":0.01,"maximum":16}
    }});
    let widget = json!({"type":"object","additionalProperties":false,"required":["kind"],"properties":{
        "kind":{"type":"string","enum":["container","label","button","toggle","slider","progress"]},
        "text":text,"action":action,"checked":{"type":"boolean"},
        "value":{"type":"number"},"min":{"type":"number"},"max":{"type":"number"},"step":{"type":"number","minimum":0}
    }});
    // parent is omitted for roots in authoring JSON; native serializers omit None.
    let node = json!({"type":"object","additionalProperties":false,"required":["id"],"properties":{
        "id":{"type":"string","minLength":1,"maxLength":128},
        "parent":{"type":"string","minLength":1,"maxLength":128},
        "widget":widget,"style":style,"visible":{"type":"boolean"},"enabled":{"type":"boolean"}
    }});
    ComponentDescriptor {
        name: "engine.UiRoot".into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "version":{"type":"integer","enum":[1]},
            "tree":{"type":"object","additionalProperties":false,"properties":{
                "nodes":{"type":"array","maxItems":MAX_UI_NODES,"items":node}
            }},
            "layer":{"type":"integer","minimum":i32::MIN,"maximum":i32::MAX},
            "visible":{"type":"boolean"},"enabled":{"type":"boolean"}
        }}),
        defaults: json!({"version":1,"tree":{"nodes":[]},"layer":0,"visible":true,"enabled":true}),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}

fn array(items: Value, count: usize) -> Value {
    json!({"type":"array","items":items,"minItems":count,"maxItems":count})
}

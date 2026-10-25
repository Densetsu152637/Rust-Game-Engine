//! Authoring descriptors and typed validation of the styling components:
//! `engine.UiTheme`, `engine.UiData` and `engine.UiWorldPanel`. The catalog
//! schema language has no `oneOf`, so fields that accept a literal or a
//! `$token` (colors, numbers, lengths, `font`) and mixed-shape fields (easing,
//! iterations) are left open in the schema; `validate_component` checks them
//! exactly at the typed boundary.
use super::*;
use crate::catalog::ComponentDescriptor;
use serde_json::{Value, json};

/// Descriptors of every UI component, `engine.UiRoot` first.
pub fn descriptors() -> Vec<ComponentDescriptor> {
    vec![
        descriptor(),
        theme_descriptor(),
        data_descriptor(),
        world_panel_descriptor(),
    ]
}

/// Typed validation of an authored component value of a UI component; other
/// component names pass.
pub fn validate_component(name: &str, data: &Value) -> Result<()> {
    match name {
        "engine.UiRoot" => serde_json::from_value::<UiRoot>(data.clone())?.validate(),
        "engine.UiTheme" => serde_json::from_value::<UiTheme>(data.clone())?.validate(),
        "engine.UiData" => serde_json::from_value::<UiData>(data.clone())?.validate(),
        "engine.UiWorldPanel" => serde_json::from_value::<UiWorldPanel>(data.clone())?.validate(),
        _ => Ok(()),
    }
}

/// JSON schema of one binding of `UiRoot.bindings`.
pub(super) fn binding_schema() -> Value {
    json!({"type":"object","additionalProperties":false,
    "required":["node","property","path"],
    "properties":{
        "node":{"type":"string","minLength":1,"maxLength":128},
        "property":{"type":"string","enum":[
            "text","value","checked","selected","visible","enabled","itemCount","itemText"]},
        "path":{"type":"string","minLength":1,"maxLength":MAX_BIND_PATH},
        "mode":{"type":"string","enum":["one_way","two_way"]},
        "format":{"type":"string","maxLength":MAX_BIND_FORMAT},
        "message":{"type":"string","maxLength":128},
        "args":{"type":"object","additionalProperties":true},
        "itemPath":{"type":"string","maxLength":MAX_BIND_PATH}
    }})
}

fn open_object() -> Value {
    json!({"type":"object","additionalProperties":true})
}

fn theme_descriptor() -> ComponentDescriptor {
    let name = json!({"type":"string","minLength":1,"maxLength":64});
    let seconds = json!({"type":"number","minimum":0,"maximum":3600});
    let align = json!({"type":"string","enum":["start","end","center","baseline","stretch"]});
    let justify = json!({"type":"string","enum":["start","end","center","stretch","space_between","space_around","space_evenly"]});
    // Fields without a token form are typed; token-capable ones pass through
    // (additionalProperties) and are validated by `UiTheme::validate`.
    let style = json!({"type":"object","additionalProperties":true,
    "description":"Partial UiStyle; colors, numbers, lengths and font accept $token strings",
    "properties":{
        "layout":{"type":"string","enum":["absolute","row","column","row_reverse","column_reverse","grid"]},
        "position":{"type":"string","enum":["flow","absolute"]},
        "wrap":{"type":"string","enum":["no_wrap","wrap","wrap_reverse"]},
        "justify":justify,"alignContent":justify,
        "alignItems":align,"alignSelf":align,"justifyItems":align,"justifySelf":align,
        "gridAutoFlow":{"type":"string","enum":["row","column","row_dense","column_dense"]},
        "clip":{"type":"boolean"},
        "aspectRatio":{"type":"number","minimum":0.001,"maximum":1000},
        "offset":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"number"}}
    }});
    let property = json!({"type":"string","enum":[
        "all","background","foreground","accent","opacity","scale","offset","width","height"]});
    let transition = json!({"type":"object","additionalProperties":true,"properties":{
        "property":property,"duration":seconds,"delay":seconds}});
    let animation = json!({"type":"object","additionalProperties":true,"properties":{
        "name":name,
        "duration":{"type":"number","minimum":0.001,"maximum":3600},
        "delay":seconds,
        "direction":{"type":"string","enum":["normal","reverse","alternate","alternate_reverse"]},
        "hold":{"type":"boolean"},"essential":{"type":"boolean"}}});
    let rule = json!({"type":"object","additionalProperties":false,
    "required":["selector"],
    "properties":{
        "selector":{"type":"string","minLength":1,"maxLength":selector::MAX_SELECTOR_LEN},
        "style":style,
        "transition":{"type":"array","maxItems":animation::MAX_RULE_TRANSITIONS,"items":transition},
        "animation":animation
    }});
    ComponentDescriptor {
        name: "engine.UiTheme".into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "version":{"type":"integer","enum":[1]},
            "name":name,
            "extends":name,
            "tokens":{"type":"object","additionalProperties":false,"properties":{
                "colors":open_object(),"numbers":open_object(),"fonts":open_object()}},
            "rules":{"type":"array","maxItems":stylesheet::MAX_THEME_RULES,"items":rule},
            "keyframes":open_object()
        }}),
        defaults: json!({"version":1,"name":"theme","tokens":{},"rules":[],"keyframes":{}}),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}

fn data_descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: "engine.UiData".into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "version":{"type":"integer","enum":[1]},
            "values":open_object()
        }}),
        defaults: json!({"version":1,"values":{}}),
        requires: vec![],
        entity_references: vec![],
        data_only: false,
    }
}

fn world_panel_descriptor() -> ComponentDescriptor {
    let positive = json!({"type":"number","minimum":0.0001,"maximum":10000});
    ComponentDescriptor {
        name: "engine.UiWorldPanel".into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "version":{"type":"integer","enum":[1]},
            "size":{"type":"array","minItems":2,"maxItems":2,"items":positive},
            "pixelsPerUnit":{"type":"number","minimum":1,"maximum":4096},
            "pivot":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"number","minimum":0,"maximum":1}},
            "billboard":{"type":"string","enum":["none","full","yaw"]},
            "pointer":{"type":"string","enum":["auto","camera2d","camera3d","none"]},
            "keyboard":{"type":"boolean"}
        }}),
        defaults: json!({"version":1,"size":[2,1],"pixelsPerUnit":100,"pivot":[0.5,0.5],
            "billboard":"none","pointer":"auto","keyboard":false}),
        requires: vec!["engine.UiRoot".into(), "Transform".into()],
        entity_references: vec![],
        data_only: false,
    }
}

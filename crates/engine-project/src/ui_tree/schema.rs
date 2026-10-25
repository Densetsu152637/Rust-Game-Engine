use super::*;
use crate::catalog::ComponentDescriptor;
use serde_json::{Value, json};

/// Authoring metadata for MCP, scene documents, and the shared editor inspector.
/// The catalog schema language has no `oneOf`, so tagged variants (widgets,
/// lengths, tracks) share one field schema per tag; `UiRoot::validate` checks
/// the exact variant shape, range relationships and hierarchy at the typed
/// boundary.
pub fn descriptor() -> ComponentDescriptor {
    let extent = json!({"type":"number","minimum":0,"maximum":MAX_UI_EXTENT});
    let signed = json!({"type":"number","minimum":-MAX_UI_EXTENT,"maximum":MAX_UI_EXTENT});
    let length = |units: &[&str]| {
        json!({"type":"object","additionalProperties":false,"required":["unit"],"properties":{
            "unit":{"type":"string","enum":units},
            "value":{"type":"number","minimum":0,"maximum":MAX_UI_EXTENT}
        }})
    };
    let size = length(&["pixels", "percent", "fill", "auto"]);
    let limit = length(&["pixels", "percent", "auto"]);
    let color = array(json!({"type":"number","minimum":0,"maximum":1}), 4);
    let text = json!({"type":"string","maxLength":MAX_UI_TEXT});
    let short = json!({"type":"string","maxLength":128});
    let label = json!({"type":"string","maxLength":MAX_UI_LABEL});
    let edges = json!({"type":"object","additionalProperties":false,"properties":{
        "left":extent,"top":extent,"right":extent,"bottom":extent
    }});
    let margin = json!({"type":"object","additionalProperties":false,"properties":{
        "left":signed,"top":signed,"right":signed,"bottom":signed
    }});
    let breadth_kinds = ["px", "percent", "fr", "auto", "min_content", "max_content"];
    let breadth = json!({"type":"object","additionalProperties":false,"required":["kind"],"properties":{
        "kind":{"type":"string","enum":breadth_kinds},
        "value":{"type":"number","minimum":0,"maximum":MAX_UI_EXTENT}
    }});
    let track = json!({"type":"object","additionalProperties":false,"required":["kind"],"properties":{
        "kind":{"type":"string","enum":["px","percent","fr","auto","min_content","max_content","minmax"]},
        "value":{"type":"number","minimum":0,"maximum":MAX_UI_EXTENT},
        "min":breadth,"max":breadth
    }});
    let tracks = json!({"type":"array","maxItems":MAX_GRID_TRACKS,"items":track});
    let line = json!({"type":"integer","minimum":-(MAX_GRID_TRACKS as i64 + 1),"maximum":MAX_GRID_TRACKS + 1});
    let placement = json!({"type":"object","additionalProperties":false,"properties":{
        "start":line,"end":line,"span":{"type":"integer","minimum":1,"maximum":MAX_GRID_TRACKS}
    }});
    let align = json!({"type":"string","enum":["start","end","center","baseline","stretch"]});
    let justify = json!({"type":"string","enum":["start","end","center","stretch","space_between","space_around","space_evenly"]});
    let factor = json!({"type":"number","minimum":0,"maximum":1024});
    let style = closed(&[
        json!({
            "width":size,"height":size,
            "minWidth":limit,"minHeight":limit,"maxWidth":limit,"maxHeight":limit,
            "aspectRatio":{"type":"number","minimum":0.001,"maximum":1000},
            "offset":array(signed.clone(),2),
            "layout":{"type":"string","enum":["absolute","row","column","row_reverse","column_reverse","grid"]},
            "position":{"type":"string","enum":["flow","absolute"]},
            "inset":margin,"margin":margin
        }),
        json!({
            "padding":extent,"paddingEdges":edges,
            "gap":extent,"rowGap":extent,"columnGap":extent,
            "wrap":{"type":"string","enum":["no_wrap","wrap","wrap_reverse"]},
            "justify":justify,"alignContent":justify,
            "alignItems":align,"alignSelf":align,"justifyItems":align,"justifySelf":align,
            "grow":factor,"shrink":factor,"basis":limit
        }),
        json!({
            "gridColumns":tracks,"gridRows":tracks,"gridAutoColumns":track,"gridAutoRows":track,
            "gridAutoFlow":{"type":"string","enum":["row","column","row_dense","column_dense"]},
            "gridColumn":placement,"gridRow":placement,
            "clip":{"type":"boolean"},"background":color,"foreground":color,"accent":color,
            "font":short,"fontScale":{"type":"number","minimum":0.01,"maximum":16}
        }),
    ]);
    let strings = |max: usize| json!({"type":"array","maxItems":max,"items":label});
    let count = |max: u64| json!({"type":"integer","minimum":0,"maximum":max});
    let bar = json!({"type":"number","minimum":0,"maximum":256});
    let mut widget = closed(&[
        json!({
            "kind":{"type":"string","enum":[
                "container","label","rich_label","image","nine_slice","button","toggle","slider","progress",
                "text_input","dropdown","scroll_view","list","virtual_list","tabs","modal_dialog"
            ]},
            "text":text,"markup":text,"action":short,"checked":{"type":"boolean"},
            "value":{"type":"number"},"min":{"type":"number"},"max":{"type":"number"},"step":{"type":"number","minimum":0},
            "texture":short,"uv":array(json!({"type":"number","minimum":0,"maximum":1}),4),"tint":color,
            "fit":{"type":"string","enum":["stretch","contain","cover"]},
            "slices":array(extent.clone(),4),"border":array(extent.clone(),4),"center":{"type":"boolean"}
        }),
        json!({
            "placeholder":label,"maxLength":{"type":"integer","minimum":1,"maximum":MAX_UI_TEXT},
            "password":{"type":"boolean"},"multiline":{"type":"boolean"},
            "caret":count(MAX_UI_TEXT as u64),"anchor":count(MAX_UI_TEXT as u64),
            "options":strings(MAX_DROPDOWN_OPTIONS),"selected":count(MAX_VIRTUAL_ITEMS as u64),
            "maxVisible":{"type":"integer","minimum":1,"maximum":MAX_DROPDOWN_OPTIONS},
            "offset":array(json!({"type":"number","minimum":0,"maximum":1.0e7}),2),
            "axes":{"type":"string","enum":["horizontal","vertical","both"]},
            "scrollbars":{"type":"boolean"}
        }),
        json!({
            "itemCount":count(MAX_VIRTUAL_ITEMS as u64),"itemExtent":{"type":"number","minimum":1,"maximum":4096},
            "template":short,"scroll":{"type":"number","minimum":0,"maximum":1.0e7},
            "overscan":count(16),"horizontal":{"type":"boolean"},
            "labels":strings(MAX_TABS),"headerHeight":bar,
            "title":label,"open":{"type":"boolean"},"buttons":strings(MAX_DIALOG_BUTTONS),
            "titleHeight":bar,"buttonHeight":bar
        }),
    ]);
    widget["required"] = json!(["kind"]);
    let tooltip = json!({"type":"object","additionalProperties":false,"required":["text"],"properties":{
        "text":{"type":"string","maxLength":1024},"delay":{"type":"number","minimum":0,"maximum":10}
    }});
    let id = json!({"type":"string","minLength":1,"maxLength":128});
    let nav = json!({"type":"object","additionalProperties":false,"properties":{
        "up":id,"down":id,"left":id,"right":id,
        "tabIndex":{"type":"integer","minimum":-1024,"maximum":1024},
        "focusable":{"type":"boolean"}
    }});
    // parent is omitted for roots in authoring JSON; native serializers omit None.
    let node = json!({"type":"object","additionalProperties":false,"required":["id"],"properties":{
        "id":id,"parent":id,
        "widget":widget,"style":style,"visible":{"type":"boolean"},"enabled":{"type":"boolean"},
        "classes":{"type":"array","maxItems":16,"items":{"type":"string","minLength":1,"maxLength":64}},
        "tooltip":tooltip,"nav":nav
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

/// Closed object schema whose properties are the union of `parts`.
fn closed(parts: &[Value]) -> Value {
    let mut properties = serde_json::Map::new();
    for part in parts {
        if let Some(map) = part.as_object() {
            properties.extend(map.clone());
        }
    }
    json!({"type":"object","additionalProperties":false,"properties":properties})
}

fn array(items: Value, count: usize) -> Value {
    json!({"type":"array","items":items,"minItems":count,"maxItems":count})
}

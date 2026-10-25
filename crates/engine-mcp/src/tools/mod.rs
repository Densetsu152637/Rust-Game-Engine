//! Built-in tools, one module per family. Read-only tools are always listed;
//! the write family is listed only when the server allows write access.

mod acoustics;
mod assets;
mod components;
mod edit;
mod files;
mod model_import;
mod navigation;
mod pcg;
mod physics;
mod project;
mod scene;
mod tilemap;

pub use components::{COMPONENT_NAMES, ComponentDoc, component_docs};

use crate::{
    config::AccessLevel,
    registry::{Annotations, Tool, ToolContext, ToolDefinition, ToolOutcome},
};
use serde_json::{Map, Value};

type Run = fn(&ToolContext<'_>, &Map<String, Value>) -> ToolOutcome;

/// A tool implemented by a plain function.
struct FnTool {
    definition: ToolDefinition,
    run: Run,
}

impl Tool for FnTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn call(&self, cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
        (self.run)(cx, args)
    }
}

/// Declarative description of a function tool.
struct Spec {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    input: Value,
    output: Value,
    annotations: Annotations,
    access: AccessLevel,
}

/// Builds a boxed function tool.
fn tool(spec: Spec, run: Run) -> Box<dyn Tool> {
    Box::new(FnTool {
        definition: ToolDefinition {
            name: spec.name.into(),
            title: spec.title.into(),
            description: spec.description.into(),
            input_schema: spec.input,
            output_schema: Some(spec.output),
            annotations: spec.annotations,
            access: spec.access,
        },
        run,
    })
}

/// Every built-in tool in listing order. The registry drops those the server's
/// access does not allow.
pub(crate) fn builtin() -> Vec<Box<dyn Tool>> {
    let mut tools = Vec::new();
    tools.extend(project::tools());
    tools.extend(scene::tools());
    tools.extend(components::tools());
    tools.extend(assets::tools());
    tools.extend(files::read_tools());
    tools.extend(edit::tools());
    tools.extend(files::write_tools());
    tools.extend(model_import::tools());
    tools.extend(navigation::tools());
    tools.extend(acoustics::tools());
    tools.extend(physics::tools());
    tools.extend(tilemap::tools());
    tools.extend(pcg::tools());
    tools
}

/// Converts any serializable value to JSON through its text form so that `f32`
/// fields print as their shortest decimal (`0.1`, not `0.10000000149011612`).
pub(crate) fn clean<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_string(value)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// Forward-slash rendering of a manifest-relative path.
pub(crate) fn slash(path: &std::path::Path) -> String {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

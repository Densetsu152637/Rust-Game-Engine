//! Bounded, non-executing compilation of Rhai source.

use super::engine;
use crate::{ScriptError, Site, codes, graph::valid_event_name};
use rhai::{AST, Array, ImmutableString};
use std::collections::BTreeSet;

/// Maximum source bytes, including comments and whitespace.
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
/// Maximum static event subscriptions per source.
pub const MAX_EVENTS: usize = 64;

/// Validated text script. Its executable AST contains functions only.
#[derive(Clone)]
pub struct TextProgram {
    pub(crate) ast: AST,
    functions: BTreeSet<(String, usize)>,
    subscriptions: Vec<String>,
}

impl TextProgram {
    /// Parse and structurally validate bounded UTF-8 source without executing it.
    /// The optional `const events = ["name", ...];` declaration must be literal.
    pub fn compile(bytes: &[u8]) -> Result<Self, ScriptError> {
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(ScriptError::new(
                codes::TEXT_PARSE,
                "text script exceeds 256 KiB",
            ));
        }
        let source = std::str::from_utf8(bytes)
            .map_err(|_| ScriptError::new(codes::TEXT_PARSE, "text script is not UTF-8"))?;
        let ast = engine().compile(source).map_err(|error| {
            let pos = error.position();
            let mut converted = ScriptError::new(codes::TEXT_PARSE, error.to_string());
            if let Some(line) = pos.line() {
                converted = converted.at(Site::Pos {
                    line: line as u32,
                    col: pos.position().unwrap_or(1) as u32,
                });
            }
            converted
        })?;
        let functions: BTreeSet<(String, usize)> = ast
            .iter_functions()
            .map(|f| (f.name.to_owned(), f.params.len()))
            .collect();
        for (name, arity) in [
            ("on_start", 0),
            ("on_update", 1),
            ("on_event", 3),
            ("on_stop", 0),
        ] {
            if functions.iter().any(|(found, _)| found == name)
                && !functions.contains(&(name.to_owned(), arity))
            {
                return Err(ScriptError::new(
                    codes::TEXT_HANDLER,
                    format!("{name} requires {arity} parameters"),
                ));
            }
        }
        let mut subscriptions = BTreeSet::new();
        for (name, _, value) in ast.iter_literal_variables(true, false) {
            if name != "events" {
                continue;
            }
            let events = value.try_cast::<Array>().ok_or_else(|| {
                ScriptError::new(codes::TEXT_EVENTS, "events must be a literal array")
            })?;
            if events.len() > MAX_EVENTS {
                return Err(ScriptError::new(codes::TEXT_EVENTS, "too many event names"));
            }
            for event in events {
                let event = event.try_cast::<ImmutableString>().ok_or_else(|| {
                    ScriptError::new(codes::TEXT_EVENTS, "event names must be strings")
                })?;
                if !valid_event_name(event.as_str()) || !subscriptions.insert(event.to_string()) {
                    return Err(ScriptError::new(
                        codes::TEXT_EVENTS,
                        "invalid or duplicate event name",
                    ));
                }
            }
        }
        if !subscriptions.is_empty() && !functions.contains(&("on_event".to_owned(), 3)) {
            return Err(ScriptError::new(
                codes::TEXT_HANDLER,
                "events require on_event(name, payload, source)",
            ));
        }
        Ok(Self {
            ast: ast.clone_functions_only(),
            functions,
            subscriptions: subscriptions.into_iter().collect(),
        })
    }

    pub fn subscriptions(&self) -> &[String] {
        &self.subscriptions
    }

    pub(crate) fn has_handler(&self, name: &str, arity: usize) -> bool {
        self.functions.contains(&(name.to_owned(), arity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_static_event_list_without_running_source() {
        let program = TextProgram::compile(
            br#"
                const events = ["player.hit", "scene.ready"];
                let ignored = 1 / 0;
                fn on_event(name, payload, source) {}
            "#,
        )
        .unwrap();
        assert_eq!(program.subscriptions(), &["player.hit", "scene.ready"]);
    }

    #[test]
    fn import_and_eval_are_rejected_during_compilation() {
        for source in [
            r#"import "path" as other; fn on_start() {}"#,
            r#"let p = "path"; import p as other; fn on_start() {}"#,
            r#"fn on_start() { eval("40 + 2"); }"#,
        ] {
            assert!(TextProgram::compile(source.as_bytes()).is_err(), "{source}");
        }
    }

    #[test]
    fn source_size_is_bounded_before_parsing() {
        let source = vec![b' '; MAX_SOURCE_BYTES + 1];
        assert_eq!(
            TextProgram::compile(&source).err().unwrap().code,
            codes::TEXT_PARSE
        );
    }
}

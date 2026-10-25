//! One locale's compiled Fluent messages.

use std::collections::{BTreeMap, BTreeSet};

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use fluent_syntax::ast::Entry;

use crate::error::LocaleError;
use crate::limits::LocaleLimits;
use crate::locale::LocaleId;

/// What a message defines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageInfo {
    /// The message has a value pattern (`key = text`).
    pub has_value: bool,
    /// Attribute names (`.title`), in source order.
    pub attributes: Vec<String>,
}

pub(crate) struct Catalog {
    bundle: FluentBundle<FluentResource>,
    messages: BTreeMap<String, MessageInfo>,
    terms: BTreeSet<String>,
}

/// Result of formatting one pattern.
pub(crate) struct Formatted {
    pub text: String,
    pub errors: Vec<String>,
}

impl Catalog {
    pub(crate) fn new(locale: &LocaleId, isolating: bool) -> Self {
        let mut bundle = FluentBundle::new_concurrent(vec![locale.language_identifier().clone()]);
        bundle.set_use_isolating(isolating);
        Self {
            bundle,
            messages: BTreeMap::new(),
            terms: BTreeSet::new(),
        }
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.messages.len() + self.terms.len()
    }

    pub(crate) fn messages(&self) -> &BTreeMap<String, MessageInfo> {
        &self.messages
    }

    /// Validates and adds a source. On error the catalog is unchanged.
    pub(crate) fn add_source(
        &mut self,
        locale: &LocaleId,
        source: &str,
        limits: &LocaleLimits,
    ) -> Result<usize, LocaleError> {
        if source.len() > limits.max_resource_bytes {
            return Err(LocaleError::ResourceTooLarge {
                bytes: source.len(),
                max: limits.max_resource_bytes,
            });
        }
        check_depth(source, limits.max_placeable_depth)?;
        let resource = match FluentResource::try_new(source.to_string()) {
            Ok(r) => r,
            Err((_, errors)) => {
                let first = errors.first();
                let offset = first.map_or(0, |e| e.pos.start.min(source.len()));
                let line = source
                    .as_bytes()
                    .get(..offset)
                    .map_or(1, |b| b.iter().filter(|&&c| c == b'\n').count() + 1);
                return Err(LocaleError::Parse {
                    count: errors.len(),
                    line,
                    detail: first.map(|e| e.to_string()).unwrap_or_default(),
                });
            }
        };

        let mut new_messages: BTreeMap<String, MessageInfo> = BTreeMap::new();
        let mut new_terms: BTreeSet<String> = BTreeSet::new();
        for entry in resource.entries() {
            match entry {
                Entry::Message(m) => {
                    let id = m.id.name.to_string();
                    if self.messages.contains_key(&id) || new_messages.contains_key(&id) {
                        return Err(duplicate(locale, id));
                    }
                    new_messages.insert(
                        id,
                        MessageInfo {
                            has_value: m.value.is_some(),
                            attributes: m
                                .attributes
                                .iter()
                                .map(|a| a.id.name.to_string())
                                .collect(),
                        },
                    );
                }
                Entry::Term(t) => {
                    let id = format!("-{}", t.id.name);
                    if self.terms.contains(&id) || !new_terms.insert(id.clone()) {
                        return Err(duplicate(locale, id));
                    }
                }
                _ => {}
            }
        }
        let added = new_messages.len() + new_terms.len();
        let total = self.entry_count() + added;
        if total > limits.max_messages_per_locale {
            return Err(LocaleError::TooManyMessages {
                count: total,
                max: limits.max_messages_per_locale,
            });
        }
        if self.bundle.add_resource(resource).is_err() {
            // Unreachable after the duplicate pre-check, but never leave a half-added source.
            return Err(duplicate(locale, "<unknown>".to_string()));
        }
        self.messages.extend(new_messages);
        self.terms.extend(new_terms);
        Ok(added)
    }

    /// Formats `id` (or its attribute). `None` when the message, attribute or value is absent.
    pub(crate) fn format(
        &self,
        id: &str,
        attribute: Option<&str>,
        args: &FluentArgs<'_>,
    ) -> Option<Formatted> {
        let message = self.bundle.get_message(id)?;
        let pattern = match attribute {
            Some(name) => message.get_attribute(name)?.value(),
            None => message.value()?,
        };
        let mut errors = Vec::new();
        let text = self
            .bundle
            .format_pattern(pattern, Some(args), &mut errors)
            .into_owned();
        Some(Formatted {
            text,
            errors: errors.iter().map(ToString::to_string).collect(),
        })
    }

    pub(crate) fn defines(&self, id: &str, attribute: Option<&str>) -> bool {
        match (self.messages.get(id), attribute) {
            (Some(m), None) => m.has_value,
            (Some(m), Some(a)) => m.attributes.iter().any(|x| x == a),
            (None, _) => false,
        }
    }
}

fn duplicate(locale: &LocaleId, id: String) -> LocaleError {
    LocaleError::DuplicateMessage {
        locale: locale.to_string(),
        id,
    }
}

/// Conservative nesting scan done before parsing, because the Fluent parser recurses on nested
/// placeables and call arguments. Counts `{` and, inside a placeable, `(`.
fn check_depth(source: &str, max: usize) -> Result<(), LocaleError> {
    let mut braces = 0usize;
    let mut parens = 0usize;
    let mut deepest = 0usize;
    for b in source.bytes() {
        match b {
            b'{' => braces += 1,
            b'}' => {
                braces = braces.saturating_sub(1);
                if braces == 0 {
                    parens = 0;
                }
            }
            b'(' if braces > 0 => parens += 1,
            b')' => parens = parens.saturating_sub(1),
            _ => {}
        }
        deepest = deepest.max(braces + parens);
        if deepest > max {
            return Err(LocaleError::TooDeep {
                depth: deepest,
                max,
            });
        }
    }
    Ok(())
}

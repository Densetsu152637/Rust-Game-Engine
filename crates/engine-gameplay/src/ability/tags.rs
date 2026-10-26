//! Hierarchical gameplay tags and counted tag containers.
//!
//! A tag is a dotted path such as `State.Debuff.Stunned`. A hierarchical query for `State`
//! matches `State` itself and every descendant (`State.Debuff.Stunned`); an exact query
//! matches only the identical tag. Containers count tags so that two sources granting the
//! same tag keep it until both are gone.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Longest tag, in bytes.
pub const MAX_TAG_LEN: usize = 128;

/// A tag is nonempty dot-separated segments of ASCII letters, digits and `_`.
pub fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= MAX_TAG_LEN
        && tag
            .split('.')
            .all(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'))
}

/// `tag` is `ancestor` or one of its descendants (`State.Stunned` matches `State`).
pub fn tag_matches(tag: &str, ancestor: &str) -> bool {
    tag == ancestor
        || (tag.len() > ancestor.len()
            && tag.starts_with(ancestor)
            && tag.as_bytes()[ancestor.len()] == b'.')
}

/// Counted set of tags, ordered by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagContainer {
    counts: BTreeMap<String, u32>,
}

impl TagContainer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, tag: &str) {
        *self.counts.entry(tag.to_owned()).or_default() += 1;
    }

    pub fn add_all<'a>(&mut self, tags: impl IntoIterator<Item = &'a String>) {
        for tag in tags {
            self.add(tag);
        }
    }

    /// Drops one count of `tag`; false when the tag was absent.
    pub fn remove(&mut self, tag: &str) -> bool {
        match self.counts.get_mut(tag) {
            Some(n) if *n > 1 => {
                *n -= 1;
                true
            }
            Some(_) => {
                self.counts.remove(tag);
                true
            }
            None => false,
        }
    }

    /// Count of exactly `tag`.
    pub fn count(&self, tag: &str) -> u32 {
        self.counts.get(tag).copied().unwrap_or(0)
    }

    /// Holds `tag` exactly, or (`exact == false`) `tag` or any of its descendants.
    pub fn has(&self, tag: &str, exact: bool) -> bool {
        if exact {
            return self.counts.contains_key(tag);
        }
        // Every string with the prefix `tag` sorts in one contiguous run from `tag`.
        self.counts
            .range::<str, _>((std::ops::Bound::Included(tag), std::ops::Bound::Unbounded))
            .take_while(|(k, _)| k.starts_with(tag))
            .any(|(k, _)| tag_matches(k, tag))
    }

    pub fn has_all(&self, tags: &[String], exact: bool) -> bool {
        tags.iter().all(|t| self.has(t, exact))
    }

    pub fn has_any(&self, tags: &[String], exact: bool) -> bool {
        tags.iter().any(|t| self.has(t, exact))
    }

    pub fn has_none(&self, tags: &[String], exact: bool) -> bool {
        !self.has_any(tags, exact)
    }

    /// Tags with their counts, ordered by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u32)> {
        self.counts.iter().map(|(k, v)| (k.as_str(), *v))
    }

    /// Distinct tags.
    pub fn len(&self) -> usize {
        self.counts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }
}

/// `all` / `any` / `none` query over a container.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TagQuery {
    /// Every tag must be present.
    pub all: Vec<String>,
    /// At least one must be present (ignored when empty).
    pub any: Vec<String>,
    /// None may be present.
    pub none: Vec<String>,
    /// Match tags exactly instead of hierarchically.
    pub exact: bool,
}

impl TagQuery {
    pub fn matches(&self, tags: &TagContainer) -> bool {
        tags.has_all(&self.all, self.exact)
            && (self.any.is_empty() || tags.has_any(&self.any, self.exact))
            && tags.has_none(&self.none, self.exact)
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty() && self.any.is_empty() && self.none.is_empty()
    }
}

/// Required / blocked tag rule (hierarchical), as used by effects and abilities.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Requirements {
    /// Every tag must be present.
    pub required: Vec<String>,
    /// None may be present.
    pub blocked: Vec<String>,
}

impl Requirements {
    pub fn met(&self, tags: &TagContainer) -> bool {
        tags.has_all(&self.required, false) && tags.has_none(&self.blocked, false)
    }

    pub fn is_empty(&self) -> bool {
        self.required.is_empty() && self.blocked.is_empty()
    }

    pub(crate) fn tags(&self) -> impl Iterator<Item = &String> {
        self.required.iter().chain(&self.blocked)
    }
}

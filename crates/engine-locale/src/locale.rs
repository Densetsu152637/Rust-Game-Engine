//! Locale identifiers, fallback chains and negotiation.

use std::cmp::Ordering;
use std::fmt;

use unic_langid::LanguageIdentifier;

use crate::error::LocaleError;

/// Longest accepted locale identifier in bytes.
pub const MAX_LOCALE_BYTES: usize = 64;

/// A parsed, canonicalised BCP 47 / Unicode locale identifier (`en`, `fr-CA`, `zh-Hant-TW`).
///
/// Equality, hashing and ordering use the canonical form, so `fr-ca` equals `fr-CA`.
#[derive(Debug, Clone)]
pub struct LocaleId {
    tag: String,
    lang: LanguageIdentifier,
}

impl LocaleId {
    /// Parses an identifier. Accepts `-` and `_` separators; the result is canonical.
    pub fn parse(input: &str) -> Result<Self, LocaleError> {
        let invalid = |reason: &str| LocaleError::InvalidLocale {
            input: input.chars().take(MAX_LOCALE_BYTES).collect(),
            reason: reason.to_string(),
        };
        if input.is_empty() {
            return Err(invalid("empty"));
        }
        if input.len() > MAX_LOCALE_BYTES {
            return Err(invalid("too long"));
        }
        let lang: LanguageIdentifier = input.parse().map_err(|e| invalid(&format!("{e}")))?;
        Ok(Self {
            tag: lang.to_string(),
            lang,
        })
    }

    /// Canonical string form.
    pub fn as_str(&self) -> &str {
        &self.tag
    }

    /// The underlying Unicode language identifier.
    pub fn language_identifier(&self) -> &LanguageIdentifier {
        &self.lang
    }

    /// The bare language subtag (`fr` for `fr-CA`).
    pub fn language(&self) -> &str {
        self.lang.language.as_str()
    }

    /// `true` for right-to-left scripts/languages commonly needing mirrored layout.
    pub fn is_rtl(&self) -> bool {
        match self.lang.script.as_ref().map(|s| s.as_str()) {
            Some("Arab" | "Hebr" | "Thaa" | "Syrc" | "Nkoo" | "Adlm") => return true,
            Some(_) => return false,
            None => {}
        }
        matches!(
            self.language(),
            "ar" | "he" | "fa" | "ur" | "ps" | "sd" | "ug" | "yi" | "dv" | "ckb"
        )
    }

    /// Progressively less specific forms: the identifier itself, then without variants, region
    /// and script, ending at the bare language. Consecutive duplicates are removed.
    pub fn truncations(&self) -> Vec<LocaleId> {
        let mut out = vec![self.clone()];
        let mut cur = self.lang.clone();
        let push = |cur: &LanguageIdentifier, out: &mut Vec<LocaleId>| {
            let tag = cur.to_string();
            if out.last().map(|l| l.tag.as_str()) != Some(tag.as_str()) {
                out.push(LocaleId {
                    tag,
                    lang: cur.clone(),
                });
            }
        };
        if cur.variants().next().is_some() {
            cur.clear_variants();
            push(&cur, &mut out);
        }
        if cur.region.is_some() {
            cur.region = None;
            push(&cur, &mut out);
        }
        if cur.script.is_some() {
            cur.script = None;
            push(&cur, &mut out);
        }
        out
    }

    /// The fallback chain: the truncations of `self`, then the truncations of `default`.
    /// For `fr-CA` with default `en` this is `fr-CA`, `fr`, `en`.
    pub fn fallback_chain(&self, default: &LocaleId) -> Vec<LocaleId> {
        let mut out = self.truncations();
        for l in default.truncations() {
            if !out.contains(&l) {
                out.push(l);
            }
        }
        out
    }
}

impl PartialEq for LocaleId {
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag
    }
}
impl Eq for LocaleId {}
impl std::hash::Hash for LocaleId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.tag.hash(state);
    }
}
impl PartialOrd for LocaleId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for LocaleId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tag.cmp(&other.tag)
    }
}
impl fmt::Display for LocaleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.tag)
    }
}
impl std::str::FromStr for LocaleId {
    type Err = LocaleError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Picks the best available locale for an ordered list of requested locales.
///
/// For each request, in order: an available locale equal to the request or one of its
/// truncations (`fr-CA-x` -> `fr-CA` -> `fr`); then, any available locale with the same
/// language (request `pt`, available `pt-BR`; the first in `available` order wins). When no
/// request matches, `default` is returned.
pub fn negotiate(requested: &[LocaleId], available: &[LocaleId], default: &LocaleId) -> LocaleId {
    for req in requested {
        for cand in req.truncations() {
            if available.contains(&cand) {
                return cand;
            }
        }
        if let Some(found) = available.iter().find(|a| a.language() == req.language()) {
            return found.clone();
        }
    }
    default.clone()
}

//! Host-path scrubbing. Tool results go to a remote agent, so they must never
//! reveal where the project (or the toolchain) lives on the host's disk.
//!
//! One [`Scrubber`] rewrites every string of a result, successes and failures
//! alike, in this order:
//!
//! 1. the canonical project root, in every spelling (`\\?\` verbatim or plain,
//!    single or doubled backslashes as Rust `Debug` output prints them, `/`
//!    separators, any case on Windows), becomes a project-relative path
//!    (`C:\proj\src\lib.rs` -> `src/lib.rs`, the bare root -> `.`);
//! 2. well-known host directories (cargo and rustup home, the user's home, temp
//!    and cache directories) and any other absolute Windows or Unix path become
//!    [`HOST`].
//!
//! Tools whose payload is project file text or authored data (`file_read`,
//! `search_text`, `scene_inspect`, `entity_get`) mark their result
//! [verbatim](crate::ToolResult::verbatim): rewriting file content would corrupt
//! the text an agent edits and writes back.

use regex::{Captures, Regex, RegexBuilder};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Placeholder for host paths that are not inside the project.
pub const HOST: &str = "<host>";

/// Characters that end a path inside prose or compiler output.
const STOP: &str = r#"\s"'<>|*?:,;()\[\]{}`"#;
/// What may precede a path: anything that is not part of a longer path or word.
const LEAD: &str = r"(^|[^A-Za-z0-9._~\-\\/])";
/// What may follow a path component run when no separator follows.
const TAIL: &str = r"([^A-Za-z0-9_~\-\\/]|$)";
const VERBATIM: &str = r"(?:[\\/]{2,}\?[\\/]+)?";

#[derive(Clone)]
pub(crate) struct Scrubber {
    root: PathBuf,
    root_rule: Option<Regex>,
    host_rules: Vec<Regex>,
    /// Every rule compiled. When one did not, scrubbing fails closed: any string
    /// that could hold a path is replaced by [`HOST`].
    complete: bool,
}

fn build(pattern: &str) -> Option<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(cfg!(windows))
        .size_limit(8 << 20)
        .build()
        .ok()
}

/// Regex source matching `path` with any separators (`\`, `/`, doubled,
/// verbatim prefix). `None` when the path has too few components to be a useful
/// needle (a filesystem root).
fn path_source(path: &Path, minimum_parts: usize) -> Option<String> {
    let lossy = path.to_string_lossy();
    let text = lossy.strip_prefix(r"\\?\").unwrap_or(&lossy);
    let parts: Vec<String> = text
        .split(['\\', '/'])
        .filter(|p| !p.is_empty())
        .map(regex::escape)
        .collect();
    if parts.len() < minimum_parts {
        return None;
    }
    let body = parts.join(r"[\\/]+");
    Some(if text.starts_with(['/', '\\']) {
        format!(r"[\\/]+{body}")
    } else {
        format!("{VERBATIM}{body}")
    })
}

/// `LEAD path (separator REST | TAIL)`; groups: 1 lead, 2 rest, 3 tail.
fn rooted(source: &str, allow_bare: bool) -> Option<Regex> {
    let end = if allow_bare {
        format!(r"(?:[\\/]+([^{STOP}]*)|{TAIL})")
    } else {
        format!(r"(?:[\\/]+([^{STOP}]*))")
    };
    build(&format!("{LEAD}(?:{source}){end}"))
}

fn collapse_separators(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous = false;
    for c in text.chars() {
        let separator = c == '\\' || c == '/';
        if !(separator && previous) {
            out.push(if separator { '/' } else { c });
        }
        previous = separator;
    }
    out
}

/// Directories that identify the host: toolchain homes, user and temp roots.
fn host_directories() -> Vec<PathBuf> {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut found: Vec<PathBuf> = [
        "CARGO_HOME",
        "RUSTUP_HOME",
        "USERPROFILE",
        "HOME",
        "LOCALAPPDATA",
        "APPDATA",
        "TEMP",
        "TMP",
        "TMPDIR",
        "XDG_CACHE_HOME",
        "ENGINE_NATIVE_TARGET_DIR",
    ]
    .iter()
    .filter_map(|name| var(name))
    .collect();
    found.push(std::env::temp_dir());
    if let Some(home) = var("USERPROFILE").or_else(|| var("HOME")) {
        found.push(home.join(".cargo"));
        found.push(home.join(".rustup"));
    }
    if let Ok(current) = std::env::current_dir() {
        found.push(current);
    }
    found.sort();
    found.dedup();
    found
}

impl Scrubber {
    pub(crate) fn new(root: &Path) -> Self {
        let root_source = path_source(root, 1);
        let root_rule = root_source
            .as_deref()
            .and_then(|source| rooted(source, true));
        let mut complete = root_source.is_none() || root_rule.is_some();
        // Known directories first (they may contain spaces), then any other
        // absolute path. A drive path needs a separator after the drive so that
        // prose such as `a: b` is left alone.
        let known: Vec<String> = host_directories()
            .iter()
            .filter_map(|dir| path_source(dir, 2))
            .collect();
        let mut sources = Vec::new();
        if !known.is_empty() {
            sources.push((known.join("|"), true));
        }
        sources.push((format!("{VERBATIM}[A-Za-z]:"), false));
        sources.push((
            "/(?:home|Users|root|tmp|var|usr|opt|private|mnt|run|etc|nix|srv|proc|snap|Library|Volumes)"
                .to_owned(),
            true,
        ));
        let mut host_rules = Vec::new();
        for (source, allow_bare) in sources {
            match rooted(&source, allow_bare) {
                Some(rule) => host_rules.push(rule),
                None => complete = false,
            }
        }
        Self {
            root: root.to_path_buf(),
            root_rule,
            host_rules,
            complete,
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn scrub_str(&self, text: &str) -> String {
        if !self.complete && text.contains(['/', '\\', ':']) {
            return HOST.to_owned();
        }
        let mut text = text.to_owned();
        if let Some(rule) = &self.root_rule {
            text = rule
                .replace_all(&text, |c: &Captures<'_>| {
                    let rest = match c.get(2) {
                        Some(rest) if !rest.as_str().is_empty() => {
                            collapse_separators(rest.as_str())
                        }
                        _ => ".".to_owned(),
                    };
                    let tail = c.get(3).map_or("", |m| m.as_str());
                    format!("{}{rest}{tail}", &c[1])
                })
                .into_owned();
        }
        for rule in &self.host_rules {
            text = rule
                .replace_all(&text, |c: &Captures<'_>| {
                    let tail = c.get(3).map_or("", |m| m.as_str());
                    format!("{}{HOST}{tail}", &c[1])
                })
                .into_owned();
        }
        text
    }

    /// Rewrites every string inside `value`. A `file` member that is still an
    /// absolute path afterwards (a layout the patterns do not know) is replaced
    /// outright.
    pub(crate) fn scrub_value(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.scrub_str(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.scrub_value(item)),
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    self.scrub_value(item);
                    if key == "file"
                        && let Some(file) = item.as_str()
                        && looks_absolute(file)
                    {
                        *item = json!(HOST);
                    }
                }
            }
            _ => {}
        }
    }
}

fn looks_absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('\\') || path.as_bytes().get(1) == Some(&b':')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure_text(scrubber: &Scrubber, text: &str) -> String {
        scrubber.scrub_str(text)
    }

    #[test]
    fn the_root_becomes_project_relative_in_every_spelling() {
        let scrubber = Scrubber::new(Path::new(r"\\?\D:\Work Space\proj"));
        for text in [
            r"\\?\D:\Work Space\proj\src\lib.rs",
            r"D:\Work Space\proj\src\lib.rs",
            // Rust `Debug` output doubles the backslashes.
            r"\\\\?\\D:\\Work Space\\proj\\src\\lib.rs",
            "D:/Work Space/proj/src/lib.rs",
        ] {
            assert_eq!(failure_text(&scrubber, text), "src/lib.rs", "{text}");
        }
        assert_eq!(
            failure_text(&scrubber, r"error --> D:\Work Space\proj\src\lib.rs:5:9"),
            "error --> src/lib.rs:5:9"
        );
        assert_eq!(
            failure_text(&scrubber, r"root is D:\Work Space\proj, ok"),
            "root is ., ok"
        );
        assert_eq!(failure_text(&scrubber, r"D:\Work Space\proj"), ".");
    }

    #[cfg(windows)]
    #[test]
    fn root_matching_ignores_case_on_windows() {
        let scrubber = Scrubber::new(Path::new(r"\\?\C:\Users\Dev\Proj"));
        assert_eq!(
            failure_text(&scrubber, r"c:\users\dev\proj\SRC\lib.rs"),
            "SRC/lib.rs"
        );
    }

    #[test]
    fn longer_names_sharing_the_root_prefix_are_not_the_root() {
        let scrubber = Scrubber::new(Path::new("/home/dev/proj"));
        assert_eq!(
            failure_text(&scrubber, "/home/dev/proj2/src"),
            "<host>",
            "a sibling directory is a foreign host path, not the project"
        );
        assert_eq!(failure_text(&scrubber, "/home/dev/proj/src"), "src");
    }

    #[test]
    fn foreign_absolute_paths_become_the_host_placeholder() {
        let scrubber = Scrubber::new(Path::new("/home/dev/proj"));
        for text in [
            r"C:\Users\Someone\.cargo\registry\src\index.crates.io-1\serde-1.0.0\src\lib.rs",
            r"\\?\C:\Windows\win.ini",
            "C:/Windows/win.ini",
            "/etc/passwd",
            "/home/other/.cargo/registry/src/x/lib.rs",
            "/tmp/.tmpAbC123/out.txt",
            "/Users/mac/Library/x",
        ] {
            assert_eq!(failure_text(&scrubber, text), HOST, "{text}");
        }
        assert_eq!(
            failure_text(&scrubber, r"warning: unused in C:\x\y\z.rs:3:1 (see C:\x)"),
            "warning: unused in <host>:3:1 (see <host>)"
        );
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        let scrubber = Scrubber::new(Path::new("/home/dev/proj"));
        for text in [
            r"regex \d+ is invalid",
            "a: b, c: d",
            "https://example.com/home/page",
            "see crates/engine-mcp/src/lib.rs",
            "the home directory",
            "x:y",
        ] {
            assert_eq!(failure_text(&scrubber, text), text, "{text}");
        }
    }

    #[test]
    fn values_are_scrubbed_recursively_and_absolute_files_are_replaced() {
        let scrubber = Scrubber::new(Path::new("/home/dev/proj"));
        let mut value = json!({"a": ["/home/dev/proj/src/lib.rs", 3, null],
            "nested": {"file": "weird-root:/x", "message": "at /home/dev/proj/b.rs"},
            "diagnostics": [{"file": "/opt/thing/lib.rs"}]});
        scrubber.scrub_value(&mut value);
        assert_eq!(value["a"][0], "src/lib.rs");
        assert_eq!(value["a"][1], 3);
        assert_eq!(value["nested"]["message"], "at b.rs");
        assert_eq!(value["diagnostics"][0]["file"], HOST);
    }
}

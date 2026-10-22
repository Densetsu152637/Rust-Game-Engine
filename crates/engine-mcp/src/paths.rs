//! Path confinement and bounded file access. Every file operation of the server
//! goes through this module: client paths are parsed into plain relative
//! components, resolved against the canonical project root, and checked again
//! after canonicalisation so symlinks and junctions cannot leave the root.

use crate::registry::ToolFailure;
use std::{
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

/// Directories that are never listed, searched or served as resources.
const HIDDEN_DIRS: [&str; 3] = [".git", ".derived", "target"];
/// Top-level directories the server refuses to write to.
const PROTECTED_WRITE_DIRS: [&str; 2] = [".derived", "target"];
const MAX_PATH_BYTES: usize = 1024;
const MAX_DEPTH: usize = 32;

/// "path" for the `path` argument, "<field> path" for others (`scene path`).
fn subject(field: &str) -> String {
    if field == "path" {
        "path".to_owned()
    } else {
        format!("{field} path")
    }
}

pub fn invalid_path(field: &str) -> ToolFailure {
    ToolFailure::new(
        "path_invalid",
        format!(
            "{} is invalid, missing, or outside the opened project",
            subject(field)
        ),
    )
    .with_hint(
        "Use an existing project-relative path with forward slashes and no '..' segments; \
         discover paths with project_info, scene_list or file_list.",
    )
}

/// Parses a client path into normal components. `None` for anything that is not
/// a plain project-relative path. An empty result means the project root itself.
pub fn parse_relative(input: &str) -> Option<PathBuf> {
    if input.len() > MAX_PATH_BYTES
        || input.starts_with('/')
        || input
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return None;
    }
    let mut result = PathBuf::new();
    for part in input.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            name => {
                if name.len() > 255
                    || name.ends_with('.')
                    || name.ends_with(' ')
                    || name.eq_ignore_ascii_case(".git")
                {
                    return None;
                }
                result.push(name);
            }
        }
    }
    Some(result)
}

/// An existing regular file inside the root (canonical), or `None`.
pub fn resolve_existing_file(root: &Path, input: &str) -> Option<PathBuf> {
    let relative = parse_relative(input).filter(|p| !p.as_os_str().is_empty())?;
    engine_project::confined_file(root, &relative).ok()
}

/// An existing directory inside the root (canonical); `""` and `"."` are the root.
pub fn resolve_existing_dir(root: &Path, input: &str) -> Option<PathBuf> {
    let relative = parse_relative(input)?;
    let canonical = fs::canonicalize(root.join(relative)).ok()?;
    (canonical.starts_with(root) && canonical.is_dir()).then_some(canonical)
}

/// Forward-slash path of `canonical` relative to `root`.
pub fn display_relative(root: &Path, canonical: &Path) -> String {
    let relative = canonical.strip_prefix(root).unwrap_or(canonical);
    relative
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub struct WriteTarget {
    /// Absolute destination (not canonical when it does not exist yet).
    pub path: PathBuf,
    pub exists: bool,
    /// The parent directories must be created before writing.
    pub needs_directories: bool,
}

/// Resolves a destination for a write without following any link: every
/// existing component must be a real directory (or, for the last one, a real
/// file) inside the root.
pub fn resolve_write_target(
    root: &Path,
    input: &str,
    field: &str,
) -> Result<WriteTarget, ToolFailure> {
    let relative = parse_relative(input)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| invalid_path(field))?;
    let names: Vec<_> = relative.components().collect();
    if names.first().is_some_and(|first| {
        let first = first.as_os_str().to_string_lossy().to_ascii_lowercase();
        PROTECTED_WRITE_DIRS.contains(&first.as_str())
    }) {
        return Err(ToolFailure::new(
            "path_protected",
            format!(
                "{} is in a generated or version-control directory",
                subject(field)
            ),
        )
        .with_hint("Writes to .git, .derived and target are never allowed."));
    }
    let mut current = root.to_path_buf();
    let mut exists = false;
    let mut needs_directories = false;
    for (index, name) in names.iter().enumerate() {
        let next = current.join(name);
        let last = index + 1 == names.len();
        match fs::symlink_metadata(&next) {
            Ok(metadata) => {
                let kind = metadata.file_type();
                if kind.is_symlink() {
                    return Err(ToolFailure::new(
                        "path_link",
                        format!("{} crosses a symbolic link or junction", subject(field)),
                    )
                    .with_hint(
                        "Writes never follow links; write to a real file inside the project.",
                    ));
                }
                if last && kind.is_file() {
                    exists = true;
                } else if last || !kind.is_dir() {
                    return Err(invalid_path(field));
                }
                current = next;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                needs_directories = !last;
                current = next;
                // Everything below a missing directory is new.
                current.extend(names[index + 1..].iter());
                break;
            }
            Err(_) => return Err(invalid_path(field)),
        }
    }
    // Backstop: the deepest existing ancestor must canonicalise inside the root.
    let mut ancestor = current.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or_else(|| invalid_path(field))?;
    }
    let canonical = fs::canonicalize(ancestor).map_err(|_| invalid_path(field))?;
    if !canonical.starts_with(root) {
        return Err(invalid_path(field));
    }
    Ok(WriteTarget {
        path: current,
        exists,
        needs_directories,
    })
}

/// Atomically replaces (or creates) `path`: the bytes go to a temporary file in
/// the same directory, are flushed to disk and then renamed over the target, so
/// readers see either the old or the new content, never a partial write.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("destination has no parent directory"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    // Windows briefly refuses to rename over a file another handle just closed.
    let mut attempt = 0_u64;
    loop {
        match temporary.persist(path) {
            Ok(_) => return Ok(()),
            Err(error) if attempt < 8 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(5 * attempt));
                temporary = error.file;
            }
            Err(error) => return Err(error.error),
        }
    }
}

pub fn is_hidden_name(name: &str) -> bool {
    HIDDEN_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d))
}

pub struct Entry {
    /// Forward-slash path relative to the project root.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Lists entries below `start` (a canonical directory inside `root`) in sorted,
/// deterministic order. Links and hidden directories are skipped. At most
/// `budget` entries are visited; the flag reports whether the walk was cut short.
pub fn walk(root: &Path, start: &Path, recursive: bool, budget: usize) -> (Vec<Entry>, bool) {
    let mut entries = Vec::new();
    let mut remaining = budget;
    let mut truncated = false;
    walk_directory(
        root,
        start,
        recursive,
        0,
        &mut remaining,
        &mut entries,
        &mut truncated,
    );
    (entries, truncated)
}

fn walk_directory(
    root: &Path,
    directory: &Path,
    recursive: bool,
    depth: usize,
    remaining: &mut usize,
    entries: &mut Vec<Entry>,
    truncated: &mut bool,
) {
    let Ok(read) = fs::read_dir(directory) else {
        return;
    };
    let mut children: Vec<_> = read
        .flatten()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry))
        .collect();
    children.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, entry) in children {
        if is_hidden_name(&name) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() || !(kind.is_dir() || kind.is_file()) {
            continue;
        }
        if *remaining == 0 {
            *truncated = true;
            return;
        }
        *remaining -= 1;
        let path = display_relative(root, &entry.path());
        if kind.is_dir() {
            entries.push(Entry {
                path,
                is_dir: true,
                size: 0,
            });
            if recursive && depth < MAX_DEPTH {
                walk_directory(
                    root,
                    &entry.path(),
                    true,
                    depth + 1,
                    remaining,
                    entries,
                    truncated,
                );
                if *truncated {
                    return;
                }
            }
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            entries.push(Entry {
                path,
                is_dir: false,
                size,
            });
        }
    }
}

pub enum ReadError {
    TooLarge,
    Io,
}

/// Reads at most `limit` bytes; a longer file is an error, never a truncation.
pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, ReadError> {
    let file = fs::File::open(path).map_err(|_| ReadError::Io)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ReadError::Io)?;
    if bytes.len() > limit {
        return Err(ReadError::TooLarge);
    }
    Ok(bytes)
}

/// UTF-8 text without NUL bytes, or `None` for binary content.
pub fn as_text(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// Hex digest used as the optimistic-concurrency token of text files.
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub fn mime_for_extension(path: &str) -> &'static str {
    let extension = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match extension.as_deref() {
        Some("json") => "application/json",
        Some("rs") => "text/x-rust",
        Some("toml") => "application/toml",
        Some("md") => "text/markdown",
        Some("txt" | "mesh" | "wgsl" | "lock" | "ron") => "text/plain",
        Some("ppm") => "image/x-portable-pixmap",
        Some("png") => "image/png",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        _ => "application/octet-stream",
    }
}

const UNRESERVED: &[u8] = b"-._~/";

pub fn percent_encode(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || UNRESERVED.contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Matches `path` against a glob supporting `*` (within a path segment), `**`
/// (across segments) and `?`. A pattern without `/` matches the file name only,
/// and a leading `**/` also matches zero directories (`**/*.rs` matches `a.rs`).
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix("**/")
        && glob_matches(rest, path)
    {
        return true;
    }
    let subject = if pattern.contains('/') {
        path
    } else {
        path.rsplit('/').next().unwrap_or(path)
    };
    #[derive(Clone, Copy, PartialEq)]
    enum Token {
        Literal(char),
        One,
        Star,
        DoubleStar,
    }
    let mut tokens = Vec::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        tokens.push(match c {
            '?' => Token::One,
            '*' if chars.peek() == Some(&'*') => {
                while chars.peek() == Some(&'*') {
                    chars.next();
                }
                Token::DoubleStar
            }
            '*' => Token::Star,
            other => Token::Literal(other),
        });
    }
    let text: Vec<char> = subject.chars().collect();
    // reachable[j]: the first i tokens can match text[..j].
    let mut reachable = vec![false; text.len() + 1];
    reachable[0] = true;
    for token in tokens {
        let mut next = vec![false; text.len() + 1];
        for j in 0..=text.len() {
            if !reachable[j] {
                continue;
            }
            match token {
                Token::Literal(c) => {
                    if text.get(j) == Some(&c) {
                        next[j + 1] = true;
                    }
                }
                Token::One => {
                    if text.get(j).is_some_and(|c| *c != '/') {
                        next[j + 1] = true;
                    }
                }
                Token::Star => {
                    next[j] = true;
                    let mut k = j;
                    while text.get(k).is_some_and(|c| *c != '/') {
                        k += 1;
                        next[k] = true;
                    }
                }
                Token::DoubleStar => {
                    for slot in &mut next[j..] {
                        *slot = true;
                    }
                }
            }
        }
        reachable = next;
    }
    reachable[text.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_follow_the_documented_rules() {
        assert!(glob_matches("*.json", "scenes/start.json"));
        assert!(!glob_matches("*.json", "scenes/start.rs"));
        assert!(glob_matches("scenes/*.json", "scenes/start.json"));
        assert!(!glob_matches("scenes/*.json", "scenes/deep/start.json"));
        assert!(glob_matches(
            "scenes/**/*.json",
            "scenes/deep/er/start.json"
        ));
        assert!(glob_matches("**/*.rs", "src/lib.rs"));
        assert!(glob_matches("**/*.rs", "lib.rs"));
        assert!(glob_matches("**/src/*.rs", "a/b/src/lib.rs"));
        assert!(glob_matches("start.???", "start.rs1"));
        assert!(!glob_matches("a?b", "a/b"));
    }

    #[test]
    fn relative_paths_are_normalised_or_rejected() {
        assert_eq!(parse_relative("./a//b/"), Some(PathBuf::from("a/b")));
        assert_eq!(parse_relative("."), Some(PathBuf::new()));
        for bad in [
            "../a",
            "a/../b",
            "/a",
            "a\\b",
            "C:/x",
            "a:b",
            ".git/config",
            "x/.GIT/y",
            "a.",
            "a*",
            "a\0b",
        ] {
            assert_eq!(parse_relative(bad), None, "{bad}");
        }
    }

    #[test]
    fn percent_coding_round_trips_unicode_and_spaces() {
        let path = "assets/my file é.txt";
        let encoded = percent_encode(path);
        assert_eq!(encoded, "assets/my%20file%20%C3%A9.txt");
        assert_eq!(percent_decode(&encoded).as_deref(), Some(path));
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%2"), None);
    }
}

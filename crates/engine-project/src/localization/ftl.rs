//! Line-level editing of Project Fluent (`.ftl`) text.
//!
//! The editor keeps everything it does not touch byte for byte (comments,
//! ordering, blank lines, line endings). It finds entries with the same rule
//! Fluent uses: an entry starts at a line whose first character is not
//! whitespace and that reads `id =` or `-term =`; indented lines and the blank
//! lines between them continue the entry; a run of `#` comment lines directly
//! above an entry belongs to it. Callers validate the edited text with the real
//! Fluent parser (`Localizer::load_ftl`) before writing it anywhere.
use crate::{Error, Result};

/// One message or term in a source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FtlEntry {
    /// Message id, or `-name` for a term.
    pub id: String,
    pub is_term: bool,
    /// 1-based line of the `id =` line.
    pub line: usize,
    /// The entry text without its comment or trailing newline.
    pub source: String,
    /// The attached `#` comment (without trailing newline), when present.
    pub comment: Option<String>,
    /// Byte range of the entry text in the source.
    start: usize,
    end: usize,
    /// Byte offset where the attached comment (if any) begins; `start` otherwise.
    comment_start: usize,
}

fn error(message: impl Into<String>) -> Error {
    Error::new("locale.ftl.shape", message)
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_')
}

/// `Some((id, is_term))` when `line` starts a message or term.
fn entry_head(line: &str) -> Option<(String, bool)> {
    let (rest, is_term) = match line.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (line, false),
    };
    let end = rest.find(|c: char| !is_ident_char(c)).unwrap_or(rest.len());
    let ident = &rest[..end];
    if !ident
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return None;
    }
    rest[end..]
        .trim_start_matches(' ')
        .starts_with('=')
        .then(|| {
            (
                if is_term {
                    format!("-{ident}")
                } else {
                    ident.to_owned()
                },
                is_term,
            )
        })
}

/// A single-`#` comment line (group `##` and file `###` comments are not attached).
fn attached_comment(line: &str) -> bool {
    line == "#" || line.starts_with("# ")
}

fn lines_of(source: &str) -> Vec<(usize, &str)> {
    let mut out = vec![];
    let mut offset = 0;
    for raw in source.split_inclusive('\n') {
        let text = raw.strip_suffix('\n').unwrap_or(raw);
        out.push((offset, text.strip_suffix('\r').unwrap_or(text)));
        offset += raw.len();
    }
    out
}

/// Every message and term of `source`, in file order.
pub fn entries(source: &str) -> Vec<FtlEntry> {
    let lines = lines_of(source);
    let mut out = vec![];
    let mut i = 0;
    while i < lines.len() {
        let Some((id, is_term)) = entry_head(lines[i].1) else {
            i += 1;
            continue;
        };
        let mut first = i;
        while first > 0 && attached_comment(lines[first - 1].1) {
            first -= 1;
        }
        let mut next = i + 1;
        while next < lines.len()
            && (lines[next].1.trim().is_empty() || lines[next].1.starts_with([' ', '\t']))
        {
            next += 1;
        }
        let mut last = next;
        while last > i + 1 && lines[last - 1].1.trim().is_empty() {
            last -= 1;
        }
        let start = lines[i].0;
        let end = lines[last - 1].0 + lines[last - 1].1.len();
        let comment_start = lines[first].0;
        out.push(FtlEntry {
            id,
            is_term,
            line: i + 1,
            source: source[start..end].to_owned(),
            comment: (first < i).then(|| {
                source[comment_start..start]
                    .trim_end_matches(['\n', '\r'])
                    .to_owned()
            }),
            start,
            end,
            comment_start,
        });
        i = next;
    }
    out
}

/// The entry with `id` (`name` for a message, `-name` for a term).
pub fn find<'a>(entries: &'a [FtlEntry], id: &str) -> Option<&'a FtlEntry> {
    entries.iter().find(|e| e.id == id)
}

/// Result of [`set_entry`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetResult {
    pub source: String,
    pub id: String,
    /// The text of the entry that was replaced; `None` when the entry is new.
    pub previous: Option<String>,
}

/// Replace the entry `ftl` defines, or append it. `ftl` must be exactly one
/// message or term (optionally with a `#` comment above it); a replaced entry
/// keeps its old comment unless `ftl` brings its own.
pub fn set_entry(source: &str, ftl: &str) -> Result<SetResult> {
    let normalized = ftl.replace("\r\n", "\n");
    let block = normalized.trim_matches(['\n', '\r']);
    let block = block.trim_end();
    let parsed = entries(block);
    let [entry] = parsed.as_slice() else {
        return Err(error(
            "ftl must define exactly one message or term, for example `greeting = Hello`",
        ));
    };
    if entry.end != block.len() || entry.comment_start != 0 {
        return Err(error(
            "ftl must contain one message or term and nothing else (no extra lines before or after it)",
        ));
    }
    let crlf = source.contains("\r\n");
    let encode = |text: &str| {
        if crlf {
            text.replace('\n', "\r\n")
        } else {
            text.to_owned()
        }
    };
    let existing = entries(source);
    let Some(old) = find(&existing, &entry.id) else {
        let mut out = source.to_owned();
        if !out.is_empty() {
            let newline = if crlf { "\r\n" } else { "\n" };
            if !out.ends_with('\n') {
                out.push_str(newline);
            }
            if !out.ends_with("\n\n") && !out.ends_with("\r\n\r\n") {
                out.push_str(newline);
            }
        }
        out.push_str(&encode(block));
        out.push_str(if crlf { "\r\n" } else { "\n" });
        return Ok(SetResult {
            source: out,
            id: entry.id.clone(),
            previous: None,
        });
    };
    let from = if entry.comment.is_some() {
        old.comment_start
    } else {
        old.start
    };
    let mut out = String::with_capacity(source.len() + block.len());
    out.push_str(&source[..from]);
    out.push_str(&encode(block));
    out.push_str(&source[old.end..]);
    Ok(SetResult {
        source: out,
        id: entry.id.clone(),
        previous: Some(old.source.clone()),
    })
}

/// Remove the entry `id` and its attached comment. Returns the new text and the
/// removed entry text, or `None` when no such entry exists.
pub fn remove_entry(source: &str, id: &str) -> Option<(String, String)> {
    let existing = entries(source);
    let old = find(&existing, id)?;
    let mut tail = &source[old.end..];
    // Swallow the entry's own line terminator.
    tail = tail
        .strip_prefix("\r\n")
        .or_else(|| tail.strip_prefix('\n'))
        .unwrap_or(tail);
    let head = &source[..old.comment_start];
    // Do not leave two blank lines (or a blank first line) behind.
    let blank_before = head.is_empty() || head.ends_with("\n\n") || head.ends_with("\r\n\r\n");
    if blank_before {
        tail = tail
            .strip_prefix("\r\n")
            .or_else(|| tail.strip_prefix('\n'))
            .unwrap_or(tail);
    }
    Some((format!("{head}{tail}"), old.source.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r"# Greeting shown on the title screen.
greeting = Hello, { $name }!
    .tooltip = Say hi

items = { $count ->
    [one] One item
   *[other] { $count } items
 }

## Group

-brand = Foundry
bye = Bye
";

    #[test]
    fn finds_entries_with_attributes_selectors_terms_and_comments() {
        let found = entries(FILE);
        let ids: Vec<_> = found.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["greeting", "items", "-brand", "bye"]);
        assert_eq!(
            found[0].source,
            "greeting = Hello, { $name }!\n    .tooltip = Say hi"
        );
        assert_eq!(
            found[0].comment.as_deref(),
            Some("# Greeting shown on the title screen.")
        );
        assert!(found[1].source.ends_with("items\n }"));
        assert!(found[2].is_term && found[3].comment.is_none());
        assert_eq!(found[3].line, 13);
    }

    #[test]
    fn set_replaces_in_place_and_keeps_the_rest_byte_for_byte() {
        let result = set_entry(FILE, "bye = Goodbye").unwrap();
        assert_eq!(result.previous.as_deref(), Some("bye = Bye"));
        assert_eq!(result.source, FILE.replace("bye = Bye", "bye = Goodbye"));
        let result = set_entry(FILE, "greeting = Hi").unwrap();
        assert!(
            result
                .source
                .starts_with("# Greeting shown on the title screen.\ngreeting = Hi\n\nitems")
        );
        let with_comment = set_entry(FILE, "# New\ngreeting = Hi").unwrap();
        assert!(
            with_comment
                .source
                .starts_with("# New\ngreeting = Hi\n\nitems")
        );
    }

    #[test]
    fn set_appends_new_entries_separated_by_a_blank_line() {
        let result = set_entry("a = A\n", "b = B").unwrap();
        assert_eq!(result.source, "a = A\n\nb = B\n");
        assert_eq!(result.previous, None);
        assert_eq!(set_entry("", "b = B").unwrap().source, "b = B\n");
        assert_eq!(
            set_entry("a = A", "b = B").unwrap().source,
            "a = A\n\nb = B\n"
        );
        let crlf = set_entry("a = A\r\n", "b = B\n    more").unwrap();
        assert_eq!(crlf.source, "a = A\r\n\r\nb = B\r\n    more\r\n");
    }

    #[test]
    fn set_rejects_anything_but_one_entry() {
        for bad in [
            "",
            "just text",
            "a = 1\nb = 2",
            "a = 1\n\n# stray",
            "  a = 1",
        ] {
            let error = set_entry("x = X\n", bad).unwrap_err();
            assert_eq!(error.diagnostics[0].code, "locale.ftl.shape", "{bad:?}");
        }
    }

    #[test]
    fn remove_takes_the_comment_and_collapses_blank_lines() {
        let (text, removed) = remove_entry(FILE, "greeting").unwrap();
        assert!(removed.starts_with("greeting = "));
        assert!(text.starts_with("items = "), "{text}");
        let (text, _) = remove_entry(FILE, "bye").unwrap();
        assert!(text.ends_with("-brand = Foundry\n"));
        let (text, _) = remove_entry("a = A\n\nb = B\n\nc = C\n", "b").unwrap();
        assert_eq!(text, "a = A\n\nc = C\n");
        assert!(remove_entry(FILE, "nope").is_none());
    }
}

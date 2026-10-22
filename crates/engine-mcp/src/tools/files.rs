//! Project file tools: `file_list`, `file_read`, `search_text` (read-only) and
//! `file_write` (write access only). Everything is confined to the project root
//! and bounded in size; see [`crate::paths`].

use super::{Spec, scene, tool};
use crate::{
    config::AccessLevel,
    paths::{self, ReadError},
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::{Project, Scene};
use regex::RegexBuilder;
use serde_json::{Map, Value, json};
use std::{fs, path::PathBuf};

const LINE_CLIP: usize = 300;
const SEARCH_FILE_CAP: usize = 2 * 1024 * 1024;
const SEARCH_TOTAL_BUDGET: usize = 64 * 1024 * 1024;

pub(super) fn read_tools() -> Vec<Box<dyn Tool>> {
    let path_arg = |description: &str| schema::string(description);
    vec![
        tool(
            Spec {
                name: "file_list",
                title: "List files",
                description: "List files and directories inside the project, confined to the \
                    project root (links, .git, .derived and target are hidden). Non-recursive \
                    by default; set recursive, extension or glob to walk subdirectories. Sorted \
                    and paged with offset/limit. `glob` supports `*` (within a segment), `**` \
                    (across segments) and `?`; a pattern without `/` matches file names only.",
                input: schema::object(
                    vec![
                        (
                            "path",
                            json!({"type": "string", "default": ".",
                            "description": "Project-relative directory to list."}),
                        ),
                        (
                            "recursive",
                            schema::boolean("Descend into subdirectories.", false),
                        ),
                        (
                            "extension",
                            schema::string(
                                "Only files with this extension, for example `rs` or `.json`.",
                            ),
                        ),
                        (
                            "glob",
                            schema::string(
                                "Only files whose project-relative path matches, for example `scenes/**/*.json` (at most 512 bytes).",
                            ),
                        ),
                        (
                            "offset",
                            schema::integer("Entries to skip (paging).", 0, 10_000_000, Some(0)),
                        ),
                        (
                            "limit",
                            schema::integer("Maximum entries to return.", 1, 1000, Some(200)),
                        ),
                    ],
                    &[],
                ),
                output: schema::output(vec![
                    (
                        "path",
                        schema::string("Listed directory, `.` for the root."),
                    ),
                    (
                        "entries",
                        json!({"type": "array", "items": {"type": "object", "properties": {
                        "path": {"type": "string"}, "kind": {"type": "string", "enum": ["file", "dir"]},
                        "size_bytes": {"type": "integer"}}}}),
                    ),
                    ("total", json!({"type": "integer"})),
                    ("returned", json!({"type": "integer"})),
                    ("offset", json!({"type": "integer"})),
                    ("truncated", json!({"type": "boolean"})),
                    ("next_offset", json!({"type": "integer"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            file_list,
        ),
        tool(
            Spec {
                name: "file_read",
                title: "Read file",
                description: "Read a UTF-8 text file inside the project, optionally a 1-based \
                    inclusive line range. Returns the content, total line count and a content \
                    `hash` (pass it as expected_hash to file_write). Output is capped; when it \
                    is cut, `truncated` is true and `next_start_line` says where to continue. \
                    Binary files are refused: use resources/read for those.",
                input: schema::object(
                    vec![
                        ("path", path_arg("Project-relative file path.")),
                        (
                            "start_line",
                            schema::integer(
                                "First line to return (1-based).",
                                1,
                                100_000_000,
                                Some(1),
                            ),
                        ),
                        (
                            "end_line",
                            schema::integer(
                                "Last line to return (inclusive); default end of file.",
                                1,
                                100_000_000,
                                None,
                            ),
                        ),
                    ],
                    &["path"],
                ),
                output: schema::output(vec![
                    ("path", schema::string("Project-relative path.")),
                    (
                        "hash",
                        schema::string("Hash of the whole file; use as expected_hash."),
                    ),
                    ("size_bytes", json!({"type": "integer"})),
                    ("total_lines", json!({"type": "integer"})),
                    ("start_line", json!({"type": "integer"})),
                    (
                        "end_line",
                        json!({"type": "integer", "description": "Last returned line (start_line-1 when empty)."}),
                    ),
                    ("truncated", json!({"type": "boolean"})),
                    ("line_truncated", json!({"type": "boolean"})),
                    ("next_start_line", json!({"type": "integer"})),
                    (
                        "content",
                        schema::string("The requested text, line endings preserved."),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            file_read,
        ),
        tool(
            Spec {
                name: "search_text",
                title: "Search text",
                description: "Search project text files for a substring (default, case-insensitive) \
                    or a regular expression, like a bounded grep. Returns path, line, column and \
                    the line text for up to max_results matches, optionally with context lines. \
                    Binary and oversized files are skipped and counted.",
                input: schema::object(
                    vec![
                        (
                            "query",
                            schema::string(
                                "Text or regular expression to find (at most 512 characters).",
                            ),
                        ),
                        (
                            "regex",
                            schema::boolean("Treat query as a regular expression.", false),
                        ),
                        (
                            "case_sensitive",
                            schema::boolean("Match case exactly.", false),
                        ),
                        (
                            "path",
                            json!({"type": "string", "default": ".",
                            "description": "Project-relative directory to search."}),
                        ),
                        (
                            "extension",
                            schema::string("Only files with this extension."),
                        ),
                        (
                            "glob",
                            schema::string(
                                "Only files whose project-relative path matches this glob (at most 512 bytes).",
                            ),
                        ),
                        (
                            "max_results",
                            schema::integer("Stop after this many matches.", 1, 500, Some(50)),
                        ),
                        (
                            "context_lines",
                            schema::integer(
                                "Lines of context before and after each match.",
                                0,
                                3,
                                Some(0),
                            ),
                        ),
                    ],
                    &["query"],
                ),
                output: schema::output(vec![
                    ("query", json!({"type": "string"})),
                    (
                        "matches",
                        json!({"type": "array", "items": {"type": "object", "properties": {
                        "path": {"type": "string"}, "line": {"type": "integer"},
                        "column": {"type": "integer"}, "text": {"type": "string"},
                        "before": {"type": "array", "items": {"type": "string"}},
                        "after": {"type": "array", "items": {"type": "string"}}}}}),
                    ),
                    ("match_count", json!({"type": "integer"})),
                    ("files_scanned", json!({"type": "integer"})),
                    ("skipped_binary", json!({"type": "integer"})),
                    ("skipped_large", json!({"type": "integer"})),
                    ("truncated", json!({"type": "boolean"})),
                    ("truncation_reason", json!({"type": "string"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            search_text,
        ),
    ]
}

pub(super) fn write_tools() -> Vec<Box<dyn Tool>> {
    vec![tool(
        Spec {
            name: "file_write",
            title: "Write file",
            description: "Create or overwrite a UTF-8 text file inside the project (scripts, \
                Cargo.toml, scenes, project.json, notes). Atomic: readers see the old or the new \
                content, never a partial write. Overwriting requires expected_hash from file_read \
                (omit it only to create a new file). Writes never follow links, never leave the \
                root and are refused in .git, .derived and target. Scene files are validated \
                against the project before they are written; project.json is validated after \
                and restored if it breaks a valid project. Prefer entity_* and component_* for \
                scene edits.",
            input: schema::object(
                vec![
                    ("path", schema::string("Project-relative destination path.")),
                    (
                        "content",
                        schema::string("Complete new file content (UTF-8, no NUL)."),
                    ),
                    (
                        "expected_hash",
                        schema::string(
                            "`hash` from file_read of the version you are replacing. Required when the file exists; omit to create.",
                        ),
                    ),
                ],
                &["path", "content"],
            ),
            output: schema::output(vec![
                ("path", schema::string("Project-relative path written.")),
                ("created", json!({"type": "boolean"})),
                ("bytes", json!({"type": "integer"})),
                ("hash", schema::string("Hash of the new content.")),
                (
                    "previous_hash",
                    schema::string("Hash of the replaced content."),
                ),
                (
                    "validated",
                    json!({"type": "string", "enum": ["scene", "manifest", "none"]}),
                ),
                (
                    "revision",
                    schema::string("Scene revision, when a scene was written."),
                ),
            ]),
            annotations: Annotations::mutation(true, true),
            access: AccessLevel::Write,
        },
        file_write,
    )]
}

fn extension_filter(value: Option<&str>) -> Option<String> {
    value.map(|e| e.trim_start_matches('.').to_ascii_lowercase())
}

fn has_extension(path: &str, wanted: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case(wanted))
}

/// The optional `glob` argument, capped like `query`: matching costs
/// O(pattern x path) per entry, so an unbounded pattern is a CPU lever.
fn glob_argument<'a>(a: &Args<'a>) -> Result<Option<&'a str>, ToolFailure> {
    let glob = a.optional_string("glob")?;
    if glob.is_some_and(|g| g.len() > paths::MAX_GLOB_BYTES) {
        return Err(schema::invalid(
            "glob",
            &format!("at most {} bytes", paths::MAX_GLOB_BYTES),
        ));
    }
    Ok(glob)
}

fn directory_argument(cx: &ToolContext<'_>, a: &Args<'_>) -> Result<PathBuf, ToolFailure> {
    let text = a.optional_string("path")?.unwrap_or(".");
    paths::resolve_existing_dir(cx.root, text).ok_or_else(|| paths::invalid_path("path"))
}

fn file_list(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let directory = directory_argument(cx, &a)?;
    let extension = extension_filter(a.optional_string("extension")?);
    let glob = glob_argument(&a)?;
    let recursive = a.boolean("recursive", false)? || extension.is_some() || glob.is_some();
    let offset = a.integer("offset", 0, 10_000_000, 0)? as usize;
    let limit = a.integer("limit", 1, cx.limits.max_page_size as u64, 200)? as usize;
    let (entries, walk_truncated) =
        paths::walk(cx.root, &directory, recursive, cx.limits.max_walk_entries);
    let filtering = extension.is_some() || glob.is_some();
    let matching: Vec<_> = entries
        .into_iter()
        .filter(|e| {
            !filtering
                || (!e.is_dir
                    && extension
                        .as_deref()
                        .is_none_or(|x| has_extension(&e.path, x))
                    && glob.is_none_or(|g| paths::glob_matches(g, &e.path)))
        })
        .collect();
    let page: Vec<Value> = matching
        .iter()
        .skip(offset)
        .take(limit)
        .map(|e| {
            if e.is_dir {
                json!({"path": e.path, "kind": "dir"})
            } else {
                json!({"path": e.path, "kind": "file", "size_bytes": e.size})
            }
        })
        .collect();
    let end = offset + page.len();
    let shown = paths::display_relative(cx.root, &directory);
    let mut result = json!({
        "path": if shown.is_empty() { ".".to_owned() } else { shown },
        "total": matching.len(), "returned": page.len(), "offset": offset,
        "truncated": end < matching.len() || walk_truncated, "entries": page,
    });
    if end < matching.len() {
        result["next_offset"] = json!(end);
    }
    Ok(ToolResult::new(result))
}

fn load_text(
    cx: &ToolContext<'_>,
    file: &std::path::Path,
    action: &str,
) -> Result<(Vec<u8>, String), ToolFailure> {
    let bytes = paths::read_bounded(file, cx.limits.max_file_scan_bytes).map_err(|e| match e {
        ReadError::TooLarge => ToolFailure::new(
            "file_too_large",
            format!(
                "file exceeds the {} byte limit for {action}",
                cx.limits.max_file_scan_bytes
            ),
        )
        .with_hint("Use search_text to locate the part you need, or work on a smaller file."),
        ReadError::Io => ToolFailure::new("io_error", "the file could not be read"),
    })?;
    let hash = paths::content_hash(&bytes);
    Ok((bytes, hash))
}

fn not_text() -> ToolFailure {
    ToolFailure::new("not_text", "the file is not UTF-8 text").with_hint(
        "Text tools handle UTF-8 files without NUL bytes; use resources/read for binary files.",
    )
}

fn file_read(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let start = a.integer("start_line", 1, 100_000_000, 1)? as usize;
    let end = a
        .optional_integer("end_line", 1, 100_000_000)?
        .map(|n| n as usize);
    if end.is_some_and(|end| end < start) {
        return Err(schema::invalid("end_line", "not smaller than start_line"));
    }
    let file = cx.existing_file("path", args.get("path").unwrap_or(&Value::Null))?;
    let (bytes, hash) = load_text(cx, &file, "file_read")?;
    let text = paths::as_text(&bytes).ok_or_else(not_text)?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total = lines.len();
    if start > total.max(1) {
        return Err(ToolFailure::new(
            "line_out_of_range",
            format!("start_line {start} is past the end of the file ({total} lines)"),
        )
        .with_hint("Pick a start_line between 1 and total_lines."));
    }
    let wanted_end = end.unwrap_or(usize::MAX).min(total);
    let cap = cx.limits.max_file_read_bytes;
    let mut content = String::new();
    let mut last = start - 1;
    let mut line_truncated = false;
    for (index, line) in lines.iter().enumerate().take(wanted_end).skip(start - 1) {
        if content.len() + line.len() > cap {
            if content.is_empty() {
                let cut = line.floor_char_boundary(cap);
                content.push_str(&line[..cut]);
                last = index + 1;
                line_truncated = true;
            }
            break;
        }
        content.push_str(line);
        last = index + 1;
    }
    let truncated = last < wanted_end;
    let mut result = json!({
        "path": cx.relative(&file), "hash": hash, "size_bytes": bytes.len(),
        "total_lines": total, "start_line": start, "end_line": last,
        "truncated": truncated, "content": content,
    });
    if line_truncated {
        result["line_truncated"] = json!(true);
    }
    if truncated {
        result["next_start_line"] = json!(last + 1);
    }
    Ok(ToolResult::new(result).verbatim())
}

fn clip(line: &str) -> String {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.len() <= LINE_CLIP {
        return line.to_owned();
    }
    format!("{}…", &line[..line.floor_char_boundary(LINE_CLIP)])
}

fn search_text(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let query = a.string("query")?;
    if query.is_empty() || query.len() > 512 {
        return Err(schema::invalid("query", "1 to 512 characters"));
    }
    let as_regex = a.boolean("regex", false)?;
    let case_sensitive = a.boolean("case_sensitive", false)?;
    let extension = extension_filter(a.optional_string("extension")?);
    let glob = glob_argument(&a)?;
    let max_results =
        a.integer("max_results", 1, cx.limits.max_search_results as u64, 50)? as usize;
    let context = a.integer("context_lines", 0, 3, 0)? as usize;
    let directory = directory_argument(cx, &a)?;
    let pattern = if as_regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .size_limit(1 << 20)
        .dfa_size_limit(4 << 20)
        .build()
        .map_err(|error| {
            ToolFailure::new(
                "invalid_argument",
                format!("invalid regular expression: {error}"),
            )
            .with_hint("Fix the pattern or set regex:false to search for the literal text.")
        })?;

    let (entries, walk_truncated) =
        paths::walk(cx.root, &directory, true, cx.limits.max_walk_entries);
    let per_file = cx.limits.max_file_scan_bytes.min(SEARCH_FILE_CAP);
    let (mut matches, mut scanned, mut binary, mut large, mut budget) =
        (Vec::new(), 0_usize, 0_usize, 0_usize, SEARCH_TOTAL_BUDGET);
    let mut reason: Option<&str> = walk_truncated.then_some("directory_walk_limit");
    'files: for entry in entries.iter().filter(|e| !e.is_dir) {
        if extension
            .as_deref()
            .is_some_and(|x| !has_extension(&entry.path, x))
            || glob.is_some_and(|g| !paths::glob_matches(g, &entry.path))
        {
            continue;
        }
        if entry.size as usize > per_file {
            large += 1;
            continue;
        }
        if budget < entry.size as usize {
            reason = Some("scan_budget");
            break;
        }
        budget -= entry.size as usize;
        let Ok(bytes) = paths::read_bounded(&cx.root.join(&entry.path), per_file) else {
            continue;
        };
        let Some(text) = paths::as_text(&bytes) else {
            binary += 1;
            continue;
        };
        scanned += 1;
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let Some(found) = regex.find(line) else {
                continue;
            };
            let mut item = json!({"path": entry.path, "line": index + 1,
                "column": line[..found.start()].chars().count() + 1, "text": clip(line)});
            if context > 0 {
                let before = &lines[index.saturating_sub(context)..index];
                let after =
                    &lines[(index + 1).min(lines.len())..(index + 1 + context).min(lines.len())];
                item["before"] = json!(before.iter().map(|l| clip(l)).collect::<Vec<_>>());
                item["after"] = json!(after.iter().map(|l| clip(l)).collect::<Vec<_>>());
            }
            matches.push(item);
            if matches.len() >= max_results {
                reason = Some("max_results");
                break 'files;
            }
        }
    }
    let mut result = json!({"query": query, "match_count": matches.len(),
        "files_scanned": scanned, "skipped_binary": binary, "skipped_large": large,
        "truncated": reason.is_some(), "matches": matches});
    if let Some(reason) = reason {
        result["truncation_reason"] = json!(reason);
    }
    Ok(ToolResult::new(result).verbatim())
}

/// What a write was checked against, reported to the client.
enum Validation {
    None,
    Scene(String),
    Manifest,
}

fn file_write(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let path = a.string("path")?;
    let content = a.string("content")?;
    let expected = a.optional_string("expected_hash")?;
    if content.len() > cx.limits.max_file_write_bytes {
        return Err(ToolFailure::new(
            "content_too_large",
            format!(
                "content exceeds the {} byte write limit",
                cx.limits.max_file_write_bytes
            ),
        )
        .with_hint("Split the content across several files."));
    }
    if content.contains('\0') {
        return Err(not_text().with_hint("Content must not contain NUL bytes."));
    }
    let target = paths::resolve_write_target(cx.root, path, "path")?;
    // The destination is canonical (real on-disk spelling), so the reported path
    // and every decision below are immune to case variants and 8.3 aliases.
    let relative = paths::display_relative(cx.root, &target.path);

    let mut previous: Option<(Vec<u8>, String)> = None;
    if target.exists {
        let (bytes, hash) = load_text(cx, &target.path, "file_write")?;
        paths::as_text(&bytes).ok_or_else(not_text)?;
        match expected {
            None => {
                return Err(ToolFailure::new(
                    "hash_required",
                    "the file already exists; overwriting needs expected_hash",
                )
                .with_hint("Call file_read and pass its `hash` as expected_hash."));
            }
            Some(expected) if expected != hash => {
                return Err(ToolFailure::new(
                    "revision_conflict",
                    "the file changed since the supplied expected_hash",
                )
                .with_hint("Re-read the file with file_read, merge your change and retry."));
            }
            Some(_) => {}
        }
        previous = Some((bytes, hash));
    } else if expected.is_some() {
        return Err(ToolFailure::new(
            "file_not_found",
            "expected_hash was supplied but the file does not exist",
        )
        .with_hint("Omit expected_hash to create a new file."));
    }

    // Validate before committing wherever the content can be checked in memory.
    let project = cx.project().ok();
    let mut validation = Validation::None;
    // Decide by comparing canonical files, never strings: `PROJECT.JSON` and
    // 8.3 aliases are the same file as `project.json` on Windows. A file that
    // does not exist yet cannot be the manifest or a catalog scene.
    let is_manifest = target.exists && scene::is_manifest(cx.root, &target.path);
    if let Some(project) = &project
        && !is_manifest
        && target.exists
        && scene::is_catalog_scene(cx.root, project, &target.path)
    {
        let revision = validate_scene_text(project, &relative, content)?;
        validation = Validation::Scene(revision);
    } else if project.is_some() && is_manifest {
        validation = Validation::Manifest;
    }

    if target.needs_directories {
        let parent = target
            .path
            .parent()
            .ok_or_else(|| paths::invalid_path("path"))?;
        fs::create_dir_all(parent).map_err(io_failure)?;
        if !fs::canonicalize(parent).is_ok_and(|p| p.starts_with(cx.root)) {
            return Err(paths::invalid_path("path"));
        }
    }
    paths::atomic_write(&target.path, content.as_bytes()).map_err(io_failure)?;

    if matches!(validation, Validation::Manifest)
        && let Err(error) = Project::load(&target.path)
    {
        // The manifest was valid before this write: put the old bytes back.
        if let Some((bytes, _)) = &previous {
            paths::atomic_write(&target.path, bytes).map_err(io_failure)?;
        }
        let mut failure = ToolFailure::from(error);
        failure.details = Some(json!({"rolled_back": previous.is_some()}));
        failure.hint = Some(
            "The write would have invalidated project.json, so the previous content was \
             restored. Fix the reported problems and write again."
                .into(),
        );
        return Err(failure);
    }

    let mut result = json!({
        "path": relative, "created": previous.is_none(), "bytes": content.len(),
        "hash": paths::content_hash(content.as_bytes()),
        "validated": match &validation {
            Validation::None => "none", Validation::Scene(_) => "scene", Validation::Manifest => "manifest",
        },
    });
    if let Some((_, hash)) = previous {
        result["previous_hash"] = json!(hash);
    }
    if let Validation::Scene(revision) = validation {
        result["revision"] = json!(revision);
    }
    Ok(ToolResult::new(result))
}

fn io_failure(error: std::io::Error) -> ToolFailure {
    ToolFailure::new(
        "io_error",
        format!("the file could not be written: {}", error.kind()),
    )
}

/// Parses `text` as a scene and validates it against `project`.
fn validate_scene_text(
    project: &Project,
    relative: &str,
    text: &str,
) -> Result<String, ToolFailure> {
    let mut scene: Scene = serde_json::from_str(text).map_err(|error| {
        ToolFailure::new(
            "scene_invalid",
            format!("the content is not a valid scene: {error}"),
        )
        .with_hint(
            "Scenes use {schema, scene_id, entities:[{id,name,components}]}; see component_schema.",
        )
    })?;
    scene.source = PathBuf::from(relative);
    project.validate_scene(&scene).map_err(ToolFailure::from)?;
    Ok(scene.revision())
}

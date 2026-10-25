//! Localization tools: inspect string tables, preview formatting and
//! pseudo-localization, and edit `.ftl` files (`docs/localization.md`).
//!
//! Reads never execute game code. Every write re-parses the whole locale with
//! the real Fluent parser before touching disk, goes through the confined,
//! atomic file helpers, reloads the project afterwards and restores the previous
//! bytes if the project no longer validates.
use super::{Spec, slash, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::{
    Project,
    localization::{
        self, LocaleConfigInfo, MAX_STRING_TABLE_BYTES,
        engine_locale::{
            LocaleArgs, LocaleError, LocaleId, Localizer, PseudoOptions, pseudo_localize,
        },
        ftl,
    },
};
use serde_json::{Map, Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

const MAX_PREVIEW_ARGS: usize = 16;
const MAX_SOURCE_CLIP: usize = 4096;
const CONFIG_ASSET: &str = "localization";
const CONFIG_PATH: &str = "localization.json";

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "locale_list",
                title: "List locales",
                description: "List the project's localization: the locale_config asset, the default locale and, per locale, its string_table files (with content hash for expected_hash), key count, translation coverage against the default locale and the missing and extra keys. Keys are `message` or `message.attribute`. Read-only; parses every table with the Fluent parser.",
                input: schema::object(
                    vec![(
                        "max_missing",
                        schema::integer(
                            "Most missing and extra keys listed per locale (counts are always exact).",
                            0,
                            500,
                            Some(50),
                        ),
                    )],
                    &[],
                ),
                output: schema::output(vec![
                    (
                        "configured",
                        json!({"type":"boolean","description":"False when the project has no locale_config asset."}),
                    ),
                    (
                        "config",
                        json!({"type":"object","description":"The locale_config asset: id and path."}),
                    ),
                    ("default_locale", json!({"type":"string"})),
                    (
                        "pseudo",
                        json!({"type":"object","description":"The authored pseudo-localization options."}),
                    ),
                    (
                        "locales",
                        json!({"type":"array","items":{"type":"object"},"description":"One entry per locale: files, keys, translated, total, missing, extra, fonts."}),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            list,
        ),
        tool(
            Spec {
                name: "locale_messages",
                title: "Read string table messages",
                description: "Read the messages of one locale from its string_table files: id, file, line and the exact Fluent source (plural selectors, attributes, terms) so it can be edited and passed back to locale_set_message. Filter with `prefix`, or give `key` (`message` or `message.attribute`) to get that message plus a formatted preview in this locale (with fallback) using optional `args`. Paged.",
                input: schema::object(
                    vec![
                        (
                            "locale",
                            schema::string(
                                "Locale to read, for example `fr` or `pt-BR`. It must be an available locale of the locale_config.",
                            ),
                        ),
                        (
                            "key",
                            schema::string(
                                "Only this message (id or `id.attribute`); adds a formatted `preview`.",
                            ),
                        ),
                        (
                            "prefix",
                            schema::string(
                                "Only message ids starting with this text (terms start with `-`).",
                            ),
                        ),
                        (
                            "args",
                            json!({"type":"object","description":"Preview arguments for `key`: variable name to a string or number (numbers drive plural selectors).","additionalProperties":{"type":["string","number"]},"maxProperties":MAX_PREVIEW_ARGS}),
                        ),
                        (
                            "offset",
                            schema::integer("Messages to skip (paging).", 0, 100_000, Some(0)),
                        ),
                        (
                            "limit",
                            schema::integer("Most messages returned.", 1, 500, Some(100)),
                        ),
                    ],
                    &["locale"],
                ),
                output: schema::output(vec![
                    ("locale", json!({"type":"string"})),
                    (
                        "total",
                        json!({"type":"integer","description":"Matching messages and terms."}),
                    ),
                    ("returned", json!({"type":"integer"})),
                    ("offset", json!({"type":"integer"})),
                    ("truncated", json!({"type":"boolean"})),
                    ("next_offset", json!({"type":"integer"})),
                    (
                        "messages",
                        json!({"type":"array","items":{"type":"object"},"description":"id, file, line, source, optional comment."}),
                    ),
                    (
                        "preview",
                        json!({"type":"object","description":"Present with `key`: text, resolved_locale, fell_back, defined, format_errors."}),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            messages,
        ),
        tool(
            Spec {
                name: "locale_pseudo_preview",
                title: "Preview pseudo-localization",
                description: "Show how pseudo-localization transforms text so layouts can be checked without translating: accents, length expansion, markers and optional right-to-left mirroring. Placeables `{ $var }` and rich-text tags such as [b] are preserved. Give either raw `text`, or a message `key` (formatted in `locale` first). Options default to the locale_config's pseudo settings.",
                input: schema::object(
                    vec![
                        (
                            "text",
                            schema::string(
                                "Raw text or Fluent value to transform. Mutually exclusive with `key`.",
                            ),
                        ),
                        (
                            "key",
                            schema::string(
                                "Message key (`id` or `id.attribute`) to format and transform. Mutually exclusive with `text`.",
                            ),
                        ),
                        (
                            "locale",
                            schema::string(
                                "Locale used to format `key` (default: the default locale).",
                            ),
                        ),
                        (
                            "args",
                            json!({"type":"object","description":"Formatting arguments for `key`: name to string or number.","additionalProperties":{"type":["string","number"]},"maxProperties":MAX_PREVIEW_ARGS}),
                        ),
                        ("options", pseudo_schema()),
                    ],
                    &[],
                ),
                output: schema::output(vec![
                    ("original", json!({"type":"string"})),
                    ("pseudo", json!({"type":"string"})),
                    ("original_chars", json!({"type":"integer"})),
                    ("pseudo_chars", json!({"type":"integer"})),
                    (
                        "growth_percent",
                        json!({"type":"number","description":"Length growth of the transformed text over the original."}),
                    ),
                    (
                        "options",
                        json!({"type":"object","description":"The options that were applied."}),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            pseudo_preview,
        ),
        tool(
            Spec {
                name: "locale_set_message",
                title: "Set a string table message",
                description: "Add or replace one message or term in a locale's string_table (.ftl) file. `ftl` is exactly one Fluent entry, for example `greeting = Hello, { $name }!` or a plural selector spanning lines; an existing entry with the same id is replaced in place (its comment is kept unless `ftl` has its own) and everything else in the file stays byte for byte. The whole locale is re-parsed first: invalid Fluent, duplicate ids across the locale's files or a project that would stop validating are rejected and no file changes.",
                input: schema::object(
                    vec![
                        (
                            "locale",
                            schema::string("Locale whose file to edit, for example `fr`."),
                        ),
                        (
                            "ftl",
                            schema::string(
                                "One complete Fluent message or term: `id = value`, optional indented `.attribute = value` lines; may start with a `#` comment line.",
                            ),
                        ),
                        (
                            "file",
                            schema::string(
                                "string_table asset ID or project-relative path to edit when the locale has several files. Default: the file that defines the id, else the first file.",
                            ),
                        ),
                        (
                            "expected_hash",
                            schema::string(
                                "Optional hash of the target file from locale_list; the edit is refused if the file changed since.",
                            ),
                        ),
                    ],
                    &["locale", "ftl"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("asset", json!({"type":"string"})),
                    ("id", json!({"type":"string"})),
                    (
                        "action",
                        json!({"type":"string","enum":["created","replaced"]}),
                    ),
                    (
                        "previous",
                        json!({"type":"string","description":"The replaced source, when replaced."}),
                    ),
                    ("hash", json!({"type":"string"})),
                    ("previous_hash", json!({"type":"string"})),
                    (
                        "keys",
                        json!({"type":"integer","description":"Keys the locale defines afterwards."}),
                    ),
                    (
                        "missing_in",
                        json!({"type":"array","items":{"type":"string"},"description":"Other locales that still lack this key."}),
                    ),
                ]),
                annotations: Annotations::mutation(true, true),
                access: AccessLevel::Write,
            },
            set_message,
        ),
        tool(
            Spec {
                name: "locale_remove_message",
                title: "Remove a string table message",
                description: "Remove one message or term (and its attached `#` comment) from a locale's string_table file. The rest of the file is unchanged. Other locales keep their copy; the result lists them. Refused, with no change, if the edited file no longer parses or the project would stop validating.",
                input: schema::object(
                    vec![
                        ("locale", schema::string("Locale whose file to edit.")),
                        (
                            "id",
                            schema::string(
                                "Message id, or `-name` for a term. Attributes are removed with their message.",
                            ),
                        ),
                        (
                            "file",
                            schema::string(
                                "string_table asset ID or project-relative path when the locale has several files. Default: the file that defines the id.",
                            ),
                        ),
                        (
                            "expected_hash",
                            schema::string(
                                "Optional hash of the target file from locale_list; the edit is refused if the file changed since.",
                            ),
                        ),
                    ],
                    &["locale", "id"],
                ),
                output: schema::output(vec![
                    ("path", json!({"type":"string"})),
                    ("asset", json!({"type":"string"})),
                    ("id", json!({"type":"string"})),
                    (
                        "removed",
                        json!({"type":"string","description":"The removed Fluent source."}),
                    ),
                    ("hash", json!({"type":"string"})),
                    ("previous_hash", json!({"type":"string"})),
                    (
                        "still_defined_in",
                        json!({"type":"array","items":{"type":"string"},"description":"Other locales that still define this id."}),
                    ),
                ]),
                annotations: Annotations::mutation(true, false),
                access: AccessLevel::Write,
            },
            remove_message,
        ),
        tool(
            Spec {
                name: "locale_add_locale",
                title: "Add a locale",
                description: "Create a new string_table (.ftl) file for a locale and wire it into the project: registers the string_table asset in project.json and adds the locale to the locale_config's available_locales and resources (creating `localization.json` with this locale as the default when the project has none). `copy_from` seeds the file with another locale's messages as a translation starting point. Existing files are never overwritten; if the project would not validate afterwards every touched file is restored.",
                input: schema::object(
                    vec![
                        (
                            "locale",
                            schema::string("New locale, for example `de` or `pt-BR`."),
                        ),
                        (
                            "path",
                            schema::string(
                                "New project-relative .ftl file in an existing or new directory, for example `strings/de/ui.ftl`.",
                            ),
                        ),
                        (
                            "asset_id",
                            schema::string(
                                "ID for the new string_table asset. Default `strings-<locale>`.",
                            ),
                        ),
                        (
                            "copy_from",
                            schema::string(
                                "Existing locale whose messages seed the file. Default: an empty file with a header comment.",
                            ),
                        ),
                    ],
                    &["locale", "path"],
                ),
                output: schema::output(vec![
                    ("locale", json!({"type":"string"})),
                    ("path", json!({"type":"string"})),
                    (
                        "asset",
                        json!({"type":"object","description":"The registered asset entry: id, kind, path."}),
                    ),
                    (
                        "config",
                        json!({"type":"object","description":"The locale_config: asset, path and whether this call created it."}),
                    ),
                    (
                        "entries",
                        json!({"type":"integer","description":"Messages and terms seeded."}),
                    ),
                    ("hash", json!({"type":"string"})),
                ]),
                annotations: Annotations::mutation(false, false),
                access: AccessLevel::Write,
            },
            add_locale,
        ),
    ]
}

fn pseudo_schema() -> Value {
    schema::object(
        vec![
            (
                "accents",
                schema::boolean("Replace ASCII letters with accented look-alikes.", true),
            ),
            (
                "expansion_percent",
                schema::integer(
                    "Extra length as a percentage of the translatable characters.",
                    0,
                    300,
                    Some(35),
                ),
            ),
            (
                "prefix",
                schema::string("Marker placed before the text (at most 8 characters)."),
            ),
            (
                "suffix",
                schema::string("Marker placed after the text (at most 8 characters)."),
            ),
            (
                "rtl",
                schema::boolean(
                    "Wrap the result in right-to-left override marks to check mirroring.",
                    false,
                ),
            ),
        ],
        &[],
    )
}

// ---- helpers ---------------------------------------------------------------

fn locale_failure(error: LocaleError) -> ToolFailure {
    let hint = match error {
        LocaleError::Parse { .. } => {
            "Fluent syntax: `id = text`, attributes as indented `.name = text`, selectors `{ $n -> [one] ... *[other] ... }`."
        }
        LocaleError::DuplicateMessage { .. } => {
            "Each id may be defined once per locale across all of its files; use locale_set_message to replace it."
        }
        LocaleError::UnknownLocale { .. } | LocaleError::Unavailable { .. } => {
            "List the locales with locale_list."
        }
        _ => "See docs/localization.md for the limits and error codes.",
    };
    ToolFailure::new(error.code(), error.to_string()).with_hint(hint)
}

fn io_failure(error: std::io::Error) -> ToolFailure {
    ToolFailure::new(
        "io_error",
        format!("the file could not be written: {}", error.kind()),
    )
}

fn parse_locale(text: &str, field: &str) -> Result<LocaleId, ToolFailure> {
    LocaleId::parse(text).map_err(|_| schema::invalid(field, "a locale such as `en` or `pt-BR`"))
}

struct Loaded {
    project: Project,
    info: LocaleConfigInfo,
}

fn configured(cx: &ToolContext<'_>) -> Result<Loaded, ToolFailure> {
    let project = cx.project()?;
    let info = localization::locale_config(&project)
        .map_err(ToolFailure::from)?
        .ok_or_else(|| {
            ToolFailure::new(
                "locale_not_configured",
                "the project has no locale_config asset",
            )
            .with_hint("Create one with locale_add_locale; it writes localization.json and registers the assets.")
        })?;
    Ok(Loaded { project, info })
}

/// One string_table file of a locale, read from disk.
struct Table {
    asset: String,
    path: String,
    text: String,
    hash: String,
}

fn read_table(cx: &ToolContext<'_>, project: &Project, path: &str) -> Result<Table, ToolFailure> {
    let asset = project
        .assets
        .values()
        .find(|a| a.kind == "string_table" && slash(&a.path) == path)
        .map(|a| a.id.clone())
        .ok_or_else(|| {
            ToolFailure::new(
                "locale_table_missing",
                format!("{path} is not a string_table asset"),
            )
        })?;
    let file = cx.existing_file("path", &json!(path))?;
    let bytes = paths::read_bounded(&file, MAX_STRING_TABLE_BYTES as usize)
        .map_err(|_| ToolFailure::new("file_too_large", "the string table is too large"))?;
    let text = paths::as_text(&bytes)
        .ok_or_else(|| ToolFailure::new("not_text", "the string table is not UTF-8 text"))?
        .to_owned();
    Ok(Table {
        asset,
        path: path.to_owned(),
        hash: paths::content_hash(&bytes),
        text,
    })
}

fn tables_of(
    cx: &ToolContext<'_>,
    loaded: &Loaded,
    locale: &LocaleId,
) -> Result<Vec<Table>, ToolFailure> {
    let paths = loaded.info.tables_of(locale.as_str());
    if paths.is_empty() {
        return Err(ToolFailure::new(
            "locale_unknown",
            format!("{locale} is not a locale of this project"),
        )
        .with_hint("List the locales with locale_list, or create one with locale_add_locale."));
    }
    paths
        .iter()
        .map(|path| read_table(cx, &loaded.project, path))
        .collect()
}

fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    format!("{}...", &text[..text.floor_char_boundary(limit)])
}

fn preview_args(args: &Args<'_>) -> Result<LocaleArgs, ToolFailure> {
    let mut out = LocaleArgs::new();
    for (name, value) in args.optional_object("args")?.into_iter().flatten() {
        match value {
            Value::Number(n) => out = out.number(name, n.as_f64().unwrap_or(0.0)),
            Value::String(s) => out = out.text(name, s.clone()),
            _ => {
                return Err(schema::invalid(
                    "args",
                    "variable names mapped to strings or numbers",
                ));
            }
        }
    }
    Ok(out)
}

fn pseudo_options(args: &Args<'_>, base: PseudoOptions) -> Result<PseudoOptions, ToolFailure> {
    let mut options = base;
    options.enabled = true;
    if let Some(given) = args.optional_object("options")? {
        let a = Args(given);
        options.accents = a.boolean("accents", options.accents)?;
        options.rtl = a.boolean("rtl", options.rtl)?;
        options.expansion_percent = a.integer(
            "expansion_percent",
            0,
            300,
            u64::from(options.expansion_percent),
        )? as u32;
        if let Some(prefix) = a.optional_string("prefix")? {
            options.prefix = prefix.to_owned();
        }
        if let Some(suffix) = a.optional_string("suffix")? {
            options.suffix = suffix.to_owned();
        }
    }
    options.validate().map_err(locale_failure)?;
    Ok(options)
}

// ---- read tools ------------------------------------------------------------

fn list(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let max = Args(args).integer("max_missing", 0, 500, 50)? as usize;
    let project = cx.project()?;
    let Some((info, localizer)) =
        localization::build_localizer(&project).map_err(ToolFailure::from)?
    else {
        return Ok(ToolResult::new(json!({
            "configured": false, "locales": [],
            "hint": "The project has no locale_config asset; locale_add_locale creates one."
        })));
    };
    let loaded = Loaded {
        project,
        info: info.clone(),
    };
    let default = localizer.default_locale().clone();
    let mut locales = Vec::new();
    for locale in localizer.locales() {
        let files: Vec<Value> = tables_of(cx, &loaded, &locale)?
            .iter()
            .map(|t| {
                json!({"asset": t.asset, "path": t.path, "bytes": t.text.len(),
                    "hash": t.hash, "entries": ftl::entries(&t.text).len()})
            })
            .collect();
        let keys = localizer.keys(locale.as_str()).map_err(locale_failure)?;
        let mut entry = json!({
            "locale": locale.as_str(), "is_default": locale == default,
            "rtl": locale.is_rtl(), "files": files, "keys": keys.len(),
        });
        match localizer.missing_report(locale.as_str()) {
            Ok(report) if locale != default => {
                let cut =
                    |list: &[String]| -> Vec<String> { list.iter().take(max).cloned().collect() };
                entry["total"] = json!(report.total);
                entry["translated"] = json!(report.translated);
                entry["coverage"] = json!(if report.total == 0 {
                    1.0
                } else {
                    report.translated as f64 / report.total as f64
                });
                entry["missing_count"] = json!(report.missing.len());
                entry["missing"] = json!(cut(&report.missing));
                entry["missing_truncated"] = json!(report.missing.len() > max);
                entry["extra_count"] = json!(report.extra.len());
                entry["extra"] = json!(cut(&report.extra));
            }
            _ => {
                entry["total"] = json!(keys.len());
                entry["translated"] = json!(keys.len());
                entry["coverage"] = json!(1.0);
                entry["missing_count"] = json!(0);
            }
        }
        if let Some(assets) = info
            .settings
            .assets
            .iter()
            .find(|a| LocaleId::parse(&a.locale).is_ok_and(|id| id == locale))
        {
            entry["fonts"] = json!(assets.fonts);
            entry["asset_overrides"] = json!(assets.asset_overrides.len());
        }
        locales.push(entry);
    }
    Ok(ToolResult::new(json!({
        "configured": true,
        "config": {"asset": info.asset, "path": info.path},
        "default_locale": default.as_str(),
        "pseudo": info.settings.pseudo,
        "locales": locales,
    })))
}

fn messages(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let locale = parse_locale(a.string("locale")?, "locale")?;
    let key = a.optional_string("key")?;
    let prefix = a.optional_string("prefix")?;
    let offset = a.integer("offset", 0, 100_000, 0)? as usize;
    let limit = a.integer("limit", 1, cx.limits.max_page_size.min(500) as u64, 100)? as usize;
    let preview = preview_args(&a)?;
    let loaded = configured(cx)?;
    let tables = tables_of(cx, &loaded, &locale)?;
    let wanted_id = key.map(|k| k.split('.').next().unwrap_or(k));
    let mut matching = Vec::new();
    for table in &tables {
        for entry in ftl::entries(&table.text) {
            if wanted_id.is_some_and(|id| entry.id != id)
                || prefix.is_some_and(|p| !entry.id.starts_with(p))
            {
                continue;
            }
            matching.push((table.path.clone(), entry));
        }
    }
    let page: Vec<Value> = matching
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(path, entry)| {
            let mut message = json!({"id": entry.id, "file": path, "line": entry.line,
                "term": entry.is_term, "source": clip(&entry.source, MAX_SOURCE_CLIP)});
            if let Some(comment) = &entry.comment {
                message["comment"] = json!(comment);
            }
            message
        })
        .collect();
    let end = offset + page.len();
    let mut result = json!({
        "locale": locale.as_str(), "total": matching.len(), "returned": page.len(),
        "offset": offset, "truncated": end < matching.len(), "messages": page,
    });
    if end < matching.len() {
        result["next_offset"] = json!(end);
    }
    if let Some(key) = key {
        let (_, mut localizer) = localization::build_localizer(&loaded.project)
            .map_err(ToolFailure::from)?
            .expect("configured");
        localizer
            .set_locale_id(locale.clone())
            .map_err(locale_failure)?;
        let defined = localizer.contains(key);
        let resolved = localizer.resolve(key, &preview);
        result["preview"] = json!({
            "text": resolved.text, "defined": defined,
            "resolved_locale": resolved.locale.as_str(),
            "fell_back": defined && resolved.locale != locale,
            "format_errors": resolved.format_errors,
        });
    }
    Ok(ToolResult::new(result).verbatim())
}

fn pseudo_preview(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let text = a.optional_string("text")?;
    let key = a.optional_string("key")?;
    if text.is_some() == key.is_some() {
        return Err(schema::invalid(
            "text",
            "set exactly one of `text` and `key`",
        ));
    }
    let mut base = PseudoOptions::default();
    let mut original = text.map(str::to_owned);
    if let Ok(project) = cx.project()
        && let Ok(Some(info)) = localization::locale_config(&project)
    {
        base = info.settings.pseudo.clone();
    }
    if let Some(key) = key {
        let loaded = configured(cx)?;
        let (_, mut localizer) = localization::build_localizer(&loaded.project)
            .map_err(ToolFailure::from)?
            .expect("configured");
        if let Some(locale) = a.optional_string("locale")? {
            localizer
                .set_locale_id(parse_locale(locale, "locale")?)
                .map_err(locale_failure)?;
        }
        localizer
            .set_pseudo(PseudoOptions::default())
            .map_err(locale_failure)?;
        let formatted = localizer
            .try_format(key, &preview_args(&a)?)
            .map_err(locale_failure)?;
        original = Some(formatted);
    }
    let original = original.expect("one source");
    if original.len() > MAX_SOURCE_CLIP * 4 {
        return Err(schema::invalid("text", "at most 16 KiB"));
    }
    let options = pseudo_options(&a, base)?;
    let pseudo = pseudo_localize(&original, &options);
    let (before, after) = (original.chars().count(), pseudo.chars().count());
    Ok(ToolResult::new(json!({
        "original": original, "pseudo": pseudo,
        "original_chars": before, "pseudo_chars": after,
        "growth_percent": if before == 0 { 0.0 } else { (after as f64 / before as f64 - 1.0) * 100.0 },
        "options": options,
    }))
    .verbatim())
}

// ---- write tools -----------------------------------------------------------

/// Pick the table to edit: an explicit `file`, else the table defining `id`,
/// else the first.
fn pick(tables: &[Table], file: Option<&str>, id: Option<&str>) -> Result<usize, ToolFailure> {
    if let Some(file) = file {
        let wanted = file.replace('\\', "/");
        return tables
            .iter()
            .position(|t| t.asset == file || t.path == wanted)
            .ok_or_else(|| {
                ToolFailure::new(
                    "locale_table_missing",
                    format!("{file} is not one of this locale's string tables"),
                )
                .with_hint("Use an asset ID or path from locale_list `files`.")
            });
    }
    if let Some(id) = id
        && let Some(index) = tables
            .iter()
            .position(|t| ftl::entries(&t.text).iter().any(|e| e.id == id))
    {
        return Ok(index);
    }
    Ok(0)
}

/// Parse the locale as it would be after replacing `tables[index]` with `text`.
fn validate_locale(
    locale: &LocaleId,
    tables: &[Table],
    index: usize,
    text: &str,
) -> Result<Localizer, ToolFailure> {
    let mut scratch = Localizer::default();
    for (i, table) in tables.iter().enumerate() {
        let source = if i == index { text } else { &table.text };
        scratch
            .load_ftl(locale.as_str(), source)
            .map_err(locale_failure)?;
    }
    Ok(scratch)
}

/// Locales other than `locale` that do (`wanted`) or do not define `key`
/// (a message id or `id.attribute`).
fn locales_where(loaded: &Loaded, locale: &LocaleId, wanted: bool, key: &str) -> Vec<String> {
    let Ok(Some((_, localizer))) = localization::build_localizer(&loaded.project) else {
        return vec![];
    };
    let prefix = format!("{key}.");
    localizer
        .locales()
        .into_iter()
        .filter(|l| l != locale)
        .filter(|l| {
            localizer
                .keys(l.as_str())
                .is_ok_and(|keys| keys.iter().any(|k| k == key || k.starts_with(&prefix)) == wanted)
        })
        .map(|l| l.to_string())
        .collect()
}

/// Write `new_text` over `table`, reload the project and restore the old bytes
/// when it no longer validates.
fn commit(cx: &ToolContext<'_>, table: &Table, new_text: &str) -> Result<String, ToolFailure> {
    if new_text.len() as u64 > MAX_STRING_TABLE_BYTES {
        return Err(
            ToolFailure::new("content_too_large", "the string table would exceed 1 MiB")
                .with_hint("Split the strings across several string_table files."),
        );
    }
    let target = paths::resolve_write_target(cx.root, &table.path, "file")?;
    paths::atomic_write(&target.path, new_text.as_bytes()).map_err(io_failure)?;
    if let Err(failure) = cx.project() {
        paths::atomic_write(&target.path, table.text.as_bytes()).map_err(io_failure)?;
        let mut failure = failure;
        failure.details = Some(json!({"rolled_back": true}));
        failure.hint = Some(
            "The edit would have invalidated the project, so the previous content was restored."
                .into(),
        );
        return Err(failure);
    }
    Ok(paths::content_hash(new_text.as_bytes()))
}

fn check_expected(table: &Table, expected: Option<&str>) -> Result<(), ToolFailure> {
    if let Some(expected) = expected
        && expected != table.hash
    {
        return Err(ToolFailure::new(
            "revision_conflict",
            "the string table changed since the supplied expected_hash",
        )
        .with_hint("Re-read it with locale_list and retry."));
    }
    Ok(())
}

fn set_message(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let locale = parse_locale(a.string("locale")?, "locale")?;
    let block = a.string("ftl")?;
    if block.len() > MAX_SOURCE_CLIP * 4 {
        return Err(schema::invalid("ftl", "at most 16 KiB"));
    }
    let loaded = configured(cx)?;
    let tables = tables_of(cx, &loaded, &locale)?;
    // The id decides which file is edited, so parse the block first.
    let probe = ftl::set_entry("", block).map_err(ToolFailure::from)?;
    let index = pick(&tables, a.optional_string("file")?, Some(&probe.id))?;
    let table = &tables[index];
    check_expected(table, a.optional_string("expected_hash")?)?;
    // A message defined in another file of this locale is a duplicate, not an add.
    if let Some(other) = tables
        .iter()
        .enumerate()
        .find(|(i, t)| *i != index && ftl::entries(&t.text).iter().any(|e| e.id == probe.id))
    {
        return Err(ToolFailure::new(
            "locale.duplicate_message",
            format!("{} is defined in {}", probe.id, other.1.path),
        )
        .with_hint("Pass `file` to edit the file that defines it."));
    }
    let edited = ftl::set_entry(&table.text, block).map_err(ToolFailure::from)?;
    let scratch = validate_locale(&locale, &tables, index, &edited.source)?;
    // The line-level edit and the real parser must agree on what was written.
    let key = probe.id.trim_start_matches('-');
    let defined = if probe.id.starts_with('-') {
        true
    } else {
        scratch.keys(locale.as_str()).is_ok_and(|keys| {
            keys.iter()
                .any(|k| k == key || k.starts_with(&format!("{key}.")))
        })
    };
    if !defined {
        return Err(ToolFailure::new(
            "locale.ftl.shape",
            format!(
                "{} did not parse as a message with a value or attributes",
                probe.id
            ),
        ));
    }
    let entries = scratch.keys(locale.as_str()).map(|k| k.len()).unwrap_or(0);
    let hash = commit(cx, table, &edited.source)?;
    let mut result = json!({
        "path": table.path, "asset": table.asset, "id": edited.id,
        "action": if edited.previous.is_some() { "replaced" } else { "created" },
        "hash": hash, "previous_hash": table.hash, "keys": entries,
        "missing_in": locales_where(&loaded, &locale, false, &edited.id),
    });
    if let Some(previous) = edited.previous {
        result["previous"] = json!(clip(&previous, MAX_SOURCE_CLIP));
    }
    Ok(ToolResult::new(result))
}

fn remove_message(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let locale = parse_locale(a.string("locale")?, "locale")?;
    let id = a.string("id")?;
    let loaded = configured(cx)?;
    let tables = tables_of(cx, &loaded, &locale)?;
    let index = pick(&tables, a.optional_string("file")?, Some(id))?;
    let table = &tables[index];
    check_expected(table, a.optional_string("expected_hash")?)?;
    let Some((new_text, removed)) = ftl::remove_entry(&table.text, id) else {
        return Err(ToolFailure::new(
            "locale_message_not_found",
            format!("{} has no message or term {id} in {}", locale, table.path),
        )
        .with_hint("List ids with locale_messages (terms are named `-name`)."));
    };
    let scratch = validate_locale(&locale, &tables, index, &new_text)?;
    if scratch.keys(locale.as_str()).is_ok_and(|keys| {
        keys.iter()
            .any(|k| k == id || k.starts_with(&format!("{id}.")))
    }) && !id.starts_with('-')
    {
        return Err(ToolFailure::new(
            "locale_message_not_found",
            format!("{id} is still defined after the edit; it lives in another file of {locale}"),
        )
        .with_hint("Pass `file` to pick the file that defines it."));
    }
    let hash = commit(cx, table, &new_text)?;
    Ok(ToolResult::new(json!({
        "path": table.path, "asset": table.asset, "id": id,
        "removed": clip(&removed, MAX_SOURCE_CLIP),
        "hash": hash, "previous_hash": table.hash,
        "still_defined_in": locales_where(&loaded, &locale, true, id),
    })))
}

/// Byte range between the brackets of the top-level array `key` in JSON `text`.
fn top_level_array(text: &str, key: &str) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let skip_string = |mut i: usize| -> usize {
        // `i` is just past the opening quote.
        while i < b.len() && b[i] != b'"' {
            i += if b[i] == b'\\' { 2 } else { 1 };
        }
        i + 1
    };
    let skip_space = |mut i: usize| -> usize {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let (mut i, mut depth) = (0, 0usize);
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i + 1;
                let end = skip_string(start);
                let name = text.get(start..end - 1)?;
                i = end;
                if depth == 1 && name == key {
                    let colon = skip_space(i);
                    if b.get(colon) == Some(&b':') {
                        let open = skip_space(colon + 1);
                        if b.get(open) == Some(&b'[') {
                            let (mut k, mut d) = (open, 0usize);
                            while k < b.len() {
                                match b[k] {
                                    b'"' => k = skip_string(k + 1) - 1,
                                    b'[' | b'{' => d += 1,
                                    b']' | b'}' => {
                                        d -= 1;
                                        if d == 0 {
                                            return Some((open + 1, k));
                                        }
                                    }
                                    _ => {}
                                }
                                k += 1;
                            }
                            return None;
                        }
                    }
                }
            }
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Append `item` (JSON text) to the top-level array `key`, matching the
/// indentation of the existing items, without reformatting anything else.
fn append_array_item(text: &str, key: &str, item: &str) -> Option<String> {
    let (start, end) = top_level_array(text, key)?;
    let inner = &text[start..end];
    if inner.trim().is_empty() {
        let close_indent = text[..start]
            .rfind('\n')
            .map(|n| {
                text[n + 1..]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect::<String>()
            })
            .unwrap_or_default();
        return Some(format!(
            "{}\n{close_indent}  {item}\n{close_indent}{}",
            &text[..start],
            &text[end..]
        ));
    }
    let last = inner.trim_end().len();
    let first = start + inner.len() - inner.trim_start().len();
    let indent: String = text[..first]
        .rfind('\n')
        .map(|n| text[n + 1..first].to_owned())
        .filter(|s| s.chars().all(|c| c == ' ' || c == '\t'))
        .unwrap_or_else(|| " ".into());
    let at = start + last;
    let separator = if indent.trim().is_empty() && text[..first].contains('\n') {
        format!(",\n{indent}")
    } else {
        ", ".to_owned()
    };
    Some(format!("{}{separator}{item}{}", &text[..at], &text[at..]))
}

/// The config document the tools write: `schema` first, then the settings in
/// their declared order.
#[derive(serde::Serialize)]
struct ConfigDocument<'a> {
    schema: u32,
    #[serde(flatten)]
    settings: &'a localization::engine_locale::LocalizationSettings,
}

fn config_json(info: &LocaleConfigInfo) -> Result<String, ToolFailure> {
    serde_json::to_string_pretty(&ConfigDocument {
        schema: 1,
        settings: &info.settings,
    })
    .map(|mut text| {
        text.push('\n');
        text
    })
    .map_err(|e| ToolFailure::from(engine_project::Error::from(e)))
}

struct Restore {
    created: Vec<PathBuf>,
    replaced: Vec<(PathBuf, Vec<u8>)>,
    dirs: Vec<PathBuf>,
}

impl Restore {
    fn undo(self) {
        for (path, bytes) in self.replaced {
            let _ = paths::atomic_write(&path, &bytes);
        }
        for path in self.created {
            let _ = fs::remove_file(path);
        }
        for dir in self.dirs.into_iter().rev() {
            let _ = fs::remove_dir(dir);
        }
    }
}

fn add_locale(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let a = Args(args);
    let locale = parse_locale(a.string("locale")?, "locale")?;
    let path = a.string("path")?;
    if !path.to_ascii_lowercase().ends_with(".ftl") {
        return Err(schema::invalid("path", "a new project-relative .ftl file"));
    }
    let project = cx.project()?;
    let existing = localization::locale_config(&project).map_err(ToolFailure::from)?;
    if let Some(info) = &existing
        && (!info.tables_of(locale.as_str()).is_empty()
            || info
                .settings
                .available_locales
                .iter()
                .any(|l| LocaleId::parse(l).is_ok_and(|id| id == locale)))
    {
        return Err(ToolFailure::new(
            "locale_exists",
            format!("{locale} is already a locale of this project"),
        )
        .with_hint("Edit its strings with locale_set_message."));
    }
    let target = paths::resolve_write_target(cx.root, path, "path")?;
    if target.exists {
        return Err(ToolFailure::new("path_exists", "the file already exists")
            .with_hint("Existing files are never overwritten; pick a new path."));
    }
    let asset_id = a
        .optional_string("asset_id")?
        .map_or_else(|| format!("strings-{locale}"), str::to_owned);
    if asset_id.is_empty() || asset_id.len() > 256 || asset_id.chars().any(char::is_control) {
        return Err(schema::invalid("asset_id", "a short printable asset ID"));
    }
    if project.assets.contains_key(&asset_id) {
        return Err(ToolFailure::new(
            "asset_exists",
            format!("the project already has an asset {asset_id}"),
        )
        .with_hint("Pass a different asset_id."));
    }
    let relative = slash(Path::new(path));
    let (seed, entries, copied) = match a.optional_string("copy_from")? {
        Some(from) => {
            let from = parse_locale(from, "copy_from")?;
            let loaded = Loaded {
                project: project.clone(),
                info: existing.clone().ok_or_else(|| {
                    ToolFailure::new("locale_not_configured", "the project has no locale_config")
                })?,
            };
            let tables = tables_of(cx, &loaded, &from)?;
            let mut text =
                format!("# Strings for {locale}, seeded from {from}: translate each message.\n");
            let mut count = 0;
            for table in &tables {
                count += ftl::entries(&table.text).len();
                text.push('\n');
                text.push_str(table.text.trim_end());
                text.push('\n');
            }
            (text, count, Some(from.to_string()))
        }
        None => (format!("# Strings for {locale}.\n"), 0, None),
    };
    if seed.len() as u64 > MAX_STRING_TABLE_BYTES {
        return Err(ToolFailure::new(
            "content_too_large",
            "the seeded table would exceed 1 MiB",
        ));
    }

    // Plan every file before touching any.
    let manifest = cx.root.join("project.json");
    let manifest_text = fs::read_to_string(&manifest).map_err(io_failure)?;
    let asset_item = |id: &str, kind: &str, path: &str| {
        json!({"id": id, "kind": kind, "path": path}).to_string()
    };
    let mut manifest_next = append_array_item(
        &manifest_text,
        "assets",
        &asset_item(&asset_id, "string_table", &relative),
    )
    .ok_or_else(|| {
        ToolFailure::new(
            "manifest_unsupported",
            "project.json has no top-level `assets` array",
        )
    })?;
    let (config_path, config_text, config_created) = match &existing {
        Some(info) => {
            let mut info = info.clone();
            info.settings.available_locales.push(locale.to_string());
            info.settings
                .resources
                .push(localization::engine_locale::LocaleResources {
                    locale: locale.to_string(),
                    paths: vec![relative.clone()],
                });
            (info.path.clone(), config_json(&info)?, false)
        }
        None => {
            if project.assets.contains_key(CONFIG_ASSET) || cx.root.join(CONFIG_PATH).exists() {
                return Err(ToolFailure::new(
                    "path_exists",
                    format!("{CONFIG_PATH} or an asset named {CONFIG_ASSET} already exists"),
                ));
            }
            let info = LocaleConfigInfo {
                asset: CONFIG_ASSET.into(),
                path: CONFIG_PATH.into(),
                settings: localization::engine_locale::LocalizationSettings {
                    default_locale: locale.to_string(),
                    available_locales: vec![locale.to_string()],
                    resources: vec![localization::engine_locale::LocaleResources {
                        locale: locale.to_string(),
                        paths: vec![relative.clone()],
                    }],
                    ..Default::default()
                },
            };
            manifest_next = append_array_item(
                &manifest_next,
                "assets",
                &asset_item(CONFIG_ASSET, "locale_config", CONFIG_PATH),
            )
            .ok_or_else(|| {
                ToolFailure::new("manifest_unsupported", "project.json cannot be extended")
            })?;
            (CONFIG_PATH.to_owned(), config_json(&info)?, true)
        }
    };
    serde_json::from_str::<Value>(&manifest_next).map_err(|_| {
        ToolFailure::new(
            "manifest_unsupported",
            "project.json could not be extended safely",
        )
    })?;

    // Apply, restoring everything if the result does not validate.
    let mut undo = Restore {
        created: vec![],
        replaced: vec![],
        dirs: vec![],
    };
    let result = (|| -> Result<(), ToolFailure> {
        if target.needs_directories {
            let parent = target
                .path
                .parent()
                .ok_or_else(|| paths::invalid_path("path"))?;
            let mut missing = vec![];
            let mut cursor = parent;
            while !cursor.exists() {
                missing.push(cursor.to_path_buf());
                cursor = cursor.parent().ok_or_else(|| paths::invalid_path("path"))?;
            }
            fs::create_dir_all(parent).map_err(io_failure)?;
            undo.dirs.extend(missing);
            if !fs::canonicalize(parent).is_ok_and(|p| p.starts_with(cx.root)) {
                return Err(paths::invalid_path("path"));
            }
        }
        paths::atomic_write(&target.path, seed.as_bytes()).map_err(io_failure)?;
        undo.created.push(target.path.clone());
        let config_target = paths::resolve_write_target(cx.root, &config_path, "config")?;
        if config_created {
            paths::atomic_write(&config_target.path, config_text.as_bytes()).map_err(io_failure)?;
            undo.created.push(config_target.path.clone());
        } else {
            undo.replaced.push((
                config_target.path.clone(),
                fs::read(&config_target.path).map_err(io_failure)?,
            ));
            paths::atomic_write(&config_target.path, config_text.as_bytes()).map_err(io_failure)?;
        }
        undo.replaced
            .push((manifest.clone(), manifest_text.clone().into_bytes()));
        paths::atomic_write(&manifest, manifest_next.as_bytes()).map_err(io_failure)?;
        cx.project().map(|_| ())
    })();
    if let Err(mut failure) = result {
        undo.undo();
        failure.details = Some(json!({"rolled_back": true}));
        failure.hint = Some("Nothing was changed: every touched file was restored.".into());
        return Err(failure);
    }
    Ok(ToolResult::new(json!({
        "locale": locale.as_str(), "path": relative,
        "asset": {"id": asset_id, "kind": "string_table", "path": relative},
        "config": {"asset": existing.as_ref().map_or(CONFIG_ASSET, |i| i.asset.as_str()),
            "path": config_path, "created": config_created},
        "entries": entries, "copied_from": copied,
        "hash": paths::content_hash(seed.as_bytes()),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_to_a_json_array_without_reformatting_the_rest() {
        let text = "{\n  \"schema\": 1,\n  \"assets\": [\n    {\"id\": \"a\"},\n    {\"id\": \"b\"}\n  ],\n  \"x\": [1]\n}\n";
        let next = append_array_item(text, "assets", "{\"id\":\"c\"}").unwrap();
        assert_eq!(
            next,
            "{\n  \"schema\": 1,\n  \"assets\": [\n    {\"id\": \"a\"},\n    {\"id\": \"b\"},\n    {\"id\":\"c\"}\n  ],\n  \"x\": [1]\n}\n"
        );
        assert!(serde_json::from_str::<Value>(&next).is_ok());
        let empty = "{\"assets\": []}";
        assert_eq!(
            append_array_item(empty, "assets", "1").unwrap(),
            "{\"assets\": [\n  1\n]}"
        );
        let inline = "{\"assets\":[{\"id\":\"a\"}]}";
        assert_eq!(
            append_array_item(inline, "assets", "{\"id\":\"b\"}").unwrap(),
            "{\"assets\":[{\"id\":\"a\"}, {\"id\":\"b\"}]}"
        );
        // Only the top-level key counts; strings that merely say `assets` do not.
        let tricky = "{\"name\": \"assets\", \"nested\": {\"assets\": [9]}, \"assets\": [1]}";
        assert_eq!(
            append_array_item(tricky, "assets", "2").unwrap(),
            "{\"name\": \"assets\", \"nested\": {\"assets\": [9]}, \"assets\": [1, 2]}"
        );
        assert!(append_array_item("{\"a\":1}", "assets", "2").is_none());
    }
}

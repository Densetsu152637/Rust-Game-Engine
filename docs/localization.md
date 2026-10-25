# Localization (`engine-locale` and project integration)

`engine-locale` is a standalone, pure-Rust localization library: Project Fluent string tables
with plural and gender selection, locale parsing, fallback and negotiation, a runtime language
switch, missing-translation reports, per-locale font/asset mapping, pseudo-localization and a
serde descriptor for authored settings. It depends on no engine crate, no GPU or windowing
library and does no file I/O (callers pass `.ftl` text or bytes).

`engine-project` wires it into the game: `string_table` and `locale_config` assets, `@key` text
references in the game UI and `engine.WorldText`, message keys in data bindings, per-locale
fonts and asset overrides, a runtime language switch and MCP string-table tools. The library is
described first; see [Project integration](#project-integration) for the engine side.

## Quick start

```rust
use engine_locale::*;

let mut l10n = Localizer::default();                       // default locale "en"
l10n.load_ftl("en", "items = { $n -> [one] one item *[other] { $n } items }\n")?;
l10n.load_ftl("fr", "items = { $n -> [one] un objet *[other] { $n } objets }\n")?;

l10n.set_locale("fr")?;                                    // runtime switch, no reload
let text = l10n.format("items", &LocaleArgs::new().number("n", 3.0)); // "3 objets"
if l10n.generation() != cached_generation { /* re-resolve UI strings */ }
```

## API

| Item | Purpose |
| --- | --- |
| `LocaleId` | Parsed, canonical identifier (`fr_ca` -> `fr-CA`). `parse`, `language()`, `is_rtl()`, `truncations()`, `fallback_chain(default)`. Equality/order use the canonical form. |
| `negotiate(requested, available, default)` | Picks the best available locale for an ordered preference list. |
| `Localizer` / `LocalizerConfig` | Catalog set + current locale. Lookups take `&self` (`Send + Sync`); loading and switching take `&mut self`. |
| `load_ftl` / `load_ftl_bytes` / `replace_ftl` / `remove_locale` | Add a source to a locale, replace a locale atomically (hot reload), drop a locale. A failed call changes nothing. |
| `set_locale` / `set_locale_id` / `locale` / `generation` | Runtime switch; the generation counter. |
| `format(key, &args)` | Always returns text; a missing key gives the marker and a diagnostic. |
| `resolve(key, &args)` | Same, returning `Resolved { text, locale, generation, format_errors }`. |
| `try_format(key, &args)` | Strict: missing key, bad key, oversized output or any Fluent error is an `Err`. |
| `format_attribute(msg, attr, &args)` | Same as `format("msg.attr", ..)`. |
| `contains(key)`, `keys(locale)`, `all_keys()` | Key introspection (`msg` and `msg.attr`, sorted). |
| `missing_report(locale)`, `missing_reports()` | Coverage against the default locale: `total`, `translated`, `missing`, `extra`. |
| `diagnostics()`, `take_diagnostics()`, `diagnostics_dropped()` | Bounded, deduplicated log with counts. |
| `LocaleArgs` | `text(name, str)` and `number(name, f64)`; builder style. |
| `LocaleAssetMap` / `LocaleAssets` | Per-locale `fonts` and `asset_overrides`. |
| `Localizer::fonts()`, `fonts_for(&LocaleId)`, `resolve_asset(key)`, `resolve_asset_for(&LocaleId, key)` | Fallback-resolved font list and asset key. |
| `pseudo_localize(text, &PseudoOptions)`, `Localizer::set_pseudo` | Pseudo-localization. |
| `LocalizationSettings` | Serde descriptor with `validate()`, `asset_map()` and `build_localizer(loader)`. |
| `LocaleError` | Typed errors; `code()` gives the stable `locale.*` string. |

Keys are `message` or `message.attribute`. Both parts must be Fluent identifiers (an ASCII letter
then letters, digits, `-`, `_`), at most 256 bytes. Terms (`-brand`) are not addressable from
outside; use them inside messages. `format` formats the value of a message; a message that only
has attributes is addressed by its attributes.

Dates have no built-in type: Fluent's `DATETIME` is not implemented by `fluent-bundle`, so pass a
pre-formatted string argument (or a number when only plural selection is needed).

## Authoring `.ftl`

```ftl
# Comments start with #.
-game-name = Rust Foundry                      # a term: reusable, referenced as { -game-name }

welcome = Welcome to { -game-name }, { $player }!
    .tooltip = Opens the { -game-name } hub     # an attribute: key "welcome.tooltip"

# Plurals: the selector variable must be passed as a number.
items = { $count ->
    [0] No items
    [one] One item
   *[other] { $count } items
}

# Gender (or any other discrete choice): pass the variable as text.
party-left = { $gender ->
    [male] He left the party.
    [female] She left the party.
   *[other] They left the party.
}

# Rich-text tags are plain text to Fluent and pass through to engine-text.
warning = [color=#ff4040][b]Danger![/b][/color] { $name } is low on health.
```

Rules worth knowing:

- Plural selectors use CLDR categories (`zero one two few many other`) for the catalog's locale
  language, through Fluent's `intl_pluralrules`. A numeric variant key (`[0]`) wins over the
  category. Always give a default variant (`*[other]`).
- Languages need their own categories: Russian `21` is `one`, Arabic `0` is `zero`, French `1.5` is
  `one`, Japanese is always `other`. Define the variants each language needs.
- Each locale is a separate file/catalog; translators never edit the default locale's file.
- Placeable values are not wrapped in Unicode isolation marks by default
  (`LocalizerConfig::isolating = false`) because game text renderers draw them as boxes.
- A syntax error anywhere in a source rejects the whole source with `locale.parse` (line and
  detail of the first error); nothing partial is loaded.
- `.ftl` messages with duplicate ids (inside one source or across sources of one locale) are
  `locale.duplicate_message`. Use `replace_ftl` to swap a locale's content.

## Fallback algorithm

For a requested locale `L` and default `D` the chain is: `L`, then `L` without variants, without
region, without script (ending at the bare language), then `D` and its truncations; duplicates are
removed. `fr-CA` with default `en` gives `fr-CA`, `fr`, `en`; `zh-Hant-TW` gives `zh-Hant-TW`,
`zh-Hant`, `zh`, `en`.

`format(key)` walks the chain and returns the first catalog that defines the key (a message with a
value, or the attribute). Outcomes:

| Outcome | Result | Diagnostic (`locale.diag.*`) |
| --- | --- | --- |
| Defined in the current locale | its text | none |
| Defined in a fallback | fallback text | `fell_back` (detail = answering locale) |
| Defined nowhere | marker `{key}` (configurable prefix/suffix) | `missing_message` |
| Fluent error (for example a missing `$variable`) | Fluent's partial text | `format_error` |
| Output above `max_output_bytes` | marker | `output_too_large` |

Diagnostics are deduplicated by (kind, requested locale, key) with an occurrence `count`, capped at
`max_diagnostics`; further distinct entries only increment `diagnostics_dropped()`.

`set_locale` fails with `locale.unavailable` when no locale in the chain has a catalog, so the UI
cannot switch into a language with no strings. `Localizer::negotiate` and `negotiate()` map a
preference list (for example the OS languages) onto loaded locales: exact or truncated match first,
then any locale with the same language (`pt` -> `pt-BR`), else the default.

## Runtime language switch

`generation()` is bumped by `set_locale` (when the locale changes), loading, replacing or
removing messages, `set_pseudo` and `set_assets`. A consumer caches the generation it resolved at
and re-resolves its strings, fonts and assets when it differs; nothing reloads and no catalog is
rebuilt. `Resolved.generation` carries the value the answer belongs to.

## Fonts and assets per locale

`LocaleAssets { fonts: Vec<String>, asset_overrides: BTreeMap<String, String> }` per locale, held
in a `LocaleAssetMap`. Fonts are plain family names so the text engine can consume them later
(`FallbackConfig::with_language(locale, fonts)`). Lookups walk the fallback chain: fonts come from
the first entry with a non-empty list; an asset key is replaced by the first entry that overrides
it, else returned unchanged. The map is data only: it loads nothing.

## Pseudo-localization

`pseudo_localize(text, &PseudoOptions)` (and `Localizer::set_pseudo` with `enabled: true`, which
applies it to every formatted result) helps find truncation, hard-coded strings and font gaps:

- **Accents**: ASCII letters map to accented look-alikes (`Hello` -> `Ĥéļļó`).
- **Expansion**: appends `ceil(translatable_chars * expansion_percent / 100)` `~` characters
  (default 35, range 0..=300) after the last text, so the translatable length grows by exactly that
  amount (35% of 59 chars -> +21 chars). Tags and placeables do not count.
- **Markers**: `prefix`/`suffix`, default `«` and `»`, make concatenated or clipped strings visible.
  `[` and `]` are deliberately not used because they are rich-text markup delimiters.
- **RTL mirror** (`rtl: true`): wraps the whole result in U+202E (RLO) ... U+202C (PDF), which forces
  right-to-left display of every character so layouts can be checked for mirroring without Arabic
  or Hebrew text. This is an override, not true bidirectional text; use a real RTL catalog (`ar`,
  `he`) for shaping tests. `LocaleId::is_rtl()` reports RTL languages and scripts.
- **Preserved verbatim**: Fluent placeables `{ $var }`, `{ -term }`, selectors, nested placeables and
  quoted strings (balanced, string-aware); engine-text markup tags (`[b]`, `[/b]`,
  `[color=#ff0000]`, `[img=key width=8 height=8]`: `[` or `[/` followed by an ASCII letter, closed
  within 512 bytes); and the escapes `\[`, `\]`, `\\`. An unbalanced `{` is ordinary text. See
  `docs/text.md` (Rich-text markup).

`format` applies pseudo-localization to the final text, so placeable values appear in it as
already substituted; run `pseudo_localize` on raw `.ftl` values when you need the placeables
themselves preserved. Missing-key markers are never pseudo-localized.

## Settings shape

`LocalizationSettings` is the authored descriptor (serde, every field defaulted) intended to become
a component or project setting:

```json
{
  "default_locale": "en",
  "available_locales": ["en", "fr", "ja"],
  "resources": [
    { "locale": "en", "paths": ["strings/en/ui.ftl", "strings/en/items.ftl"] },
    { "locale": "fr", "paths": ["strings/fr/ui.ftl"] }
  ],
  "pseudo": { "enabled": false, "accents": true, "expansion_percent": 35,
              "prefix": "«", "suffix": "»", "rtl": false },
  "assets": [
    { "locale": "ja", "fonts": ["Noto Sans JP"], "asset_overrides": { "ui/logo": "ui/logo_ja" } }
  ]
}
```

`validate()` returns the first problem: `locale.invalid_id` (unparsable locale),
`locale.duplicate_locale` (repeats in `available_locales`, `resources` or `assets`, compared in
canonical form so `FR` equals `fr`), `locale.default_unavailable`, `locale.unknown_locale` (resources
or assets for a locale that is not available), `locale.empty_path`, `locale.bad_path` (absolute,
drive-qualified, `..` or a path listed twice), `locale.invalid_pseudo`, `locale.invalid_asset`
(empty or oversized font or override) and `locale.settings_too_large`. `build_localizer(loader)`
validates, reads each path through the caller's `loader` (`locale.load_failed` on error), loads it,
applies pseudo options and the asset map and leaves the current locale at the default.

## Project integration

### Assets

| Kind | File | Limits | Purpose |
| --- | --- | --- | --- |
| `string_table` | Project Fluent `.ftl`, UTF-8 | 1 MiB per file | One file of strings. A locale may use several tables; message ids are unique per locale across them. |
| `locale_config` | JSON, `schema: 1` | 256 KiB, at most one per project | Default locale, available locales, which tables belong to which locale, pseudo-localization, per-locale fonts and asset overrides. |

```json
{
  "assets": [
    {"id": "l10n", "kind": "locale_config", "path": "localization.json"},
    {"id": "strings-en", "kind": "string_table", "path": "strings/en/ui.ftl"},
    {"id": "strings-fr", "kind": "string_table", "path": "strings/fr/ui.ftl"},
    {"id": "strings-ja", "kind": "string_table", "path": "strings/ja/ui.ftl"}
  ]
}
```

`localization.json` is the [settings shape](#settings-shape) plus an optional `"schema": 1`
(other schema numbers and unknown fields are rejected, unlike a bare `LocalizationSettings`):

```json
{
  "schema": 1,
  "default_locale": "en",
  "available_locales": ["en", "fr", "ja"],
  "resources": [
    {"locale": "en", "paths": ["strings/en/ui.ftl"]},
    {"locale": "fr", "paths": ["strings/fr/ui.ftl"]},
    {"locale": "ja", "paths": ["strings/ja/ui.ftl"]}
  ],
  "pseudo": {"enabled": false, "expansion_percent": 35},
  "assets": [
    {"locale": "ja", "fonts": ["jp-sans"], "asset_overrides": {"logo": "logo-ja"}}
  ]
}
```

`Project::load` and `Project::validate` check, in addition to `LocalizationSettings::validate`:

| Code | Meaning |
| --- | --- |
| `asset.string_table.extension` | A `string_table` that is not a `.ftl` file. |
| `locale.parse`, `locale.invalid_utf8`, `locale.resource_too_large`, `locale.too_deep`, `locale.too_many_messages`, `locale.duplicate_message` | The table (or, for duplicates, a locale's tables together) does not load; the diagnostic names the file. |
| `asset.string_table.no_config` | `string_table` assets exist but the project has no `locale_config`. |
| `asset.string_table.unlisted` | A `string_table` that no `resources` entry lists. |
| `asset.locale_config.invalid` | Not JSON, unknown field, or `schema` other than 1. |
| `asset.locale_config.duplicate` | More than one `locale_config`. |
| `asset.locale_config.unknown_table` | A `resources` path that is not a `string_table` asset. |
| `asset.locale_config.no_strings` | An available locale with no table. |
| `asset.locale_config.unknown_font` | A locale font that is not a `font_face` asset ID or `font_config` family. |
| `asset.locale_config.unknown_asset` | An override whose key or replacement is neither an asset ID nor a font family. |

`asset_list` and `asset_info` know both kinds (`asset_info` shows entry counts and the locales
a table belongs to, or the config's locales).

### Text references

Authored UI text may name a message instead of holding text. **A text that is exactly
`@message` or `@message.attribute` is a message reference**; the whole string must be the key
(`message` is a Fluent identifier: an ASCII letter, then letters, digits, `-` or `_`).

| Authored text | Shown |
| --- | --- |
| `@greeting` | the current locale's `greeting` (`{greeting}` if no locale defines it) |
| `@welcome.tooltip` | the `tooltip` attribute of `welcome` |
| `@@greeting` | the literal text `@greeting` (a leading `@@` loses one `@`) |
| `@ greeting`, `Hello @name`, `@bob!`, `@1x`, `@a.b.c` | unchanged: not a key |

Everything else needs no escaping, so existing projects keep their text. Resolution only runs
once localization is *active* (a locale catalog is loaded); in a project without a
`locale_config`, `@x` and `@@x` stay as authored.

Slots that accept references (`engine.UiRoot`): `label.text`, `button.text`, `toggle.text`,
`rich_label.markup`, `text_input.placeholder`, `dropdown.options[]` and `.placeholder`,
`tabs.labels[]`, `modal_dialog.title` and `.buttons[]`, every node's `tooltip.text`, and every node's `accessibility.label` and `.description`.
`engine.WorldText.text` accepts one too. Text values (a text input's `text`) are never
references. Translations may contain rich-text markup: `[b]Bold[/b]` survives translation and
pseudo-localization; a translation whose markup the text engine rejects, or that the length
limits cut inside a tag, is drawn literally (brackets escaped) instead of failing the frame.
`accessibility.label` and `.description` accept references too, so the screen-reader name and
description follow the locale.

**Resolution happens on the effective root, not in the component.** Layout, interaction and
drawing read a per-tick copy of the `UiRoot` in which references are replaced; the stored
component, snapshots and `component_set` keep the key, and widget state edited by the player
(checked, selection, typed text, scroll) is written back without the translation. A root with
no references and no asset overrides costs no copy. Limits cannot be broken by a translation:
each expansion is cut to its slot (4096 bytes for text, 256 for short labels, 1024 for
tooltips) and to the remaining part of the root's 64 KiB text budget.

Nodes whose text a `text` or `itemText` data binding drives are left to the binding, so data
such as a player named `@greeting` stays literal.

`ui_tree::localize_root(&root, &localization)` returns that localized view (`None` when nothing
changes); layers that mirror the visible UI, such as an accessibility tree, should read their
text from it.

**Decision record.** A reference is the *whole string* with a leading `@` because that needs no
new field on any widget (so authored JSON, the descriptor schema and `component_set` stay
unchanged), cannot appear by accident in ordinary prose, and has a one-character escape.
Alternatives rejected: a separate `textKey` on every widget and on tooltips (a new field in
every variant, in the schema and in builders, and a second source of truth next to `text`);
`$key` (already the theme-token sigil in [UI styling](ui-styling.md)); inline `{key}` templates
inside longer strings (translators would lose control of word order, and markup and pseudo
localization would have to see through them).

### Data bindings with message keys

`UiBinding.message` (see [UI styling](ui-styling.md)) resolves through the runtime's
`UiStringResolver`, which formats the key with the binding's `args` read from the data
context: numbers become numeric variables (they drive plural selectors), strings text
variables, booleans the text `true`/`false`; null and structured values are omitted.

```json
{"node": "count", "property": "text", "path": "inventory.count",
 "message": "items", "args": {"count": "inventory.count"}}
```

With `items = { $count -> [one] { $count } item *[other] { $count } items }` this shows
`1 item` and `5 items` in English and, with Russian's `one/few/many/other` variants,
`1 предмет`, `2 предмета` and `5 предметов`. A message key that is not defined shows the
marker (`{key}`) and is counted; while localization is inactive the resolver returns `None`
and the binding falls back to its `format` or value as before.

### Runtime language switch

```rust
let mut runtime = Runtime::new(project)?;
runtime.set_locale("fr")?;                 // no reload; `locale.invalid_id` on a bad identifier
runtime.set_pseudo_localization(true)?;    // toggle pseudo-localization
let l10n = runtime.localization();         // shared `Localization` handle (also an ECS resource)
l10n.load_ftl("de", "hi = Hallo\n")?;      // natives may add strings without a locale_config
l10n.stats();                              // resolutions, cache hits, generation, missing messages
l10n.diagnostics();                        // engine_locale diagnostics (missing, fell back, ...)
```

Only a *generation* change re-resolves anything. `Localization` caches each static `@key` text
for the current generation (so N labels using one key cost one Fluent `format` per switch), and
the UI re-renders message bindings once when it sees a new generation. In a tick without a
change the runtime does one integer comparison and each reference is a cache lookup.

A switch (also one made through the resource, from a script or system) is applied on the next
tick:

- **Text** re-resolves everywhere (UI, tooltips, world text).
- **Fonts.** The current locale's `fonts` (resolved along its fallback chain, `fr-CA` then `fr`)
  are put in front of the project's `font_config` `fallback.byLanguage` entry for that locale on
  the shaping text engine (`EngineTextBackend::set_fallback`), and `set_language` receives the
  locale, so glyphs the primary family lacks (Japanese, Arabic, ...) come from the locale's
  families. They are `font_face` asset IDs or `font_config` families.
- **Assets.** `asset_overrides` map an asset key to another for the locale (fallback chain
  applied): `image.texture`, `nine_slice.texture`, a node's `style.font` and
  `engine.WorldText.font`. The replacement must exist (`asset.locale_config.unknown_asset`) and,
  like the keys it replaces, be at most 128 bytes (longer ones are `asset.locale_config.invalid`;
  settings built in code that skip that check have the override ignored where it is used). The
  stored key stays authored.
- **Event.** `locale.changed` is queued with `{"locale", "previous", "generation"}` whenever the
  locale itself changes (not for pseudo toggles, reloads or re-selecting the same locale).
- **Telemetry.** `RuntimeSnapshot.localization` and the `localization` member of
  `project_run_headless` reports carry the locale, loaded locales, generation, resolution count
  and missing-message count (absent while inactive).

A locale with no catalog of its own is accepted when its fallback chain reaches one (`xx-YY`
resolves through the default locale); only identifiers that do not parse, and projects with no
catalog at all, fail.

### MCP tools

| Tool | Access | Purpose |
| --- | --- | --- |
| `locale_list` | read | The config, default locale and per locale: string_table files (with `hash` for `expected_hash`), key count, coverage, missing and extra keys (`max_missing` bounds the lists; counts are exact). |
| `locale_messages` | read | A locale's entries with exact Fluent source, file and line, paged; `prefix` filters, `key` adds a formatted `preview` using `args` (with fallback information). |
| `locale_pseudo_preview` | read | Pseudo-localize raw `text` or a formatted `key`; `options` override the configured accents, expansion, markers and RTL mirror. |
| `locale_set_message` | write | Add or replace one message or term from one complete Fluent entry (`ftl`), in the defining file or `file`. |
| `locale_remove_message` | write | Remove one message or term and its attached `#` comment. |
| `locale_add_locale` | write | Create a new `.ftl` file (optionally seeded from another locale), register the `string_table` asset and add the locale to the config; creates `localization.json` when the project has none. |

The writes edit `.ftl` text line by line: everything outside the replaced or removed entry stays
byte for byte, including comments and line endings. Before touching disk the whole locale is
parsed with the real Fluent parser, so invalid syntax (`locale.parse`), duplicate ids across the
locale's files (`locale.duplicate_message`), nesting limits and the 1 MiB size cap are rejected
with the file unchanged. Files are confined to the project and written atomically; the project
is reloaded afterwards and the previous bytes restored if it no longer validates.
`locale_add_locale` also appends the asset to the `assets` array of `project.json` without
reformatting it, rewrites the config in canonical order and restores all three files on
failure. See [MCP](mcp.md#localization-tools).

### Not covered

Number, date and currency formatting beyond Fluent's `NUMBER()`, locale-aware sorting, and
per-locale replacement of the *primary* font family (use an `asset_overrides` entry on
`style.font`).

## Limits and errors

| Limit (`LocaleLimits`) | Default | Error |
| --- | --- | --- |
| `max_resource_bytes` | 1 MiB | `locale.resource_too_large` |
| `max_messages_per_locale` (messages + terms) | 10 000 | `locale.too_many_messages` |
| `max_locales` | 64 | `locale.too_many_locales` |
| `max_placeable_depth` (nested `{` and call `(`) | 8 | `locale.too_deep` |
| `max_key_bytes` | 256 | `locale.invalid_key` |
| `max_output_bytes` | 64 KiB | `locale.output_too_large` |
| `max_arg_bytes` (longer strings are cut at a char boundary) | 16 KiB | none (truncated) |
| `max_diagnostics` | 256 | none (`diagnostics_dropped`) |

Other codes: `locale.invalid_utf8`, `locale.parse`, `locale.duplicate_message`,
`locale.unknown_locale`, `locale.unavailable`, `locale.missing_message`, `locale.format_failed`;
the project integration adds `locale.ftl.shape` (an MCP edit that is not exactly one message or
term) and the `asset.string_table.*` / `asset.locale_config.*` codes listed above.
Settings limits: 64 locales, 64 paths per locale, 32 fonts per locale, 1024 overrides per locale,
512 bytes per path/font/key. Nothing panics on hostile input: the nesting scan runs before the
recursive Fluent parser, Fluent caps placeables per message (cycles are reported as errors), and
numbers that are not finite are formatted as text.

## Decision record

- **Fluent over alternatives.** Project Fluent (`fluent-bundle`) is pure Rust, permissively
  licensed, and gives plurals via CLDR rules, gender or any selector, terms, attributes and
  asymmetric translation. Alternatives considered: `gettext`-style catalogs (no first-class
  selectors, plural forms as C expressions), ICU4X `icu_message_format`-style crates (not stable,
  heavier dependency tree), and a hand-written `key=value` table (no plurals). No materially better
  pure-Rust option was found.
- **Concurrent bundle.** `fluent_bundle::concurrent::FluentBundle` is used so `Localizer` is
  `Send + Sync` (the default bundle's memoizer is not).
- **Own negotiation and fallback.** `fluent-langneg` is pulled in by `fluent-bundle` but the
  negotiation here is a few lines over `unic-langid` and follows the chain rules above, so the
  behaviour is documented and tested in this crate.
- **No built-in dates.** `fluent-bundle` has no `DATETIME`; adding `intl` date formatting would pull
  ICU data. Pass formatted strings.
- **Strict sources.** A source with any syntax error is rejected whole, so authoring mistakes are
  visible instead of silently dropping messages.
- **No file I/O.** Loading from disk or project content is the integrator's job, which keeps the
  crate usable in tests, tools and headless runs.

Dependencies (crates.io, exact pins):

| Crate | Version | Licence | Reason |
| --- | --- | --- | --- |
| `fluent-bundle` | `=0.16.0` | Apache-2.0 OR MIT | Fluent runtime: messages, selectors, CLDR plurals (via `intl_pluralrules`) |
| `fluent-syntax` | `=0.12.0` | Apache-2.0 OR MIT | Fluent AST, used to enumerate message ids and attributes |
| `unic-langid` | `=0.9.6` | MIT OR Apache-2.0 | Locale identifier parsing and canonicalisation |
| `serde` | `=1.0.229` | MIT OR Apache-2.0 | Settings descriptor |
| `serde_json` (dev) | `=1.0.154` | MIT OR Apache-2.0 | Descriptor round-trip tests |

Transitively added to `Cargo.lock`: `fluent-langneg`, `intl-memoizer`, `intl_pluralrules`,
`tinystr`, `unic-langid-impl`, `zerofrom`, `zerovec` (all permissive).

# Localization (`engine-locale`)

`engine-locale` is a standalone, pure-Rust localization library: Project Fluent string tables
with plural and gender selection, locale parsing, fallback and negotiation, a runtime language
switch, missing-translation reports, per-locale font/asset mapping, pseudo-localization and a
serde descriptor for authored settings. It depends on no engine crate, no GPU or windowing
library and does no file I/O (callers pass `.ftl` text or bytes).

Status: standalone library (UI parity phase A'). It is **not yet wired** into `ui_tree`,
`engine-project` content, MCP or the desktop runner; see [Not yet integrated](#not-yet-integrated).

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
`locale.unknown_locale`, `locale.unavailable`, `locale.missing_message`, `locale.format_failed`.
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

## Not yet integrated

This crate is a library only. Still to do in later phases:

- **UI resolution**: `ui_tree` labels referencing message keys (`$key`), re-resolving on a
  `generation()` change, passing arguments from data binding.
- **MCP tools**: string-table management (list keys/locales, set/replace a locale's `.ftl`,
  missing-translation reports, switching language) built on the introspection API above.
- **Per-locale fonts in the UI**: feeding `Localizer::fonts()` into `engine-text`'s
  `FallbackConfig::with_language`, and applying `resolve_asset` to image/font asset keys.
- **Project content**: reading `.ftl` files from project assets, the authored
  `LocalizationSettings` as a component or project setting and its schema descriptor.
- Number/date/currency formatting beyond Fluent's `NUMBER()` and locale-aware sorting.

use engine_locale::*;

const PLURAL_FTL: &str = "\
cat = { $n ->
    [zero] zero
    [one] one
    [two] two
    [few] few
    [many] many
   *[other] other
}
";

fn plural_category(locale: &str, n: f64) -> String {
    let mut l = Localizer::default();
    l.load_ftl(locale, PLURAL_FTL).unwrap();
    l.set_locale(locale).unwrap();
    l.try_format("cat", &LocaleArgs::new().number("n", n))
        .unwrap()
}

fn categories(locale: &str) -> Vec<String> {
    [0.0, 1.0, 2.0, 5.0, 21.0, 1.5]
        .iter()
        .map(|n| plural_category(locale, *n))
        .collect()
}

#[test]
fn plural_categories_follow_cldr() {
    // counts: 0, 1, 2, 5, 21, 1.5
    assert_eq!(
        categories("en"),
        ["other", "one", "other", "other", "other", "other"]
    );
    assert_eq!(
        categories("fr"),
        ["one", "one", "other", "other", "other", "one"]
    );
    assert_eq!(
        categories("ru"),
        ["many", "one", "few", "many", "one", "other"]
    );
    assert_eq!(
        categories("ar"),
        ["zero", "one", "two", "few", "many", "other"]
    );
    assert_eq!(categories("ja"), ["other"; 6]);
}

#[test]
fn exact_number_variant_wins_over_category() {
    let mut l = Localizer::default();
    l.load_ftl(
        "en",
        "n = { $n ->\n    [0] none\n    [one] one item\n   *[other] { $n } items\n}\n",
    )
    .unwrap();
    let f = |n: f64| l.format("n", &LocaleArgs::new().number("n", n));
    assert_eq!(f(0.0), "none");
    assert_eq!(f(1.0), "one item");
    assert_eq!(f(5.0), "5 items");
}

#[test]
fn gender_selector_terms_attributes_and_variables() {
    let mut l = Localizer::default();
    l.load_ftl(
        "en",
        "\
-brand = Foundry
welcome = Hello, { $name }! Welcome to { -brand }.
    .title = About { -brand }
pronoun = { $gender ->
    [male] He
    [female] She
   *[other] They
} left.
",
    )
    .unwrap();
    let f = |g: &str| l.format("pronoun", &LocaleArgs::new().text("gender", g));
    assert_eq!(f("male"), "He left.");
    assert_eq!(f("female"), "She left.");
    assert_eq!(f("nonbinary"), "They left.");
    let args = LocaleArgs::new().text("name", "Ada");
    assert_eq!(
        l.format("welcome", &args),
        "Hello, Ada! Welcome to Foundry."
    );
    assert_eq!(l.format("welcome.title", &args), "About Foundry");
    assert_eq!(
        l.format_attribute("welcome", "title", &args),
        "About Foundry"
    );
}

fn fr_localizer() -> Localizer {
    let mut l = Localizer::default();
    l.load_ftl("en", "hello = Hello\nonly-en = English only\nbye = Bye\n")
        .unwrap();
    l.load_ftl("fr", "hello = Bonjour\nbye = Au revoir\n")
        .unwrap();
    l.load_ftl("fr-CA", "hello = Allo\n").unwrap();
    l
}

#[test]
fn fallback_chain_and_missing_marker_with_diagnostics() {
    let chain: Vec<String> = LocaleId::parse("fr-CA")
        .unwrap()
        .fallback_chain(&LocaleId::parse("en").unwrap())
        .iter()
        .map(|l| l.to_string())
        .collect();
    assert_eq!(chain, ["fr-CA", "fr", "en"]);

    let mut l = fr_localizer();
    l.set_locale("fr-CA").unwrap();
    let none = LocaleArgs::new();
    assert_eq!(l.format("hello", &none), "Allo"); // fr-CA
    assert_eq!(l.format("bye", &none), "Au revoir"); // fr
    assert_eq!(l.format("only-en", &none), "English only"); // en
    assert_eq!(l.format("nowhere", &none), "{nowhere}"); // marker
    assert_eq!(l.resolve("bye", &none).locale.as_str(), "fr");

    let diags = l.diagnostics();
    let find = |kind, key: &str| diags.iter().find(|d| d.kind == kind && d.key == key);
    assert_eq!(find(DiagnosticKind::FellBack, "bye").unwrap().detail, "fr");
    assert_eq!(
        find(DiagnosticKind::FellBack, "only-en").unwrap().detail,
        "en"
    );
    let missing = find(DiagnosticKind::MissingMessage, "nowhere").unwrap();
    assert_eq!(missing.locale, "fr-CA");
    assert_eq!(missing.kind.code(), "locale.diag.missing_message");
    assert!(find(DiagnosticKind::FellBack, "hello").is_none());
    // Repeats are counted, not duplicated.
    l.format("nowhere", &none);
    assert_eq!(
        l.diagnostics()
            .iter()
            .filter(|d| d.key == "nowhere")
            .count(),
        1
    );
    assert_eq!(
        l.take_diagnostics()
            .iter()
            .find(|d| d.key == "nowhere")
            .unwrap()
            .count,
        2
    );
    assert!(l.diagnostics().is_empty());

    let err = l.try_format("nowhere", &none).unwrap_err();
    assert_eq!(err.code(), "locale.missing_message");
}

#[test]
fn custom_marker_and_bad_keys() {
    let l = Localizer::new(LocalizerConfig {
        missing_prefix: "<<".into(),
        missing_suffix: ">>".into(),
        ..LocalizerConfig::default()
    });
    assert_eq!(l.format("x", &LocaleArgs::new()), "<<x>>");
    assert_eq!(
        l.try_format("bad key!", &LocaleArgs::new())
            .unwrap_err()
            .code(),
        "locale.invalid_key"
    );
    assert_eq!(l.format("bad key!", &LocaleArgs::new()), "<<bad key!>>");
}

#[test]
fn runtime_switch_changes_output_and_generation() {
    let mut l = fr_localizer();
    let none = LocaleArgs::new();
    let g0 = l.generation();
    assert_eq!(l.format("hello", &none), "Hello");
    let g1 = l.set_locale("fr").unwrap();
    assert!(g1 > g0);
    assert_eq!(l.generation(), g1);
    assert_eq!(l.format("hello", &none), "Bonjour");
    let resolved = l.resolve("hello", &none);
    assert_eq!(resolved.generation, g1);
    // Same locale again: no bump.
    assert_eq!(l.set_locale("fr").unwrap(), g1);
    // A locale with no catalog anywhere in its chain is refused and nothing changes.
    let mut empty = Localizer::default();
    assert_eq!(
        empty.set_locale("fr").unwrap_err().code(),
        "locale.unavailable"
    );
    assert_eq!(empty.generation(), 0);
    // Loading messages also bumps the generation so UI re-resolves.
    l.load_ftl("fr", "extra = Plus\n").unwrap();
    assert!(l.generation() > g1);
    assert_eq!(l.format("extra", &none), "Plus");
}

#[test]
fn keys_and_missing_reports() {
    let mut l = Localizer::default();
    l.load_ftl("en", "a = A\nb = B\n    .tip = Tip\nc = C\n")
        .unwrap();
    l.load_ftl("fr", "a = A fr\nz = Z fr\n").unwrap();
    assert_eq!(l.keys("en").unwrap(), ["a", "b", "b.tip", "c"]);
    assert_eq!(l.all_keys(), ["a", "b", "b.tip", "c", "z"]);
    assert_eq!(l.keys("de").unwrap_err().code(), "locale.unknown_locale");
    let r = l.missing_report("fr").unwrap();
    assert_eq!((r.total, r.translated), (4, 1));
    assert_eq!(r.missing, ["b", "b.tip", "c"]);
    assert_eq!(r.extra, ["z"]);
    assert_eq!(l.missing_reports().len(), 1);
    assert!(l.contains("b.tip"));
    assert!(!l.contains("nope"));
}

#[test]
fn negotiation() {
    let id = |s: &str| LocaleId::parse(s).unwrap();
    let avail = [id("en"), id("fr"), id("pt-BR")];
    assert_eq!(negotiate(&[id("fr-CA")], &avail, &id("en")), id("fr"));
    assert_eq!(negotiate(&[id("pt")], &avail, &id("en")), id("pt-BR"));
    assert_eq!(
        negotiate(&[id("de"), id("fr")], &avail, &id("en")),
        id("fr")
    );
    assert_eq!(negotiate(&[id("ja")], &avail, &id("en")), id("en"));
    assert_eq!(id("fr_ca").as_str(), "fr-CA");
    assert_eq!(id("zh-Hant-TW").truncations().len(), 3);
    assert!(id("ar-EG").is_rtl() && !id("en").is_rtl());
    let mut l = fr_localizer();
    assert_eq!(l.negotiate(&[id("fr-BE")]), id("fr"));
    l.set_locale_id(l.negotiate(&[id("fr-CA")])).unwrap();
    assert_eq!(l.locale().as_str(), "fr-CA");
}

#[test]
fn fonts_and_asset_overrides_resolve_through_fallback() {
    let id = |s: &str| LocaleId::parse(s).unwrap();
    let mut map = LocaleAssetMap::new();
    map.insert(
        id("ja"),
        LocaleAssets {
            fonts: vec!["Noto Sans JP".into(), "Noto Emoji".into()],
            asset_overrides: [("ui/logo".to_string(), "ui/logo_ja".to_string())].into(),
        },
    );
    map.insert(
        id("en"),
        LocaleAssets {
            fonts: vec!["Inter".into()],
            ..LocaleAssets::default()
        },
    );
    let mut l = fr_localizer();
    l.load_ftl("ja", "hello = konnichiwa\n").unwrap();
    l.load_ftl("ja-JP", "x = y\n").unwrap();
    l.set_assets(map);
    assert_eq!(l.fonts(), ["Inter"]);
    assert_eq!(l.fonts_for(&id("ja-JP")), ["Noto Sans JP", "Noto Emoji"]);
    assert_eq!(l.resolve_asset_for(&id("ja-JP"), "ui/logo"), "ui/logo_ja");
    assert_eq!(l.resolve_asset_for(&id("ja"), "ui/other"), "ui/other");
    l.set_locale("ja-JP").unwrap();
    assert_eq!(l.resolve_asset("ui/logo"), "ui/logo_ja");
    assert_eq!(l.fonts(), ["Noto Sans JP", "Noto Emoji"]);
}

// ---- pseudo-localization ------------------------------------------------------------------

fn plain_pseudo(percent: u32) -> PseudoOptions {
    PseudoOptions {
        enabled: true,
        expansion_percent: percent,
        prefix: String::new(),
        suffix: String::new(),
        ..PseudoOptions::default()
    }
}

#[test]
fn pseudo_expands_within_ratio() {
    let text = "The quick brown fox jumps over the lazy dog again and again";
    let n = text.chars().count();
    for percent in [0u32, 30, 35, 40, 100] {
        let out = pseudo_localize(text, &plain_pseudo(percent));
        let got = out.chars().count();
        let want = n + (n * percent as usize).div_ceil(100);
        assert_eq!(got, want, "percent {percent}");
    }
    let out = pseudo_localize(text, &plain_pseudo(35));
    let ratio = out.chars().count() as f64 / n as f64;
    assert!((1.30..=1.40).contains(&ratio), "ratio {ratio}");
    assert!(out.starts_with("Ţĥé ǫúíçķ"));
}

#[test]
fn pseudo_markers_and_rtl() {
    let out = pseudo_localize("Hi", &PseudoOptions::default());
    assert!(
        out.starts_with('\u{AB}') && out.ends_with('\u{BB}'),
        "{out}"
    );
    let rtl = pseudo_localize(
        "Hi",
        &PseudoOptions {
            rtl: true,
            ..PseudoOptions::default()
        },
    );
    assert!(rtl.starts_with('\u{202E}') && rtl.ends_with('\u{202C}'));
    let no_accents = pseudo_localize(
        "Hi",
        &PseudoOptions {
            accents: false,
            expansion_percent: 0,
            ..plain_pseudo(0)
        },
    );
    assert_eq!(no_accents, "Hi");
}

#[test]
fn pseudo_preserves_placeables_and_markup_tags() {
    let src = "Hello { $user }, you have { $count -> [one] one item *[other] { $count } items } [b]now[/b] \
               [color=#ff0000]red[/color] [img=coin width=8 height=8] \\[literal\\] { \"a } b\" }";
    let out = pseudo_localize(src, &plain_pseudo(35));
    for kept in [
        "{ $user }",
        "{ $count -> [one] one item *[other] { $count } items }",
        "[b]",
        "[/b]",
        "[color=#ff0000]",
        "[/color]",
        "[img=coin width=8 height=8]",
        "\\[",
        "\\]",
        "{ \"a } b\" }",
    ] {
        assert!(out.contains(kept), "lost {kept:?} in {out:?}");
    }
    assert!(!out.contains("Hello") && out.contains("Ĥéļļó"));
    // An unbalanced brace is plain text, not a swallowed tail.
    assert!(pseudo_localize("a { b", &plain_pseudo(0)).starts_with('á'));
}

#[test]
fn localizer_applies_pseudo_to_output_and_keeps_tags() {
    let mut l = Localizer::default();
    l.load_ftl("en", "greet = [b]Hi[/b] { $name }\n").unwrap();
    let g = l.generation();
    l.set_pseudo(plain_pseudo(35)).unwrap();
    assert!(l.generation() > g);
    let out = l.format("greet", &LocaleArgs::new().text("name", "Ada"));
    assert!(out.starts_with("[b]Ĥí[/b] Áďá"), "{out}");
    assert_eq!(l.format("missing", &LocaleArgs::new()), "{missing}");
    assert_eq!(
        l.set_pseudo(PseudoOptions {
            expansion_percent: 9999,
            ..PseudoOptions::default()
        })
        .unwrap_err()
        .code(),
        "locale.invalid_pseudo"
    );
}

// ---- hostile input ------------------------------------------------------------------------

#[test]
fn hostile_resources_return_errors() {
    let mut l = Localizer::new(LocalizerConfig {
        limits: LocaleLimits {
            max_resource_bytes: 65536,
            max_messages_per_locale: 10,
            max_locales: 2,
            ..LocaleLimits::default()
        },
        ..LocalizerConfig::default()
    });
    let code = |r: Result<usize, LocaleError>| r.unwrap_err().code();

    assert_eq!(
        code(l.load_ftl("en", &"a = x\n".repeat(20000))),
        "locale.resource_too_large"
    );
    assert_eq!(
        code(l.load_ftl_bytes("en", &[0x61, 0x20, 0xFF, 0xFE])),
        "locale.invalid_utf8"
    );
    assert_eq!(
        code(l.load_ftl("en", "this is not ftl\n  = broken {")),
        "locale.parse"
    );
    assert_eq!(
        code(l.load_ftl("not a locale", "a = b")),
        "locale.invalid_id"
    );
    // Deep placeable nesting is rejected before the recursive parser sees it.
    let deep = format!("a = {}x{}\n", "{".repeat(3000), "}".repeat(3000));
    assert_eq!(code(l.load_ftl("en", &deep)), "locale.too_deep");
    let deep_call = format!("a = {{ {}1{} }}\n", "NUMBER(".repeat(500), ")".repeat(500));
    assert_eq!(code(l.load_ftl("en", &deep_call)), "locale.too_deep");
    // Duplicates and message counts.
    l.load_ftl("en", "a = 1\n").unwrap();
    assert_eq!(
        code(l.load_ftl("en", "a = 2\n")),
        "locale.duplicate_message"
    );
    assert_eq!(
        code(l.load_ftl("en", "x = 1\nx = 2\n")),
        "locale.duplicate_message"
    );
    let many: String = (0..11).map(|i| format!("m{i} = v\n")).collect();
    assert_eq!(code(l.load_ftl("en", &many)), "locale.too_many_messages");
    // Failed loads changed nothing.
    assert_eq!(l.keys("en").unwrap(), ["a"]);
    // Locale count.
    l.load_ftl("fr", "a = 1\n").unwrap();
    assert_eq!(code(l.load_ftl("de", "a = 1\n")), "locale.too_many_locales");
}

#[test]
fn hostile_lookups_and_arguments_are_bounded() {
    let mut l = Localizer::new(LocalizerConfig {
        limits: LocaleLimits {
            max_arg_bytes: 16,
            max_output_bytes: 64,
            ..LocaleLimits::default()
        },
        ..LocalizerConfig::default()
    });
    l.load_ftl(
        "en",
        "echo = { $v }\nmissing-var = Hi { $nope }\ncycle = { cycle }\n",
    )
    .unwrap();
    let long = "é".repeat(100);
    let out = l.format("echo", &LocaleArgs::new().text("v", long));
    assert_eq!(out.chars().count(), 8); // 16 bytes, cut on a char boundary
    // Output limit: 5 echoes of 16 bytes would exceed 64 only with more; use a big key instead.
    let huge_key = "k".repeat(10_000);
    assert_eq!(
        l.try_format(&huge_key, &LocaleArgs::new())
            .unwrap_err()
            .code(),
        "locale.invalid_key"
    );
    assert_eq!(
        l.try_format("a.b.c", &LocaleArgs::new())
            .unwrap_err()
            .code(),
        "locale.invalid_key"
    );
    // Fluent errors (missing variable, reference cycle) surface through try_format, never panic.
    assert_eq!(
        l.try_format("missing-var", &LocaleArgs::new())
            .unwrap_err()
            .code(),
        "locale.format_failed"
    );
    assert_eq!(
        l.try_format("cycle", &LocaleArgs::new())
            .unwrap_err()
            .code(),
        "locale.format_failed"
    );
    assert!(
        l.diagnostics()
            .iter()
            .any(|d| d.kind == DiagnosticKind::FormatError)
    );
    // Non-finite numbers do not panic.
    let _ = l.format("echo", &LocaleArgs::new().number("v", f64::NAN));
}

#[test]
fn output_size_limit_is_enforced() {
    let mut l = Localizer::new(LocalizerConfig {
        limits: LocaleLimits {
            max_output_bytes: 32,
            ..LocaleLimits::default()
        },
        ..LocalizerConfig::default()
    });
    let long_value = "x".repeat(200);
    l.load_ftl("en", &format!("big = {long_value}\n")).unwrap();
    assert_eq!(
        l.try_format("big", &LocaleArgs::new()).unwrap_err().code(),
        "locale.output_too_large"
    );
    assert_eq!(l.format("big", &LocaleArgs::new()), "{big}");
    assert!(
        l.diagnostics()
            .iter()
            .any(|d| d.kind == DiagnosticKind::OutputTooLarge)
    );
}

#[test]
fn diagnostics_log_is_bounded() {
    let l = Localizer::new(LocalizerConfig {
        limits: LocaleLimits {
            max_diagnostics: 4,
            ..LocaleLimits::default()
        },
        ..LocalizerConfig::default()
    });
    for i in 0..50 {
        l.format(&format!("k{i}"), &LocaleArgs::new());
    }
    assert_eq!(l.diagnostics().len(), 4);
    assert_eq!(l.diagnostics_dropped(), 46);
}

// ---- settings -----------------------------------------------------------------------------

fn good_settings() -> LocalizationSettings {
    LocalizationSettings {
        default_locale: "en".into(),
        available_locales: vec!["en".into(), "fr".into(), "ja".into()],
        resources: vec![
            LocaleResources {
                locale: "en".into(),
                paths: vec!["strings/en.ftl".into()],
            },
            LocaleResources {
                locale: "fr".into(),
                paths: vec!["strings/fr.ftl".into()],
            },
        ],
        pseudo: PseudoOptions::default(),
        assets: vec![LocaleAssetEntry {
            locale: "ja".into(),
            fonts: vec!["Noto Sans JP".into()],
            asset_overrides: [("logo".to_string(), "logo_ja".to_string())].into(),
        }],
    }
}

#[test]
fn settings_validate_rejects_bad_input() {
    good_settings().validate().unwrap();
    let code = |f: &dyn Fn(&mut LocalizationSettings)| {
        let mut s = good_settings();
        f(&mut s);
        s.validate().unwrap_err().code()
    };
    assert_eq!(
        code(&|s| s.default_locale = "not a locale".into()),
        "locale.invalid_id"
    );
    assert_eq!(
        code(&|s| s.available_locales.push("fr_FR!".into())),
        "locale.invalid_id"
    );
    assert_eq!(
        code(&|s| s.available_locales.push("FR".into())),
        "locale.duplicate_locale"
    );
    assert_eq!(
        code(&|s| s.default_locale = "de".into()),
        "locale.default_unavailable"
    );
    assert_eq!(
        code(&|s| s.resources[0].paths.push("  ".into())),
        "locale.empty_path"
    );
    assert_eq!(
        code(&|s| s.resources[0].paths.push(String::new())),
        "locale.empty_path"
    );
    assert_eq!(
        code(&|s| s.resources[0].paths.push("strings/en.ftl".into())),
        "locale.bad_path"
    );
    assert_eq!(
        code(&|s| s.resources[1].paths.push("strings/en.ftl".into())),
        "locale.bad_path"
    );
    assert_eq!(
        code(&|s| s.resources[0].paths.push("../secret.ftl".into())),
        "locale.bad_path"
    );
    assert_eq!(
        code(&|s| s.resources[0].paths.push("C:/x.ftl".into())),
        "locale.bad_path"
    );
    assert_eq!(
        code(&|s| s.resources[0].paths.push("/abs.ftl".into())),
        "locale.bad_path"
    );
    assert_eq!(
        code(&|s| s.resources[1].locale = "EN".into()),
        "locale.duplicate_locale"
    );
    assert_eq!(
        code(&|s| s.resources[0].locale = "de".into()),
        "locale.unknown_locale"
    );
    assert_eq!(
        code(&|s| s.pseudo.expansion_percent = 1000),
        "locale.invalid_pseudo"
    );
    assert_eq!(
        code(&|s| s.assets[0].fonts.push(" ".into())),
        "locale.invalid_asset"
    );
    assert_eq!(
        code(&|s| {
            s.assets[0]
                .asset_overrides
                .insert("k".into(), String::new());
        }),
        "locale.invalid_asset"
    );
    assert_eq!(
        code(&|s| s.available_locales = (0..100).map(|i| format!("x{i:02}")).collect()),
        "locale.settings_too_large"
    );
}

#[test]
fn settings_round_trip_json_and_build_a_localizer() {
    let json = serde_json::to_string(&good_settings()).unwrap();
    let back: LocalizationSettings = serde_json::from_str(&json).unwrap();
    assert_eq!(back, good_settings());
    // Missing fields take defaults.
    let minimal: LocalizationSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(minimal, LocalizationSettings::default());
    minimal.validate().unwrap();

    let mut l = good_settings()
        .build_localizer(|path| match path {
            "strings/en.ftl" => Ok(b"hello = Hello\n".to_vec()),
            "strings/fr.ftl" => Ok(b"hello = Bonjour\n".to_vec()),
            other => Err(format!("unexpected {other}")),
        })
        .unwrap();
    assert_eq!(l.format("hello", &LocaleArgs::new()), "Hello");
    l.set_locale("fr").unwrap();
    assert_eq!(l.format("hello", &LocaleArgs::new()), "Bonjour");
    assert_eq!(
        l.fonts_for(&LocaleId::parse("ja").unwrap()),
        ["Noto Sans JP"]
    );

    let err = good_settings()
        .build_localizer(|_| Err("disk on fire".into()))
        .err()
        .unwrap();
    assert_eq!(err.code(), "locale.load_failed");
}

#[test]
fn junk_input_never_panics() {
    let junk = [
        "",
        "{",
        "}",
        "[",
        "]",
        "\\",
        "{ $",
        "[/",
        "[b",
        "{{{{",
        "\u{0}",
        "é{\"",
        "[b]\\",
        "{ \"\\",
        "a = {",
        "= =",
        "-",
        "a =\n  .x",
        "\u{feff}a = b",
    ];
    let mut l = Localizer::default();
    for (i, s) in junk.iter().enumerate() {
        let _ = pseudo_localize(s, &plain_pseudo(300));
        let _ = l.load_ftl("en", s);
        let _ = l.load_ftl("x-junk", s);
        let _ = l.format(s, &LocaleArgs::new().text(s, *s));
        let _ = l.replace_ftl("en", &format!("k{i} = {s}\n"));
    }
}

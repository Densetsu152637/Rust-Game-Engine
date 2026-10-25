use engine_text::{
    Color, MarkupErrorKind, MarkupOptions, RichText, StyleChange, TextError, TextLimits,
    UnknownTagPolicy, parse_markup,
};

fn parse(source: &str) -> Result<RichText, TextError> {
    parse_markup(source, &MarkupOptions::default())
}

fn kind(source: &str) -> MarkupErrorKind {
    match parse(source) {
        Err(TextError::Markup { kind, .. }) => kind,
        other => panic!("{source:?}: expected a markup error, got {other:?}"),
    }
}

#[test]
fn markup_matches_equivalent_builder_output() {
    let parsed = parse(
        "Hi [b]bold [i]both[/i][/b] [color=#ff000080]red[/color] [size=20]big[/size] \
         [font=Hack]mono[/font] [u][link=menu.play]Play[/link][/u][img=ui/coin width=16 height=12]",
    )
    .unwrap();
    let mut b = RichText::builder();
    b.text("Hi ");
    b.push_style(StyleChange::Bold).text("bold ");
    b.push_style(StyleChange::Italic).text("both");
    b.pop();
    b.pop();
    b.text(" ");
    b.push_style(StyleChange::Color(Color::rgba(255, 0, 0, 128)))
        .text("red");
    b.pop();
    b.text(" ");
    b.push_style(StyleChange::Size(20.0)).text("big");
    b.pop();
    b.text(" ");
    b.push_style(StyleChange::Family("Hack".into()))
        .text("mono");
    b.pop();
    b.text(" ");
    b.push_style(StyleChange::Underline)
        .push_style(StyleChange::Link("menu.play".into()))
        .text("Play");
    b.pop();
    b.pop();
    b.image("ui/coin", 16.0, 12.0);
    let built = b.build();
    assert_eq!(parsed, built);
    assert_eq!(parsed.text, "Hi bold both red big mono Play\u{FFFC}");
    let image = parsed.spans.last().unwrap().style.image.as_ref().unwrap();
    assert_eq!(
        (image.key.as_str(), image.width, image.height),
        ("ui/coin", 16.0, 12.0)
    );
}

#[test]
fn escapes_and_literal_backslashes() {
    let rich = parse(r"\[b\] \\ C:\path").unwrap();
    assert_eq!(rich.text, r"[b] \ C:\path");
    assert!(rich.spans.is_empty());
}

#[test]
fn markup_errors_have_stable_codes_and_offsets() {
    assert_eq!(kind("a [wave]x[/wave]"), MarkupErrorKind::UnknownTag);
    assert_eq!(kind("[b]open"), MarkupErrorKind::Unclosed);
    assert_eq!(kind("[b][i]x[/b][/i]"), MarkupErrorKind::Mismatched);
    assert_eq!(kind("x[/b]"), MarkupErrorKind::Mismatched);
    assert_eq!(kind("[color=#zzzzzz]x[/color]"), MarkupErrorKind::BadValue);
    assert_eq!(kind("[size=0]x[/size]"), MarkupErrorKind::BadValue);
    assert_eq!(kind("[size=NaN]x[/size]"), MarkupErrorKind::BadValue);
    assert_eq!(kind("[b=1]x[/b]"), MarkupErrorKind::BadValue);
    assert_eq!(kind("[link=bad id]x[/link]"), MarkupErrorKind::BadValue);
    assert_eq!(kind("[img=coin width=16]"), MarkupErrorKind::BadValue);
    assert_eq!(
        kind("[img=coin width=16 height=99999]"),
        MarkupErrorKind::BadValue
    );
    assert_eq!(kind("unterminated [b"), MarkupErrorKind::Unterminated);
    assert_eq!(kind(&"[b]".repeat(33)), MarkupErrorKind::TooDeep);
    let err = parse("ab [nope]").unwrap_err();
    assert_eq!(err.code(), "text.markup.unknown_tag");
    assert!(matches!(err, TextError::Markup { offset: 3, .. }));
    assert!(
        err.to_string()
            .starts_with("text.markup.unknown_tag: at byte 3")
    );
}

#[test]
fn literal_policy_keeps_unknown_tags_verbatim() {
    let options = MarkupOptions {
        unknown_tags: UnknownTagPolicy::Literal,
        ..MarkupOptions::default()
    };
    let rich = parse_markup("[wave]hi[/wave] [b]x[/b] <3 [", &options).unwrap();
    assert_eq!(rich.text, "[wave]hi[/wave] x <3 [");
    assert_eq!(rich.spans.len(), 1);
    assert!(rich.spans[0].style.bold);
    // Known-tag errors still fail under the literal policy.
    assert!(parse_markup("[b]x", &options).is_err());
}

#[test]
fn markup_bounds_tags_and_length() {
    let limits = TextLimits {
        max_spans: 8,
        max_text_bytes: 64,
        ..TextLimits::default()
    };
    let options = MarkupOptions {
        limits,
        ..MarkupOptions::default()
    };
    let many = "[b]x[/b]".repeat(5);
    assert_eq!(
        parse_markup(&many, &options).unwrap_err().code(),
        "text.input.too_many_spans"
    );
    assert_eq!(
        parse_markup(&"x".repeat(65), &options).unwrap_err().code(),
        "text.input.too_long"
    );
}

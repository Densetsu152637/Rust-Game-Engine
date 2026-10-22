//! Modal UI: panel validation, UTF-8 layout, scrolling and clipping, choice
//! navigation, pointer hit testing and input capture of held controls.
use super::common::*;
use engine_project::{ActionFrame, ui::*};
use std::{collections::BTreeSet, sync::Arc};

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn tap(names: &[&str]) -> ActionFrame {
    ActionFrame {
        pressed: set(names),
        held: set(names),
        ..Default::default()
    }
}

fn hold(names: &[&str]) -> ActionFrame {
    ActionFrame {
        held: set(names),
        ..Default::default()
    }
}

fn release(names: &[&str]) -> ActionFrame {
    ActionFrame {
        released: set(names),
        ..Default::default()
    }
}

fn idle() -> ActionFrame {
    ActionFrame::default()
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn opts(id: &str, text: &str, r: Rect) -> PanelOptions {
    PanelOptions {
        id: id.into(),
        text: text.into(),
        rect: r,
        ..Default::default()
    }
}

fn menu(choices: &[&str], r: Rect) -> PanelOptions {
    PanelOptions {
        id: "menu".into(),
        text: "t".into(),
        rect: r,
        choices: choices.iter().map(|c| c.to_string()).collect(),
        ..Default::default()
    }
}

fn open(options: PanelOptions) -> UiModel {
    let mut ui = UiModel::default();
    ui.open(options).unwrap();
    ui
}

fn lines(ui: &UiModel) -> Vec<String> {
    ui.panels()[0].lines.clone()
}

fn events(ui: &mut UiModel) -> Vec<(String, usize)> {
    std::iter::from_fn(|| ui.poll_event())
        .map(|e| (e.kind, e.selection))
        .collect()
}

// ---- panel validation ---------------------------------------------------------------

fn invalid_panel(mutate: impl FnOnce(&mut PanelOptions)) {
    let mut options = opts("p", "text", Rect::default());
    mutate(&mut options);
    let mut ui = UiModel::default();
    expect_code(ui.open(options), "ui.panel.invalid");
    assert!(ui.panels().is_empty());
    assert!(!ui.modal_active());
}

table! { invalid_panel;
    panel_with_empty_id_is_rejected => (|o| o.id = String::new());
    panel_with_id_over_128_bytes_is_rejected => (|o| o.id = "i".repeat(129));
    panel_with_font_name_over_128_bytes_is_rejected => (|o| o.font_asset = "f".repeat(129));
    panel_with_text_over_64_kib_is_rejected => (|o| o.text = "a".repeat(65537));
    panel_with_zero_width_is_rejected => (|o| o.rect.width = 0.0);
    panel_with_negative_height_is_rejected => (|o| o.rect.height = -5.0);
    panel_wider_than_16384_is_rejected => (|o| o.rect.width = 16385.0);
    panel_taller_than_16384_is_rejected => (|o| o.rect.height = 16385.0);
    panel_with_nan_origin_is_rejected => (|o| o.rect.x = f32::NAN);
    panel_with_infinite_origin_is_rejected => (|o| o.rect.y = f32::INFINITY);
    panel_with_zero_scale_is_rejected => (|o| o.scale = 0.0);
    panel_with_negative_scale_is_rejected => (|o| o.scale = -1.0);
    panel_with_scale_above_16_is_rejected => (|o| o.scale = 16.5);
    panel_with_nan_scale_is_rejected => (|o| o.scale = f32::NAN);
    panel_with_zero_line_height_is_rejected => (|o| o.line_height = 0.0);
    panel_with_line_height_above_1024_is_rejected => (|o| o.line_height = 1025.0);
    panel_with_nan_line_height_is_rejected => (|o| o.line_height = f32::NAN);
    panel_with_zero_glyph_advance_is_rejected => (|o| o.glyph_advance = 0.0);
    panel_with_infinite_glyph_advance_is_rejected => (|o| o.glyph_advance = f32::INFINITY);
    panel_with_65_choices_is_rejected => (|o| o.choices = vec!["c".into(); 65]);
    panel_with_color_above_one_is_rejected => (|o| o.color = [1.0, 1.0, 1.0, 1.01]);
    panel_with_negative_color_is_rejected => (|o| o.color = [-0.1, 0.0, 0.0, 1.0]);
    panel_with_nan_color_is_rejected => (|o| o.color = [0.0, f32::NAN, 0.0, 1.0]);
    panel_with_multiline_choice_is_rejected => (|o| o.choices = vec!["a\nb".into()]);
    panel_with_carriage_return_choice_is_rejected => (|o| o.choices = vec!["a\rb".into()]);
    panel_with_choice_over_4096_bytes_is_rejected => (|o| o.choices = vec!["x".repeat(4097)]);
}

fn valid_panel(mutate: impl FnOnce(&mut PanelOptions)) {
    let mut options = opts("p", "", Rect::default());
    mutate(&mut options);
    let mut ui = UiModel::default();
    ui.open(options).unwrap();
    assert_eq!(ui.panels().len(), 1);
}

table! { valid_panel;
    panel_with_id_of_128_bytes_is_accepted => (|o| o.id = "i".repeat(128));
    panel_with_text_of_64_kib_is_accepted => (|o| o.text = "a".repeat(65536));
    panel_with_scale_16_is_accepted => (|o| o.scale = 16.0);
    panel_with_line_height_1024_is_accepted => (|o| o.line_height = 1024.0);
    panel_with_64_choices_is_accepted => (|o| o.choices = vec!["c".into(); 64]);
    panel_with_rect_16384_square_is_accepted => (|o| o.rect = rect(-5.0, -5.0, 16384.0, 16384.0));
    panel_with_choice_of_4096_bytes_is_accepted => (|o| o.choices = vec!["x".repeat(4096)]);
    panel_with_color_extremes_is_accepted => (|o| o.color = [0.0, 1.0, 0.0, 1.0]);
    panel_with_unicode_choices_is_accepted => (|o| o.choices = vec!["日本語".into(), "😀".into()]);
}

#[test]
fn duplicate_panel_ids_are_rejected() {
    let mut ui = open(opts("a", "x", Rect::default()));
    expect_code(ui.open(opts("a", "y", Rect::default())), "ui.panel.invalid");
    assert_eq!(ui.panels().len(), 1);
    assert_eq!(lines(&ui), vec!["x"]);
}

#[test]
fn sixty_four_panels_fit_and_the_next_is_rejected() {
    let mut ui = UiModel::default();
    for i in 0..64 {
        ui.open(opts(&format!("p{i}"), "x", Rect::default()))
            .unwrap();
    }
    expect_code(
        ui.open(opts("p64", "x", Rect::default())),
        "ui.panel.invalid",
    );
    assert_eq!(ui.panels().len(), 64);
}

#[test]
fn failed_open_does_not_change_focus_or_capture() {
    let mut ui = open(opts("a", "x", Rect::default()));
    assert!(
        ui.open(PanelOptions {
            id: "b".into(),
            scale: 0.0,
            ..Default::default()
        })
        .is_err()
    );
    assert!(ui.panels()[0].focused);
}

// ---- layout ---------------------------------------------------------------------------------

fn narrow(text: &str, width: f32) -> PanelOptions {
    opts("p", text, rect(0.0, 0.0, width, 100.0))
}

fn wrapped(text: &str, width: f32, expected: &[&str]) {
    let ui = open(narrow(text, width));
    assert_eq!(lines(&ui), expected, "{text:?} at {width}");
}

table! { wrapped;
    layout_empty_text_has_one_empty_line => ("", 100.0, &[""]);
    layout_short_text_stays_on_one_line => ("abc", 100.0, &["abc"]);
    layout_exact_fit_does_not_wrap => ("abc", 30.0, &["abc"]);
    layout_one_pixel_short_wraps_the_last_glyph => ("abc", 29.0, &["ab", "c"]);
    layout_breaks_long_words_between_glyphs => ("abcdef", 35.0, &["abc", "def"]);
    layout_newline_starts_a_new_line => ("a\nb", 100.0, &["a", "b"]);
    layout_trailing_newline_leaves_an_empty_line => ("a\n", 100.0, &["a", ""]);
    layout_consecutive_newlines_leave_blank_lines => ("a\n\nb", 100.0, &["a", "", "b"]);
    layout_carriage_returns_are_dropped => ("a\r\nb", 100.0, &["a", "b"]);
    layout_cjk_counts_characters_not_bytes => ("日本語", 25.0, &["日本", "語"]);
    layout_emoji_counts_as_one_character => ("😀😀😀", 25.0, &["😀😀", "😀"]);
    layout_combining_marks_are_separate_characters => ("e\u{301}e\u{301}", 25.0, &["e\u{301}", "e\u{301}"]);
    layout_wraps_after_a_newline_independently => ("ab\ncdef", 25.0, &["ab", "cd", "ef"]);
    layout_wide_first_glyph_is_kept_on_its_own_line => ("a", 5.0, &["a"]);
    layout_every_glyph_wider_than_the_panel_gets_its_own_line => ("abc", 5.0, &["a", "b", "c"]);
}

#[test]
fn layout_scale_multiplies_glyph_advance() {
    let ui = open(PanelOptions {
        scale: 2.0,
        ..narrow("abc", 50.0)
    });
    assert_eq!(lines(&ui), vec!["ab", "c"]);
}

#[test]
fn layout_uses_the_panel_glyph_advance() {
    let ui = open(PanelOptions {
        glyph_advance: 25.0,
        ..narrow("abc", 60.0)
    });
    assert_eq!(lines(&ui), vec!["ab", "c"]);
}

#[test]
fn layout_content_height_counts_lines_and_choices() {
    let ui = open(PanelOptions {
        choices: vec!["x".into(), "y".into()],
        ..narrow("a\nb\nc", 100.0)
    });
    assert_eq!(ui.panels()[0].content_height, (3 + 2) as f32 * 20.0);
}

#[test]
fn layout_row_height_follows_line_height_and_scale() {
    let ui = open(PanelOptions {
        line_height: 12.0,
        scale: 2.0,
        ..narrow("a\nb", 100.0)
    });
    assert_eq!(ui.panels()[0].content_height, 48.0);
}

#[test]
fn clip_matches_the_panel_rectangle() {
    let r = rect(7.0, 9.0, 120.0, 40.0);
    let ui = open(opts("p", "x", r));
    assert_eq!(ui.panels()[0].clip, r);
}

#[test]
fn new_panels_start_focused_selected_zero_and_unscrolled() {
    let ui = open(opts("p", "x", Rect::default()));
    let p = &ui.panels()[0];
    assert!(p.focused);
    assert_eq!((p.selected, p.scroll_offset), (0, 0.0));
}

#[test]
fn custom_metrics_can_make_narrow_glyphs_fit() {
    let mut ui = UiModel::default();
    ui.open_with_metrics(
        narrow("iiiiiiiiiiii", 25.0),
        Some(Arc::new(|c| if c == 'i' { 2.0 } else { 10.0 })),
    )
    .unwrap();
    assert_eq!(lines(&ui), vec!["iiiiiiiiiiii"]);
}

#[test]
fn registered_font_metrics_apply_to_panels_using_that_font() {
    let mut ui = UiModel::default();
    ui.set_font_metrics("small", Arc::new(|_| 5.0));
    ui.open(PanelOptions {
        font_asset: "small".into(),
        ..narrow("aaaa", 15.0)
    })
    .unwrap();
    assert_eq!(lines(&ui), vec!["aaa", "a"]);
}

#[test]
fn panels_with_other_fonts_keep_the_default_advance() {
    let mut ui = UiModel::default();
    ui.set_font_metrics("small", Arc::new(|_| 5.0));
    ui.open(PanelOptions {
        font_asset: "big".into(),
        ..narrow("aaaa", 25.0)
    })
    .unwrap();
    assert_eq!(lines(&ui), vec!["aa", "aa"]);
}

#[test]
fn negative_glyph_metric_is_a_layout_error() {
    let mut ui = UiModel::default();
    let result = ui.open_with_metrics(
        narrow("ab", 100.0),
        Some(Arc::new(|c| if c == 'b' { -1.0 } else { 3.0 })),
    );
    expect_code(result, "ui.layout.invalid");
    assert!(ui.panels().is_empty());
}

#[test]
fn nan_glyph_metric_is_a_layout_error() {
    let mut ui = UiModel::default();
    expect_code(
        ui.open_with_metrics(narrow("a", 100.0), Some(Arc::new(|_| f32::NAN))),
        "ui.layout.invalid",
    );
}

#[test]
fn zero_glyph_metric_keeps_everything_on_one_line() {
    let mut ui = UiModel::default();
    ui.open_with_metrics(narrow(&"a".repeat(500), 10.0), Some(Arc::new(|_| 0.0)))
        .unwrap();
    assert_eq!(ui.panels()[0].lines.len(), 1);
}

#[test]
fn text_with_4095_newlines_exceeds_the_line_limit() {
    let mut ui = UiModel::default();
    expect_code(
        ui.open(opts("p", &"\n".repeat(4095), Rect::default())),
        "ui.layout.limit",
    );
}

#[test]
fn text_with_4094_newlines_fits_exactly() {
    let ui = open(opts("p", &"\n".repeat(4094), Rect::default()));
    assert_eq!(ui.panels()[0].lines.len(), 4095);
}

#[test]
fn text_wrapping_into_more_than_4095_lines_exceeds_the_limit() {
    let mut ui = UiModel::default();
    let result = ui.open(opts("p", &"a".repeat(5000), rect(0.0, 0.0, 10.0, 100.0)));
    expect_code(result, "ui.layout.limit");
}

// ---- text, resize and scroll -----------------------------------------------------------------

#[test]
fn set_text_relayouts_the_panel() {
    let mut ui = open(narrow("a", 100.0));
    ui.set_text("p", "one\ntwo".into()).unwrap();
    assert_eq!(lines(&ui), vec!["one", "two"]);
    assert_eq!(ui.panels()[0].options.text, "one\ntwo");
}

#[test]
fn set_text_on_an_unknown_panel_is_an_error() {
    expect_code(
        UiModel::default().set_text("ghost", "x".into()),
        "ui.panel.unknown",
    );
}

#[test]
fn set_text_over_64_kib_is_refused_and_keeps_the_old_text() {
    let mut ui = open(narrow("keep", 100.0));
    expect_code(ui.set_text("p", "a".repeat(65537)), "ui.text.limit");
    assert_eq!(lines(&ui), vec!["keep"]);
}

#[test]
fn failed_set_text_layout_is_atomic() {
    let mut ui = UiModel::default();
    ui.open_with_metrics(
        narrow("ok", 100.0),
        Some(Arc::new(|c| if c == 'X' { -1.0 } else { 3.0 })),
    )
    .unwrap();
    expect_code(ui.set_text("p", "XX".into()), "ui.layout.invalid");
    assert_eq!(ui.panels()[0].options.text, "ok");
    assert_eq!(lines(&ui), vec!["ok"]);
}

#[test]
fn set_text_reuses_the_panels_custom_metrics() {
    let mut ui = UiModel::default();
    ui.open_with_metrics(narrow("a", 15.0), Some(Arc::new(|_| 5.0)))
        .unwrap();
    ui.set_text("p", "aaaa".into()).unwrap();
    assert_eq!(lines(&ui), vec!["aaa", "a"]);
}

#[test]
fn set_text_clamps_scroll_when_content_shrinks() {
    let mut ui = open(opts("p", &"x\n".repeat(20), rect(0.0, 0.0, 100.0, 60.0)));
    ui.scroll("p", 1000.0).unwrap();
    ui.set_text("p", "short".into()).unwrap();
    assert_eq!(ui.panels()[0].scroll_offset, 0.0);
}

#[test]
fn resize_rewraps_text_for_the_new_width() {
    let mut ui = open(narrow("abcdef", 100.0));
    ui.resize("p", rect(0.0, 0.0, 35.0, 100.0)).unwrap();
    assert_eq!(lines(&ui), vec!["abc", "def"]);
    assert_eq!(ui.panels()[0].clip.width, 35.0);
}

#[test]
fn resize_rejects_invalid_rectangles_and_keeps_the_old_one() {
    let mut ui = open(narrow("abc", 100.0));
    expect_code(
        ui.resize("p", rect(0.0, 0.0, 0.0, 10.0)),
        "ui.panel.invalid",
    );
    assert_eq!(ui.panels()[0].options.rect.width, 100.0);
}

#[test]
fn resize_of_an_unknown_panel_is_an_error() {
    expect_code(
        UiModel::default().resize("ghost", Rect::default()),
        "ui.panel.unknown",
    );
}

#[test]
fn resize_clamps_scroll_to_the_new_viewport() {
    let mut ui = open(opts("p", &"x\n".repeat(10), rect(0.0, 0.0, 100.0, 40.0)));
    ui.scroll("p", 1000.0).unwrap();
    let before = ui.panels()[0].scroll_offset;
    ui.resize("p", rect(0.0, 0.0, 100.0, 120.0)).unwrap();
    assert!(ui.panels()[0].scroll_offset < before);
}

fn tall(rows: usize, height: f32) -> UiModel {
    open(opts(
        "p",
        &"x\n".repeat(rows - 1),
        rect(0.0, 0.0, 100.0, height),
    ))
}

#[test]
fn scroll_moves_by_the_requested_amount() {
    let mut ui = tall(10, 60.0);
    ui.scroll("p", 30.0).unwrap();
    assert_eq!(ui.panels()[0].scroll_offset, 30.0);
}

#[test]
fn scroll_clamps_at_the_end_of_the_content() {
    let mut ui = tall(10, 60.0);
    ui.scroll("p", 10_000.0).unwrap();
    assert_eq!(ui.panels()[0].scroll_offset, 140.0);
}

#[test]
fn scroll_clamps_at_the_start_of_the_content() {
    let mut ui = tall(10, 60.0);
    ui.scroll("p", 50.0).unwrap();
    ui.scroll("p", -500.0).unwrap();
    assert_eq!(ui.panels()[0].scroll_offset, 0.0);
}

#[test]
fn short_content_cannot_scroll() {
    let mut ui = tall(2, 60.0);
    ui.scroll("p", 100.0).unwrap();
    assert_eq!(ui.panels()[0].scroll_offset, 0.0);
}

#[test]
fn scroll_rejects_non_finite_deltas() {
    let mut ui = tall(10, 60.0);
    expect_code(ui.scroll("p", f32::NAN), "ui.scroll.invalid");
    expect_code(ui.scroll("p", f32::INFINITY), "ui.scroll.invalid");
}

#[test]
fn scroll_on_an_unknown_panel_is_an_error() {
    expect_code(UiModel::default().scroll("ghost", 1.0), "ui.panel.unknown");
}

// ---- panel stack ---------------------------------------------------------------------------------

#[test]
fn opening_a_second_panel_takes_focus_from_the_first() {
    let mut ui = open(opts("a", "x", Rect::default()));
    ui.open(opts("b", "y", Rect::default())).unwrap();
    assert!(!ui.panels()[0].focused && ui.panels()[1].focused);
}

#[test]
fn closing_the_top_panel_returns_focus_to_the_one_below() {
    let mut ui = open(opts("a", "x", Rect::default()));
    ui.open(opts("b", "y", Rect::default())).unwrap();
    assert!(ui.close("b"));
    assert!(ui.panels()[0].focused);
}

#[test]
fn closing_an_unknown_panel_reports_false() {
    assert!(!UiModel::default().close("ghost"));
}

#[test]
fn closing_a_lower_panel_keeps_the_top_one_focused() {
    let mut ui = open(opts("a", "x", Rect::default()));
    ui.open(opts("b", "y", Rect::default())).unwrap();
    ui.close("a");
    assert_eq!(ui.panels().len(), 1);
    assert!(ui.panels()[0].focused);
}

#[test]
fn a_closed_panel_id_can_be_reused() {
    let mut ui = open(opts("a", "x", Rect::default()));
    ui.close("a");
    ui.open(opts("a", "z", Rect::default())).unwrap();
    assert_eq!(lines(&ui), vec!["z"]);
}

#[test]
fn modal_flag_reflects_any_modal_panel() {
    let mut ui = UiModel::default();
    ui.open(PanelOptions {
        modal: false,
        ..opts("hud", "x", Rect::default())
    })
    .unwrap();
    assert!(!ui.modal_active());
    ui.open(opts("dialog", "y", Rect::default())).unwrap();
    assert!(ui.modal_active());
    ui.close("dialog");
    assert!(!ui.modal_active());
}

#[test]
fn clear_removes_panels_and_pending_events() {
    let mut ui = open(menu(&["a", "b"], Rect::default()));
    ui.tick(tap(&["ui_down"]));
    ui.clear();
    assert!(ui.panels().is_empty());
    assert!(ui.poll_event().is_none());
}

#[test]
fn polling_an_idle_model_returns_nothing() {
    assert!(UiModel::default().poll_event().is_none());
}

// ---- choice navigation -----------------------------------------------------------------------------

fn pad_menu() -> UiModel {
    open(menu(&["one", "two", "three"], rect(0.0, 0.0, 100.0, 200.0)))
}

#[test]
fn down_selects_the_next_choice_and_reports_it() {
    let mut ui = pad_menu();
    ui.tick(tap(&["ui_down"]));
    assert_eq!(ui.panels()[0].selected, 1);
    assert_eq!(events(&mut ui), vec![("selection".to_string(), 1)]);
}

#[test]
fn up_from_the_first_choice_wraps_to_the_last() {
    let mut ui = pad_menu();
    ui.tick(tap(&["ui_up"]));
    assert_eq!(ui.panels()[0].selected, 2);
}

#[test]
fn down_from_the_last_choice_wraps_to_the_first() {
    let mut ui = pad_menu();
    for _ in 0..3 {
        ui.tick(tap(&["ui_down"]));
        ui.tick(release(&["ui_down"]));
    }
    assert_eq!(ui.panels()[0].selected, 0);
    assert_eq!(events(&mut ui).len(), 3);
}

#[test]
fn up_and_down_together_cancel_out_without_an_event() {
    let mut ui = pad_menu();
    ui.tick(tap(&["ui_up", "ui_down"]));
    assert_eq!(ui.panels()[0].selected, 0);
    assert!(ui.poll_event().is_none());
}

#[test]
fn confirm_reports_the_selected_choice() {
    let mut ui = pad_menu();
    ui.tick(tap(&["ui_down"]));
    ui.tick(release(&["ui_down"]));
    ui.tick(tap(&["ui_confirm"]));
    assert_eq!(
        events(&mut ui),
        vec![("selection".to_string(), 1), ("confirm".to_string(), 1)]
    );
}

#[test]
fn cancel_closes_a_dismissible_panel_and_reports_it() {
    let mut ui = pad_menu();
    ui.tick(tap(&["ui_back"]));
    assert!(ui.panels().is_empty());
    assert_eq!(events(&mut ui), vec![("cancel".to_string(), 0)]);
}

#[test]
fn cancel_is_ignored_by_a_non_dismissible_panel() {
    let mut ui = open(PanelOptions {
        dismissible: false,
        ..menu(&["a"], Rect::default())
    });
    ui.tick(tap(&["ui_back"]));
    assert_eq!(ui.panels().len(), 1);
    assert!(ui.poll_event().is_none());
}

#[test]
fn cancel_event_carries_the_current_selection() {
    let mut ui = pad_menu();
    ui.tick(tap(&["ui_down"]));
    ui.tick(release(&["ui_down"]));
    ui.tick(tap(&["ui_back"]));
    assert_eq!(events(&mut ui).last().unwrap(), &("cancel".to_string(), 1));
}

#[test]
fn only_the_top_panel_receives_navigation() {
    let mut ui = open(PanelOptions {
        id: "bottom".into(),
        ..menu(&["a", "b"], Rect::default())
    });
    ui.open(PanelOptions {
        id: "top".into(),
        ..menu(&["x", "y"], Rect::default())
    })
    .unwrap();
    ui.tick(tap(&["ui_down"]));
    assert_eq!(ui.panels()[0].selected, 0);
    assert_eq!(ui.panels()[1].selected, 1);
}

#[test]
fn closing_the_top_panel_hands_input_to_the_one_below() {
    let mut ui = open(PanelOptions {
        id: "bottom".into(),
        ..menu(&["a", "b"], Rect::default())
    });
    ui.open(PanelOptions {
        id: "top".into(),
        ..menu(&["x", "y"], Rect::default())
    })
    .unwrap();
    ui.tick(tap(&["ui_back"]));
    ui.tick(release(&["ui_back"]));
    ui.tick(tap(&["ui_down"]));
    assert_eq!(ui.panels()[0].selected, 1);
}

#[test]
fn custom_action_names_drive_navigation() {
    let mut ui = open(PanelOptions {
        up_action: "menu_prev".into(),
        down_action: "menu_next".into(),
        confirm_action: "menu_ok".into(),
        ..menu(&["a", "b"], Rect::default())
    });
    ui.tick(tap(&["ui_down"]));
    assert_eq!(ui.panels()[0].selected, 0);
    ui.tick(release(&["ui_down"]));
    ui.tick(tap(&["menu_next"]));
    assert_eq!(ui.panels()[0].selected, 1);
}

#[test]
fn unfocused_windows_do_not_navigate() {
    let mut ui = pad_menu();
    ui.tick(ActionFrame {
        focused: false,
        ..tap(&["ui_down", "ui_confirm", "ui_back"])
    });
    assert_eq!(ui.panels().len(), 1);
    assert_eq!(ui.panels()[0].selected, 0);
    assert!(ui.poll_event().is_none());
}

#[test]
fn a_held_confirm_from_before_the_panel_opened_is_ignored_until_released() {
    let mut ui = UiModel::default();
    ui.tick(tap(&["ui_confirm"]));
    ui.open(menu(&["a"], Rect::default())).unwrap();
    ui.tick(tap(&["ui_confirm"]));
    assert!(ui.poll_event().is_none());
    ui.tick(release(&["ui_confirm"]));
    ui.tick(tap(&["ui_confirm"]));
    assert_eq!(events(&mut ui), vec![("confirm".to_string(), 0)]);
}

#[test]
fn choice_selection_scrolls_to_stay_visible() {
    let mut ui = open(menu(
        &["1", "2", "3", "4", "5"],
        rect(0.0, 0.0, 100.0, 40.0),
    ));
    let mut offsets = Vec::new();
    for _ in 0..5 {
        ui.tick(tap(&["ui_down"]));
        ui.tick(release(&["ui_down"]));
        offsets.push(ui.panels()[0].scroll_offset);
    }
    assert_eq!(offsets, vec![20.0, 40.0, 60.0, 80.0, 20.0]);
}

#[test]
fn wrapping_up_to_the_last_choice_scrolls_to_the_end() {
    let mut ui = open(menu(
        &["1", "2", "3", "4", "5"],
        rect(0.0, 0.0, 100.0, 40.0),
    ));
    ui.tick(tap(&["ui_up"]));
    assert_eq!(ui.panels()[0].selected, 4);
    assert_eq!(ui.panels()[0].scroll_offset, 80.0);
}

#[test]
fn a_choice_taller_than_the_panel_aligns_to_its_top() {
    let mut ui = open(PanelOptions {
        line_height: 50.0,
        ..menu(&["1", "2", "3"], rect(0.0, 0.0, 100.0, 30.0))
    });
    ui.tick(tap(&["ui_down"]));
    assert_eq!(ui.panels()[0].scroll_offset, 100.0);
}

#[test]
fn single_choice_menus_report_no_selection_changes() {
    let mut ui = open(menu(&["only"], Rect::default()));
    ui.tick(tap(&["ui_down"]));
    assert_eq!(ui.panels()[0].selected, 0);
    assert!(ui.poll_event().is_none());
}

// ---- scrolling panels without choices ---------------------------------------------------------------

fn doc() -> UiModel {
    tall(10, 60.0)
}

#[test]
fn down_scrolls_a_text_panel_by_one_row() {
    let mut ui = doc();
    ui.tick(tap(&["ui_down"]));
    assert_eq!(ui.panels()[0].scroll_offset, 20.0);
}

#[test]
fn up_scrolls_back_and_clamps_at_the_top() {
    let mut ui = doc();
    ui.tick(tap(&["ui_down"]));
    ui.tick(release(&["ui_down"]));
    ui.tick(tap(&["ui_up"]));
    ui.tick(release(&["ui_up"]));
    ui.tick(tap(&["ui_up"]));
    assert_eq!(ui.panels()[0].scroll_offset, 0.0);
}

#[test]
fn wheel_inside_the_panel_scrolls_three_rows_per_notch() {
    let mut ui = doc();
    ui.tick(ActionFrame {
        pointer: [10.0, 10.0],
        wheel: [0.0, -1.0],
        ..Default::default()
    });
    assert_eq!(ui.panels()[0].scroll_offset, 60.0);
    ui.tick(ActionFrame {
        pointer: [10.0, 10.0],
        wheel: [0.0, 0.5],
        ..Default::default()
    });
    assert_eq!(ui.panels()[0].scroll_offset, 30.0);
}

#[test]
fn wheel_outside_the_panel_is_ignored() {
    let mut ui = doc();
    ui.tick(ActionFrame {
        pointer: [500.0, 10.0],
        wheel: [0.0, -1.0],
        ..Default::default()
    });
    assert_eq!(ui.panels()[0].scroll_offset, 0.0);
}

#[test]
fn the_right_and_bottom_edges_are_outside_the_panel() {
    let mut ui = doc();
    ui.tick(ActionFrame {
        pointer: [100.0, 10.0],
        wheel: [0.0, -1.0],
        ..Default::default()
    });
    ui.tick(ActionFrame {
        pointer: [10.0, 60.0],
        wheel: [0.0, -1.0],
        ..Default::default()
    });
    assert_eq!(ui.panels()[0].scroll_offset, 0.0);
}

#[test]
fn the_left_and_top_edges_are_inside_the_panel() {
    let mut ui = doc();
    ui.tick(ActionFrame {
        pointer: [0.0, 0.0],
        wheel: [0.0, -1.0],
        ..Default::default()
    });
    assert_eq!(ui.panels()[0].scroll_offset, 60.0);
}

#[test]
fn wheel_scroll_clamps_to_the_content() {
    let mut ui = doc();
    for _ in 0..10 {
        ui.tick(ActionFrame {
            pointer: [1.0, 1.0],
            wheel: [0.0, -1.0],
            ..Default::default()
        });
    }
    assert_eq!(ui.panels()[0].scroll_offset, 140.0);
}

#[test]
fn wheel_does_nothing_while_unfocused() {
    let mut ui = doc();
    ui.tick(ActionFrame {
        pointer: [1.0, 1.0],
        wheel: [0.0, -1.0],
        focused: false,
        ..Default::default()
    });
    assert_eq!(ui.panels()[0].scroll_offset, 0.0);
}

// ---- pointer clicks ----------------------------------------------------------------------------------------

fn click_menu() -> UiModel {
    open(PanelOptions {
        text: "hello".into(),
        ..menu(&["a", "b", "c"], rect(10.0, 10.0, 100.0, 200.0))
    })
}

fn click(ui: &mut UiModel, x: f32, y: f32) {
    ui.tick(ActionFrame {
        pointer: [x, y],
        ..tap(&["ui_click"])
    });
}

fn clicked(y: f32, expected: Option<usize>) {
    let mut ui = click_menu();
    click(&mut ui, 20.0, y);
    let got = events(&mut ui);
    match expected {
        Some(i) => assert_eq!(
            got,
            vec![("selection".to_string(), i), ("confirm".to_string(), i)]
        ),
        None => assert!(got.is_empty(), "{got:?}"),
    }
}

table! { clicked;
    click_on_the_first_choice_selects_and_confirms_it => (45.0, Some(0));
    click_on_the_second_choice_selects_and_confirms_it => (65.0, Some(1));
    click_on_the_third_choice_selects_and_confirms_it => (85.0, Some(2));
    click_on_the_text_line_does_nothing => (20.0, None);
    click_just_above_the_first_choice_does_nothing => (29.0, None);
    click_below_the_last_choice_does_nothing => (91.0, None);
}

#[test]
fn click_outside_the_panel_does_nothing() {
    let mut ui = click_menu();
    click(&mut ui, 500.0, 45.0);
    assert!(ui.poll_event().is_none());
}

#[test]
fn click_updates_the_selected_index() {
    let mut ui = click_menu();
    click(&mut ui, 20.0, 85.0);
    assert_eq!(ui.panels()[0].selected, 2);
}

#[test]
fn click_hit_testing_accounts_for_scroll() {
    let mut ui = open(menu(
        &["a", "b", "c", "d", "e"],
        rect(0.0, 0.0, 100.0, 40.0),
    ));
    ui.scroll("menu", 40.0).unwrap();
    click(&mut ui, 5.0, 5.0);
    assert_eq!(ui.panels()[0].selected, 1);
}

#[test]
fn click_on_a_panel_without_choices_just_confirms() {
    let mut ui = open(opts("p", "x", rect(0.0, 0.0, 100.0, 50.0)));
    click(&mut ui, 5.0, 5.0);
    assert_eq!(events(&mut ui), vec![("confirm".to_string(), 0)]);
}

#[test]
fn confirm_and_click_together_confirm_once() {
    let mut ui = open(opts("p", "x", rect(0.0, 0.0, 100.0, 50.0)));
    ui.tick(ActionFrame {
        pointer: [5.0, 5.0],
        ..tap(&["ui_click", "ui_confirm"])
    });
    assert_eq!(events(&mut ui).len(), 1);
}

#[test]
fn custom_pointer_action_triggers_clicks() {
    let mut ui = open(PanelOptions {
        pointer_action: "tap".into(),
        ..opts("p", "x", rect(0.0, 0.0, 100.0, 50.0))
    });
    ui.tick(ActionFrame {
        pointer: [5.0, 5.0],
        ..tap(&["ui_click"])
    });
    assert!(ui.poll_event().is_none());
    ui.tick(release(&["ui_click"]));
    ui.tick(ActionFrame {
        pointer: [5.0, 5.0],
        ..tap(&["tap"])
    });
    assert_eq!(events(&mut ui).len(), 1);
}

#[test]
fn event_queue_keeps_only_the_newest_256_events() {
    let mut ui = open(menu(&["a", "b"], Rect::default()));
    for _ in 0..300 {
        ui.tick(tap(&["ui_down"]));
        ui.tick(release(&["ui_down"]));
    }
    let drained = events(&mut ui);
    assert_eq!(drained.len(), 256);
    assert_eq!(drained.last().unwrap().1, 0);
}

#[test]
fn events_are_delivered_in_order() {
    let mut ui = pad_menu();
    ui.tick(tap(&["ui_down"]));
    ui.tick(release(&["ui_down"]));
    ui.tick(tap(&["ui_down", "ui_confirm"]));
    let kinds: Vec<_> = events(&mut ui).into_iter().map(|e| e.0).collect();
    assert_eq!(kinds, vec!["selection", "selection", "confirm"]);
}

// ---- gameplay input capture -------------------------------------------------------------------------------------

#[test]
fn without_panels_gameplay_sees_the_raw_frame() {
    let mut ui = UiModel::default();
    let frame = ActionFrame {
        pointer: [3.0, 4.0],
        ..tap(&["jump"])
    };
    ui.tick(frame.clone());
    assert_eq!(ui.gameplay_frame(), frame);
}

#[test]
fn a_modal_panel_hides_every_action_but_keeps_focus_state() {
    let mut ui = open(opts("p", "x", Rect::default()));
    ui.tick(ActionFrame {
        focused: false,
        ..tap(&["jump"])
    });
    let frame = ui.gameplay_frame();
    assert!(frame.held.is_empty() && frame.pressed.is_empty());
    assert!(!frame.focused);
}

#[test]
fn modal_capture_hides_pointer_and_wheel() {
    let mut ui = open(opts("p", "x", Rect::default()));
    ui.tick(ActionFrame {
        pointer: [5.0, 5.0],
        wheel: [0.0, 1.0],
        ..Default::default()
    });
    let frame = ui.gameplay_frame();
    assert_eq!((frame.pointer, frame.wheel), ([0.0; 2], [0.0; 2]));
}

#[test]
fn opening_a_panel_blocks_gameplay_before_the_next_tick() {
    let mut ui = UiModel::default();
    ui.tick(tap(&["jump"]));
    ui.open(opts("p", "x", Rect::default())).unwrap();
    assert!(!ui.gameplay_frame().pressed("jump"));
}

#[test]
fn non_modal_panels_let_unrelated_actions_through() {
    let mut ui = UiModel::default();
    ui.open(PanelOptions {
        modal: false,
        ..opts("hud", "x", Rect::default())
    })
    .unwrap();
    ui.tick(tap(&["jump"]));
    ui.tick(tap(&["jump"]));
    assert!(ui.gameplay_frame().held("jump"));
}

#[test]
fn opening_a_non_modal_panel_suppresses_already_held_actions_until_released() {
    let mut ui = UiModel::default();
    ui.tick(hold(&["run"]));
    ui.open(PanelOptions {
        modal: false,
        ..opts("hud", "x", Rect::default())
    })
    .unwrap();
    ui.tick(hold(&["run"]));
    ui.tick(hold(&["run"]));
    assert!(!ui.gameplay_frame().held("run"));
    ui.tick(release(&["run"]));
    ui.tick(tap(&["run"]));
    assert!(ui.gameplay_frame().pressed("run"));
}

#[test]
fn held_action_stays_suppressed_after_closing_until_it_is_released() {
    let mut ui = UiModel::default();
    ui.tick(hold(&["interact"]));
    ui.open(opts("p", "x", Rect::default())).unwrap();
    ui.close("p");
    ui.tick(hold(&["interact"]));
    assert!(!ui.gameplay_frame().held("interact"));
    ui.tick(release(&["interact"]));
    assert!(!ui.gameplay_frame().released("interact"));
    ui.tick(tap(&["interact"]));
    assert!(ui.gameplay_frame().pressed("interact"));
}

#[test]
fn actions_pressed_only_after_closing_reach_gameplay() {
    let mut ui = open(opts("p", "x", Rect::default()));
    ui.close("p");
    ui.tick(idle());
    ui.tick(tap(&["fire"]));
    assert!(ui.gameplay_frame().pressed("fire"));
}

#[test]
fn other_held_actions_are_not_collateral_of_a_suppression() {
    let mut ui = UiModel::default();
    ui.tick(hold(&["interact"]));
    ui.open(opts("p", "x", Rect::default())).unwrap();
    ui.close("p");
    ui.tick(ActionFrame {
        held: set(&["interact", "move"]),
        pressed: set(&["move"]),
        ..Default::default()
    });
    let frame = ui.gameplay_frame();
    assert!(frame.pressed("move") && !frame.held("interact"));
}

#[test]
fn suppressed_actions_lose_their_analog_value() {
    let mut ui = UiModel::default();
    let mut frame = hold(&["run"]);
    frame.values.insert("run".into(), 0.8);
    ui.tick(frame.clone());
    ui.open(PanelOptions {
        modal: false,
        ..opts("hud", "x", Rect::default())
    })
    .unwrap();
    ui.tick(frame);
    assert_eq!(ui.gameplay_frame().value("run"), 0.0);
}

#[test]
fn releases_of_never_captured_actions_pass_through() {
    let mut ui = UiModel::default();
    ui.tick(release(&["walk"]));
    assert!(ui.gameplay_frame().released("walk"));
}

#[test]
fn clearing_the_model_captures_held_actions_too() {
    let mut ui = open(opts("p", "x", Rect::default()));
    ui.tick(hold(&["jump"]));
    ui.clear();
    ui.tick(hold(&["jump"]));
    assert!(!ui.gameplay_frame().held("jump"));
}

#[test]
fn escape_closing_a_panel_does_not_leak_the_cancel_into_gameplay() {
    let mut ui = pad_menu();
    ui.tick(tap(&["ui_back"]));
    assert!(!ui.gameplay_frame().pressed("ui_back"));
    ui.tick(hold(&["ui_back"]));
    assert!(!ui.gameplay_frame().held("ui_back"));
}

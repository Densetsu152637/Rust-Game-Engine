//! Complex-script and collection checks against optional system fonts. Each test prints a
//! reason and returns when its font is absent.

use engine_text::{FontLibrary, LayoutOptions, RichText, TextStyle};

fn system_font(name: &str) -> Option<Vec<u8>> {
    let dirs = [
        "C:/Windows/Fonts",
        "/usr/share/fonts/truetype/msttcorefonts",
        "/Library/Fonts",
    ];
    let found = dirs
        .iter()
        .find_map(|dir| std::fs::read(format!("{dir}/{name}")).ok());
    if found.is_none() {
        println!("skipped: system font {name} not found");
    }
    found
}

#[test]
fn arabic_letters_join_into_contextual_forms() {
    let Some(bytes) = system_font("arial.ttf") else {
        return;
    };
    let mut library = FontLibrary::new();
    library.load_font(bytes).unwrap();
    let style = TextStyle::new("Arial", 32.0);
    let glyphs = |text: &str| {
        library
            .layout(&RichText::plain(text), &style, &LayoutOptions::default())
            .unwrap()
            .glyphs
    };
    // beh alone vs. beh-beh-beh: the initial and medial forms use other glyphs (Arial shares
    // the isolated glyph with the final form).
    let isolated = glyphs("\u{628}")[0].glyph_id;
    let joined = glyphs("\u{628}\u{628}\u{628}");
    assert_eq!(joined.len(), 3);
    let joining: Vec<_> = joined.iter().filter(|g| g.cluster.start < 4).collect();
    assert_eq!(joining.len(), 2);
    assert!(
        joining.iter().all(|g| g.glyph_id != isolated),
        "isolated {isolated} {joined:?}"
    );
    // RTL: the first logical letter (initial form) is drawn rightmost.
    let first = joined.iter().find(|g| g.cluster.start == 0).unwrap();
    assert!(joined.iter().all(|g| g.x <= first.x), "{joined:?}");
    let layout = library
        .layout(
            &RichText::plain("\u{628}\u{628}\u{628}"),
            &style,
            &LayoutOptions::default(),
        )
        .unwrap();
    assert!(layout.lines[0].rtl);
}

#[test]
fn collection_files_register_every_face() {
    let Some(bytes) = system_font("cambria.ttc") else {
        return;
    };
    let mut library = FontLibrary::new();
    let faces = library.load_font(bytes).unwrap();
    assert!(faces.len() >= 2, "{faces:?}");
    let indices: Vec<u32> = faces
        .iter()
        .map(|&f| library.face_info(f).unwrap().index)
        .collect();
    assert_eq!(indices, (0..faces.len() as u32).collect::<Vec<_>>());
    let families: Vec<&str> = faces
        .iter()
        .map(|&f| library.face_info(f).unwrap().family.as_str())
        .collect();
    assert!(families.contains(&"Cambria"), "{families:?}");
}

#[test]
fn real_bold_and_italic_faces_are_matched_without_synthesis() {
    let (Some(regular), Some(bold), Some(italic)) = (
        system_font("arial.ttf"),
        system_font("arialbd.ttf"),
        system_font("ariali.ttf"),
    ) else {
        return;
    };
    let mut library = FontLibrary::new();
    let r = library.load_font(regular).unwrap()[0];
    let b = library.load_font(bold).unwrap()[0];
    let i = library.load_font(italic).unwrap()[0];
    let rich = engine_text::parse_markup("a[b]b[/b][i]c[/i]", &Default::default()).unwrap();
    let layout = library
        .layout(
            &rich,
            &TextStyle::new("arial", 20.0),
            &LayoutOptions::default(),
        )
        .unwrap();
    let face_of = |byte: usize| {
        layout
            .glyphs
            .iter()
            .find(|g| g.cluster.start == byte)
            .unwrap()
    };
    assert_eq!(face_of(0).face, r);
    assert_eq!(face_of(1).face, b);
    assert_eq!(face_of(2).face, i);
    assert!(
        layout
            .glyphs
            .iter()
            .all(|g| g.synthesis == Default::default())
    );
}

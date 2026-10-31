//! Keeps the scripting docs honest: the tagged example in `docs/visual-scripting.md`
//! compiles, every node type and every graph and trap code appears in that page, and
//! every text-backend code (`script.text.*`) appears in `docs/scripting.md`.
mod common;

use engine_graph::GraphDocument;
use engine_script::{FnSig, SigTable, ValueType, codes, graph};

fn page() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/visual-scripting.md"
    );
    std::fs::read_to_string(path).expect("docs page exists")
}

fn scripting_page() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/scripting.md");
    std::fs::read_to_string(path).expect("docs page exists")
}

#[test]
fn the_documented_example_compiles() {
    let text = page();
    let mut examples = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        match (&mut current, line.trim_end()) {
            (None, "```json script_graph") => current = Some(String::new()),
            (Some(block), "```") => {
                examples.push(std::mem::take(block));
                current = None;
            }
            (Some(block), line) => {
                block.push_str(line);
                block.push('\n');
            }
            (None, _) => {}
        }
    }
    assert!(!examples.is_empty(), "the page carries a tagged example");
    let mut sigs = SigTable::new();
    sigs.insert(
        "game.chime",
        FnSig::new(&[("n", ValueType::Int)], None).cost(2),
    )
    .unwrap();
    for example in examples {
        let document = GraphDocument::from_json(example.as_bytes()).expect("example parses");
        let program = graph::compile(&document, &sigs)
            .unwrap_or_else(|d| panic!("example does not compile: {d:#?}"));
        assert_eq!(program.subscriptions(), ["collision.enter"]);
        assert_eq!(program.host_functions(), ["game.chime"]);
    }
}

#[test]
fn every_node_type_and_code_is_documented() {
    let text = page();
    for def in graph::shared_kind().registry.iter() {
        assert!(
            text.contains(&format!("`{}`", def.name)),
            "node type {} is missing from docs/visual-scripting.md",
            def.name
        );
    }
    let scripting = scripting_page();
    for code in codes::ALL {
        let (page, name) = if code.starts_with("script.text.") {
            (&scripting, "docs/scripting.md")
        } else {
            (&text, "docs/visual-scripting.md")
        };
        assert!(
            page.contains(&format!("`{code}`")),
            "code {code} is missing from {name}"
        );
    }
}

//! Keeps `docs/ai-behavior-trees.md` honest: its tagged example must compile.
use engine_ai::behavior_tree::compile;
use engine_graph::GraphDocument;

#[test]
fn the_documented_example_tree_compiles() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/ai-behavior-trees.md"
    );
    let text = std::fs::read_to_string(path).expect("docs page exists");
    let mut examples = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        match (&mut current, line.trim_end()) {
            (None, "```json behavior_tree") => current = Some(String::new()),
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
    for example in examples {
        let document = GraphDocument::from_json(example.as_bytes()).expect("example parses");
        let tree =
            compile(&document).unwrap_or_else(|d| panic!("example does not compile: {d:#?}"));
        assert!(tree.uses_move_to());
        assert_eq!(tree.native_names().len(), 2);
    }
}

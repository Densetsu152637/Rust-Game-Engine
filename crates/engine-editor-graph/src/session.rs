use engine_graph::{
    GraphDiagnostic, GraphDocument, GraphEdit, GraphKind, Strictness, apply_edits, validate,
};

/// A graph editing transaction boundary. UI previews never mutate this document.
pub struct GraphSession {
    document: GraphDocument,
    kind: GraphKind,
    undo: Vec<GraphDocument>,
    redo: Vec<GraphDocument>,
    saved: GraphDocument,
    read_only: bool,
}
impl GraphSession {
    pub fn new(document: GraphDocument, kind: GraphKind) -> Result<Self, Vec<GraphDiagnostic>> {
        if document.kind != kind.name {
            return Err(vec![GraphDiagnostic::new(
                "graph.kind.mismatch",
                "/kind",
                "Document and schema kinds differ",
            )]);
        }
        Ok(Self {
            saved: document.clone(),
            document,
            kind,
            undo: Vec::new(),
            redo: Vec::new(),
            read_only: false,
        })
    }
    pub fn document(&self) -> &GraphDocument {
        &self.document
    }
    pub fn kind(&self) -> &GraphKind {
        &self.kind
    }
    pub fn diagnostics(&self) -> Vec<GraphDiagnostic> {
        validate(&self.document, &self.kind, Strictness::Complete)
    }
    pub fn is_dirty(&self) -> bool {
        self.document != self.saved
    }
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
    pub fn set_read_only(&mut self, value: bool) {
        self.read_only = value;
    }
    pub fn can_undo(&self) -> bool {
        !self.read_only && !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.read_only && !self.redo.is_empty()
    }
    pub fn apply(&mut self, edits: &[GraphEdit]) -> Result<(), Vec<GraphDiagnostic>> {
        if self.read_only {
            return Err(vec![GraphDiagnostic::new(
                "graph.editor.read_only",
                "",
                "Graph is read only",
            )]);
        }
        let mut candidate = self.document.clone();
        apply_edits(&mut candidate, edits, &self.kind, Strictness::Structure)
            .map_err(|e| e.diagnostics)?;
        if candidate != self.document {
            self.undo
                .push(std::mem::replace(&mut self.document, candidate));
            if self.undo.len() > 64 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        Ok(())
    }
    pub fn undo(&mut self) {
        if self.can_undo() {
            let previous = self.undo.pop().expect("checked history");
            self.redo
                .push(std::mem::replace(&mut self.document, previous));
        }
    }
    pub fn redo(&mut self) {
        if self.can_redo() {
            let next = self.redo.pop().expect("checked history");
            self.undo.push(std::mem::replace(&mut self.document, next));
        }
    }
    pub(crate) fn mark_saved(&mut self) {
        self.saved = self.document.clone();
    }
}

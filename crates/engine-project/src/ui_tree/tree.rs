//! Flat-tree hierarchy resolution and whole-tree validation.
use super::*;
use std::collections::BTreeMap;

/// Resolved parent/child indices of a validated tree.
pub(super) struct Hierarchy {
    pub children: Vec<Vec<usize>>,
    pub roots: Vec<usize>,
    pub parents: Vec<Option<usize>>,
    pub index: BTreeMap<String, usize>,
}

impl Hierarchy {
    /// True when `ancestor` is `node` or one of its ancestors.
    pub fn contains(&self, ancestor: usize, node: usize) -> bool {
        let mut cursor = Some(node);
        while let Some(current) = cursor {
            if current == ancestor {
                return true;
            }
            cursor = self.parents[current];
        }
        false
    }
    pub fn ancestors(&self, node: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.parents[node], |n| self.parents[*n])
    }
}

fn valid_class(class: &str) -> bool {
    (1..=64).contains(&class.len())
        && class
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

impl UiNode {
    fn validate(&self) -> Result<()> {
        if self.id.is_empty() || self.id.len() > 128 {
            return Err(invalid(format!("Invalid ID on UI node {}", self.id)));
        }
        self.style.validate(&self.id)?;
        self.widget.validate(&self.id)?;
        if let Some(tooltip) = &self.tooltip {
            tooltip.validate(&self.id)?;
        }
        if let Some(accessibility) = &self.accessibility {
            accessibility.validate(&self.id)?;
        }
        if self.classes.len() > 16 || !self.classes.iter().all(|c| valid_class(c)) {
            return Err(invalid(format!("Invalid classes on UI node {}", self.id)));
        }
        Ok(())
    }
}

impl UiTree {
    pub(super) fn hierarchy(&self) -> Result<Hierarchy> {
        if self.nodes.len() > MAX_UI_NODES {
            return Err(invalid("UI exceeds 1024 nodes"));
        }
        let text: usize = self
            .nodes
            .iter()
            .map(|n| {
                n.widget.text_bytes()
                    + n.tooltip.as_ref().map_or(0, |t| t.text.len())
                    + n.accessibility.as_ref().map_or(0, |a| a.text_bytes())
            })
            .sum();
        if text > MAX_UI_ROOT_TEXT {
            return Err(invalid("UI root text exceeds 64KiB"));
        }
        let mut index = BTreeMap::new();
        for (i, node) in self.nodes.iter().enumerate() {
            node.validate()?;
            if index.insert(node.id.clone(), i).is_some() {
                return Err(invalid("Duplicate UI node ID"));
            }
        }
        let mut hierarchy = Hierarchy {
            children: vec![vec![]; self.nodes.len()],
            roots: vec![],
            parents: vec![None; self.nodes.len()],
            index,
        };
        for (i, node) in self.nodes.iter().enumerate() {
            if let Some(parent) = &node.parent {
                let parent = *hierarchy
                    .index
                    .get(parent)
                    .ok_or_else(|| invalid("Unknown UI parent"))?;
                hierarchy.parents[i] = Some(parent);
                hierarchy.children[parent].push(i);
            } else {
                hierarchy.roots.push(i);
            }
        }
        // Bound every ancestor walk: cycles and excessive depth fail without recursion.
        for i in 0..self.nodes.len() {
            let mut cursor = Some(i);
            let mut depth = 0;
            while let Some(node) = cursor {
                depth += 1;
                if depth > MAX_UI_DEPTH {
                    return Err(invalid("UI cycle or depth exceeds 64"));
                }
                cursor = hierarchy.parents[node];
            }
        }
        for (i, node) in self.nodes.iter().enumerate() {
            let nav = &node.nav;
            for target in [&nav.up, &nav.down, &nav.left, &nav.right]
                .into_iter()
                .flatten()
            {
                if *target == node.id || !hierarchy.index.contains_key(target) {
                    return Err(invalid(format!(
                        "Navigation target {target} of UI node {} is unknown",
                        node.id
                    )));
                }
            }
            if !(-1024..=1024).contains(&nav.tab_index) {
                return Err(invalid(format!("Invalid tab index on UI node {}", node.id)));
            }
            if let UiWidget::VirtualList { template, .. } = &node.widget {
                let children = &hierarchy.children[i];
                if children.len() != 1 || self.nodes[children[0]].id != *template {
                    return Err(invalid(format!(
                        "Virtual list {} must have its template as its only child",
                        node.id
                    )));
                }
                let size = (0..self.nodes.len())
                    .filter(|n| hierarchy.contains(children[0], *n))
                    .count();
                if size > MAX_TEMPLATE_NODES {
                    return Err(invalid("Virtual list template exceeds 64 nodes"));
                }
            }
        }
        Ok(hierarchy)
    }

    pub fn validate(&self) -> Result<()> {
        self.hierarchy().map(|_| ())
    }

    /// Index of the node with `id`.
    pub fn find(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }
}

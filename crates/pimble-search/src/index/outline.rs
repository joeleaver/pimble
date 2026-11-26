//! A node's headings as the index keeps them (`Node.outline`): what says
//! which heading a hit is under.

use pimble_core::IndexUnit;

/// The block a unit or chunk locator starts in: `"b:12"` and `"b:12/r:3"` are 12.
pub(super) fn block_of(path: &str) -> Option<u32> {
    path.strip_prefix("b:")?.split('/').next()?.parse().ok()
}

/// One heading of a node, as the index keeps it (`Node.outline`).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct OutlineEntry {
    pub(super) block: Option<u32>,
    /// Its byte offset in the node's indexed `text`, when it is found there.
    pub(super) offset: Option<usize>,
    pub(super) level: u8,
    pub(super) text: String,
}

/// A node's headings in order: what says which heading a hit is under.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Outline {
    pub(super) entries: Vec<OutlineEntry>,
}

impl Outline {
    /// The headings among `units`, each placed in `text` (the units' text
    /// joined by newlines, as the server builds it; searched for otherwise).
    pub(super) fn of(text: &str, units: &[IndexUnit]) -> Self {
        let mut entries = Vec::new();
        let mut offset = 0usize;
        let mut searched_from = 0usize;
        for unit in units {
            if let pimble_core::UnitKind::Heading(level) = unit.kind {
                let heading = unit.text.replace(['\t', '\n'], " ");
                let at = if text.get(offset..).is_some_and(|rest| rest.starts_with(unit.text.as_str())) {
                    Some(offset)
                } else {
                    text.get(searched_from..).and_then(|rest| rest.find(unit.text.as_str())).map(|p| p + searched_from)
                };
                if let Some(at) = at {
                    searched_from = at + unit.text.len();
                }
                if !heading.trim().is_empty() {
                    entries.push(OutlineEntry { block: block_of(&unit.path), offset: at, level, text: heading.trim().to_string() });
                }
            }
            offset += unit.text.len() + 1;
        }
        Self { entries }
    }

    /// The `headings` field: one heading per line.
    pub(super) fn headings_text(&self) -> String {
        self.entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// The `outline` field.
    pub(super) fn encode(&self) -> String {
        let num = |n: Option<String>| n.unwrap_or_else(|| "-".to_string());
        self.entries
            .iter()
            .map(|e| format!("{}\t{}\t{}\t{}", num(e.block.map(|b| b.to_string())), num(e.offset.map(|o| o.to_string())), e.level, e.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The `outline` field read back; a line it cannot read is left out.
    pub(super) fn decode(field: &str) -> Self {
        let entries = field
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(4, '\t');
                let block = parts.next()?.parse().ok();
                let offset = parts.next()?.parse().ok();
                let level = parts.next()?.parse().ok()?;
                let text = parts.next()?.to_string();
                Some(OutlineEntry { block, offset, level, text })
            })
            .collect();
        Self { entries }
    }

    /// The path of headings ending with entry `i`: it and the ones above it.
    pub(super) fn path_to(&self, i: usize) -> Vec<String> {
        let mut stack: Vec<&OutlineEntry> = Vec::new();
        for e in &self.entries[..=i] {
            while stack.last().is_some_and(|top| top.level >= e.level) {
                stack.pop();
            }
            stack.push(e);
        }
        stack.into_iter().map(|e| e.text.clone()).collect()
    }

    /// The path of headings above the last entry `before` accepts.
    fn last_path(&self, before: impl Fn(&OutlineEntry) -> bool) -> Vec<String> {
        match self.entries.iter().rposition(before) {
            Some(i) => self.path_to(i),
            None => Vec::new(),
        }
    }

    /// The headings above byte `at` of the text.
    pub(super) fn section_at_offset(&self, at: usize) -> Vec<String> {
        self.last_path(|e| e.offset.is_some_and(|o| o <= at))
    }

    /// The headings above block `block` (or that block itself, a heading).
    pub(super) fn section_at_block(&self, block: u32) -> Vec<String> {
        self.last_path(|e| e.block.is_some_and(|b| b <= block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::UnitKind;

    #[test]
    fn an_outline_places_headings_and_reads_back() {
        let units = vec![
            IndexUnit::new(UnitKind::Prose, "b:0", "intro"),
            IndexUnit::new(UnitKind::Heading(1), "b:1", "Design"),
            IndexUnit::new(UnitKind::Prose, "b:2", "body"),
            IndexUnit::new(UnitKind::Heading(2), "b:3", "Edges"),
            IndexUnit::new(UnitKind::Heading(1), "b:4", "Budget"),
        ];
        let text = "intro\nDesign\nbody\nEdges\nBudget";
        let outline = Outline::of(text, &units);
        assert_eq!(Outline::decode(&outline.encode()), outline);
        assert_eq!(outline.entries[1].offset, Some(text.find("Edges").unwrap()));
        assert_eq!(outline.section_at_block(3), vec!["Design", "Edges"]);
        assert_eq!(outline.section_at_block(2), vec!["Design"]);
        assert_eq!(outline.section_at_block(0), Vec::<String>::new());
        assert_eq!(outline.section_at_offset(text.find("Budget").unwrap()), vec!["Budget"]);
        assert_eq!(block_of("b:12/r:3/c:1"), Some(12));
    }
}

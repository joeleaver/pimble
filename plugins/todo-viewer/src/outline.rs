//! A todo list as data: its forest read into items in the tree's order, which
//! of them show, how far along each is, and where a keyboard move takes one.
//! Nothing here draws or writes.

use pimble_plugin_sdk::pimble_core::{Node, Place};
use pimble_plugin_sdk::prelude::{Addr, ForestEntry};

/// The node field that says an item is done, holding when (docs/TODO_CONTRACT.md
/// decision 1).
pub const DONE: &str = "done";
/// The node field shown as an item's due date, as typed.
pub const DUE: &str = "due";

/// One item: a node of the list's subtree.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub addr: Addr,
    /// Its parent in the list; `None` for an item of the list itself.
    pub parent: Option<Addr>,
    pub label: String,
    pub icon: String,
    pub color: Option<String>,
    pub background: Option<String>,
    /// When it was checked (the `done` field), if it was.
    pub done: Option<String>,
    pub due: Option<String>,
    /// 1 for the list's own items.
    pub depth: u32,
    /// Its sub-todos, in order.
    pub children: Vec<Addr>,
    pub writable: bool,
}

/// The list: its items depth first, each parent before its children.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outline {
    /// The metanode, once read.
    pub metanode: Option<Addr>,
    /// Whether the person may add items to the list itself.
    pub writable: bool,
    pub items: Vec<Item>,
}

/// Where a move puts a node: under `parent`, at `place`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub parent: Addr,
    pub place: Place,
}

/// A keyboard move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Past the sibling above.
    Up,
    /// Past the sibling below.
    Down,
    /// Last under the sibling above.
    Indent,
    /// Right after its parent.
    Outdent,
}

/// Node `node`'s field `name`, found case-insensitively, as typed.
pub fn field(node: &Node, name: &str) -> Option<String> {
    node.metadata.fields.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).filter(|v| !v.trim().is_empty())
}

/// How an item is keyed in what the device remembers (its folded state).
pub fn key_of(addr: Addr) -> String {
    format!("{}/{}", addr.store, addr.node)
}

impl Outline {
    /// The outline of a forest's entries (`Forest::entries`, depth first).
    /// A node the source found by a match is an item of the list, with no
    /// sub-todos of its own here.
    pub fn of(metanode: Addr, writable: bool, entries: &[ForestEntry], label: impl Fn(&Node) -> String) -> Outline {
        let items = entries
            .iter()
            .map(|e| Item {
                addr: e.addr,
                parent: if e.found { None } else { e.parent },
                label: label(&e.node),
                icon: pimble_plugin_sdk::pimble_ui::appearance::node_icon(&e.node),
                color: e.node.metadata.color().map(str::to_string),
                background: e.node.metadata.background().map(str::to_string),
                done: field(&e.node, DONE),
                due: field(&e.node, DUE),
                depth: if e.found { 1 } else { e.depth.max(1) },
                children: if e.found { Vec::new() } else { e.children.clone() },
                writable: e.node.access.allows_write(),
            })
            .collect();
        Outline { metanode: Some(metanode), writable, items }
    }

    pub fn get(&self, addr: Addr) -> Option<&Item> {
        self.items.iter().find(|i| i.addr == addr)
    }

    /// The items under `parent` (`None`: the list's own), in order.
    pub fn siblings(&self, parent: Option<Addr>) -> Vec<Addr> {
        match parent.and_then(|p| self.get(p)) {
            Some(p) => p.children.iter().copied().filter(|c| self.get(*c).is_some()).collect(),
            None => self.items.iter().filter(|i| i.parent.is_none()).map(|i| i.addr).collect(),
        }
    }

    /// How many of `addr`'s sub-todos are done, of how many.
    pub fn progress(&self, addr: Addr) -> (usize, usize) {
        let Some(item) = self.get(addr) else { return (0, 0) };
        let kids: Vec<&Item> = item.children.iter().filter_map(|c| self.get(*c)).collect();
        (kids.iter().filter(|k| k.done.is_some()).count(), kids.len())
    }

    /// How many items are done, of how many, at every depth.
    pub fn counts(&self) -> (usize, usize) {
        (self.items.iter().filter(|i| i.done.is_some()).count(), self.items.len())
    }

    /// The items shown: none under a folded item (by [`key_of`]), and with
    /// `hide_done` no done item, nor anything under one.
    pub fn visible(&self, folded: &[String], hide_done: bool) -> Vec<Item> {
        let mut out = Vec::new();
        // Depth of the item whose subtree is being skipped, while one is.
        let mut skipping: Option<u32> = None;
        for item in &self.items {
            if let Some(depth) = skipping {
                if item.depth > depth {
                    continue;
                }
                skipping = None;
            }
            if hide_done && item.done.is_some() {
                skipping = Some(item.depth);
                continue;
            }
            out.push(item.clone());
            if folded.contains(&key_of(item.addr)) && !item.children.is_empty() {
                skipping = Some(item.depth);
            }
        }
        out
    }

    /// The node an item's place is under: its parent, or the list.
    fn under(&self, item: &Item) -> Option<Addr> {
        item.parent.or(self.metanode)
    }

    /// Where `step` takes `addr`, or `None` when it goes nowhere (the first
    /// sibling up, the last down, the first indented, a list item outdented).
    pub fn step(&self, addr: Addr, step: Step) -> Option<Target> {
        let item = self.get(addr)?;
        let sibs = self.siblings(item.parent);
        let i = sibs.iter().position(|a| *a == addr)?;
        let parent = self.under(item)?;
        match step {
            Step::Up => {
                let place = match i {
                    0 => return None,
                    1 => Place::First,
                    _ => Place::After(sibs[i - 2].node),
                };
                Some(Target { parent, place })
            }
            Step::Down => sibs.get(i + 1).map(|next| Target { parent, place: Place::After(next.node) }),
            Step::Indent => (i > 0).then(|| Target { parent: sibs[i - 1], place: Place::Last }),
            Step::Outdent => {
                let p = self.get(item.parent?)?;
                Some(Target { parent: self.under(p)?, place: Place::After(p.addr.node) })
            }
        }
    }
}

/// Where a drop lands in the rows shown (`rows`, what [`Outline::visible`]
/// answered), right after the row `after` (`None`: at the top): the list's
/// first item at the top; the first sub-todo of a row whose sub-todos show
/// right below it; otherwise the next sibling of that row. A drop never
/// guesses a depth: a row goes deeper or shallower by Tab and Shift+Tab.
pub fn drop_target(outline: &Outline, rows: &[Item], after: Option<Addr>) -> Option<Target> {
    let list = outline.metanode?;
    let Some(after) = after else { return Some(Target { parent: list, place: Place::First }) };
    let i = rows.iter().position(|r| r.addr == after)?;
    let row = &rows[i];
    if rows.get(i + 1).is_some_and(|next| next.parent == Some(row.addr)) {
        return Some(Target { parent: row.addr, place: Place::First });
    }
    Some(Target { parent: row.parent.unwrap_or(list), place: Place::After(row.addr.node) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_plugin_sdk::pimble_core::{NodeId, StoreId};

    /// An outline from `(depth, done)` pairs in depth-first order.
    fn outline(shape: &[(u32, bool)]) -> Outline {
        let store = StoreId::new();
        let metanode = Addr::new(store, NodeId::new());
        let mut items: Vec<Item> = Vec::new();
        let mut path: Vec<Addr> = Vec::new();
        for (n, (depth, done)) in shape.iter().enumerate() {
            let addr = Addr::new(store, NodeId::new());
            path.truncate(*depth as usize - 1);
            let parent = path.last().copied();
            if let Some(p) = parent {
                items.iter_mut().find(|i| i.addr == p).unwrap().children.push(addr);
            }
            items.push(Item {
                addr,
                parent,
                label: format!("item {n}"),
                icon: "file".into(),
                color: None,
                background: None,
                done: done.then(|| "2026-11-05T00:00:00Z".to_string()),
                due: None,
                depth: *depth,
                children: Vec::new(),
                writable: true,
            });
            path.push(addr);
        }
        Outline { metanode: Some(metanode), writable: true, items }
    }

    fn labels(items: &[Item]) -> Vec<String> {
        items.iter().map(|i| i.label.clone()).collect()
    }

    #[test]
    fn progress_counts_direct_sub_todos_and_counts_every_item() {
        let o = outline(&[(1, false), (2, true), (2, false), (3, true), (1, true)]);
        assert_eq!(o.progress(o.items[0].addr), (1, 2));
        assert_eq!(o.progress(o.items[2].addr), (1, 1));
        assert_eq!(o.progress(o.items[4].addr), (0, 0));
        assert_eq!(o.counts(), (3, 5));
    }

    #[test]
    fn folding_and_hiding_done_leave_out_whole_subtrees() {
        let o = outline(&[(1, false), (2, true), (3, false), (2, false), (1, false)]);
        assert_eq!(labels(&o.visible(&[], false)).len(), 5);
        assert_eq!(labels(&o.visible(&[], true)), ["item 0", "item 3", "item 4"]);
        assert_eq!(labels(&o.visible(&[key_of(o.items[0].addr)], false)), ["item 0", "item 4"]);
        // Folding an item without sub-todos hides nothing.
        assert_eq!(o.visible(&[key_of(o.items[4].addr)], false).len(), 5);
    }

    #[test]
    fn a_drop_lands_first_under_an_open_row_and_beside_any_other() {
        let o = outline(&[(1, false), (2, false), (1, false), (2, false)]);
        let [a, a1, b, b1] = [0, 1, 2, 3].map(|i| o.items[i].addr);
        let list = o.metanode.unwrap();
        let open = o.visible(&[], false);
        assert_eq!(drop_target(&o, &open, None), Some(Target { parent: list, place: Place::First }));
        assert_eq!(drop_target(&o, &open, Some(a)), Some(Target { parent: a, place: Place::First }));
        assert_eq!(drop_target(&o, &open, Some(a1)), Some(Target { parent: a, place: Place::After(a1.node) }));
        assert_eq!(drop_target(&o, &open, Some(b1)), Some(Target { parent: b, place: Place::After(b1.node) }));
        // B folded: after B is beside it, not into it.
        let folded = o.visible(&[key_of(b)], false);
        assert_eq!(drop_target(&o, &folded, Some(b)), Some(Target { parent: list, place: Place::After(b.node) }));
    }

    #[test]
    fn keyboard_moves_land_beside_under_and_after() {
        let o = outline(&[(1, false), (1, false), (2, false), (2, false), (1, false)]);
        let [a, b, b1, b2, c] = [0, 1, 2, 3, 4].map(|i| o.items[i].addr);
        let list = o.metanode.unwrap();
        assert_eq!(o.step(a, Step::Up), None);
        assert_eq!(o.step(b, Step::Up), Some(Target { parent: list, place: Place::First }));
        assert_eq!(o.step(c, Step::Up), Some(Target { parent: list, place: Place::After(a.node) }));
        assert_eq!(o.step(a, Step::Down), Some(Target { parent: list, place: Place::After(b.node) }));
        assert_eq!(o.step(c, Step::Down), None);
        assert_eq!(o.step(a, Step::Indent), None);
        assert_eq!(o.step(c, Step::Indent), Some(Target { parent: b, place: Place::Last }));
        assert_eq!(o.step(b2, Step::Indent), Some(Target { parent: b1, place: Place::Last }));
        assert_eq!(o.step(b1, Step::Outdent), Some(Target { parent: list, place: Place::After(b.node) }));
        assert_eq!(o.step(a, Step::Outdent), None);
    }
}

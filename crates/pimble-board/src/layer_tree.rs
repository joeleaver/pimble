//! The layers of a board read as a tree (docs/DRAWING_CONTRACT.md section 1,
//! "Layer groups"): the one place a layer's `parent`, `order`, flags and
//! blend are resolved, so the snapshot, the renderer, the locks and the
//! choice of where new elements go all read the same tree.
//!
//! The rules, all on the raw entries, nothing ever rewritten:
//!
//! - A `parent` that names nothing, or names a layer that is not a group,
//!   reads as top level (a group deleted while someone moved a layer into
//!   it).
//! - A cycle (two people nesting A into B and B into A at once) is read
//!   deterministically: of the entries on the cycle, the one with the
//!   smallest id reads as top level, so walking up from any of them, the
//!   first entry met twice stops there. Every device reads the same tree.
//! - Siblings are ordered by `order`, ties by id. The draw order of the
//!   whole board is depth-first over the tree in sibling order.
//! - A layer is effectively hidden or locked when it or any group above it
//!   is; its own flags are kept and apply again when the group's do not.
//! - A layer needs a buffer of its own (is *isolated*) when its blend is
//!   not normal or its opacity is below 1; a group in pass-through at full
//!   opacity does not, and its children draw as if ungrouped.

use crate::doc::RawLayer;
use crate::model::{Blend, LayerKind};

/// One layer entry, resolved. `index` is its place in the slice given to
/// [`resolve`].
#[derive(Debug, Clone, PartialEq)]
pub struct TreeEntry {
    pub index: usize,
    /// The group it is read inside: an index into [`LayerTree::entries`].
    pub parent: Option<usize>,
    /// 0 at top level.
    pub depth: usize,
    pub kind: LayerKind,
    /// Its blend as read (the kind's default when it names none, normal for
    /// a layer that is not a group naming pass-through).
    pub blend: Blend,
    pub effective_visible: bool,
    pub effective_locked: bool,
    /// The nearest entry, itself or a group above it, whose own lock makes
    /// it effectively locked (an index into [`LayerTree::entries`]).
    pub locked_by: Option<usize>,
    /// Whether it draws into a buffer of its own (see the module doc).
    pub isolated: bool,
    /// Its children in sibling order, bottom to top (indices into
    /// [`LayerTree::entries`]).
    pub children: Vec<usize>,
}

/// The tree of a board's layers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerTree {
    /// Entry per layer, in draw order bottom to top: depth first, a group
    /// right after its children (so read top to bottom, a group comes
    /// before what it holds, as a layers panel lists them).
    pub entries: Vec<TreeEntry>,
    /// The top-level entries in sibling order, bottom to top.
    pub roots: Vec<usize>,
}

impl LayerTree {
    /// The entry of `layers[index]`.
    pub fn of(&self, index: usize) -> Option<usize> {
        self.entries.iter().position(|e| e.index == index)
    }

    /// The entries above `entry`, nearest first.
    pub fn ancestors(&self, entry: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.entries[entry].parent, move |p| self.entries[*p].parent)
    }

    /// Whether `entry` is `ancestor` or inside it, at any depth.
    pub fn is_within(&self, entry: usize, ancestor: usize) -> bool {
        entry == ancestor || self.ancestors(entry).any(|a| a == ancestor)
    }

    /// Every entry inside `entry`, at any depth, in draw order.
    pub fn descendants(&self, entry: usize) -> Vec<usize> {
        let mut out = Vec::new();
        fn walk(t: &LayerTree, at: usize, out: &mut Vec<usize>) {
            for c in &t.entries[at].children {
                walk(t, *c, out);
                out.push(*c);
            }
        }
        walk(self, entry, &mut out);
        out
    }

    /// The isolated entries `entry` draws through, outermost first (itself
    /// last when it is isolated): the buffers the renderer opens for it.
    pub fn chain(&self, entry: usize) -> Vec<usize> {
        let mut out: Vec<usize> = std::iter::once(entry).chain(self.ancestors(entry)).filter(|e| self.entries[*e].isolated).collect();
        out.reverse();
        out
    }

    /// Where new elements go when nothing names a layer: the top vector
    /// layer, in draw order, that is not effectively locked and not inside a
    /// hidden group (the contract: tools never pick a layer inside a hidden
    /// or locked group). A layer hidden by its own flag is still picked, as
    /// before groups.
    pub fn default_target(&self) -> Option<usize> {
        (0..self.entries.len()).rev().find(|at| {
            let e = &self.entries[*at];
            e.kind.is_vector() && !e.effective_locked && e.parent.is_none_or(|p| self.entries[p].effective_visible)
        })
    }

    /// Whether any layer is a vector layer.
    pub fn has_vector(&self) -> bool {
        self.entries.iter().any(|e| e.kind.is_vector())
    }

    /// The layer an element whose own layer is gone (or is a group) is drawn
    /// in: the top vector layer, else the top layer that is not a group.
    pub fn fallback(&self) -> Option<usize> {
        (0..self.entries.len())
            .rev()
            .find(|at| self.entries[*at].kind.is_vector())
            .or_else(|| (0..self.entries.len()).rev().find(|at| !self.entries[*at].kind.is_group()))
    }

    /// The entry an element naming layer `id` is drawn in: that layer when
    /// it exists and is not a group, else [`LayerTree::fallback`].
    pub fn home_of(&self, layers: &[RawLayer], id: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| layers[e.index].id == id && !e.kind.is_group())
            .or_else(|| self.fallback())
    }

    /// The siblings of `entry` (its parent's children, or the roots), bottom
    /// to top, itself included.
    pub fn siblings(&self, entry: usize) -> &[usize] {
        match self.entries[entry].parent {
            Some(p) => &self.entries[p].children,
            None => &self.roots,
        }
    }
}

/// Resolve `layers` (in any order) into a tree. Pure: see the module doc
/// for the rules.
pub fn resolve(layers: &[RawLayer]) -> LayerTree {
    let n = layers.len();
    let mut by_id: std::collections::HashMap<&str, usize> = std::collections::HashMap::with_capacity(n);
    for (i, l) in layers.iter().enumerate() {
        // Two entries cannot share an id (map keys); the first wins anyway.
        by_id.entry(l.id.as_str()).or_insert(i);
    }
    // Parents by raw index: only a group is a parent.
    let mut parent: Vec<Option<usize>> = layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let p = l.parent.as_deref().and_then(|p| by_id.get(p).copied())?;
            (p != i && layers[p].kind.is_group()).then_some(p)
        })
        .collect();
    // Cycles: each cycle's smallest id reads as top level.
    let mut state = vec![0u8; n]; // 0 unseen, 1 on the current walk, 2 done
    for start in 0..n {
        let mut path = Vec::new();
        let mut at = Some(start);
        while let Some(i) = at {
            match state[i] {
                2 => break,
                1 => {
                    // `i` closes a cycle: the entries from it on the path.
                    let from = path.iter().position(|p| *p == i).unwrap_or(0);
                    let cut = path[from..].iter().copied().min_by(|a: &usize, b: &usize| layers[*a].id.cmp(&layers[*b].id));
                    if let Some(cut) = cut {
                        parent[cut] = None;
                    }
                    break;
                }
                _ => {
                    state[i] = 1;
                    path.push(i);
                    at = parent[i];
                }
            }
        }
        for p in path {
            state[p] = 2;
        }
    }
    // Children by raw index, in sibling order.
    let sibling_order = |a: &usize, b: &usize| layers[*a].order.cmp(&layers[*b].order).then_with(|| layers[*a].id.cmp(&layers[*b].id));
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut roots_raw = Vec::new();
    for (i, p) in parent.iter().enumerate() {
        match p {
            Some(p) => kids[*p].push(i),
            None => roots_raw.push(i),
        }
    }
    for k in &mut kids {
        k.sort_by(sibling_order);
    }
    roots_raw.sort_by(sibling_order);

    // Depth first, a group after its children.
    let mut tree = LayerTree { entries: Vec::with_capacity(n), roots: Vec::new() };
    fn visit(layers: &[RawLayer], kids: &[Vec<usize>], tree: &mut LayerTree, i: usize, depth: usize) -> usize {
        let l = &layers[i];
        let blend = match l.blend {
            Some(Blend::PassThrough) if !l.kind.is_group() => Blend::Normal,
            Some(b) => b,
            None => l.kind.default_blend(),
        };
        let isolated = if l.kind.is_group() { blend != Blend::PassThrough || l.opacity < 1.0 } else { blend != Blend::Normal || l.opacity < 1.0 };
        let children: Vec<usize> = kids[i].iter().map(|c| visit(layers, kids, tree, *c, depth + 1)).collect();
        let at = tree.entries.len();
        for c in &children {
            tree.entries[*c].parent = Some(at);
        }
        tree.entries.push(TreeEntry {
            index: i,
            parent: None,
            depth,
            kind: l.kind.clone(),
            blend,
            effective_visible: l.visible,
            effective_locked: l.locked,
            locked_by: None,
            isolated,
            children,
        });
        at
    }
    tree.roots = roots_raw.into_iter().map(|r| visit(layers, &kids, &mut tree, r, 0)).collect();
    // Flags from the top down: in reverse draw order a group comes before
    // its children.
    for at in (0..tree.entries.len()).rev() {
        let (hidden_above, locked_above) = match tree.entries[at].parent {
            Some(p) => (!tree.entries[p].effective_visible, tree.entries[p].locked_by),
            None => (false, None),
        };
        let e = &mut tree.entries[at];
        let own_lock = layers[e.index].locked;
        e.effective_visible = !hidden_above && layers[e.index].visible;
        e.locked_by = locked_above.or(own_lock.then_some(at));
        e.effective_locked = e.locked_by.is_some();
    }
    tree
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(id: &str, order: &str, kind: LayerKind, parent: Option<&str>) -> RawLayer {
        RawLayer {
            id: id.into(),
            name: id.to_uppercase(),
            order: order.into(),
            visible: true,
            locked: false,
            opacity: 1.0,
            note: None,
            fields: Vec::new(),
            kind,
            blend: None,
            parent: parent.map(Into::into),
            clip: None,
        }
    }

    fn ids(layers: &[RawLayer], t: &LayerTree) -> Vec<String> {
        t.entries.iter().map(|e| layers[e.index].id.clone()).collect()
    }

    fn entry<'a>(layers: &[RawLayer], t: &'a LayerTree, id: &str) -> (usize, &'a TreeEntry) {
        let i = t.entries.iter().position(|e| layers[e.index].id == id).unwrap();
        (i, &t.entries[i])
    }

    #[test]
    fn flat_layers_read_in_order() {
        let ls = vec![layer("b", "b", LayerKind::Vector, None), layer("a", "a", LayerKind::Vector, None)];
        let t = resolve(&ls);
        assert_eq!(ids(&ls, &t), ["a", "b"]);
        assert!(t.entries.iter().all(|e| e.depth == 0 && e.parent.is_none() && !e.isolated));
    }

    #[test]
    fn nesting_draws_depth_first_with_a_group_after_its_children() {
        // Top level: x (bottom), g, y (top). g holds c1, h; h holds d1.
        let ls = vec![
            layer("y", "c", LayerKind::Vector, None),
            layer("g", "b", LayerKind::Group, None),
            layer("x", "a", LayerKind::Vector, None),
            layer("h", "b", LayerKind::Group, Some("g")),
            layer("c1", "a", LayerKind::Vector, Some("g")),
            layer("d1", "a", LayerKind::Paint, Some("h")),
        ];
        let t = resolve(&ls);
        assert_eq!(ids(&ls, &t), ["x", "c1", "d1", "h", "g", "y"]);
        let (g, _) = entry(&ls, &t, "g");
        let (h, he) = entry(&ls, &t, "h");
        let (_, d1) = entry(&ls, &t, "d1");
        assert_eq!(he.parent, Some(g));
        assert_eq!(d1.parent, Some(h));
        assert_eq!(d1.depth, 2);
        assert_eq!(t.roots.len(), 3);
        assert_eq!(t.descendants(g).len(), 3);
    }

    #[test]
    fn a_dangling_parent_or_a_parent_that_is_not_a_group_reads_as_top_level() {
        let ls = vec![
            layer("a", "a", LayerKind::Vector, Some("gone")),
            layer("b", "b", LayerKind::Vector, Some("a")),
        ];
        let t = resolve(&ls);
        assert_eq!(ids(&ls, &t), ["a", "b"]);
        assert!(t.entries.iter().all(|e| e.parent.is_none()));
    }

    #[test]
    fn a_cycle_reads_with_its_smallest_id_at_top_level() {
        // a in b, b in c, c in a; d in c. The smallest id on the cycle is a,
        // which reads as top level: a holds c, c holds b and d.
        let ls = vec![
            layer("c", "c", LayerKind::Group, Some("a")),
            layer("a", "a", LayerKind::Group, Some("b")),
            layer("b", "b", LayerKind::Group, Some("c")),
            layer("d", "d", LayerKind::Vector, Some("c")),
        ];
        let t = resolve(&ls);
        let (a, ae) = entry(&ls, &t, "a");
        let (b, be) = entry(&ls, &t, "b");
        let (c, ce) = entry(&ls, &t, "c");
        assert_eq!(ae.parent, None);
        assert_eq!(ce.parent, Some(a));
        assert_eq!(be.parent, Some(c));
        assert_eq!(entry(&ls, &t, "d").1.parent, Some(c));
        assert_eq!(ids(&ls, &t), ["b", "d", "c", "a"]);
        assert_eq!(b, 0);
        // The same from any order of the input.
        let mut rev = ls.clone();
        rev.reverse();
        let t2 = resolve(&rev);
        assert_eq!(ids(&rev, &t2), ["b", "d", "c", "a"]);
        // A two-cycle: the smaller id is top level.
        let two = vec![layer("q", "a", LayerKind::Group, Some("p")), layer("p", "b", LayerKind::Group, Some("q"))];
        let t3 = resolve(&two);
        assert_eq!(entry(&two, &t3, "p").1.parent, None);
        assert!(entry(&two, &t3, "q").1.parent.is_some());
    }

    #[test]
    fn visibility_and_locks_inherit_and_children_keep_their_own() {
        let mut ls = vec![
            layer("g", "a", LayerKind::Group, None),
            layer("c", "a", LayerKind::Vector, Some("g")),
            layer("d", "b", LayerKind::Vector, Some("g")),
            layer("top", "b", LayerKind::Vector, None),
        ];
        ls[0].visible = false;
        ls[0].locked = true;
        ls[2].locked = true;
        let t = resolve(&ls);
        let (g, ge) = entry(&ls, &t, "g");
        let (d, de) = entry(&ls, &t, "d");
        let (_, ce) = entry(&ls, &t, "c");
        assert!(!ge.effective_visible && ge.effective_locked && ge.locked_by == Some(g));
        assert!(!ce.effective_visible && ce.effective_locked && ce.locked_by == Some(g));
        // The group's lock is named first: unlocking the layer would not help.
        assert_eq!(de.locked_by, Some(g));
        assert!(!entry(&ls, &t, "top").1.effective_locked);
        // The group unlocked and shown: the children's own flags apply.
        ls[0].visible = true;
        ls[0].locked = false;
        let t = resolve(&ls);
        let (d2, de) = entry(&ls, &t, "d");
        assert_eq!(d2, d);
        assert!(de.effective_visible && de.effective_locked && de.locked_by == Some(d));
        assert!(!entry(&ls, &t, "c").1.effective_locked);
    }

    #[test]
    fn isolation_follows_blend_and_opacity() {
        let mut ls = vec![
            layer("g", "a", LayerKind::Group, None),
            layer("c", "a", LayerKind::Vector, Some("g")),
            layer("m", "b", LayerKind::Vector, None),
            layer("p", "c", LayerKind::Vector, None),
        ];
        ls[2].blend = Some(Blend::Multiply);
        ls[3].blend = Some(Blend::PassThrough);
        let t = resolve(&ls);
        let (g, ge) = entry(&ls, &t, "g");
        assert_eq!(ge.blend, Blend::PassThrough);
        assert!(!ge.isolated);
        assert!(entry(&ls, &t, "m").1.isolated);
        let (_, pe) = entry(&ls, &t, "p");
        assert_eq!(pe.blend, Blend::Normal, "pass-through on a layer reads as normal");
        assert!(!pe.isolated);
        let (c, _) = entry(&ls, &t, "c");
        assert!(t.chain(c).is_empty());
        ls[0].opacity = 0.5;
        let t = resolve(&ls);
        assert!(t.entries[g].isolated);
        assert_eq!(t.chain(c), vec![g]);
    }
}

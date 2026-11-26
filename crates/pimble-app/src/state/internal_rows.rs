//! The explorer's grouping rows for internal nodes (docs/THEMES_CONTRACT.md
//! section 3.1a, "Grouped by kind"; Joe, 2026-11-26: "Actually, not
//! Internal. Just Themes"): a shown internal node is drawn in a row of its
//! kind ("Themes") among its parent's rows, rather than among the parent's
//! other children. The grouping is the explorer's alone: nothing is stored,
//! the nodes stay where they are, and each parent that holds internal nodes
//! has its own group (a theme moved into a share shows under the share's
//! Themes).

use super::*;
use pimble_core::internal::InternalKind;

/// What every grouping row's tree value starts with.
pub const INTERNAL_ROW: &str = "internal_";

/// A grouping row of the explorer: one kind's row ("Themes") under a parent.
#[derive(Debug, Clone, PartialEq)]
pub struct InternalGroup {
    /// The node whose children it groups, as the tree lists them (a mount
    /// row's own id for a group shown through it).
    pub store_id: StoreId,
    pub parent_id: NodeId,
    /// The row of that node (a store row for a store's root), which is what
    /// a drop into the group lands in.
    pub parent_row: String,
    pub kind: InternalKind,
    /// The mount path the group's value and its rows' values carry
    /// (`/{mount_store}_{mount_node}` per mount crossed; empty for none).
    pub suffix: String,
    /// How many nodes it holds.
    pub count: usize,
}

thread_local! {
    /// The grouping rows of the explorer as last built, by tree value: what
    /// a row, a drop and a key ask of one ([`internal_group`]). Rebuilt with
    /// the tree's data.
    pub(super) static GROUP_ROWS: RefCell<HashMap<String, InternalGroup>> = RefCell::new(HashMap::new());
}

/// Whether `value` is a grouping row's (built or not).
pub fn is_internal_group(value: &str) -> bool {
    value.starts_with(INTERNAL_ROW)
}

/// The grouping row `value` is, as last built; `None` for every other row.
pub fn internal_group(value: &str) -> Option<InternalGroup> {
    GROUP_ROWS.with(|rows| rows.borrow().get(value).cloned())
}

/// The tree value of the grouping row of `(store_id, parent_id)`'s
/// children of `kind`, at the mount level `suffix` (as the children's own
/// values carry it).
pub fn internal_group_value(store_id: StoreId, parent_id: NodeId, kind: InternalKind, suffix: &str) -> String {
    format!("{INTERNAL_ROW}{store_id}_{parent_id}_{}{suffix}", kind.id())
}

/// A parent's children as the explorer draws them: the ordinary ones, and
/// the shown internal ones by kind (in `InternalKind::ALL`'s order). An
/// internal child of a kind not shown is in neither.
pub(super) struct SortedChildren {
    pub ordinary: Vec<(StoreId, NodeId)>,
    pub internal: Vec<(InternalKind, Vec<(StoreId, NodeId)>)>,
}

impl AppStore {
    /// `children` sorted as [`SortedChildren`] says, by the kinds shown.
    pub(super) fn sort_children(&self, children: &[(StoreId, NodeId)]) -> SortedChildren {
        let shown = self.internal_kinds_shown();
        let mut ordinary = Vec::with_capacity(children.len());
        let mut internal: Vec<(InternalKind, Vec<(StoreId, NodeId)>)> = Vec::new();
        for &(s, n) in children {
            let kind = self.get_node_signal(s, n).and_then(|sig| sig.with(|node| pimble_core::internal::internal_kind(&node.node_type)));
            match kind {
                None => ordinary.push((s, n)),
                Some(kind) if shown.contains(&kind) => match internal.iter_mut().find(|(k, _)| *k == kind) {
                    Some((_, rows)) => rows.push((s, n)),
                    None => internal.push((kind, vec![(s, n)])),
                },
                Some(_) => {}
            }
        }
        internal.sort_by_key(|(kind, _)| kind.index());
        SortedChildren { ordinary, internal }
    }
}

/// One row per kind of `(store_id, parent_id)` (whose row is `parent_row`)
/// at the mount level `suffix`, each holding that kind's rows, to go last
/// among the parent's rows. Each is recorded for [`internal_group`].
pub(super) fn group_rows(
    store_id: StoreId,
    parent_id: NodeId,
    parent_row: &str,
    suffix: &str,
    kinds: Vec<(InternalKind, Vec<TreeNodeData>)>,
) -> Vec<TreeNodeData> {
    kinds
        .into_iter()
        .map(|(kind, rows)| {
            let value = internal_group_value(store_id, parent_id, kind, suffix);
            let group = InternalGroup { store_id, parent_id, parent_row: parent_row.to_string(), kind, suffix: suffix.to_string(), count: rows.len() };
            GROUP_ROWS.with(|groups| groups.borrow_mut().insert(value.clone(), group));
            // The label carries the count, so the row redraws when it changes.
            TreeNodeData::new(value, format!("{}|{}", kind.name(), rows.len())).with_children(rows)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::a_store_with_themes;

    fn values(rows: &[TreeNodeData]) -> Vec<String> {
        rows.iter().map(|r| r.value.clone()).collect()
    }

    /// docs/THEMES_CONTRACT.md section 3.1a "Grouped by kind": hidden, the
    /// themes are nowhere; shown, they are under a Themes row of the parent
    /// they live in, last, in their own order, each its plain row, and every
    /// grouping row knows what it groups.
    #[test]
    fn shown_themes_are_grouped_under_themes_where_they_live() {
        let (app, t) = a_store_with_themes();
        let s = t.store_id;

        let hidden = untracked(|| app.build_tree_data_structural());
        assert_eq!(values(&hidden[0].children), [format!("node_{s}_{}", t.recipes), format!("node_{s}_{}", t.folder)]);
        assert!(hidden[0].children[1].children.is_empty(), "the folder's only child is a hidden theme");

        app.internal_shown[InternalKind::Theme.index()].set(true);
        let data = untracked(|| app.build_tree_data_structural());
        let top = &data[0].children;
        let themes = internal_group_value(s, t.root, InternalKind::Theme, "");
        assert_eq!(values(top), [format!("node_{s}_{}", t.recipes), format!("node_{s}_{}", t.folder), themes.clone()]);
        assert_eq!(values(&top[2].children), [format!("node_{s}_{}", t.book), format!("node_{s}_{}", t.code)]);

        let group = internal_group(&themes).expect("recorded");
        assert_eq!((group.store_id, group.parent_id, group.kind, group.count), (s, t.root, InternalKind::Theme, 2));
        assert_eq!((group.parent_row, group.suffix), (format!("store_{s}"), String::new()));
        assert!(is_internal_group(&themes) && parse_tree_value(&themes).is_none(), "no node's row");

        // The folder's theme is in the folder's own group.
        let under_folder = &top[1].children;
        let folder_themes = internal_group_value(s, t.folder, InternalKind::Theme, "");
        assert_eq!(values(under_folder), [folder_themes.clone()]);
        assert_eq!(values(&under_folder[0].children), [format!("node_{s}_{}", t.inner)]);
        assert_eq!(internal_group(&folder_themes).map(|g| g.parent_row), Some(format!("node_{s}_{}", t.folder)));
        assert!(internal_group(&format!("node_{s}_{}", t.book)).is_none());

        // Hidden again: no group is left to ask about.
        app.internal_shown[InternalKind::Theme.index()].set(false);
        untracked(|| app.build_tree_data_structural());
        assert!(internal_group(&themes).is_none());
    }

    /// Through a mount the source's themes are grouped under the mount
    /// row, at the mount's level, and a drop into that group lands in the
    /// mount's row (which moves into the source, as any drop into a mount).
    #[test]
    fn a_mounts_themes_are_grouped_under_the_mount() {
        let (app, t) = a_store_with_themes();
        let s = t.store_id;
        let mount = Node::document("Mounted");
        let m = mount.id;
        app.upsert_node(s, mount);
        app.set_children(s, t.root, vec![(s, t.recipes), (s, m)]);
        let source = pimble_core::MountRef { source_store: s, source_node: t.folder, source_path: None, source_remote: None };
        app.set_mount_state(s, m, pimble_core::MountState::Live, source);
        app.set_children(s, m, vec![(s, t.inner)]);
        app.internal_shown[InternalKind::Theme.index()].set(true);

        let data = untracked(|| app.build_tree_data_structural());
        let mount_row = &data[0].children[1];
        assert_eq!(mount_row.value, format!("node_{s}_{m}"));
        let at = format!("/{s}_{m}");
        let themes = internal_group_value(s, m, InternalKind::Theme, &at);
        assert_eq!(values(&mount_row.children), [themes.clone()]);
        assert_eq!(values(&mount_row.children[0].children), [format!("node_{s}_{}{at}", t.inner)]);
        let group = internal_group(&themes).unwrap();
        assert_eq!((group.parent_row, group.suffix), (mount_row.value.clone(), at));
    }

    /// A group carries the mount path of the level it is at, as the rows
    /// beside it do, so one parent's group in two places is two rows.
    #[test]
    fn a_groups_value_carries_its_mount_path() {
        let (s, p, m_store, m_node) = (StoreId::new(), NodeId::new(), StoreId::new(), NodeId::new());
        let at = format!("/{m_store}_{m_node}");
        assert_eq!(internal_group_value(s, p, InternalKind::Theme, &at), format!("internal_{s}_{p}_themes{at}"));
        assert_ne!(internal_group_value(s, p, InternalKind::Theme, ""), internal_group_value(s, p, InternalKind::Theme, &at));
    }
}

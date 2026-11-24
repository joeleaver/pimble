//! The rows of the explorer a query metanode found
//! (docs/QUERY_METANODES_CONTRACT.md section 4): what each says, and the
//! record of them the row menus, drops and keys ask.

use super::*;

/// One node a query metanode found, as the explorer shows it
/// (docs/QUERY_METANODES_CONTRACT.md section 4).
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub store_id: StoreId,
    pub node_id: NodeId,
    /// Where it lives: "Personal / Projects".
    pub lives_in: String,
    /// What a refused action on its row says: "Shown here because it is
    /// tagged #waiting. It lives in Personal / Projects."
    pub refusal: String,
}

/// A row of the explorer that a query metanode found: the real node, shown
/// where it does not live. Its place cannot be changed from there
/// (`pimble_core::plan::FoundRowAction`).
#[derive(Debug, Clone, PartialEq)]
pub struct FoundRow {
    /// The metanode that found it.
    pub metanode: (StoreId, NodeId),
    /// The metanode's own row (where a node made beside the found row goes).
    pub metanode_row: String,
    pub lives_in: String,
    pub refusal: String,
}

thread_local! {
    /// The found rows of the explorer as last built, by tree value: what the
    /// row menus, drops and keys ask whether a row is found
    /// ([`found_row`]). Rebuilt with the tree's data.
    pub(super) static FOUND_ROWS: RefCell<HashMap<String, FoundRow>> = RefCell::new(HashMap::new());
}

/// The found row `value` is, or `None` for every other row: one a query
/// metanode found (docs/QUERY_METANODES_CONTRACT.md section 4). A found
/// row's own children are ordinary rows.
pub fn found_row(value: &str) -> Option<FoundRow> {
    FOUND_ROWS.with(|rows| rows.borrow().get(value).cloned())
}

/// The first of `values` that is a found row, with what it says when its
/// place would change: what refuses a drag, a drop, a cut or a delete of
/// several rows at once.
pub fn first_found(values: &[String]) -> Option<FoundRow> {
    values.iter().find_map(|v| found_row(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::store_with_a_child;

    /// A query metanode with one own child, finding `Recipes` (which lives under
    /// the root) and `Smart` (a query metanode itself, finding `Deep`).
    fn a_query_metanode() -> (AppStore, StoreId, NodeId, NodeId, NodeId, NodeId, NodeId) {
        use pimble_core::metanode::{Condition, Metanode, Source};
        let (app, s, recipes) = store_with_a_child();
        let root = app.root_node_id(s).unwrap();
        let query = || Some(Metanode { viewer: "list".into(), source: Source::matching(vec![Condition::Tag { tag: "x".into() }]) });
        let mut list = Node::document("Waiting");
        list.metadata.metanode = query();
        let own = Node::document("Own");
        let mut smart = Node::document("Smart");
        smart.metadata.metanode = query();
        let deep = Node::document("Deep");
        let (list_id, own_id, smart_id, deep_id) = (list.id, own.id, smart.id, deep.id);
        for n in [list, own, smart, deep] {
            app.upsert_node(s, n);
        }
        app.set_children(s, root, vec![(s, recipes), (s, list_id)]);
        app.set_children(s, list_id, vec![(s, own_id)]);
        app.set_children(s, smart_id, Vec::new());
        let found = |n: NodeId| Found { store_id: s, node_id: n, lives_in: "Notes".into(), refusal: format!("Shown here because it is tagged #x. It lives in Notes ({n}).") };
        app.finds.set(HashMap::from([((s, list_id), vec![found(recipes), found(smart_id)]), ((s, smart_id), vec![found(deep_id)])]));
        (app, s, recipes, list_id, own_id, smart_id, deep_id)
    }

    /// docs/QUERY_METANODES_CONTRACT.md section 4: a query metanode's row
    /// has its own children, then its finds, each a row of its own reached
    /// through the metanode (so the same node also sits in its own place);
    /// a found row knows it is one and what it says; a found metanode shows
    /// its own finds, and no deeper.
    #[test]
    fn a_query_metanode_shows_its_finds_after_its_own_children() {
        let (app, s, recipes, list, own, smart, deep) = a_query_metanode();
        let data = untracked(|| app.build_tree_data_structural());
        let top = &data[0].children;
        assert_eq!(top[0].value, format!("node_{s}_{recipes}"), "in its own place");
        let list_row = &top[1];
        let values: Vec<&str> = list_row.children.iter().map(|c| c.value.as_str()).collect();
        let found_recipes = format!("node_{s}_{recipes}/{s}_{list}");
        let found_smart = format!("node_{s}_{smart}/{s}_{list}");
        assert_eq!(values, vec![format!("node_{s}_{own}").as_str(), found_recipes.as_str(), found_smart.as_str()]);
        assert_eq!(parse_tree_value(&found_recipes), Some((s, Some(recipes))), "the real node");

        assert!(found_row(&format!("node_{s}_{own}")).is_none(), "an own child is ordinary");
        assert!(found_row(&top[0].value).is_none(), "its own place is ordinary");
        let row = found_row(&found_recipes).expect("found");
        assert_eq!(row.metanode, (s, list));
        assert_eq!(row.metanode_row, list_row.value);
        assert!(list_row.children[1].label.contains("found: Shown here because it is tagged #x."), "the row's data carries it");

        // The found metanode shows its finds one level down, through itself.
        let smart_row = &list_row.children[2];
        let deep_value = format!("node_{s}_{deep}/{s}_{list}/{s}_{smart}");
        assert_eq!(smart_row.children.iter().map(|c| c.value.clone()).collect::<Vec<_>>(), vec![deep_value.clone()]);
        assert_eq!(found_row(&deep_value).map(|f| f.metanode), Some((s, smart)));
        // ...and a metanode found two finds down shows none of its own.
        app.finds.update(|f| {
            f.insert((s, deep), vec![Found { store_id: s, node_id: own, lives_in: String::new(), refusal: String::new() }]);
        });
        let data = untracked(|| app.build_tree_data_structural());
        let deep_row = &data[0].children[1].children[2].children[0];
        assert!(deep_row.children.is_empty(), "no recursion through finds");
        assert_eq!(first_found(&[format!("node_{s}_{own}"), found_recipes.clone()]).map(|f| f.metanode), Some((s, list)));
    }

    /// A found row's own children are its, reached through it: ordinary rows.
    #[test]
    fn a_found_rows_children_are_ordinary() {
        let (app, s, recipes, list, ..) = a_query_metanode();
        let leaf = Node::document("Leaf");
        let leaf_id = leaf.id;
        app.upsert_node(s, leaf);
        app.set_children(s, recipes, vec![(s, leaf_id)]);
        let data = untracked(|| app.build_tree_data_structural());
        let found = &data[0].children[1].children[1];
        let child = &found.children[0].value;
        assert_eq!(*child, format!("node_{s}_{leaf_id}/{s}_{list}/{s}_{recipes}"));
        assert!(found_row(child).is_none());
        // In its own place the same child is its plain value.
        assert_eq!(data[0].children[0].children[0].value, format!("node_{s}_{leaf_id}"));
    }
}

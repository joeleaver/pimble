//! What points at the node (docs/PROPERTIES_CONTRACT.md section 5, wave 3):
//! the Metanodes that find it, what links to it, the table rows that choose
//! it. It comes from `BackendCommand::GetReferences`, asked when the panel
//! shows another node and again once changes anywhere have rested
//! (`store_changed`).

use std::cell::RefCell;

use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;

use super::{open, shown};
use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// How long changes must rest before the references are asked again.
const REFERENCES_REST_MS: u32 = 800;
/// When they are asked once more after that: a server's search index, which
/// "Links here" reads, takes in a node's text 2 s after it rests.
const INDEX_LAG_MS: u32 = 2500;

/// A node the references name, by the label its row shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Mention {
    pub store_id: StoreId,
    pub node_id: NodeId,
    pub label: String,
}

/// One table whose rows choose the node: the node it is in, its name, and
/// the rows by their first column's value.
#[derive(Debug, Clone, PartialEq)]
pub struct RowsMention {
    pub at: Mention,
    pub table_name: String,
    pub rows: Vec<String>,
}

/// What points at a node (docs/PROPERTIES_CONTRACT.md section 5, wave 3),
/// as `BackendEvent::ReferencesLoaded` answers it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct References {
    /// The Metanodes whose queries find it.
    pub shown_in: Vec<Mention>,
    /// The nodes whose text links to it.
    pub links_here: Vec<Mention>,
    /// The tables whose link columns choose it, grouped by table.
    pub rows_here: Vec<RowsMention>,
    /// Why a part could not be read, said under the sections.
    pub not_known: Vec<String>,
}

thread_local! {
    /// The node the references were last asked for, and the debounce's
    /// count: a change bumps it, and only the last timer asks.
    static REFERENCES_ASKED: RefCell<Option<(StoreId, NodeId)>> = const { RefCell::new(None) };
    static REFERENCES_WAIT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Ask for what points at `(s, n)`.
fn ask_references(store: AppStore, s: StoreId, n: NodeId) {
    REFERENCES_ASKED.with(|a| *a.borrow_mut() = Some((s, n)));
    store.send(BackendCommand::GetReferences { store_id: s, node_id: n });
}

/// The panel closed: opened again, it asks again.
pub(super) fn forget_asked() {
    REFERENCES_ASKED.with(|a| *a.borrow_mut() = None);
}

/// The panel shows `(s, n)`: what points at it is asked at once when it is
/// another node than last asked (changes ask again once they rest,
/// `store_changed`).
pub(super) fn ask_if_another(store: AppStore, s: StoreId, n: NodeId) {
    if REFERENCES_ASKED.with(|a| *a.borrow() != Some((s, n))) {
        ask_references(store, s, n);
    }
}

/// Something changed in a store (any store: a Metanode, a link or a table
/// elsewhere may now point at the node shown): once changes rest, the
/// references of the node the panel shows are asked again. Nothing while the
/// panel is closed.
pub fn store_changed(store: AppStore) {
    if !untracked(|| store.properties_open.get()) || REFERENCES_ASKED.with(|a| a.borrow().is_none()) {
        return;
    }
    let wait = REFERENCES_WAIT.with(|w| {
        w.set(w.get() + 1);
        w.get()
    });
    // Asked when changes rest, and once more when the index has them too;
    // a newer change makes both its own.
    for after in [REFERENCES_REST_MS, REFERENCES_REST_MS + INDEX_LAG_MS] {
        rinch::core::reactive::unowned(|| {
            set_timeout(after, move || {
                if REFERENCES_WAIT.with(|w| w.get()) != wait || !untracked(|| store.properties_open.get()) {
                    return;
                }
                if let Some((s, n)) = untracked(|| shown(store)) {
                    ask_references(store, s, n);
                }
            })
        });
    }
}

/// `BackendEvent::ReferencesLoaded`: kept when it is about the node shown.
pub fn references_loaded(store: AppStore, s: StoreId, n: NodeId, references: References) {
    if untracked(|| shown(store)) == Some((s, n)) {
        store.properties_references.set(Some((s, n, references)));
    }
}

/// A group of rows pointing here, as its head says it: "Budget in Projects".
pub(crate) fn table_line(g: &RowsMention) -> String {
    let table = if g.table_name.trim().is_empty() { "A table" } else { g.table_name.trim() };
    format!("{table} in {}", g.at.label)
}

/// The rows of a group, by their first column's value: "Alpha, Beta", or
/// "2 rows" when none has one to say.
pub(crate) fn rows_line(rows: &[String]) -> String {
    let named: Vec<&str> = rows.iter().map(|r| r.trim()).filter(|r| !r.is_empty()).collect();
    let count = if rows.len() == 1 { "1 row".to_string() } else { format!("{} rows", rows.len()) };
    if named.is_empty() {
        return count;
    }
    let mut line = named.iter().take(8).copied().collect::<Vec<_>>().join(", ");
    if named.len() > 8 {
        line.push_str(&format!(", … ({count})"));
    } else if named.len() < rows.len() {
        line.push_str(&format!(" ({count})"));
    }
    line
}

/// The panel's "Shown in", "Links here" and "Rows pointing here" sections,
/// in that order.
pub(super) fn render_references(__scope: &mut RenderScope, store: AppStore) -> [NodeHandle; 3] {
    let references = move || -> Option<References> {
        let here = shown(store)?;
        store.properties_references.with(|r| r.as_ref().filter(|(s, n, _)| (*s, *n) == here).map(|(_, _, r)| r.clone()))
    };
    // A section's line when it lists nothing: "…" until the first answer.
    let none_yet = move |empty: fn(&References) -> bool| -> Option<&'static str> {
        match references() {
            None => Some("…"),
            Some(r) if empty(&r) => Some("None"),
            Some(_) => None,
        }
    };
    let shown_in = rsx! {
        div {
            class: "pimble-properties__section",
            div {
                class: "pimble-properties__heading pimble-properties__heading--line",
                "Shown in"
                span { class: "pimble-properties__count", {move || none_yet(|r| r.shown_in.is_empty()).unwrap_or("").to_string()} }
            }
            for m in references().map(|r| r.shown_in).unwrap_or_default() {
                div {
                    key: format!("{} {} {}", m.store_id, m.node_id, m.label),
                    class: "pimble-properties__ref",
                    span {
                        class: "pimble-properties__link",
                        onclick: {
                            let (s, n) = (m.store_id, m.node_id);
                            move || open(store, s, n)
                        },
                        {m.label.clone()}
                    }
                }
            }
        }
    };
    let links_here = rsx! {
        div {
            class: "pimble-properties__section",
            div {
                class: "pimble-properties__heading pimble-properties__heading--line",
                "Links here"
                span { class: "pimble-properties__count", {move || none_yet(|r| r.links_here.is_empty()).unwrap_or("").to_string()} }
            }
            for m in references().map(|r| r.links_here).unwrap_or_default() {
                div {
                    key: format!("{} {} {}", m.store_id, m.node_id, m.label),
                    class: "pimble-properties__ref",
                    span {
                        class: "pimble-properties__link",
                        onclick: {
                            let (s, n) = (m.store_id, m.node_id);
                            move || open(store, s, n)
                        },
                        {m.label.clone()}
                    }
                }
            }
        }
    };
    let rows_here = rsx! {
        div {
            class: "pimble-properties__section",
            div {
                class: "pimble-properties__heading pimble-properties__heading--line",
                "Rows pointing here"
                span { class: "pimble-properties__count", {move || none_yet(|r| r.rows_here.is_empty()).unwrap_or("").to_string()} }
            }
            for (i, g) in references().map(|r| r.rows_here).unwrap_or_default().into_iter().enumerate() {
                div {
                    key: format!("{i} {} {} {} {}", g.at.node_id, g.table_name, g.at.label, g.rows.join("\u{1f}")),
                    class: "pimble-properties__ref",
                    div {
                        class: "pimble-properties__ref-head",
                        span {
                            class: "pimble-properties__link",
                            onclick: {
                                let (s, n) = (g.at.store_id, g.at.node_id);
                                move || open(store, s, n)
                            },
                            {table_line(&g)}
                        }
                    }
                    div { class: "pimble-properties__rows", {rows_line(&g.rows)} }
                }
            }
            div {
                class: "pimble-properties__through",
                style: {move || if references().is_some_and(|r| !r.not_known.is_empty()) { "" } else { "display: none;" }},
                {move || references().map(|r| r.not_known.join(" ")).unwrap_or_default()}
            }
        }
    };
    [shown_in, links_here, rows_here]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn a_group_of_rows_says_its_table_and_rows() {
        let at = Mention { store_id: StoreId::new(), node_id: NodeId::new(), label: "Projects".into() };
        let g = RowsMention { at, table_name: "Budget".into(), rows: tags(&["Alpha", "Beta"]) };
        assert_eq!(table_line(&g), "Budget in Projects");
        assert_eq!(table_line(&RowsMention { table_name: " ".into(), ..g.clone() }), "A table in Projects");
        assert_eq!(rows_line(&g.rows), "Alpha, Beta");
        assert_eq!(rows_line(&tags(&["", ""])), "2 rows", "rows with nothing to say are counted");
        assert_eq!(rows_line(&tags(&["Alpha", ""])), "Alpha (2 rows)");
        let many: Vec<String> = (1..=10).map(|i| format!("r{i}")).collect();
        assert_eq!(rows_line(&many), "r1, r2, r3, r4, r5, r6, r7, r8, … (10 rows)");
    }
}

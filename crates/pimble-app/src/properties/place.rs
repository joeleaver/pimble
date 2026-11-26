//! Where the node lives: the "Lives in" path from its store's top, how the
//! focused pane reached it (a mount, a Metanode), the other computers that
//! open its store's folder, and when it was created and changed.

use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;

use super::{open, panel_context, panel_node, shown};
use crate::state::AppStore;

/// A node's created and changed times, in this device's local time.
pub(crate) fn when(at: chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local).format("%-d %b %Y, %H:%M").to_string()
}

/// How the focused pane reached its node, when not through its own place:
/// "Shown through the mount Recipes." / "Found by Waiting."
fn reached_through(store: AppStore) -> Option<String> {
    let pane = store.focused_pane.get();
    let value = store.pane(pane).selected.get()?;
    if let Some(found) = crate::state::found_row(&value) {
        let (s, n) = found.metanode;
        let name = untracked(|| store.get_node_signal(s, n).map(|sig| sig.with(crate::state::display_label_from_node)));
        return Some(format!("Found by {}.", name.unwrap_or_else(|| "a Metanode".to_string())));
    }
    // The innermost mount crossed is the last segment of the value's path.
    let (_, rest) = value.split_once('/')?;
    let last = rest.rsplit('/').next()?;
    let (s, n) = last.split_once('_')?;
    let (s, n) = (StoreId(s.parse().ok()?), NodeId(n.parse().ok()?));
    let (name, metanode) = untracked(|| {
        store.get_node_signal(s, n).map(|sig| sig.with(|node| (crate::state::display_label_from_node(node), crate::metanodes::is_metanode(node))))
    })?;
    Some(if metanode { format!("Shown by the Metanode {name}.") } else { format!("Shown through the mount {name}.") })
}

/// The panel's "Lives in" section.
pub(super) fn render_lives_in(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let context = move || panel_context(store);
    let steps = move || -> Vec<(StoreId, NodeId, String)> {
        let Some((s, n)) = shown(store) else { return Vec::new() };
        let store_sig = store.store_data.with(|map| map.get(&s).copied());
        let Some((store_name, root)) = store_sig.map(|sig| sig.with(|st| (st.name.clone(), st.root_node_id))) else { return Vec::new() };
        let mut out = vec![(s, root, store_name)];
        if let Some(c) = context() {
            out.extend(c.lives_in.into_iter().filter(|(id, _)| *id != root && *id != n).map(|(id, label)| (s, id, label)));
        }
        out
    };
    // Other computers that open the store's folder through a sync tool, and
    // an older Pimble among them asked to be updated
    // (docs/STORE_FOLDERS_CONTRACT.md rule 5).
    let elsewhere = move || -> Vec<String> {
        let Some((s, _)) = shown(store) else { return Vec::new() };
        let Some(sig) = store.store_data.with(|map| map.get(&s).copied()) else { return Vec::new() };
        let now = chrono::Utc::now();
        sig.with(|st| st.also_open_on.iter().flat_map(|other| std::iter::once(other.sentence(now)).chain(other.update_sentence())).collect())
    };
    rsx! {
        div {
            class: "pimble-properties__section",
            div { class: "pimble-properties__heading", "Lives in" }
            div {
                class: "pimble-properties__path",
                for (i, (s, n, label)) in steps().into_iter().enumerate() {
                    span {
                        key: format!("{i} {n} {label}"),
                        class: "pimble-properties__step",
                        span { class: "pimble-properties__sep", style: if i == 0 { "display: none;" } else { "" }, " / " }
                        span {
                            class: "pimble-properties__link",
                            onclick: move || open(store, s, n),
                            {label.clone()}
                        }
                    }
                }
            }
            div {
                class: "pimble-properties__through",
                style: {move || if reached_through(store).is_some() { "" } else { "display: none;" }},
                {move || reached_through(store).unwrap_or_default()}
            }
            for line in elsewhere() {
                div {
                    key: line.clone(),
                    class: "pimble-properties__through",
                    {line.clone()}
                }
            }
        }
    }
}

/// The panel's last line: when the node was created and last changed.
pub(super) fn render_times(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let node = move || panel_node(store);
    rsx! {
        div {
            class: "pimble-properties__section",
            div {
                class: "pimble-properties__times",
                {move || node().map(|n| format!("Created {} · Changed {}", when(n.created_at), when(n.modified_at))).unwrap_or_default()}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_shown_in_local_time() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-04T12:30:00Z").unwrap().with_timezone(&chrono::Utc);
        let local = at.with_timezone(&chrono::Local);
        assert_eq!(when(at), local.format("%-d %b %Y, %H:%M").to_string());
        assert!(when(at).contains("2026"));
    }
}

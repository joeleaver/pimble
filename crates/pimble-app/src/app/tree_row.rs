//! One explorer row, drawn for rinch's Tree.

use rinch::prelude::*;
use rinch_tabler_icons::{TablerIcon, TablerIconStyle, render_tabler_icon};

use crate::appearance::{display_color, icon_by_name, row_background, IconGlyph};
use crate::state::{parse_tree_value, display_label_from_node, mount_is_dimmed, mount_label_suffix, AppStore};
use pimble_ui::presence::PersonDot;
use super::explorer::{RENAME_INPUTS, commit_rename, mark_fresh, start_rename};
use super::row_menu::ExplorerMenu;
use super::rows::{REVEAL_ROW, TREE_ROWS};
use super::status::{badge_with_waiting, sync_badge_text};

/// The row of a saved store still opening, or one that did not open
/// (docs/STARTUP_CONTRACT.md "Each saved store has its row at once"): the
/// store row's look, a spinner while it opens, and why not once it did not.
fn opening_row_view(__scope: &mut RenderScope, opening: &crate::state::OpeningStore) -> NodeHandle {
    let icon_el = render_tabler_icon(__scope, if opening.error.is_some() { TablerIcon::DatabaseX } else { TablerIcon::Database }, TablerIconStyle::Outline);
    let failed = opening.error.is_some();
    let words = opening.row_words();
    let tooltip = opening.row_tooltip();
    let name = opening.name.clone();
    rsx! {
        span {
            class: "rinch-tree__label",
            style: "flex: 1; min-width: 0; display: inline-flex; align-items: center;",
            title: tooltip,
            span { class: "rinch-tree__icon", {icon_el} }
            span {
                style: "cursor: default; font-weight: 600; text-transform: uppercase; font-size: 11px; letter-spacing: 0.03em; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; opacity: 0.7;",
                {name}
            }
            span {
                class: "pimble-tree__opening-spinner",
                style: if failed { "display: none;" } else { "" },
                Loader { size: "xs" }
            }
            span {
                class: if failed { "pimble-tree__opening pimble-tree__opening--failed" } else { "pimble-tree__opening" },
                {words}
            }
        }
    }
}

/// One explorer row: the Tree's `render_node`, with per-node reactive
/// effects for its label, icon and mount state.
pub(super) fn render_tree_row(__scope: &mut RenderScope, store: AppStore, tree_state: UseTreeReturn, menu: ExplorerMenu, payload: &RenderTreeNodePayload) -> NodeHandle {
    let node_value = payload.node.value.clone();
    // A saved store still opening (docs/STARTUP_CONTRACT.md): its
    // name from the saved list, a spinner and "opening...", or why
    // it did not open. A snapshot: its data carries the error.
    if let Some(opening) = store.opening_row(&node_value) {
        return opening_row_view(__scope, &opening);
    }
    // A grouping row over shown internal nodes ("Themes"): no node.
    if let Some(group) = crate::state::internal_group(&node_value) {
        return super::internal_row::internal_group_view(__scope, store, tree_state, menu, &node_value, &group);
    }
    let is_store_root = node_value.starts_with("store_");
    let has_children = payload.has_children;

    // Parse once for reuse
    let parsed = parse_tree_value(&node_value);
    // A row a query metanode found (docs/QUERY_METANODES_CONTRACT.md
    // section 4): the real node, shown where it does not live. Its
    // place is not changed from here (`FoundRowAction`), and the
    // menu's top line says why. A snapshot: the row's data carries
    // it, so the row re-renders when it changes.
    let found = crate::state::found_row(&node_value);
    let found_on = found.is_some();
    let level = payload.level;

    // Determine if this node is a mount point (static — mount status is structural)
    let is_mount = if !is_store_root {
        if let Some((s_id, Some(n_id))) = parsed {
            store.is_mount(s_id, n_id)
        } else {
            false
        }
    } else {
        false
    };

    // Capture per-entity signals for reactive label/icon Effects.
    // These are looked up once (untracked) and the inner Signal is captured.
    let node_sig = if !is_store_root {
        if let Some((s_id, Some(n_id))) = parsed {
            store.get_node_signal(s_id, n_id)
        } else {
            None
        }
    } else {
        None
    };

    let store_sig = if is_store_root {
        parsed.and_then(|(s_id, _)| store.get_store_signal(s_id))
    } else {
        None
    };

    let mount_sig = if is_mount {
        if let Some((s_id, Some(n_id))) = parsed {
            store.get_mount_signal(s_id, n_id)
        } else {
            None
        }
    } else {
        None
    };

    // Sync status (store roots only, docs/SYNC_CONTRACT.md "B: app
    // side"): looked up once (an entry always exists by the time a
    // registered store's row renders — see `ensure_sync_entry`) and
    // read reactively below for the badge.
    let sync_sig = if is_store_root {
        parsed.and_then(|(s_id, _)| store.get_sync_signal(s_id))
    } else {
        None
    };

    // Whether the row's node carries a share marker (a store row takes
    // its root's), for the badge. What the menu may do with a row is
    // judged when the menu opens on it (`RowMenu::of`).
    let caps = parsed
        .map(|(s_id, n_id)| pimble_core::plan::capabilities(&store.view(), s_id, n_id))
        .unwrap_or_default();
    let is_shared_now = caps.shared_root;

    // Choose icon (static — changes only on structural rebuild). Folders
    // are folders even when empty; documents are documents even with
    // children, so the icon follows node_type, not has_children.
    let is_folder = node_sig
        .map(|sig| sig.with(|n| n.node_type == pimble_core::node_types::FOLDER))
        .unwrap_or(has_children);
    // A custom icon or colour from the node's metadata (the "Appearance..."
    // picker, or an import); a store row takes its root node's, and a
    // partial replica's row has none. Snapshotted here like the type
    // icon: the row's `TreeNodeData` carries both, so a change
    // re-renders the row.
    let appearance_sig = if is_store_root {
        parsed.and_then(|(s_id, _)| store.store_row_node(s_id).and_then(|root| store.get_node_signal(s_id, root)))
    } else {
        node_sig
    };
    // A query metanode, a query metanode: violet, its own icon unless
    // the person chose one (section 6).
    let is_query = !is_store_root && node_sig.is_some_and(|sig| sig.with(crate::metanodes::is_query));
    // A metanode with no icon of its own shows its viewer's, and a node a
    // child viewer shows (a kanban's column) that viewer's.
    let viewer_icon = if is_store_root {
        None
    } else {
        let child_icon = |n: &pimble_core::Node| parsed.and_then(|(s_id, _)| store.child_viewer(s_id, n)).and_then(|c| c.icon);
        node_sig
            .and_then(|sig| sig.with(|n| match n.metadata.metanode.as_ref() {
                Some(m) => store.viewer_icon(&m.viewer),
                None => child_icon(n),
            }))
            .and_then(|name| icon_by_name(&name))
    };
    let (custom_icon, node_color, row_color) = appearance_sig
        .map(|sig| {
            sig.with(|n| {
                (
                    n.metadata.icon().and_then(icon_by_name),
                    n.metadata.color().map(String::from),
                    n.metadata.background().map(String::from),
                )
            })
        })
        .unwrap_or((None, None, None));
    // An internal node shown (View > "Show Internal", docs/THEMES_CONTRACT.md
    // section 3.1a) is dimmed and marked with its kind's icon.
    let internal = if is_store_root { None } else { node_sig.and_then(|sig| sig.with(|n| pimble_core::internal::internal_kind(&n.node_type))) };
    let icon = if let Some(custom) = custom_icon {
        custom
    } else if let Some(kind) = internal {
        icon_by_name(kind.icon()).unwrap_or(TablerIcon::File)
    } else if is_store_root {
        TablerIcon::Database
    } else if is_mount {
        TablerIcon::Link
    } else if is_query {
        TablerIcon::FilterSearch
    } else if let Some(icon) = viewer_icon {
        icon
    } else if is_folder {
        TablerIcon::Folder
    } else {
        TablerIcon::File
    };
    // The CSS for the node's colour on the current theme; reactive on the
    // theme so a toggle recolours rows in place. One clone per closure
    // (rinch: two closures cannot share one moved String).
    let color_css = move |color: &Option<String>| -> String {
        color.as_ref().map(|c| format!(" color: {};", display_color(c, store.dark_mode.get()))).unwrap_or_default()
    };
    // A query metanode's icon is violet unless the person colored
    // the node; then a small violet mark on the icon says it still
    // gathers.
    let query_mark = is_query && node_color.is_some();
    let query_violet = is_query && node_color.is_none();
    let icon_color = node_color.clone();
    let label_color = node_color;

    let icon_class = if is_mount {
        "rinch-tree__icon rinch-tree__icon--mount"
    } else {
        "rinch-tree__icon"
    };

    let base_label_style = if is_store_root {
        "cursor: default; font-weight: 600; text-transform: uppercase; font-size: 11px; letter-spacing: 0.03em; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;"
    } else {
        // A long title ends in an ellipsis at the row's end, rather
        // than running on under the scrollbar and past the explorer.
        "cursor: default; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;"
    };

    let icon_el = render_tabler_icon(__scope, icon, TablerIconStyle::Outline);

    // The badge a shared node's row carries after its label. Drawn
    // from the render-time snapshot, like the type icon: the row's
    // data holds the marker, so it re-renders when one appears or goes
    // (docs/NODE_DOCUMENT_CONTRACT.md section 5).
    let share_badge: Option<NodeHandle> = if is_shared_now {
        let badge_icon = render_tabler_icon(__scope, TablerIcon::Share, TablerIconStyle::Outline);
        Some(rsx! {
            span {
                class: "pimble-tree__share",
                title: "Shared with other people",
                {badge_icon}
            }
        })
    } else {
        None
    };

    // A note out of step on this device says so on its row
    // (docs/CONVERGENCE_CONTRACT.md section 4), the reason and since
    // when as the tooltip. Drawn from the render-time snapshot: the
    // row's data carries the mark (`out_of_step::row_snapshot`).
    let out_of_step_mark: Option<NodeHandle> = match parsed {
        Some((s_id, Some(n_id))) if !is_store_root => untracked(|| store.out_of_step_shown(s_id, n_id)).map(|entry| {
            let mark = render_tabler_icon(__scope, TablerIcon::AlertTriangle, TablerIconStyle::Outline);
            rsx! {
                span {
                    class: "pimble-tree__out-of-step",
                    title: crate::out_of_step::row_tooltip(&entry),
                    {mark}
                }
            }
        }),
        _ => None,
    };

    // A found row's arrow at its end: it lives elsewhere, and the
    // tooltip says where (section 6).
    let found_arrow: Option<NodeHandle> = match &found {
        Some(f) => {
            let arrow = render_tabler_icon(__scope, TablerIcon::ArrowUpRight, TablerIconStyle::Outline);
            let title = if f.lives_in.is_empty() { "Lives elsewhere".to_string() } else { format!("Lives in {}", f.lives_in) };
            Some(rsx! {
                span {
                    class: "pimble-tree__found",
                    title: title,
                    style: {move || format!("color: {};", crate::appearance::gathered_violet(store.dark_mode.get()))},
                    {arrow}
                }
            })
        }
        None => None,
    };

    // A store someone else shared with this account says so on its
    // row, and what it lets this device do. Reactive on the store's
    // own signal so it appears as soon as the store arrives.
    let shared_by_note: Option<NodeHandle> = if is_store_root {
        // The sidebar is narrow enough to clip this, so the same
        // sentence is the row's tooltip.
        // Once every share of it has ended the words are "no longer
        // shared", in place of whose it was and what it allowed.
        let shared_by_text = move || -> String {
            store_sig.map(|sig| sig.with(crate::state::shared_by_words)).unwrap_or_default()
        };
        Some(rsx! {
            span {
                class: "pimble-tree__shared-by",
                style: {
                    move || {
                        if shared_by_text().is_empty() { "display: none;" } else { "" }
                    }
                },
                title: {move || shared_by_text()},
                {move || shared_by_text()}
            }
        })
    } else {
        None
    };

    // While a store's search index opens or is built, its row says
    // so, quietly (docs/STARTUP_CONTRACT.md "Indexing is said where
    // search is"): reactive on the store's own signal, no rebuild.
    let index_note: Option<NodeHandle> = if is_store_root {
        let words = move || store_sig.map(|sig| sig.with(|s| s.index.row_words())).unwrap_or_default();
        Some(rsx! {
            span {
                class: "pimble-tree__indexing",
                style: {move || if words().is_empty() { "display: none;" } else { "" }},
                title: {move || store_sig.map(|sig| sig.with(|s| s.index.row_tooltip())).unwrap_or_default()},
                {move || words()}
            }
        })
    } else {
        None
    };

    // Children asked for and slow to come: a spinner where the
    // chevron goes (the row's data has no children meanwhile, so
    // rinch drew its spacer there). A snapshot: the data carries it.
    let loading_children = match parsed {
        Some((s_id, Some(n_id))) => store.children_loading(s_id, n_id),
        Some((s_id, None)) => store.store_row_node(s_id).is_some_and(|root| store.children_loading(s_id, root)) && store.get_store_signal(s_id).is_some_and(|sig| untracked(|| sig.with(|s| s.roots.is_empty()))),
        None => false,
    };
    let children_spinner: Option<NodeHandle> = if loading_children && !has_children {
        Some(rsx! {
            span {
                class: "pimble-tree__children-loading",
                style: format!("left: calc(var(--tree-level-offset) * {level});"),
                title: "Loading...",
                Loader { size: "xs" }
            }
        })
    } else {
        None
    };

    let nv_submit = node_value.clone();

    // This row's own locally-computed label (see `AppStore::live_label`):
    // the label below subscribes to it and not to any other row's.
    let live_label = if is_store_root {
        None
    } else {
        parsed.and_then(|(s_id, n_id)| Some(store.live_label_signal((s_id, n_id?))))
    };

    // Whether THIS node is being renamed. (A rinch `Memo` re-notifies
    // its readers whenever `renaming_node` changes, equal result or
    // not, so every row's rename-dependent closures re-run on a
    // rename; renames are rare enough for that to be fine.)
    let is_renaming = {
        let nv = node_value.clone();
        Memo::new(move || store.renaming_node.get().as_deref() == Some(&nv))
    };

    let rename_input = rsx! {
        input {
            r#type: "text",
            class: "rinch-text-input__input",
            style: {
                move || {
                    if is_renaming.get() {
                        "width: 100%; font-size: inherit; padding: 0 2px; height: 22px; line-height: 22px;"
                    } else {
                        "display: none;"
                    }
                }
            },
            value: {move || store.rename_text.get()},
            oninput: move |value: String| {
                store.rename_text.set(value);
            },
            onsubmit: move || commit_rename(store, tree_state, &nv_submit),
        }
    };

    // Register this row's rename box so `start_rename` can focus it
    RENAME_INPUTS.with(|inputs| inputs.borrow_mut().insert(node_value.clone(), rename_input.clone()));
    // A node the keyboard just made is named as soon as its row is
    // drawn (docs/TREE_CONTRACT.md "The keyboard").
    if crate::state::take_pending_rename(&node_value) {
        let value = node_value.clone();
        run_on_main_thread(move || {
            if start_rename(store, tree_state, &value) {
                // Typed from nothing, not after the name it was made
                // with.
                mark_fresh(&value);
                store.rename_text.set(String::new());
            }
        });
    }

    let row_value = node_value.clone();

    let wrapper = rsx! {
        span {
            class: "rinch-tree__label",
            style: "flex: 1; min-width: 0; display: inline-flex; align-items: center;",

            // The row color: a band across the whole row, the
            // indent and chevron included, behind the icon and title
            // (the row is its stacking context, `APP_CSS`).
            span {
                class: "pimble-row-band",
                style: {
                    move || match &row_color {
                        Some(hex) => format!("background: {};", row_background(hex, store.dark_mode.get())),
                        None => "display: none;".to_string(),
                    }
                },
            }

            // Beside a found row, its metanode's guide line drawn
                // violet: the finds read as one group after its own
                // children (section 6). Over the Tree's own guide,
                // which is one line for every child alike.
            span {
                class: "pimble-found-guide",
                style: {
                    move || if found_on {
                        format!(
                            "left: calc(var(--tree-level-offset) * {} + var(--tree-guide-offset)); background: {};",
                            level.saturating_sub(1),
                            crate::appearance::gathered_violet(store.dark_mode.get())
                        )
                    } else {
                        "display: none;".to_string()
                    }
                },
            }

            span {
                class: icon_class,
                style: {
                    // Reactive icon opacity: a mount whose source is
                    // out of reach or not reachable yet renders dimmed.
                    || {
                        let dimmed = mount_sig.map_or(false, |ms| {
                            ms.with(|m| m.mount_state.as_ref().map_or(false, mount_is_dimmed))
                        });
                        let color = if query_violet {
                            format!(" color: {};", crate::appearance::gathered_violet(store.dark_mode.get()))
                        } else {
                            color_css(&icon_color)
                        };
                        if dimmed {
                            format!("width: 1rem; height: 1rem; margin-right: 4px; opacity: 0.4;{color}")
                        } else if internal.is_some() {
                            format!("opacity: 0.55;{color}")
                        } else {
                            color
                        }
                    }
                },
                {icon_el}
                span {
                    class: "pimble-query-mark",
                    style: {move || if query_mark { format!("background: {};", crate::appearance::gathered_violet(store.dark_mode.get())) } else { "display: none;".to_string() }},
                }
            }

            span {
                style: {
                    // Reactive label style — hides during rename, dims
                    // a mount whose source is out of reach or not
                    // reachable yet.
                    || {
                        if is_renaming.get() {
                            return "display: none;".to_string();
                        }
                        let dimmed = mount_sig.map_or(false, |ms| {
                            ms.with(|m| m.mount_state.as_ref().map_or(false, mount_is_dimmed))
                        });
                        let color = color_css(&label_color);
                        if dimmed {
                            format!("cursor: default; opacity: 0.4;{color}")
                        } else if internal.is_some() {
                            format!("{base_label_style} opacity: 0.55;{color}")
                        } else {
                            format!("{base_label_style}{color}")
                        }
                    }
                },
                // A store row says how many internal nodes it hides.
                title: {move || match parsed {
                    Some((s_id, None)) => crate::state::hidden_count_text(store, s_id),
                    _ => String::new(),
                }},

                // Reactive label text — subscribes to per-node/per-store signal
                // (and, for a node under active local edit, `live_label`).
                {move || {
                    let label = if is_store_root {
                        store_sig.map(|s| s.with(|st| st.name.clone()))
                            .unwrap_or_default()
                    } else {
                        let live = live_label.and_then(|sig| sig.get());
                        live.or_else(|| node_sig.map(|s| s.with(|n| display_label_from_node(n))))
                            .unwrap_or_else(|| "Untitled".to_string())
                    };

                    if let Some(ms) = mount_sig {
                        let suffix = ms.with(|m| {
                            m.mount_state.as_ref().map_or("", mount_label_suffix)
                        });
                        if suffix.is_empty() { label } else { format!("{label}{suffix}") }
                    } else {
                        label
                    }
                }}
            }

            {children_spinner}
            {share_badge}
            {out_of_step_mark}
            {shared_by_note}
            {index_note}
            {found_arrow}

            // Who else has the note open (docs/PRESENCE_CONTRACT.md
            // section 6): a dot each, up to three, from the
            // presence signal, so it follows without a tree rebuild.
            span {
                class: "pimble-tree-people",
                for person in (match parsed {
                    Some((s_id, Some(n_id))) => crate::presence::people_in(store, s_id, n_id).into_iter().take(3).collect::<Vec<_>>(),
                    _ => Vec::new(),
                }) {
                    PersonDot { key: person.key.clone(), person: person.clone() }
                }
            }

            // How many discussions are on the node
            // (docs/DISCUSSIONS_CONTRACT.md section 1): read from
            // its signal, so it follows without a tree rebuild.
            span {
                class: "pimble-tree-discussions",
                title: "Discussions on this node",
                style: {move || if node_sig.map_or(0, |s| s.with(|n| n.attachments)) > 0 { "" } else { "display: none;" }},
                span { class: "pimble-tree-discussions__icon", IconGlyph { name: "messages".to_string() } }
                {move || node_sig.map_or(String::new(), |s| s.with(|n| n.attachments.to_string()))}
                // Something unread in them (docs/DISCUSSIONS_CONTRACT.md section 8).
                span {
                    class: "pimble-tree-discussions__unread",
                    style: {move || match parsed {
                        Some((s_id, Some(n_id))) if crate::discussions::host_unread(store, s_id, n_id) > 0 => "",
                        _ => "display: none;",
                    }},
                }
            }

            // Sync status badge (store roots only, linked stores
            // only) — reactive per store, no tree rebuild
            // (docs/SYNC_CONTRACT.md "B: app side"). A store whose
            // owner's computer is known to be off says so whether or
            // not the backend calls what reaches it a link (the
            // browser holds no links, docs/RELAY_CONTRACT.md).
            span {
                style: {
                    move || {
                        let linked = sync_sig
                            .map(|sig| sig.with(|(remote, _)| remote.is_some()))
                            .unwrap_or(false);
                        let owner_offline = parsed
                            .map_or(false, |(s_id, _)| store.owner_offline.with(|set| set.contains(&s_id)));
                        // A replica whose every share has ended has a
                        // link that can reach nothing: "no longer
                        // shared" is said once, beside the name, and
                        // how that link is doing is not.
                        let ended = store_sig.map_or(false, |sig| sig.with(|s| s.every_share_ended()));
                        // A store shared from this computer has no
                        // badge: its link is to its own twin here, so
                        // it says nothing about the people it is
                        // shared with, and the shared nodes carry the
                        // share icon (Joe, 2026-11-06).
                        let owner = store_sig.map_or(false, |sig| sig.with(|s| s.relay == pimble_core::RelaySide::Owner));
                        // Documents the link is holding back are said
                        // on every row, an owner's and the browser's
                        // included (docs/SYNC_LIMITS_CONTRACT.md).
                        let waits = parsed.map_or(0, |(s_id, _)| store.waiting.with(|m| m.get(&s_id).map_or(0, Vec::len)));
                        if ((linked || owner_offline) && !owner || waits > 0) && !ended {
                            "margin-left: 6px; cursor: default; font-size: 10px; font-weight: 400; \
                             text-transform: none; letter-spacing: normal; opacity: 0.6;"
                        } else {
                            "display: none;"
                        }
                    }
                },
                {move || {
                    // The mode is the store's own signal (decision 4),
                    // read here so the badge follows a new vault link;
                    // so is which end of the relay this device is.
                    let (mode, relay) = store_sig
                        .map(|sig| sig.with(|s| (s.sync_mode, s.relay)))
                        .unwrap_or_default();
                    let owner_offline = parsed
                        .map_or(false, |(s_id, _)| store.owner_offline.with(|set| set.contains(&s_id)));
                    let waits = parsed.map_or(0, |(s_id, _)| store.waiting.with(|m| m.get(&s_id).map_or(0, Vec::len)));
                    let linked = sync_sig.map_or(false, |sig| sig.with(|(remote, _)| remote.is_some()));
                    let base = sync_sig
                        .filter(|_| linked || owner_offline)
                        .map(|sig| sig.with(|(_, state)| sync_badge_text(state, mode, relay, owner_offline).to_string()))
                        .unwrap_or_default();
                    badge_with_waiting(&base, waits)
                }}
            }

            {rename_input}
        }
    };

    // Remember this row for `reveal_row`, and scroll to it now if it
    // is the one a followed link revealed.
    TREE_ROWS.with(|rows| rows.borrow_mut().insert(row_value.clone(), wrapper.clone()));
    if REVEAL_ROW.with(|pending| pending.borrow().as_deref() == Some(row_value.as_str())) {
        REVEAL_ROW.with(|pending| pending.borrow_mut().take());
        wrapper.scroll_into_view();
    }

    // The explorer has ONE context menu (`explorer_menu`), built once
    // for the whole tree: a right press on a row only names the row it
    // is for, so a row carries no menu of its own (2026-11-12: each
    // row built its full menu, about 500 DOM nodes, and every change
    // to the tree's shape rebuilt them all).
    let row_for_menu = node_value.clone();
    let hit_area = rsx! {
        div {
            // `min-width: 0`: a flex item never narrower than its content
            // pushed a long title out past the row (and the scrollbar).
            style: "flex: 1; min-width: 0; display: flex; align-items: center; border-radius: 2px;",
            onmousedown: move || {
                if rinch::core::get_click_context().button == rinch::core::MouseButton::Right {
                    menu.aim(store, tree_state, Some(row_for_menu.clone()));
                }
            },
            {wrapper}
        }
    };
    // Image files dropped on the row from the OS become image nodes
    // under it, one per file, in order (docs/IMAGE_MARKUP_CONTRACT.md
    // section 1). The browser finds the row by this attribute
    // (`boards::install_browser_handlers`).
    hit_area.set_attribute("data-pimble-row", &node_value);
    #[cfg(feature = "native")]
    {
        let nv = node_value.clone();
        let handler = __scope.register_file_drop_handler(move |paths: Vec<std::path::PathBuf>| {
            if let Some((s_id, parent)) = crate::tree_view::create_parent_of_row(store, &nv) {
                crate::boards::add_image_files(store, crate::boards::Dest::Under(s_id, parent), paths);
            }
        });
        hit_area.set_attribute("data-onfiledrop", &handler.0.to_string());
    }
    hit_area
}

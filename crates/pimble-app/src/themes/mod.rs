//! The document theme each pane draws its note with (docs/THEMES_CONTRACT.md
//! sections 3.2, 4 and 7).
//!
//! The theme in force for a node is the one the node names
//! (`custom["theme"]`) or its nearest ancestor names; with none, Document
//! for a note whose own paragraphs were made spaced before themes, and
//! Default otherwise. It is looked up, never copied, so it is a pure
//! function of the chain's nodes as the app holds them (`AppStore::node_data`,
//! read with tracking, one signal at a time): a rename of the theme on any
//! node of the chain, a move, or a node arriving re-resolves every pane that
//! reads through it.
//!
//! A pane's node is its canonical `(StoreId, NodeId)`, so a node opened
//! through a mount walks its source store's tree and reads as it does there
//! (decision 5). When the chain runs into a node the app does not hold (a
//! note opened by link with its ancestors collapsed), the rest is asked for
//! once with `GetThemeChain` and the answer arrives as nodes like any other.
//!
//! What the theme draws is `pimble_themes::draw`'s custom properties, put
//! on the pane's `.pimble-editor__content-wrap` as an inline style, and its
//! structural modifiers as that element's classes; `document_theme.css`
//! reads them, with today's values as every fallback.

use std::cell::RefCell;

use rinch::prelude::untracked;
use std::collections::{HashMap, HashSet};

use pimble_core::{Node, NodeId, StoreId};
use pimble_themes::{Drawn, InForce, Link, Origin, Theme, ThemeSource, WithStore};

use crate::panes::PaneId;
use crate::protocol::BackendCommand;
use crate::state::{display_label_from_node, AppStore};

/// A chain is not walked further than this: no real tree is this deep, and a
/// broken one (a loop of parents) stops here at the latest.
const MAX_DEPTH: usize = 256;

/// The class every pane's content wrapper carries; a modifier is added as
/// `<WRAP_CLASS>--<modifier>`.
pub(crate) const WRAP_CLASS: &str = "pimble-editor__content-wrap";

thread_local! {
    /// The nodes asked for with `GetThemeChain` and not yet answered with
    /// them: each is asked once, so a node this device may not read (the
    /// parent of a share's root) is not asked for again on every redraw.
    static ASKED: RefCell<HashSet<(StoreId, NodeId)>> = RefCell::new(HashSet::new());

    /// The stores whose themes are needed and not listed yet, each with
    /// whether a `ThemeCommand::List` is on its way: each is asked once at
    /// a time, and again after a failed one once the store is caught up.
    static LISTING: RefCell<HashMap<StoreId, bool>> = RefCell::new(HashMap::new());
}

/// One node of a chain, as the picker names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step {
    pub node_id: NodeId,
    /// The node's label, as the explorer shows it.
    pub label: String,
    /// The theme the node names, if any.
    pub theme: Option<String>,
}

/// What one node of a chain says, as [`walk`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Held {
    pub step: Step,
    pub parent_id: Option<NodeId>,
    /// `custom["paragraphs"]` is `"spaced"` (only the first node's counts).
    pub spaced: bool,
}

impl Held {
    fn of(node: &Node) -> Held {
        Held {
            step: Step { node_id: node.id, label: display_label_from_node(node), theme: node.metadata.theme().map(String::from) },
            parent_id: node.parent_id,
            spaced: node.metadata.spaced_paragraphs(),
        }
    }
}

/// A chain as far as it was walked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Walk {
    /// The node, then its ancestors, nearest first, up to the first one
    /// naming a theme a source answers, or the root.
    pub steps: Vec<Step>,
    /// The first node's own paragraphs are spaced.
    pub spaced: bool,
    /// The ancestor the walk needed and the app does not hold.
    pub missing: Option<NodeId>,
}

/// Walks the chain from `node_id` up through `held`, stopping at the first
/// name `source` answers: nothing above it can matter. A node `held` does not
/// answer ends the walk and is named in `missing`, unless it is the first
/// (a node the app does not hold has nothing to draw yet).
pub(crate) fn walk(node_id: NodeId, held: impl Fn(NodeId) -> Option<Held>, source: &impl ThemeSource) -> Walk {
    let mut out = Walk::default();
    let mut at = Some(node_id);
    while let Some(id) = at.take() {
        if out.steps.len() >= MAX_DEPTH || out.steps.iter().any(|s| s.node_id == id) {
            break;
        }
        let Some(node) = held(id) else {
            if !out.steps.is_empty() {
                out.missing = Some(id);
            }
            break;
        };
        if out.steps.is_empty() {
            out.spaced = node.spaced;
        }
        let found = node.step.theme.as_deref().is_some_and(|name| source.theme(name).is_some());
        at = node.parent_id;
        out.steps.push(node.step);
        if found {
            break;
        }
    }
    out
}

/// The theme in force at the end of a walk.
pub(crate) fn in_force_of(walk: &Walk, source: &impl ThemeSource) -> InForce {
    pimble_themes::in_force(walk.steps.iter().map(|s| Link { theme: s.theme.as_deref() }), walk.spaced, source)
}

/// The themes `store_id`'s chains resolve through besides the built-ins: its
/// own theme nodes, as last listed. Tracked.
pub(crate) fn of_store(store: AppStore, store_id: StoreId) -> Vec<Theme> {
    store.store_themes.with(|all| all.get(&store_id).map(|listed| crate::commands::as_themes(listed)).unwrap_or_default())
}

/// The theme in force for `(store_id, node_id)` and the chain it was found
/// on, tracked. Asks for the rest of the chain when the app does not hold
/// it (once, a turn later: never from inside the reactive read).
pub(crate) fn resolve(store: AppStore, store_id: StoreId, node_id: NodeId) -> (InForce, Walk) {
    let held = |id: NodeId| {
        // One signal read at a time: a tracked read inside another's `with`
        // panics.
        let node = store.node_data.with(|map| map.get(&(store_id, id)).copied())?;
        Some(node.with(Held::of))
    };
    let themes = of_store(store, store_id);
    let source = WithStore(&themes);
    let walk = walk(node_id, held, &source);
    if let Some(missing) = walk.missing {
        ask(store_id, missing);
    }
    // A chain naming a theme that is not a built-in needs the store's
    // themes, listed the first time one does.
    if walk.steps.iter().any(|s| s.theme.as_deref().is_some_and(|t| pimble_themes::builtin(t).is_none())) {
        want_listed(store, store_id);
    }
    (in_force_of(&walk, &source), walk)
}

/// The theme `(store_id, node_id)` would read in without a theme of its own
/// (the picker's "Use the parent's"), tracked: its parent's chain, with the
/// node's own paragraphs still deciding the fallback.
pub(crate) fn in_force_without_own(store: AppStore, store_id: StoreId, node_id: NodeId) -> InForce {
    let node = store.node_data.with(|map| map.get(&(store_id, node_id)).copied());
    let (parent_id, spaced) = node.map(|n| n.with(|n| (n.parent_id, n.metadata.spaced_paragraphs()))).unwrap_or((None, false));
    let above = parent_id.map(|parent| resolve(store, store_id, parent).1).unwrap_or_default();
    let chain = std::iter::once(Link { theme: None }).chain(above.steps.iter().map(|s| Link { theme: s.theme.as_deref() }));
    pimble_themes::in_force(chain, spaced, &WithStore(&of_store(store, store_id)))
}

/// What the picker previews for `(store_id, node_id)`: the note's own blocks
/// from an editor holding it, or the sample when none does. Untracked: read
/// once, when the picker opens.
pub(crate) fn preview_blocks(store: AppStore, store_id: StoreId, node_id: NodeId) -> (Vec<preview::Block>, bool) {
    match store.panes_editing(store_id, node_id).first() {
        Some(pane) => (preview::of_doc(&crate::editor::editor(*pane).doc()), true),
        None => (preview::sample(), false),
    }
}

/// Open theme node `(store_id, node_id)` in the focused pane, as a followed
/// link opens a node: "Edit Theme..." and a theme just made. It opens
/// whether or not Themes are shown in the explorer (section 3.1a).
pub(crate) fn open_theme_node(store: AppStore, store_id: StoreId, node_id: NodeId) {
    let value = format!("node_{store_id}_{node_id}");
    store.link_open.set(Some(crate::state::LinkOpen { value, store_id, node_id, anchor: None, pane: None }));
}

/// Node > "New Theme": a theme of the selected row's store (a mount row's
/// source store) based on Default and setting nothing, named so no theme
/// of the store has its name, opened in the theme editor.
pub(crate) fn new_theme_for_selected(store: AppStore) {
    let target = untracked(|| store.selected_row_node()).and_then(|(s, n)| crate::app::theme_target(store, s, n));
    let Some((store_id, _)) = target else {
        crate::events::show_notice(store, "Select a node of the store the theme is for.".to_string());
        return;
    };
    // Numbered past the store's themes by the backend, which reads them.
    let name = "New theme".to_string();
    store.send(BackendCommand::Themes(crate::protocol::ThemeCommand::New { store_id, name, base: None, apply_to: None }));
}

/// `store_id`'s themes are needed (a chain names one, the picker or the
/// theme editor opened): listed now unless they are or are being. A store's
/// themes are listed only once something needs them, as listing holds the
/// whole store (docs/THEMES_CONTRACT.md "Built: wave 2"); once listed they
/// are listed again whenever one changes. Asked a turn later, never from
/// inside a reactive read.
pub(crate) fn want_listed(store: AppStore, store_id: StoreId) {
    if untracked(|| store.store_themes.with(|all| all.contains_key(&store_id))) {
        return;
    }
    let on_its_way = LISTING.with(|asked| asked.borrow_mut().insert(store_id, true)) == Some(true);
    if !on_its_way {
        crate::links::later(move |store| store.send(BackendCommand::Themes(crate::protocol::ThemeCommand::List { store_id })));
    }
}

/// `store_id`'s themes were listed, or the store closed: it may be asked
/// for again.
pub(crate) fn listing_ended(store_id: StoreId) {
    LISTING.with(|asked| asked.borrow_mut().remove(&store_id));
}

/// `store_id`'s themes could not be listed: still needed, asked again by
/// `relist` or the next `want_listed`.
pub(crate) fn listing_failed(store_id: StoreId) {
    LISTING.with(|asked| asked.borrow_mut().insert(store_id, false));
}

/// `store_id` was caught up (`Resynced`): its themes are listed again if
/// they were listed or are needed, as anything may have changed while it
/// was away or a listing may have failed with the connection.
pub(crate) fn relist(store: AppStore, store_id: StoreId) {
    let listed = untracked(|| store.store_themes.with(|all| all.contains_key(&store_id)));
    let needed = LISTING.with(|asked| asked.borrow_mut().get_mut(&store_id).map(|on_its_way| std::mem::replace(on_its_way, true)));
    if listed || needed == Some(false) {
        store.send(BackendCommand::Themes(crate::protocol::ThemeCommand::List { store_id }));
    }
}

/// `wanted`, or `wanted 2`, `wanted 3`, ...: the first no name in `taken`
/// (lowercase) is.
pub(crate) fn free_name(wanted: &str, taken: &[String]) -> String {
    (1..)
        .map(|n| if n == 1 { wanted.to_string() } else { format!("{wanted} {n}") })
        .find(|name| !taken.contains(&name.to_lowercase()))
        .unwrap_or_else(|| wanted.to_string())
}

/// Asks for `node_id` and the nodes above it, unless that is already asked.
fn ask(store_id: StoreId, node_id: NodeId) {
    if ASKED.with(|asked| asked.borrow_mut().insert((store_id, node_id))) {
        crate::links::later(move |store| store.send(BackendCommand::GetThemeChain { store_id, node_id }));
    }
}

/// `GetThemeChain`'s answer has arrived: the node it starts with may be asked
/// for again should the app drop it later. One that could not be read (an
/// empty answer) stays asked.
pub(crate) fn chain_answered(store_id: StoreId, nodes: &[Node]) {
    if let Some(first) = nodes.first() {
        ASKED.with(|asked| asked.borrow_mut().remove(&(store_id, first.id)));
    }
}

/// Theme `id`'s name among `themes` and the built-ins, as the picker shows
/// it.
pub(crate) fn theme_name(themes: &[Theme], id: &str) -> String {
    WithStore(themes).theme(id).map(|t| t.name).unwrap_or_else(|| id.to_string())
}

/// The sentence saying which theme is in force and where it comes from
/// (section 9: "Book, set here", "Book, from 'Novel'").
pub(crate) fn in_force_sentence(themes: &[Theme], in_force: &InForce, walk: &Walk) -> String {
    let name = theme_name(themes, &in_force.id);
    match in_force.origin {
        Origin::Named { depth: 0 } => format!("{name}, set here"),
        Origin::Named { depth } => match walk.steps.get(depth) {
            Some(step) => format!("{name}, from '{}'", step.label),
            None => name,
        },
        Origin::SpacedParagraphs => format!("{name}: this note's paragraphs were spaced before themes"),
        Origin::Default => format!("{name}: nothing above this note names a theme"),
    }
}

/// [`in_force_sentence`] for `(store_id, node_id)` as it resolves now,
/// tracked.
pub(crate) fn sentence(store: AppStore, store_id: StoreId, node_id: NodeId) -> String {
    let (in_force, walk) = resolve(store, store_id, node_id);
    in_force_sentence(&of_store(store, store_id), &in_force, &walk)
}

/// One sentence per name on the chain that no theme answers (a theme that
/// was deleted, or that this device does not hold): the theme above it is
/// used instead.
pub(crate) fn unresolved_sentences(in_force: &InForce, walk: &Walk) -> Vec<String> {
    in_force
        .unresolved
        .iter()
        .map(|(depth, _)| match walk.steps.get(*depth) {
            Some(step) => format!("The theme that '{}' names is not one this device has, so the one above it is used.", step.label),
            None => "A theme named above this note is not one this device has.".to_string(),
        })
        .collect()
}

/// What pane `pane`'s note draws with, tracked: its theme folded with its
/// bases, in the app's scheme. Nothing (Default) for a pane with no note.
pub(crate) fn pane_drawn(store: AppStore, pane: PaneId) -> Drawn {
    let Some((store_id, node_id)) = crate::pane_view::pane_document(store, pane) else { return Drawn::default() };
    let (in_force, _) = resolve(store, store_id, node_id);
    let dark = store.dark_mode.get();
    drawn_for(&of_store(store, store_id), &in_force.id, dark)
}

/// Theme `id` (among `themes` and the built-ins) folded with its bases and
/// drawn in one scheme.
pub(crate) fn drawn_for(themes: &[Theme], id: &str, dark: bool) -> Drawn {
    let folded = pimble_themes::fold(id, &WithStore(themes));
    pimble_themes::draw(&folded.tokens, dark)
}

/// `block`'s classes for a drawn theme: the block, then one
/// `<block>--<modifier>` per modifier.
pub(crate) fn classes(block: &str, drawn: &Drawn) -> String {
    let mut out = block.to_string();
    for modifier in &drawn.modifiers {
        out.push(' ');
        out.push_str(block);
        out.push_str("--");
        out.push_str(modifier);
    }
    out
}

/// A pane wrap's classes: the drawn theme's (`classes`), then the scheme
/// (`<WRAP_CLASS>--dark` or `--light`). The editor inside says its scheme
/// with `data-pm-theme`; the wrap needs its own to paint the page beside a
/// measured column (`document_theme.css`, "Measure").
pub(crate) fn wrap_classes(drawn: &Drawn, dark: bool) -> String {
    let mut out = classes(WRAP_CLASS, drawn);
    out.push(' ');
    out.push_str(WRAP_CLASS);
    out.push_str(if dark { "--dark" } else { "--light" });
    out
}

pub(crate) mod preview;

#[cfg(test)]
mod tests;

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
use std::collections::HashSet;

use pimble_core::{Node, NodeId, StoreId};
use pimble_themes::{Builtins, Drawn, InForce, Link, Origin, ThemeSource};

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
    let walk = walk(node_id, held, &Builtins);
    if let Some(missing) = walk.missing {
        ask(store_id, missing);
    }
    (in_force_of(&walk, &Builtins), walk)
}

/// The theme `(store_id, node_id)` would read in without a theme of its own
/// (the picker's "Use the parent's"), tracked: its parent's chain, with the
/// node's own paragraphs still deciding the fallback.
pub(crate) fn in_force_without_own(store: AppStore, store_id: StoreId, node_id: NodeId) -> InForce {
    let node = store.node_data.with(|map| map.get(&(store_id, node_id)).copied());
    let (parent_id, spaced) = node.map(|n| n.with(|n| (n.parent_id, n.metadata.spaced_paragraphs()))).unwrap_or((None, false));
    let above = parent_id.map(|parent| resolve(store, store_id, parent).1).unwrap_or_default();
    let chain = std::iter::once(Link { theme: None }).chain(above.steps.iter().map(|s| Link { theme: s.theme.as_deref() }));
    pimble_themes::in_force(chain, spaced, &Builtins)
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

/// The theme's name, as the picker shows it.
pub(crate) fn theme_name(id: &str) -> String {
    Builtins.theme(id).map(|t| t.name).unwrap_or_else(|| id.to_string())
}

/// The sentence saying which theme is in force and where it comes from
/// (section 9: "Book, set here", "Book, from 'Novel'").
pub(crate) fn in_force_sentence(in_force: &InForce, walk: &Walk) -> String {
    let name = theme_name(&in_force.id);
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

/// One sentence per name on the chain that no theme answers (a theme that
/// was deleted, or that this device does not hold): the theme above it is
/// used instead.
pub(crate) fn unresolved_sentences(in_force: &InForce, walk: &Walk) -> Vec<String> {
    in_force
        .unresolved
        .iter()
        .map(|(depth, _)| match walk.steps.get(*depth) {
            Some(step) => format!("The theme '{}' names is not one this device has, so the one above it is used.", step.label),
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
    drawn_for(&in_force.id, dark)
}

/// Theme `id` folded with its bases and drawn in one scheme.
pub(crate) fn drawn_for(id: &str, dark: bool) -> Drawn {
    let folded = pimble_themes::fold(id, &Builtins);
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

//! The kanban viewer (docs/KANBAN_CONTRACT.md waves 1 and 2): a metanode whose
//! source is its subtree two levels deep, drawn as columns (the first level)
//! of cards (the second), in the tree's own order, so a card's place in its
//! column is its priority. A child that is itself a kanban is a lane, drawn
//! below from its own source (`board`'s module comment).
//!
//! Everything it changes is an ordinary edit of real nodes through the Pimble
//! API, judged by core as the explorer's edits are: a rename is the one write
//! (`Pimble::edit`) with the explicit-title rule, a new card or column is a
//! create (last, then renamed in place), a move is planned and judged in the
//! metanode first (`Pimble::judge_in_metanode`: `plan`'s refusals and
//! warnings, and that it leaves the view), and a delete is asked of
//! `can_delete` first. A read-only board shows no gestures. It follows every
//! change the API hears (`next_change`, `Forest::touched_by`), from this
//! window, another device, the CLI or an agent.
//!
//! Moving is by dragging and by the keyboard (Alt+Shift+arrows, as in the
//! explorer, Up and Down carrying a card into the lane above or below; a
//! column moves with Alt+Shift+Left/Right once its header is chosen). Drags are rinch's
//! sortable lists (joeleaver/rinch#2093): each column's cards are a vertical
//! list of the group "cards", and the columns a horizontal list of the group
//! "columns" dragged by their headers. While a drag is over a place, the list
//! asks [`Kanban::judge_drop`], `plan`'s judgement over the documents held,
//! and a refusal's sentence shows at the caret; a drop becomes the same
//! `Target` the keys make (`board::drop_of`), and
//! `Kanban::move_to` is the one function every kind of move ends in.
//!
//! Presence is the app's (docs/PRESENCE_CONTRACT.md "Plugins"): choosing a
//! card tells the host the person is on it (`Host::focus`), which the app
//! publishes as part of the pane's own session, and each card and column
//! shows who else is on it with the app's own chips (`pimble_ui::presence`); a
//! click on a chip follows that person, and a pane following someone scrolls
//! to the card they are on.

mod board;
mod actions;
mod cards;
mod columns;
mod keys;
mod lanes;
mod look;
mod moves;
mod styles;

use std::sync::Arc;

use pimble_plugin_sdk::pimble_core::{custom_keys, plan};
use pimble_plugin_sdk::pimble_ui::appearance::{display_color, row_background, IconGlyph};
use pimble_plugin_sdk::pimble_ui::presence::{more_label, PersonChip};
use pimble_plugin_sdk::pimble_ui::viewer as viewer_look;
use pimble_plugin_sdk::prelude::*;
use rinch::core::dom::NodeId as DomId;

use std::collections::BTreeMap;

use board::{board_of, column_key, drop_of, key_of, step_card, step_column, Board, Lane, Step, Target};

use cards::*;
use columns::*;
use keys::*;
use lanes::*;
use look::*;
use moves::*;
use styles::*;
use viewer_look::{FIELD_STYLE, ICON_BUTTON, LINE_STYLE, SORT_THEME};


/// The sortable groups: a drag of one only ever lands in lists of its own,
/// so a card never lands among the columns or a column among the cards.
const CARDS: &str = "cards";
const COLUMNS: &str = "columns";

/// The viewer's id, as a metanode names it in `metanode:viewer`.
pub const VIEWER: &str = "kanban";

/// The columns "New Kanban" starts with (open decision 4, as recommended).
pub const FIRST_COLUMNS: &[&str] = &["To do", "Doing", "Done"];

fn describe() -> Manifest {
    manifest("kanban-viewer", "Kanban", env!("CARGO_PKG_VERSION"))
        .with(viewer(VIEWER, "Kanban", &[SourceShape::Subtree { depth: 2 }]))
        .with(recipe("new-kanban", "New Kanban", "Kanban", Some("layout-kanban"), VIEWER, 2, FIRST_COLUMNS))
        .writes(StoreAsk::Chosen)
}

/// What is chosen on the board: a card, a column by its header, or a lane by
/// its header (its node on the board).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Chosen {
    Card(Addr),
    Column(Addr),
    Lane(Addr),
}

impl Chosen {
    fn addr(self) -> Addr {
        match self {
            Chosen::Card(a) | Chosen::Column(a) | Chosen::Lane(a) => a,
        }
    }
}

/// A kanban: a node shown with this viewer.
fn is_kanban(node: &pimble_plugin_sdk::pimble_core::Node) -> bool {
    node.metadata.metanode.as_ref().is_some_and(|m| m.viewer == VIEWER)
}

/// A move that `plan` or the metanode warned about, waiting for "Move anyway".
#[derive(Debug, Clone, PartialEq)]
struct Waiting {
    node: Addr,
    target: Target,
    sentence: String,
}

/// The board's state and what acts on it.
#[derive(Clone)]
struct Kanban {
    api: Arc<Pimble>,
    host: Host,
    metanode: Addr,
    board: Signal<Board>,
    /// The forest has been read once.
    loaded: Signal<bool>,
    chosen: Signal<Option<Chosen>>,
    /// The card or column whose name is being typed.
    renaming: Signal<Option<Addr>>,
    draft: Signal<String>,
    /// What came of the last gesture: a refusal, a warning, what was done.
    status: Signal<String>,
    waiting: Signal<Option<Waiting>>,
    /// The board's element, which holds the keyboard.
    root: Signal<Option<NodeHandle>>,
    /// The name field while one is open.
    field: Signal<Option<NodeHandle>>,
    /// How the person has the board on this device.
    look: Signal<Look>,
    /// The column cell the pointer is over: it offers "Add card".
    hovered: Signal<Option<Addr>>,
    /// The card the pointer is over: it offers its trash can.
    hovered_card: Signal<Option<Addr>>,
}


fn dom_id(addr: Addr) -> String {
    format!("kanban-{}", addr.node)
}


/// The viewer.
pub fn view(__scope: &mut RenderScope, mount: Mount) -> NodeHandle {
    let Some(metanode) = mount.metanode() else {
        return rsx! { div { style: LINE_STYLE, "This viewer was shown without a metanode." } };
    };
    let k = Kanban {
        api: mount.api.clone(),
        host: mount.host.clone(),
        metanode,
        board: Signal::new(Board::default()),
        loaded: Signal::new(false),
        chosen: Signal::new(None),
        renaming: Signal::new(None),
        draft: Signal::new(String::new()),
        status: Signal::new(String::new()),
        waiting: Signal::new(None),
        root: Signal::new(None),
        field: Signal::new(None),
        look: Signal::new(mount.remembered().map(|r| Look::parse(&r)).unwrap_or_default()),
        hovered: Signal::new(None),
        hovered_card: Signal::new(None),
    };

    // Read the forest, then keep it live from every change the API hears.
    {
        let k = k.clone();
        spawn_local(async move {
            let mut forests = k.read().await;
            while let Some(change) = k.api.next_change().await {
                if forests.as_ref().map_or(true, |fs| fs.iter().any(|f| f.touched_by(&change))) {
                    forests = k.read().await.or(forests);
                }
            }
        });
    }

    // A name field takes the keyboard when it is drawn, a turn later (the
    // host applies this turn's writes first).
    {
        let (field, renaming) = (k.field, k.renaming);
        __scope.create_effect(move || {
            if renaming.get().is_none() {
                return;
            }
            if let Some(field) = field.get() {
                let _ = set_timeout(0, move || field.focus());
            }
        });
    }

    // Following someone (docs/PRESENCE_CONTRACT.md section 7): the pane opened
    // this board because they are here; take their place, the card they are on.
    {
        let (k, doc) = (k.clone(), __scope.doc_weak());
        __scope.create_effect(move || {
            let Some(leader) = k.host.following() else { return };
            let cards: Vec<Addr> = k.board.with(|b| b.cards().map(|card| card.addr).collect());
            let Some(card) = cards.into_iter().find(|card| k.host.people_in(*card).iter().any(|p| p.key == leader)) else { return };
            let Some(doc) = doc.upgrade() else { return };
            let found = doc.borrow().query_selector(&format!("#{}", dom_id(card)));
            if let Some(found) = found {
                let _ = set_timeout(0, {
                    let doc = std::rc::Rc::downgrade(&doc);
                    move || NodeHandle::new(DomId(found.0), doc.clone()).scroll_into_view()
                });
            }
        });
    }

    let (kk, kw1, kw2, ka, kl, kt, kh) = (k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone());
    let judge = drop_judge(&k);
    let (judge_h, judge_l) = (judge.clone(), judge.clone());
    let (board, status, waiting, loaded, root, look) = (k.board, k.status, k.waiting, k.loaded, k.root, k.look);
    rsx! {
        div {
            class: "kanban",
            style: {let seen = k.host.seen(); move || format!("{BOARD_STYLE}{}", viewer_look::surfaces(seen.dark()))},
            tabindex: "0",
            ref: root,
            onkeydown: move |key: &KeyEventData| kk.key(key),
            // What came of the last gesture, and a move waiting for "Move anyway".
            if status.with(|s| !s.is_empty()) {
                div { class: "kanban__status", style: LINE_STYLE, {move || status.get()} }
            }
            if let Some(waiting) = waiting.get() {
                div {
                    class: "kanban__waiting",
                    style: "display: flex; align-items: center; gap: 8px; font-size: 12px;",
                    span { {waiting.sentence.clone()} }
                    Button {
                        size: "xs",
                        variant: "filled",
                        onclick: {
                            let (k, w) = (kw1.clone(), waiting.clone());
                            move || k.move_to(w.node, w.target, true)
                        },
                        "Move anyway"
                    }
                    Button { size: "xs", variant: "default", onclick: { let k = kw2.clone(); move || k.waiting.set(None) }, "Cancel" }
                }
            }
            // With lanes whose columns differ there is no shared header row,
            // and so no corner above the lane names: the lane tools go above
            // the board instead.
            if board.with(|b| b.lanes.len() > 1 && !b.aligned()) {
                div {
                    class: "kanban__toolbar",
                    style: "display: flex; gap: 2px; align-items: center;",
                    {lane_tools(__scope, &kt)}
                }
            }
            if loaded.get() && board.with(|b| b.lanes.len() < 2 && b.lanes.iter().all(|l| l.columns.is_empty())) {
                div { style: LINE_STYLE, "This kanban has no columns yet." }
            }
            div {
                class: "kanban__grid",
                style: GRID_STYLE,
                // Lanes with the same columns share one header row.
                if board.with(|b| b.aligned() && !b.lanes.is_empty()) {
                    {header_row(__scope, &kh, None, judge_h.clone())}
                }
                // A lane is drawn again when its name, its size or what the
                // person may do with it changes (a list's props are read when
                // it is drawn); its columns and cards follow the board on their
                // own.
                for shown in lanes_shown(board, look, &kl) {
                    div {
                        key: shown.key(),
                        class: "kanban__lane",
                        style: "display: flex; flex-direction: column; flex-shrink: 0;",
                        {render_lane(__scope, &kl, shown.clone(), judge_l.clone())}
                    }
                }
                if board.with(|b| b.writable) {
                    div {
                        class: "kanban__add-lane",
                        style: "font-size: 13px; color: var(--rinch-color-dimmed); cursor: pointer; padding: 4px; flex-shrink: 0;",
                        onclick: { let k = ka.clone(); move || k.add_lane() },
                        "+ Add lane"
                    }
                }
            }
        }
    }
}


pimble_plugin_sdk::plugin!(describe: describe, view: view, guide: include_bytes!("../guide.json"));

#[cfg(test)]
mod tests {
    /// What it tells agents (docs/AGENT_GUIDES_CONTRACT.md) reads, names
    /// every viewer the manifest provides, and nothing else.
    #[test]
    fn the_guide_reads_and_names_its_viewers() {
        let guide = pimble_plugin_sdk::abi::guide::Guide::parse(include_bytes!("../guide.json")).unwrap();
        let mut guided: Vec<&str> = guide.viewers.iter().map(|v| v.viewer.as_str()).collect();
        let manifest = super::describe();
        let mut provided: Vec<&str> = manifest.viewers().map(|v| v.id.as_str()).collect();
        guided.sort();
        provided.sort();
        assert_eq!(guided, provided);
    }

    use super::*;

    #[test]
    fn it_provides_the_kanban_viewer_and_new_kanban() {
        let m = describe();
        assert_eq!(m.viewers().next().map(|v| v.id.as_str()), Some(VIEWER));
        let recipe = m.recipes().next().expect("New Kanban");
        assert_eq!(recipe.label, "New Kanban");
        assert_eq!(recipe.depth, 2);
        assert_eq!(recipe.children, vec!["To do", "Doing", "Done"]);
        assert!(m.asks.write);
    }
}

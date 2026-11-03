//! The kanban viewer (docs/KANBAN_CONTRACT.md wave 1): a metanode whose source
//! is its subtree two levels deep, drawn as columns (the first level) of cards
//! (the second), in the tree's own order, so a card's place in its column is
//! its priority.
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
//! Moving is by dragging, by the card's "Move to" buttons and by the keyboard
//! (Alt+Shift+arrows, as in the explorer; a column moves with
//! Alt+Shift+Left/Right once its header is chosen). Drags are rinch's
//! sortable lists (joeleaver/rinch#2093): each column's cards are a vertical
//! list of the group "cards", and the columns a horizontal list of the group
//! "columns" dragged by their headers. While a drag is over a place, the list
//! asks [`Kanban::judge_drop`], `plan`'s judgement over the documents held,
//! and a refusal's sentence shows at the caret; a drop becomes the same
//! `Target` the menu and the keys make (`board::drop_of`), and
//! `Kanban::move_to` is the one function every kind of move ends in.
//!
//! Presence is the app's (docs/PRESENCE_CONTRACT.md "Plugins"): choosing a
//! card tells the host the person is on it (`Host::focus`), which the app
//! publishes as part of the pane's own session, and each card and column
//! shows who else is on it with the app's own chips (`pimble_ui::presence`); a
//! click on a chip follows that person, and a pane following someone scrolls
//! to the card they are on.

mod board;

use std::sync::Arc;

use pimble_plugin_sdk::pimble_core::{custom_keys, plan};
use pimble_plugin_sdk::pimble_ui::appearance::{display_color, row_background, IconGlyph};
use pimble_plugin_sdk::pimble_ui::presence::{more_label, PersonChip};
use pimble_plugin_sdk::prelude::*;
use rinch::core::dom::NodeId as DomId;

use board::{board_of, drop_of, key_of, step_card, step_column, to_column, Board, Step, Target};

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

/// What is chosen on the board: a card, or a column by its header.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Chosen {
    Card(Addr),
    Column(Addr),
}

impl Chosen {
    fn addr(self) -> Addr {
        match self {
            Chosen::Card(a) | Chosen::Column(a) => a,
        }
    }
}

/// A column a card's "Move to" offers.
#[derive(Debug, Clone, PartialEq)]
struct MoveTo {
    to: Addr,
    name: String,
}

/// The columns a card in column `home` may be moved to, tracked.
fn move_targets(board: Signal<Board>, home: Addr, writable: bool) -> Vec<MoveTo> {
    if !writable {
        return Vec::new();
    }
    board.with(|b| b.columns.iter().filter(|c| c.addr != home).map(|c| MoveTo { to: c.addr, name: c.label.clone() }).collect())
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
}

fn dom_id(addr: Addr) -> String {
    format!("kanban-{}", addr.node)
}

impl Kanban {
    /// Read the forest into the board.
    async fn read(&self) -> Option<Forest> {
        // The stores listed again with every read: a drag is judged
        // synchronously over what is held (`judge_drop`), so a mounted
        // column's store closed since is refused at the caret, not after
        // the drop.
        let _ = self.api.stores().await;
        match self.api.metanode_source(self.metanode).await {
            Ok(forest) => {
                let board = board_of(&forest, display_label);
                // Something chosen that is no longer on the board is let go,
                // unless it is being named: a card just made is chosen before
                // the board has read it.
                let naming = untracked(|| self.renaming.get());
                if let Some(chosen) = untracked(|| self.chosen.get()).filter(|c| Some(c.addr()) != naming) {
                    let there = match chosen {
                        Chosen::Card(a) => board.find_card(a).is_some(),
                        Chosen::Column(a) => board.column_index(a).is_some(),
                    };
                    if !there {
                        self.choose(None);
                    }
                }
                if untracked(|| self.board.get()) != board {
                    self.board.set(board);
                }
                self.loaded.set(true);
                Some(forest)
            }
            Err(e) => {
                self.status.set(e.sentence());
                None
            }
        }
    }

    /// Choose `chosen`, and tell the host the person is on it, so everyone
    /// else sees them there.
    fn choose(&self, chosen: Option<Chosen>) {
        if untracked(|| self.chosen.get()) == chosen {
            return;
        }
        self.chosen.set(chosen);
        self.host.focus(chosen.map(Chosen::addr));
    }

    /// Give the board the keyboard, so Alt+Shift+arrows reach it.
    fn take_keyboard(&self) {
        if let Some(root) = untracked(|| self.root.get()) {
            root.focus();
        }
    }

    fn say(&self, line: impl Into<String>) {
        self.status.set(line.into());
    }

    /// The one way anything moves on the board: a card to a column or a place
    /// in one, a column along the board. Judged in the metanode first, so a
    /// refusal is the explorer's sentence and nothing is sent; a warning
    /// (another store, leaving a share, leaving this view) waits for "Move
    /// anyway" unless `confirmed`. A drop (rinch #2091) will call this with
    /// `board::drop_target`'s answer.
    fn move_to(&self, node: Addr, target: Target, confirmed: bool) {
        let k = self.clone();
        self.waiting.set(None);
        spawn_local(async move {
            // Judged against the stores open now: a column mounted from a
            // store closed since the board was read is refused, not planned.
            let _ = k.api.stores().await;
            let edit = MetanodeEdit::Move { nodes: vec![node], target: target.parent, place: target.place };
            let judged = match k.api.judge_in_metanode(k.metanode, edit).await {
                Ok(judged) => judged,
                Err(e) => return k.say(e.sentence()),
            };
            let Some(plan) = judged.plan else { return };
            if plan.stays() {
                return k.say("");
            }
            if !judged.warnings.is_empty() && !confirmed {
                k.waiting.set(Some(Waiting { node, target, sentence: judged.warnings.join(" ") }));
                return;
            }
            match k.api.move_nodes(&plan).await {
                Ok(moved) => {
                    k.say("");
                    // A move out of a share or a store gives the node a new
                    // id: keep it chosen as it is now.
                    if let Some(to) = moved.first() {
                        let chosen = untracked(|| k.chosen.get());
                        if chosen.map(Chosen::addr) == Some(node) && to.addr != node {
                            k.choose(chosen.map(|c| match c {
                                Chosen::Card(_) => Chosen::Card(to.addr),
                                Chosen::Column(_) => Chosen::Column(to.addr),
                            }));
                        }
                    }
                }
                Err(e) => k.say(e.sentence()),
            }
        });
    }

    /// Whether a drop of a sortable list may land where it is over, asked
    /// for every place a drag offers, synchronously, before the release:
    /// `plan`'s judgement over the documents held now (what the board read),
    /// so a refusal's sentence (a column the person may only read, a mount
    /// whose store is not open here) shows at the caret and the drop is not
    /// delivered. A warning is not a refusal: the drop is accepted, and
    /// [`Kanban::move_to`] judges it again in full and holds it for "Move
    /// anyway" as it does a menu's or a key's move.
    fn judge_drop(&self, m: &SortMove) -> SortVerdict {
        let Some((nodes, target)) = drop_of(&m.keys, &m.to_list, m.place.after.as_deref()) else {
            return SortVerdict::Refuse(String::new());
        };
        let pairs: Vec<_> = nodes.iter().map(|a| (a.store, a.node)).collect();
        let parent = (target.parent.store, Some(target.parent.node));
        match self.api.view(|v| plan::plan_move(v, &pairs, parent, target.place)) {
            Ok(_) => SortVerdict::Accept,
            Err(refusal) => SortVerdict::Refuse(refusal.sentence().to_string()),
        }
    }

    /// An accepted drop: the move, through the one function, and what was
    /// dragged chosen, so the keyboard and the card's actions follow it.
    fn drop_move(&self, m: SortMove) {
        let Some((nodes, target)) = drop_of(&m.keys, &m.to_list, m.place.after.as_deref()) else { return };
        // The board has one choice, so a drag carries one card or one column.
        let [node] = nodes[..] else { return };
        self.choose(Some(if m.group == COLUMNS { Chosen::Column(node) } else { Chosen::Card(node) }));
        self.take_keyboard();
        self.move_to(node, target, false);
    }

    /// Alt+Shift+arrow on what is chosen.
    fn step(&self, step: Step) {
        let board = untracked(|| self.board.get());
        let (node, target) = match untracked(|| self.chosen.get()) {
            Some(Chosen::Card(card)) => (card, step_card(&board, card, step)),
            Some(Chosen::Column(column)) => (column, step_column(&board, column, step)),
            None => return,
        };
        if let Some(target) = target {
            self.move_to(node, target, false);
        }
    }

    /// Start typing a name for `addr`, from what it is called now.
    fn start_rename(&self, addr: Addr, now: String) {
        self.draft.set(now);
        self.renaming.set(Some(addr));
        // The field takes the keyboard when it is drawn (`view`'s effect over
        // `field`): a new card's is drawn only once the board has read it.
    }

    /// Enter in the name field: rename through the one write, with the
    /// explicit-title rule (a name typed here is the node's name, not a
    /// guess from its text).
    fn commit_rename(&self) {
        let Some(addr) = untracked(|| self.renaming.get()) else { return };
        let title = untracked(|| self.draft.get()).trim().to_string();
        self.renaming.set(None);
        self.take_keyboard();
        if title.is_empty() {
            return;
        }
        let k = self.clone();
        spawn_local(async move {
            let edited = k
                .api
                .edit(addr, move |doc, now| -> Result<(), ApiError> {
                    doc.set_title(&title, now)?;
                    doc.set_custom(custom_keys::EXPLICIT_TITLE, &serde_json::Value::Bool(true), now)?;
                    Ok(())
                })
                .await;
            if let Err(e) = edited {
                k.say(e.sentence());
            }
        });
    }

    fn cancel_rename(&self) {
        self.renaming.set(None);
        self.take_keyboard();
    }

    /// "+ Add card" (under `column`, last) or "+ Add column" (`column` is the
    /// metanode): a create, then its name typed in place.
    fn add(&self, under: Addr, card: bool) {
        let k = self.clone();
        spawn_local(async move {
            let init = match pimble_api::recipes::document("", None) {
                Ok(init) => init,
                Err(e) => return k.say(e.sentence()),
            };
            match k.api.create(under, Place::Last, init).await {
                Ok(made) => {
                    k.say("");
                    k.choose(Some(if card { Chosen::Card(made) } else { Chosen::Column(made) }));
                    k.start_rename(made, String::new());
                }
                Err(e) => k.say(e.sentence()),
            }
        });
    }

    /// Delete `card` (a tombstone, put back from Recently Deleted), asked of
    /// `plan` first as the explorer's Delete is.
    fn delete(&self, card: Addr) {
        let k = self.clone();
        let label = untracked(|| self.board.with(|b| b.card(card).map(|c| c.label.clone()))).unwrap_or_default();
        spawn_local(async move {
            if let Err(e) = k.api.can_delete(&[card]).await {
                return k.say(e.sentence());
            }
            match k.api.delete(card).await {
                Ok(()) => {
                    k.choose(None);
                    k.say(format!("Deleted \u{201c}{label}\u{201d}. View > Recently Deleted puts it back."));
                }
                Err(e) => k.say(e.sentence()),
            }
        });
        self.take_keyboard();
    }

    /// The board's keys: arrows choose, Alt+Shift+arrows move, Enter opens the chosen
    /// card, F2 renames, Delete deletes, Escape lets go. Answers whether it
    /// took the key.
    fn key(&self, key: &KeyEventData) -> bool {
        if untracked(|| self.renaming.get()).is_some() {
            return false;
        }
        let board = untracked(|| self.board.get());
        let chosen = untracked(|| self.chosen.get());
        let step = match key.key.as_str() {
            "ArrowUp" => Some(Step::Up),
            "ArrowDown" => Some(Step::Down),
            "ArrowLeft" => Some(Step::Left),
            "ArrowRight" => Some(Step::Right),
            _ => None,
        };
        if let Some(step) = step {
            // Alt+Shift+arrows move, as they move a row in the explorer
            // (Alt+Left and Alt+Right alone are the app's Back and Forward).
            if key.alt && key.shift {
                if board.writable {
                    self.step(step);
                }
                return true;
            }
            self.choose(walk(&board, chosen, step).or(chosen));
            return true;
        }
        let writable = |addr: Addr| match board.card(addr) {
            Some(card) => card.writable,
            None => board.columns.iter().find(|c| c.addr == addr).is_some_and(|c| c.writable),
        };
        match (key.key.as_str(), chosen) {
            ("Enter", Some(Chosen::Card(card))) => self.host.open(card),
            ("F2", Some(c)) if writable(c.addr()) => {
                let now = match c {
                    Chosen::Card(a) => board.card(a).map(|k| k.label.clone()),
                    Chosen::Column(a) => board.columns.iter().find(|col| col.addr == a).map(|col| col.label.clone()),
                };
                self.start_rename(c.addr(), now.unwrap_or_default());
            }
            ("Delete", Some(Chosen::Card(card))) if writable(card) => self.delete(card),
            ("Escape", Some(_)) => self.choose(None),
            _ => return false,
        }
        true
    }
}

/// Where a plain arrow takes the choice: along a column's cards, across to
/// the neighbor column's card at the same height, or onto a column's header
/// from above its first card.
fn walk(board: &Board, chosen: Option<Chosen>, step: Step) -> Option<Chosen> {
    let first = || board.columns.first().map(|c| c.cards.first().map_or(Chosen::Column(c.addr), |k| Chosen::Card(k.addr)));
    match chosen {
        None => first(),
        Some(Chosen::Card(card)) => {
            let (c, i) = board.find_card(card)?;
            let column = &board.columns[c];
            match step {
                Step::Up if i == 0 => Some(Chosen::Column(column.addr)),
                Step::Up => Some(Chosen::Card(column.cards[i - 1].addr)),
                Step::Down => column.cards.get(i + 1).map(|k| Chosen::Card(k.addr)),
                Step::Left | Step::Right => {
                    let to = if step == Step::Left { c.checked_sub(1)? } else { c + 1 };
                    let next = board.columns.get(to)?;
                    Some(next.cards.get(i.min(next.cards.len().saturating_sub(1))).map_or(Chosen::Column(next.addr), |k| Chosen::Card(k.addr)))
                }
            }
        }
        Some(Chosen::Column(column)) => {
            let c = board.column_index(column)?;
            match step {
                Step::Down => board.columns[c].cards.first().map(|k| Chosen::Card(k.addr)),
                Step::Up => None,
                Step::Left => c.checked_sub(1).map(|to| Chosen::Column(board.columns[to].addr)),
                Step::Right => board.columns.get(c + 1).map(|col| Chosen::Column(col.addr)),
            }
        }
    }
}

const BOARD_STYLE: &str = "display: flex; flex-direction: column; gap: 8px; padding: 12px 16px; height: 100%; box-sizing: border-box; outline: none;";
const COLUMNS_STYLE: &str = "display: flex; gap: 12px; align-items: flex-start; overflow-x: auto; flex: 1; min-height: 0; padding-bottom: 12px;";
// The sortable lists' layout is written here as well as in rinch's stylesheet,
// which is the app's: an app on a rinch from before sortable lists still lays
// the board out (only the caret and the refusal box need its stylesheet).
const COLUMN_LIST_STYLE: &str = "display: flex; flex-direction: row; gap: 12px; align-items: flex-start; flex-shrink: 0; max-height: 100%; position: relative;";
const COLUMN_STYLE: &str = "display: flex; flex-direction: column; gap: 6px; width: 260px; flex-shrink: 0; padding: 8px; border-radius: 8px; background: var(--rinch-color-default-hover, rgba(127,127,127,0.08)); max-height: 100%; overflow-y: auto; box-sizing: border-box; position: relative;";
const CARD_LIST_STYLE: &str = "display: flex; flex-direction: column; gap: 6px; min-height: 28px; position: relative;";
/// The caret and the refusal box in the app's own theme colors, through the
/// properties rinch's sortable stylesheet reads (a plugin cannot add a
/// stylesheet; custom properties set inline here are inherited by the boxes
/// the lists draw).
const SORT_THEME: &str = " --rinch-sortable-caret-color: var(--rinch-primary-color); --rinch-sortable-refused-color: var(--rinch-color-red-6); --rinch-sortable-reason-bg: var(--rinch-color-red-6); --rinch-sortable-reason-color: var(--rinch-color-white, #ffffff); --rinch-sortable-reason-font-size: 12px; --rinch-sortable-reason-radius: 4px; --rinch-sortable-reason-white-space: normal; --rinch-sortable-reason-max-width: 220px; --rinch-sortable-dragging-opacity: 0.45;";

/// The keys that lift a focused card or column for a keyboard move: Space
/// only, so Enter stays "open the card", as everywhere else in Pimble.
fn lift_keys() -> Vec<String> {
    vec![" ".to_string()]
}
const HEADER_STYLE: &str = "display: flex; align-items: center; gap: 6px; font-weight: 600; font-size: 13px; padding: 2px 4px; border-radius: 4px; cursor: pointer;";
const FIELD_STYLE: &str = "width: 100%; box-sizing: border-box; font: inherit; padding: 2px 4px;";
const ADD_STYLE: &str = "font-size: 12px; color: var(--rinch-color-dimmed); padding: 4px; cursor: pointer; border-radius: 4px;";
const LINE_STYLE: &str = "font-size: 12px; color: var(--rinch-color-dimmed);";

/// A card's style: chosen or not, its row color, and the color of the person
/// the pane follows when they are on it.
fn card_style(chosen: bool, background: Option<String>, leader: Option<String>) -> String {
    let border = match (leader, chosen) {
        (Some(color), _) => format!("2px solid {color}"),
        (None, true) => "2px solid var(--rinch-primary-color)".to_string(),
        (None, false) => "1px solid var(--rinch-color-default-border, rgba(127,127,127,0.3))".to_string(),
    };
    let background = background.unwrap_or_else(|| "var(--rinch-color-body)".to_string());
    format!("display: flex; flex-direction: column; gap: 4px; padding: 6px 8px; border-radius: 6px; cursor: pointer; border: {border}; background: {background};")
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
    };

    // Read the forest, then keep it live from every change the API hears.
    {
        let k = k.clone();
        spawn_local(async move {
            let mut forest = k.read().await;
            while let Some(change) = k.api.next_change().await {
                if forest.as_ref().map_or(true, |f| f.touched_by(&change)) {
                    forest = k.read().await.or(forest);
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
            let cards: Vec<Addr> = k.board.with(|b| b.columns.iter().flat_map(|c| c.cards.iter().map(|card| card.addr)).collect());
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

    let (kk, kw1, kw2, ka, kcol, kd) = (k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone());
    let judge = drop_judge(&k);
    let metanode_key = key_of(metanode);
    let (board, status, waiting, loaded, root) = (k.board, k.status, k.waiting, k.loaded, k.root);
    rsx! {
        div {
            class: "kanban",
            style: BOARD_STYLE,
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
            if loaded.get() && board.with(|b| b.columns.is_empty()) {
                div { style: LINE_STYLE, "This kanban has no columns yet." }
            }
            div {
                class: "kanban__columns",
                style: COLUMNS_STYLE,
                // The columns, dragged by their headers along the board. A
                // list's props are read when it is drawn, so it is drawn again
                // when the board's writability changes (the board reads as not
                // writable until its first read).
                if board.with(|b| b.writable) {
                    {column_list(__scope, &kcol, &metanode_key, judge.clone(), true)}
                } else {
                    {column_list(__scope, &kd, &metanode_key, judge.clone(), false)}
                }
                if board.with(|b| b.writable) {
                    div {
                        class: "kanban__add-column",
                        style: "width: 200px; flex-shrink: 0; padding: 8px; font-size: 13px; color: var(--rinch-color-dimmed); cursor: pointer;",
                        onclick: { let k = ka.clone(); move || k.add(k.metanode, false) },
                        "+ Add column"
                    }
                }
            }
        }
    }
}

/// The board's columns, a horizontal sortable list dragged by the headers;
/// `writable` is the board's (a board the person may only read takes no
/// drags at all).
fn column_list(__scope: &mut RenderScope, k: &Kanban, metanode_key: &str, judge: SortJudge, writable: bool) -> NodeHandle {
    let (board, kd, kcol) = (k.board, k.clone(), k.clone());
    rsx! {
        SortableList {
            group: COLUMNS,
            id: metanode_key.to_string(),
            axis: SortAxis::Horizontal,
            label: "Columns",
            class: "kanban__column-list",
            style: format!("{COLUMN_LIST_STYLE}{SORT_THEME}"),
            disabled: !writable,
            lift_keys: lift_keys(),
            judge: Some(judge.clone()),
            ondrop: move |m: SortMove| kd.drop_move(m),
            for column in board.get().columns {
                SortableItem {
                    key: format!("{column:?}"),
                    value: key_of(column.addr),
                    handle: true,
                    disabled: !(column.writable && writable),
                    label: column.label.clone(),
                    class: "kanban__column",
                    style: COLUMN_STYLE,
                    {render_column(__scope, &kcol, column.clone(), judge.clone(), writable)}
                }
            }
        }
    }
}

/// How the board's lists judge a drop ([`Kanban::judge_drop`]).
fn drop_judge(k: &Kanban) -> SortJudge {
    let k = k.clone();
    std::rc::Rc::new(move |m: &SortMove| k.judge_drop(m))
}

/// One column: its header (renamed in place, its people; what a column is
/// dragged by), its cards (a sortable list of their own), "+ Add card".
fn render_column(__scope: &mut RenderScope, k: &Kanban, column: board::Column, judge: SortJudge, board_writable: bool) -> NodeHandle {
    let addr = column.addr;
    let seen = k.host.seen();
    let color = column.color.clone();
    let (kc, kr, kh, ka, kf, kf2) = (k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone());
    let (label, label2) = (column.label.clone(), column.label.clone());
    let writable = column.writable;
    let chosen = k.chosen;
    let renaming = k.renaming;
    let header = rsx! {
        div {
            class: "kanban__header",
            id: dom_id(addr),
            style: {|| format!(
                "{HEADER_STYLE}{}{}",
                color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default(),
                if chosen.get() == Some(Chosen::Column(addr)) { " box-shadow: inset 0 0 0 2px var(--rinch-primary-color);" } else { "" },
            )},
            onclick: move || {
                kc.choose(Some(Chosen::Column(addr)));
                kc.take_keyboard();
            },
            if renaming.get() == Some(addr) {
                input {
                    class: "kanban__field",
                    style: FIELD_STYLE,
                    ref: kf.field,
                    placeholder: "Column name",
                    value: {move || kf.draft.get()},
                    oninput: move |v: String| kf2.draft.set(v),
                    onkeydown: {
                        let k = kr.clone();
                        move |key: &KeyEventData| match key.key.as_str() {
                            "Enter" => { k.commit_rename(); true }
                            "Escape" => { k.cancel_rename(); true }
                            _ => false,
                        }
                    },
                }
            } else {
                span {
                    style: "flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                    title: if writable { "Click to choose; F2 or Rename to rename" } else { "" },
                    {label.clone()}
                }
            }
            span {
                style: LINE_STYLE,
                {column.cards.len().to_string()}
            }
            {people(__scope, k, addr)}
            if writable && chosen.get() == Some(Chosen::Column(addr)) && renaming.get() != Some(addr) {
                Button {
                    size: "xs",
                    variant: "subtle",
                    onclick: { let (k, label) = (kh.clone(), label2.clone()); move || k.start_rename(addr, label.clone()) },
                    "Rename"
                }
            }
        }
    };
    // A column is dragged by its header, when the person may move it.
    let movable = board_writable && writable;
    let header = if movable { rsx! { SortableHandle { {header} } } } else { header };
    let cards: Vec<NodeHandle> = column.cards.iter().map(|card| render_card(__scope, k, &column, card.clone())).collect();
    let can_add = column.writable;
    let kd = k.clone();
    rsx! {
        div {
            style: "display: contents;",
            {header}
            SortableList {
                group: CARDS,
                id: key_of(addr),
                label: column.label.clone(),
                class: "kanban__cards",
                style: format!("{CARD_LIST_STYLE}{SORT_THEME}"),
                // A read-only board takes no drags; a read-only column on a
                // writable board still judges a drop into it, so the caret says
                // why not ("You can read this, not change it.").
                disabled: !board_writable,
                lift_keys: lift_keys(),
                judge: Some(judge),
                ondrop: move |m: SortMove| kd.drop_move(m),
                {cards}
            }
            if can_add {
                div {
                    class: "kanban__add-card",
                    style: ADD_STYLE,
                    onclick: { let k = ka.clone(); move || k.add(addr, true) },
                    "+ Add card"
                }
            }
        }
    }
}

/// Who else is on `addr`: the app's own chips, up to three and "+n"; a click
/// follows them.
fn people(__scope: &mut RenderScope, k: &Kanban, addr: Addr) -> NodeHandle {
    let (seen, follow) = (k.host.seen(), k.host.clone());
    rsx! {
        span {
            class: "pimble-pane__people",
            style: "margin-left: auto;",
            for person in seen.people_in(addr).into_iter().take(3) {
                PersonChip {
                    key: person.key.clone(),
                    person: person.clone(),
                    onfollow: { let f = follow.clone(); move |key: String| f.follow(&key) },
                }
            }
            span {
                class: "pimble-pane__more",
                style: {move || if seen.people_in(addr).len() > 3 { "" } else { "display: none;" }},
                {move || more_label(seen.people_in(addr).len().saturating_sub(3))}
            }
        }
    }
}

/// One card: its icon, label and subtasks in its colors, who is on it, and
/// while chosen, what can be done with it.
fn render_card(__scope: &mut RenderScope, k: &Kanban, column: &board::Column, card: board::Card) -> NodeHandle {
    let addr = card.addr;
    let seen = k.host.seen();
    let (kc, ko, kr, kd, kf, kf2, kn) = (k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone());
    let chosen = k.chosen;
    let renaming = k.renaming;
    let (color, background) = (card.color.clone(), card.background.clone());
    let (label, label2) = (card.label.clone(), card.label.clone());
    let writable = card.writable;
    let (icon, subtasks, label_color) = (card.icon.clone(), card.subtasks, card.color.clone());
    // "Move to" the other columns, in their order, as the board has them now
    // (a column added or renamed since this card was drawn included).
    let (board, home) = (k.board, column.addr);

    let km = k.clone();
    let item_label = card.label.clone();
    let card = rsx! {
        div {
            class: "kanban__card",
            id: dom_id(addr),
            style: {|| {
                let leader = seen.following().and_then(|key| seen.people_in(addr).into_iter().find(|p| p.key == key).map(|p| p.color));
                card_style(chosen.get() == Some(Chosen::Card(addr)), background.as_deref().map(|b| row_background(b, seen.dark())), leader)
            }},
            onclick: move || {
                kc.choose(Some(Chosen::Card(addr)));
                if untracked(|| kc.renaming.get()) != Some(addr) {
                    kc.take_keyboard();
                }
            },
            div {
                style: "display: flex; align-items: center; gap: 6px; font-size: 13px;",
                span {
                    style: {|| format!("display: inline-flex; width: 14px; height: 14px; flex-shrink: 0;{}", color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default())},
                    IconGlyph { name: icon.clone() }
                }
                if renaming.get() == Some(addr) {
                    input {
                        class: "kanban__field",
                        style: FIELD_STYLE,
                        ref: kf.field,
                        placeholder: "Card name",
                        value: {move || kf.draft.get()},
                        oninput: move |v: String| kf2.draft.set(v),
                        onkeydown: {
                            let k = kr.clone();
                            move |key: &KeyEventData| match key.key.as_str() {
                                "Enter" => { k.commit_rename(); true }
                                "Escape" => { k.cancel_rename(); true }
                                _ => false,
                            }
                        },
                    }
                } else {
                    span {
                        class: "kanban__label",
                        style: {
                            let color = label_color.clone();
                            move || format!("flex: 1; overflow-wrap: anywhere;{}", color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default())
                        },
                        {label.clone()}
                    }
                }
                if subtasks > 0 {
                    span {
                        class: "kanban__subtasks",
                        style: "display: inline-flex; align-items: center; gap: 2px; font-size: 11px; color: var(--rinch-color-dimmed);",
                        title: format!("{} subtask{}", subtasks, if subtasks == 1 { "" } else { "s" }),
                        span { style: "display: inline-flex; width: 12px; height: 12px;", IconGlyph { name: "subtask".to_string() } }
                        {subtasks.to_string()}
                    }
                }
            }
            div {
                style: "display: flex; align-items: center; min-height: 0;",
                {people(__scope, k, addr)}
            }
            if chosen.get() == Some(Chosen::Card(addr)) && renaming.get() != Some(addr) {
                div {
                    class: "kanban__actions",
                    style: "display: flex; flex-wrap: wrap; align-items: center; gap: 4px; padding-top: 4px; border-top: 1px solid var(--rinch-color-default-border, rgba(127,127,127,0.3));",
                    Button { size: "xs", variant: "subtle", onclick: { let k = ko.clone(); move || k.host.open(addr) }, "Open" }
                    if writable {
                        Button { size: "xs", variant: "subtle", onclick: { let (k, l) = (kn.clone(), label2.clone()); move || k.start_rename(addr, l.clone()) }, "Rename" }
                        Button { size: "xs", variant: "subtle", color: "red", onclick: { let k = kd.clone(); move || k.delete(addr) }, "Delete" }
                    }
                    if !move_targets(board, home, writable).is_empty() {
                        span { style: LINE_STYLE, "Move to \u{25b8}" }
                    }
                    for target in move_targets(board, home, writable) {
                        Button {
                            key: format!("{} {}", target.to.node, target.name),
                            size: "xs",
                            variant: "default",
                            onclick: { let (k, to) = (km.clone(), target.to); move || k.move_to(addr, to_column(to), false) },
                            {target.name.clone()}
                        }
                    }
                }
            }
        }
    };
    // The whole card drags (a press that never moves 5 px stays a click);
    // a card the person may only read does not.
    rsx! {
        SortableItem {
            value: key_of(addr),
            disabled: !writable,
            label: item_label,
            class: "kanban__card-item",
            style: "position: relative;",
            {card}
        }
    }
}

pimble_plugin_sdk::plugin!(describe: describe, view: view);

#[cfg(test)]
mod tests {
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

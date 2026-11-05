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

use std::sync::Arc;

use pimble_plugin_sdk::pimble_core::{custom_keys, plan};
use pimble_plugin_sdk::pimble_ui::appearance::{display_color, row_background, IconGlyph};
use pimble_plugin_sdk::pimble_ui::presence::{more_label, PersonChip};
use pimble_plugin_sdk::prelude::*;
use rinch::core::dom::NodeId as DomId;

use std::collections::BTreeMap;

use board::{board_of, column_key, drop_of, key_of, step_card, step_column, Board, Lane, Step, Target};

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

/// How big a lane is drawn (kobayban's three): one bar with its name and
/// count, about three cards a column with the rest scrolled, or everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    Collapsed,
    Minimized,
    Expanded,
}

impl Size {
    fn name(self) -> &'static str {
        match self {
            Size::Collapsed => "collapsed",
            Size::Minimized => "minimized",
            Size::Expanded => "expanded",
        }
    }

    fn parse(s: &str) -> Option<Size> {
        match s {
            "collapsed" => Some(Size::Collapsed),
            "minimized" => Some(Size::Minimized),
            "expanded" => Some(Size::Expanded),
            _ => None,
        }
    }
}

/// How this person has the board on this device: the lanes they sized (by
/// [`lane_key`]) and the columns they folded (by [`column_key`], so a folded
/// "Done" is folded in every lane). Kept by the host
/// (`Host::remember`), never in the store: someone else's board is
/// not folded because this one is.
#[derive(Debug, Clone, Default, PartialEq)]
struct Look {
    lanes: BTreeMap<String, Size>,
    folded: Vec<String>,
}

impl Look {
    fn parse(text: &str) -> Look {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else { return Look::default() };
        let lanes = value
            .get("lanes")
            .and_then(|l| l.as_object())
            .map(|l| l.iter().filter_map(|(k, v)| Some((k.clone(), Size::parse(v.as_str()?)?))).collect())
            .unwrap_or_default();
        let folded = value
            .get("folded")
            .and_then(|f| f.as_array())
            .map(|f| f.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        Look { lanes, folded }
    }

    fn to_json(&self) -> String {
        let lanes: serde_json::Map<String, serde_json::Value> = self.lanes.iter().map(|(k, v)| (k.clone(), serde_json::Value::from(v.name()))).collect();
        serde_json::json!({ "lanes": lanes, "folded": self.folded }).to_string()
    }
}

/// A lane's key in the [`Look`]: its node on the board, or "top" for the
/// unnamed lane.
fn lane_key(lane: &Lane) -> String {
    lane.node.map_or_else(|| "top".to_string(), key_of)
}

fn dom_id(addr: Addr) -> String {
    format!("kanban-{}", addr.node)
}

impl Kanban {
    /// Read the forest into the board, and each lane's own forest: the
    /// forests read, which say what a change must touch to be read again.
    async fn read(&self) -> Option<Vec<Forest>> {
        // The stores listed again with every read: a drag is judged
        // synchronously over what is held (`judge_drop`), so a mounted
        // column's store closed since is refused at the caret, not after
        // the drop.
        let _ = self.api.stores().await;
        match self.api.metanode_source(self.metanode).await {
            Ok(forest) => {
                // The tops that are kanbans are lanes: a kanban itself, or a
                // mount of one (read from its source).
                let tops: Vec<(Addr, pimble_plugin_sdk::pimble_core::Node)> = forest.tops().map(|t| (t.addr, t.node.clone())).collect();
                let mut lanes: Vec<(Addr, Forest)> = Vec::new();
                for (top, node) in tops {
                    let source = if is_kanban(&node) {
                        Some(top)
                    } else if let Some(m) = node.mount_ref() {
                        let source = Addr::new(m.source_store, m.source_node);
                        self.api.node(source).await.ok().filter(is_kanban).map(|_| source)
                    } else {
                        None
                    };
                    let Some(source) = source else { continue };
                    match self.api.metanode_source(source).await {
                        Ok(lane) => lanes.push((top, lane)),
                        Err(e) => self.status.set(e.sentence()),
                    }
                }
                let board = board_of(&forest, &lanes, display_label);
                // Something chosen that is no longer on the board is let go,
                // unless it is being named: a card just made is chosen before
                // the board has read it.
                let naming = untracked(|| self.renaming.get());
                if let Some(chosen) = untracked(|| self.chosen.get()).filter(|c| Some(c.addr()) != naming) {
                    let there = match chosen {
                        Chosen::Card(a) => board.find_card(a).is_some(),
                        Chosen::Column(a) => board.find_column(a).is_some(),
                        Chosen::Lane(a) => board.lane_of_node(a).is_some(),
                    };
                    if !there {
                        self.choose(None);
                    }
                }
                if untracked(|| self.board.get()) != board {
                    self.board.set(board);
                }
                self.loaded.set(true);
                let mut forests = vec![forest];
                forests.extend(lanes.into_iter().map(|(_, lane)| lane));
                Some(forests)
            }
            Err(e) => {
                self.status.set(e.sentence());
                None
            }
        }
    }

    /// A click on `card`: it opens beside the board, or with Ctrl (Cmd)
    /// held in the board's place; a second click on it within
    /// [`DOUBLE_CLICK`] renames it instead, as a double-click does a row of the
    /// explorer. The board keeps the keyboard.
    fn click_card(&self, card: Addr, modifiers: rinch::core::ModifierState) {
        let again = clicked_again(card);
        self.choose(Some(Chosen::Card(card)));
        if untracked(|| self.renaming.get()) == Some(card) {
            return;
        }
        let board = untracked(|| self.board.get());
        let Some(shown) = board.card(card) else { return };
        if again {
            if shown.writable {
                self.start_rename(card, shown.label.clone());
            }
            return;
        }
        if modifiers.ctrl || modifiers.meta {
            self.host.open(card);
        } else {
            self.host.open_beside(card);
        }
        self.take_keyboard();
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

    /// How big `lane` is drawn, tracked: a board with one lane shows it
    /// whole; with several, each is minimized until the person sizes it.
    fn size_of(&self, lane: &Lane, lanes: usize) -> Size {
        if lanes < 2 {
            return Size::Expanded;
        }
        self.look.with(|l| l.lanes.get(&lane_key(lane)).copied()).unwrap_or(Size::Minimized)
    }

    /// Change how the board looks, and have the host remember it.
    fn relook(&self, f: impl FnOnce(&mut Look)) {
        let mut look = untracked(|| self.look.get());
        f(&mut look);
        if untracked(|| self.look.get()) != look {
            self.host.remember(look.to_json());
            self.look.set(look);
        }
    }

    fn set_size(&self, lane: &Lane, size: Size) {
        let key = lane_key(lane);
        self.relook(|l| {
            l.lanes.insert(key, size);
        });
    }

    /// Fold or unfold the column named `name`, in every lane.
    fn toggle_fold(&self, name: &str) {
        let key = column_key(name);
        self.relook(|l| match l.folded.iter().position(|f| *f == key) {
            Some(i) => {
                l.folded.remove(i);
            }
            None => l.folded.push(key),
        });
    }

    /// Whether the column named `name` is folded, tracked.
    fn folded(&self, name: &str) -> bool {
        let key = column_key(name);
        self.look.with(|l| l.folded.contains(&key))
    }

    /// Move the column at place `from` of every lane to place `at` (counted
    /// without it) of its lane: an aligned board's header moved.
    fn columns_everywhere(&self, from: usize, at: usize) {
        let board = untracked(|| self.board.get());
        for lane in &board.lanes {
            let Some(column) = lane.columns.get(from) else { continue };
            let others: Vec<Addr> = lane.columns.iter().map(|c| c.addr).filter(|a| *a != column.addr).collect();
            let place = match at.checked_sub(1).and_then(|i| others.get(i)) {
                None => Place::First,
                Some(before) => Place::After(before.node),
            };
            self.move_to(column.addr, Target { parent: lane.metanode, place }, false);
        }
    }

    /// The one way anything moves on the board: a card to a column or a place
    /// in one, a column along the board. Judged in the metanode first, so a
    /// refusal is the explorer's sentence and nothing is sent; a warning
    /// (another store, leaving a share, leaving this view) waits for "Move
    /// anyway" unless `confirmed`. It is judged in the lane it lands in (the
    /// lane's own kanban), so a card carried into another lane is not taken
    /// for one leaving the view.
    fn move_to(&self, node: Addr, target: Target, confirmed: bool) {
        let k = self.clone();
        let judged_in = untracked(|| self.board.with(|b| b.lane_for_parent(target.parent).map(|l| l.metanode))).unwrap_or(self.metanode);
        self.waiting.set(None);
        spawn_local(async move {
            // Judged against the stores open now: a column mounted from a
            // store closed since the board was read is refused, not planned.
            let _ = k.api.stores().await;
            let edit = MetanodeEdit::Move { nodes: vec![node], target: target.parent, place: target.place };
            let judged = match k.api.judge_in_metanode(judged_in, edit).await {
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
                                Chosen::Lane(_) => Chosen::Lane(to.addr),
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
        // An aligned board's header moves that column in every lane.
        let board = untracked(|| self.board.get());
        if m.group == COLUMNS && board.aligned() && board.lanes.len() > 1 {
            let Some((0, from)) = board.find_column(node) else { return };
            let others: Vec<Addr> = board.lanes[0].columns.iter().map(|c| c.addr).filter(|a| *a != node).collect();
            let at = match target.place {
                Place::After(after) => others.iter().position(|a| a.node == after).map_or(0, |i| i + 1),
                _ => 0,
            };
            return self.columns_everywhere(from, at);
        }
        self.move_to(node, target, false);
    }

    /// Alt+Shift+arrow on what is chosen.
    fn step(&self, step: Step) {
        let board = untracked(|| self.board.get());
        let (node, target) = match untracked(|| self.chosen.get()) {
            Some(Chosen::Card(card)) => (card, step_card(&board, card, step)),
            Some(Chosen::Column(column)) if board.aligned() && board.lanes.len() > 1 => {
                let Some((_, c)) = board.find_column(column) else { return };
                let at = match step {
                    Step::Left => c.checked_sub(1),
                    Step::Right => (c + 1 < board.lanes[0].columns.len()).then_some(c + 1),
                    Step::Up | Step::Down => None,
                };
                if let Some(at) = at {
                    self.columns_everywhere(c, at);
                }
                return;
            }
            Some(Chosen::Column(column)) => (column, step_column(&board, column, step)),
            Some(Chosen::Lane(_)) | None => return,
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
        // An aligned board's header renames that column in every lane: one
        // rename each, as the explorer would make them.
        let board = untracked(|| self.board.get());
        let nodes: Vec<Addr> = match board.find_column(addr) {
            Some((_, c)) if board.aligned() && board.lanes.len() > 1 => board.across(c).iter().map(|col| col.addr).collect(),
            _ => vec![addr],
        };
        let k = self.clone();
        spawn_local(async move {
            for node in nodes {
                let title = title.clone();
                let edited = k
                    .api
                    .edit(node, move |doc, now| -> Result<(), ApiError> {
                        doc.set_title(&title, now)?;
                        doc.set_custom(custom_keys::EXPLICIT_TITLE, &serde_json::Value::Bool(true), now)?;
                        Ok(())
                    })
                    .await;
                if let Err(e) = edited {
                    k.say(e.sentence());
                }
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

    /// "+ Add column" under each of `lanes` (one lane's, or every lane's on a
    /// shared header), last, then its name typed in place: on a shared header
    /// the rename names them all, as the lanes still line up.
    fn add_column(&self, lanes: Vec<Addr>) {
        let k = self.clone();
        spawn_local(async move {
            let mut first = None;
            for lane in lanes {
                let made = match pimble_api::recipes::document("", None) {
                    Ok(init) => k.api.create(lane, Place::Last, init).await,
                    Err(e) => Err(e),
                };
                match made {
                    Ok(made) => {
                        first.get_or_insert(made);
                    }
                    Err(e) => {
                        k.say(e.sentence());
                        break;
                    }
                }
            }
            if let Some(made) = first {
                k.say("");
                k.choose(Some(Chosen::Column(made)));
                k.start_rename(made, String::new());
            }
        });
    }

    /// "+ Add lane": a kanban under the board, last, with the columns of the
    /// lane above it (the first columns when there are none), then its name
    /// typed in place.
    fn add_lane(&self) {
        let k = self.clone();
        let mut names: Vec<String> = untracked(|| self.board.with(|b| b.lanes.last().map(|l| l.columns.iter().map(|c| c.label.clone()).collect()).unwrap_or_default()));
        if names.is_empty() {
            names = FIRST_COLUMNS.iter().map(|n| n.to_string()).collect();
        }
        spawn_local(async move {
            let init = pimble_api::recipes::metanode("", Some("layout-kanban"), VIEWER, 2);
            let lane = match k.api.create(k.metanode, Place::Last, init).await {
                Ok(lane) => lane,
                Err(e) => return k.say(e.sentence()),
            };
            for name in names {
                let made = match pimble_api::recipes::document(&name, None) {
                    Ok(init) => k.api.create(lane, Place::Last, init).await.map(|_| ()),
                    Err(e) => Err(e),
                };
                if let Err(e) = made {
                    k.say(e.sentence());
                    break;
                }
            }
            k.choose(Some(Chosen::Lane(lane)));
            k.start_rename(lane, String::new());
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
    /// card beside the board (Ctrl+Enter in its place), F2 renames, Delete deletes, Escape lets go. Answers whether it
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
        let writable = |chosen: Chosen| match chosen {
            Chosen::Card(a) => board.card(a).is_some_and(|c| c.writable),
            Chosen::Column(a) => board.column(a).is_some_and(|c| c.writable),
            Chosen::Lane(a) => board.lane_of_node(a).is_some_and(|l| l.writable),
        };
        match (key.key.as_str(), chosen) {
            // Enter opens beside, as a click does; Ctrl (Cmd) opens in the board's place.
            ("Enter", Some(Chosen::Card(card))) if key.ctrl || key.meta => self.host.open(card),
            ("Enter", Some(Chosen::Card(card))) => self.host.open_beside(card),
            ("F2", Some(c)) if writable(c) => {
                let now = match c {
                    Chosen::Card(a) => board.card(a).map(|k| k.label.clone()),
                    Chosen::Column(a) => board.column(a).map(|col| col.label.clone()),
                    Chosen::Lane(a) => board.lane_of_node(a).and_then(|l| l.label.clone()),
                };
                self.start_rename(c.addr(), now.unwrap_or_default());
            }
            ("Delete", Some(c @ Chosen::Card(card))) if writable(c) => self.delete(card),
            ("Escape", Some(_)) => self.choose(None),
            _ => return false,
        }
        true
    }
}

/// Where a plain arrow takes the choice: along a column's cards, across to
/// the neighbor column's card at the same height, onto a column's header from
/// above its first card, and past a column's top or bottom into the lined-up
/// column of the lane above or below.
fn walk(board: &Board, chosen: Option<Chosen>, step: Step) -> Option<Chosen> {
    if board.aligned() && board.lanes.len() > 1 {
        return walk_aligned(board, chosen, step);
    }
    let top_of = |l: usize, c: usize| {
        let col = &board.lanes[l].columns[c];
        col.cards.first().map_or(Chosen::Column(col.addr), |k| Chosen::Card(k.addr))
    };
    let first = || board.lanes.iter().position(|l| !l.columns.is_empty()).map(|l| top_of(l, 0));
    match chosen {
        None | Some(Chosen::Lane(_)) => first(),
        Some(Chosen::Card(card)) => {
            let (l, c, i) = board.find_card(card)?;
            let column = &board.lanes[l].columns[c];
            match step {
                Step::Up if i == 0 => Some(Chosen::Column(column.addr)),
                Step::Up => Some(Chosen::Card(column.cards[i - 1].addr)),
                Step::Down => match column.cards.get(i + 1) {
                    Some(k) => Some(Chosen::Card(k.addr)),
                    None => board.matching_column(l, c, l + 1).map(|to| Chosen::Column(board.lanes[l + 1].columns[to].addr)),
                },
                Step::Left | Step::Right => {
                    let to = if step == Step::Left { c.checked_sub(1)? } else { c + 1 };
                    let next = board.lanes[l].columns.get(to)?;
                    Some(next.cards.get(i.min(next.cards.len().saturating_sub(1))).map_or(Chosen::Column(next.addr), |k| Chosen::Card(k.addr)))
                }
            }
        }
        Some(Chosen::Column(column)) => {
            let (l, c) = board.find_column(column)?;
            let lane = &board.lanes[l];
            match step {
                Step::Down => match lane.columns[c].cards.first() {
                    Some(k) => Some(Chosen::Card(k.addr)),
                    None => board.matching_column(l, c, l + 1).map(|to| Chosen::Column(board.lanes[l + 1].columns[to].addr)),
                },
                Step::Up => {
                    let up = l.checked_sub(1)?;
                    let col = &board.lanes[up].columns[board.matching_column(l, c, up)?];
                    Some(col.cards.last().map_or(Chosen::Column(col.addr), |k| Chosen::Card(k.addr)))
                }
                Step::Left => c.checked_sub(1).map(|to| Chosen::Column(lane.columns[to].addr)),
                Step::Right => lane.columns.get(c + 1).map(|col| Chosen::Column(col.addr)),
            }
        }
    }
}

/// [`walk`] on an aligned board: a column is its header and then its cards
/// lane after lane, so the arrows go down a column through every lane.
fn walk_aligned(board: &Board, chosen: Option<Chosen>, step: Step) -> Option<Chosen> {
    let header = |c: usize| board.lanes[0].columns.get(c).map(|col| Chosen::Column(col.addr));
    let down = |c: usize| -> Vec<Chosen> {
        header(c).into_iter().chain(board.across(c).into_iter().flat_map(|col| col.cards.iter().map(|k| Chosen::Card(k.addr)))).collect()
    };
    match chosen {
        None | Some(Chosen::Lane(_)) => down(0).get(1).or(down(0).first()).copied(),
        Some(Chosen::Column(column)) => {
            let (_, c) = board.find_column(column)?;
            match step {
                Step::Down => down(c).get(1).copied(),
                Step::Up => None,
                Step::Left => header(c.checked_sub(1)?),
                Step::Right => header(c + 1),
            }
        }
        Some(Chosen::Card(card)) => {
            let (l, c, i) = board.find_card(card)?;
            let at = down(c).iter().position(|x| *x == Chosen::Card(card))?;
            match step {
                Step::Up => down(c).get(at - 1).copied(),
                Step::Down => down(c).get(at + 1).copied(),
                Step::Left | Step::Right => {
                    let to = if step == Step::Left { c.checked_sub(1)? } else { c + 1 };
                    let next = board.lanes[l].columns.get(to)?;
                    Some(next.cards.get(i.min(next.cards.len().saturating_sub(1))).map_or_else(|| header(to).unwrap_or(Chosen::Column(next.addr)), |k| Chosen::Card(k.addr)))
                }
            }
        }
    }
}

const BOARD_STYLE: &str = "display: flex; flex-direction: column; gap: 8px; padding: 12px 16px; height: 100%; box-sizing: border-box; outline: none;";
/// The board's rows (the header row, then a row per lane), scrolled together
/// so columns stay lined up.
const GRID_STYLE: &str = "display: flex; flex-direction: column; gap: 10px; overflow: auto; flex: 1; min-height: 0; padding-bottom: 12px;";
/// A column's width, a folded column's, and the lane names' column.
const COLUMN_W: u32 = 280;
const FOLDED_W: u32 = 44;
const LABEL_W: u32 = 168;
/// A minimized lane shows about three cards a column (kobayban's cap).
const MINIMIZED_MAX: u32 = 220;
const ROW_STYLE: &str = "display: flex; gap: 12px; align-items: stretch; flex-shrink: 0;";
const HEADER_ROW_STYLE: &str = "display: flex; gap: 12px; align-items: flex-end; flex-shrink: 0;";
// The sortable lists' layout is written here as well as in rinch's stylesheet,
// which is the app's: an app on a rinch from before sortable lists still lays
// the board out (only the caret and the refusal box need its stylesheet).
const COLUMN_LIST_STYLE: &str = "display: flex; flex-direction: row; gap: 12px; align-items: flex-end; flex-shrink: 0; position: relative;";
const CARD_LIST_STYLE: &str = "display: flex; flex-direction: column; gap: 8px; min-height: 28px; position: relative;";
const CELL_BG: &str = "background: var(--kanban-cell); border: 1px solid var(--kanban-cell-border); border-radius: 8px;";

/// The board's surfaces, set on its root and inherited: a column's cell a
/// step off the pane and each card a step further, lighter in the dark
/// theme and white in the light one, so cards stand on their cells.
fn surfaces(dark: bool) -> &'static str {
    if dark {
        " --kanban-cell: var(--rinch-color-dark-6); --kanban-cell-border: var(--rinch-color-dark-5); --kanban-card: var(--rinch-color-dark-5); --kanban-card-border: var(--rinch-color-dark-4);"
    } else {
        " --kanban-cell: var(--rinch-color-gray-1); --kanban-cell-border: var(--rinch-color-gray-3); --kanban-card: var(--rinch-color-white); --kanban-card-border: var(--rinch-color-gray-3);"
    }
}
const LANE_LABEL_STYLE: &str = "display: flex; flex-direction: column; gap: 4px; padding: 6px 4px; flex-shrink: 0; box-sizing: border-box; min-width: 0;";
const ICON_BUTTON: &str = "display: inline-flex; align-items: center; justify-content: center; width: 22px; height: 22px; border-radius: 4px; cursor: pointer; color: var(--rinch-color-dimmed);";
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
const FIELD_STYLE: &str = "flex: 1 1 0; min-width: 0; width: 100%; box-sizing: border-box; font: inherit; padding: 2px 4px;";
/// The icon a plain note has (`node_icon`), which a card leaves out.
const DEFAULT_CARD_ICON: &str = "file";

/// How soon a second click on a card renames it rather than opening it again.
const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(500);

/// Whether this click on `addr` (a card, a column's header, a lane's name)
/// is the second of a double-click: what renames each of them.
fn clicked_again(addr: Addr) -> bool {
    LAST_CLICK.with(|last| {
        let now = std::time::Instant::now();
        let again = last.get().is_some_and(|(at, on)| on == addr && now.duration_since(at) < DOUBLE_CLICK);
        last.set(if again { None } else { Some((now, addr)) });
        again
    })
}

thread_local! {
    /// The last click on a card, a header or a lane's name, for [`DOUBLE_CLICK`].
    static LAST_CLICK: std::cell::Cell<Option<(std::time::Instant, Addr)>> = const { std::cell::Cell::new(None) };
}

const LINE_STYLE: &str = "font-size: 12px; color: var(--rinch-color-dimmed);";

/// A card's style: chosen or not, its row color, and the color of the person
/// the pane follows when they are on it.
fn card_style(chosen: bool, background: Option<String>, leader: Option<String>) -> String {
    // A wider border takes its width from the padding, so choosing a card
    // never moves or rewraps what is in it.
    let (border, padding) = match (leader, chosen) {
        (Some(color), _) => (format!("2px solid {color}"), "9px 11px"),
        (None, true) => ("2px solid var(--rinch-primary-color)".to_string(), "9px 11px"),
        (None, false) => ("1px solid var(--kanban-card-border)".to_string(), "10px 12px"),
    };
    let background = background.unwrap_or_else(|| "var(--kanban-card)".to_string());
    format!("display: flex; flex-direction: column; gap: 4px; padding: {padding}; border-radius: 6px; cursor: pointer; border: {border}; background: {background};")
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
            style: {let seen = k.host.seen(); move || format!("{BOARD_STYLE}{}", surfaces(seen.dark()))},
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

/// A lane as the board draws it now.
#[derive(Debug, Clone, PartialEq)]
struct LaneShown {
    /// The lane, without its columns.
    lane: Lane,
    size: Size,
    /// The board shares one header row.
    aligned: bool,
    /// The board has a column of lane names (it has named lanes).
    laned: bool,
}

impl LaneShown {
    /// What redraws the lane: never its cards, which its cells follow on
    /// their own (a card redrawn mid-drag would lose the drag).
    fn key(&self) -> String {
        let l = &self.lane;
        format!("{:?} {:?} {:?} {:?} {} {:?} {} {}", l.metanode, l.node, l.label, l.color, l.writable, self.size, self.aligned, self.laned)
    }
}

/// How many cards lane `lane` has, tracked.
fn lane_count(board: Signal<Board>, lane: Addr) -> usize {
    board.with(|b| b.lanes.iter().find(|l| l.metanode == lane).map_or(0, |l| l.columns.iter().map(|c| c.cards.len()).sum()))
}

/// The lanes as the board draws them, tracked: the board and the look.
fn lanes_shown(board: Signal<Board>, _look: Signal<Look>, k: &Kanban) -> Vec<LaneShown> {
    board.with(|b| {
        let (aligned, laned) = (b.aligned(), b.lanes.iter().any(|l| l.node.is_some()));
        // Without its columns: a `for` row is drawn again when its item
        // changes, and a lane's cells follow the board on their own.
        b.lanes.iter().map(|lane| LaneShown { lane: Lane { columns: Vec::new(), ..lane.clone() }, size: k.size_of(lane, b.lanes.len()), aligned, laned }).collect()
    })
}

/// A lane's columns as the board has them now, tracked, each with whether it
/// is the last (where done things pile up) and folded.
fn lane_columns(k: &Kanban, lane: Addr) -> Vec<(board::Column, bool, bool)> {
    let columns = k.board.with(|b| b.lanes.iter().find(|l| l.metanode == lane).map(|l| l.columns.clone()).unwrap_or_default());
    let n = columns.len();
    columns.into_iter().enumerate().map(|(i, c)| {
        let folded = k.folded(&c.label);
        (c, n > 1 && i + 1 == n, folded)
    }).collect()
}

/// A header's columns, tracked: a lane's own (`lane`), or on an aligned board
/// the first lane's, each with its cards counted in every lane, and folded.
fn header_columns(k: &Kanban, lane: Option<Addr>) -> Vec<(board::Column, usize, bool)> {
    let columns: Vec<(board::Column, usize)> = k.board.with(|b| match lane {
        Some(lane) => b.lanes.iter().find(|l| l.metanode == lane).map(|l| l.columns.iter().map(|c| (c.clone(), c.cards.len())).collect()).unwrap_or_default(),
        None => b.lanes.first().map(|l| {
            l.columns.iter().enumerate().map(|(i, c)| (c.clone(), b.across(i).iter().map(|col| col.cards.len()).sum())).collect()
        }).unwrap_or_default(),
    });
    columns.into_iter().map(|(c, n)| {
        let folded = k.folded(&c.label);
        (c, n, folded)
    }).collect()
}

/// A row of column headers: the board's (`lane` `None`: one for every lane,
/// after a space the width of the lane names when there are any) or a lane's
/// own. A horizontal sortable list dragged by the headers, then "+ Add
/// column".
fn header_row(__scope: &mut RenderScope, k: &Kanban, lane: Option<Lane>, judge: SortJudge) -> NodeHandle {
    let (kd, kc, ka, kt) = (k.clone(), k.clone(), k.clone(), k.clone());
    let board = untracked(|| k.board.get());
    let first = board.lanes.first().cloned();
    let Some(owner) = lane.clone().or(first) else { return rsx! { span { style: "display: none;" } } };
    let spacer = lane.is_none() && board.lanes.iter().any(|l| l.node.is_some());
    let writable = owner.writable;
    let (list_lane, list_label) = (owner.metanode, owner.label.clone().unwrap_or_else(|| "Columns".to_string()));
    let shared = lane.is_none() && board.lanes.len() > 1;
    rsx! {
        div {
            class: "kanban__header-row",
            style: HEADER_ROW_STYLE,
            // The corner above the lane names holds the lane tools.
            if spacer {
                div {
                    class: "kanban__corner",
                    style: format!("width: {LABEL_W}px; flex-shrink: 0; display: flex; gap: 2px; align-items: center; padding: 2px 0;"),
                    if shared {
                        {lane_tools(__scope, &kt)}
                    }
                }
            }
            SortableList {
                group: COLUMNS,
                id: key_of(list_lane),
                axis: SortAxis::Horizontal,
                label: list_label,
                class: "kanban__column-list",
                style: format!("{COLUMN_LIST_STYLE}{SORT_THEME}"),
                disabled: !writable,
                lift_keys: lift_keys(),
                indicator: SortIndicator::Gap,
                source: SortSource::Collapse,
                // The app's own picture of the item under the pointer: a copy of
                // a plugin's elements does not keep its look across the host.
                preview: SortPreview::Snapshot,
                judge: Some(judge.clone()),
                ondrop: move |m: SortMove| kd.drop_move(m),
                for (column, count, folded) in header_columns(&kc, lane.as_ref().map(|l| l.metanode)) {
                    SortableItem {
                        key: format!("{column:?} {count} {folded}"),
                        value: key_of(column.addr),
                        handle: true,
                        disabled: folded || !(column.writable && writable),
                        label: column.label.clone(),
                        class: "kanban__column",
                        style: format!("width: {}px; flex-shrink: 0; position: relative;", if folded { FOLDED_W } else { COLUMN_W }),
                        {column_header(__scope, &kc, column.clone(), count, folded, writable)}
                    }
                }
            }
            if writable {
                div {
                    class: "kanban__add-column",
                    style: "flex-shrink: 0; padding: 4px 8px; font-size: 13px; color: var(--rinch-color-dimmed); cursor: pointer; white-space: nowrap;",
                    onclick: {
                        let k = ka.clone();
                        move || {
                            // On a shared header, a new column goes in every lane.
                            let lanes: Vec<Addr> = if shared { untracked(|| k.board.get()).lanes.iter().map(|l| l.metanode).collect() } else { vec![list_lane] };
                            k.add_column(lanes);
                        }
                    },
                    "+ Add column"
                }
            }
        }
    }
}

/// A column's header: its name (renamed in place; on a shared header, in
/// every lane), its count, its people, fold; what a column is dragged by. A
/// folded column's header is a narrow "+" that unfolds it.
fn column_header(__scope: &mut RenderScope, k: &Kanban, column: board::Column, count: usize, folded: bool, board_writable: bool) -> NodeHandle {
    let addr = column.addr;
    let name = column.label.clone();
    if folded {
        let kf = k.clone();
        let tip = format!("Unfold {name} ({count})");
        return rsx! {
            div {
                class: "kanban__header kanban__header--folded",
                style: format!("{ICON_BUTTON} width: {FOLDED_W}px; height: 28px; {CELL_BG}"),
                title: tip,
                onclick: move || kf.toggle_fold(&name),
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "plus".to_string() } }
            }
        };
    }
    let seen = k.host.seen();
    let color = column.color.clone();
    let (kc, kr, kf, kf2, kfold) = (k.clone(), k.clone(), k.clone(), k.clone(), k.clone());
    let (label, label2) = (column.label.clone(), column.label.clone());
    let writable = column.writable;
    let (chosen, renaming) = (k.chosen, k.renaming);
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
                let again = clicked_again(addr);
                kc.choose(Some(Chosen::Column(addr)));
                if untracked(|| kc.renaming.get()) == Some(addr) {
                    return;
                }
                if again && writable {
                    kc.start_rename(addr, label2.clone());
                } else {
                    kc.take_keyboard();
                }
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
                    style: "overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                    title: if writable { "Double-click or F2 to rename" } else { "" },
                    {label.clone()}
                }
                span { style: LINE_STYLE, {count.to_string()} }
            }
            {people(__scope, k, addr)}
            span {
                style: ICON_BUTTON,
                title: "Fold this column",
                onclick: { let name = name.clone(); move || kfold.toggle_fold(&name) },
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "minus".to_string() } }
            }
        }
    };
    // A column is dragged by its header, when the person may move it.
    if board_writable && writable { rsx! { SortableHandle { {header} } } } else { header }
}

/// What a board with lanes offers for all of them, as icons: every lane shown
/// whole, and each back to its own size.
fn lane_tools(__scope: &mut RenderScope, k: &Kanban) -> NodeHandle {
    let (ke, kr) = (k.clone(), k.clone());
    rsx! {
        span {
            style: "display: inline-flex; gap: 2px; align-items: center;",
            span {
                class: "kanban__expand-all",
                style: ICON_BUTTON,
                title: "Show every lane whole",
                onclick: move || {
                    let lanes = untracked(|| ke.board.get()).lanes;
                    ke.relook(|l| {
                        for lane in &lanes {
                            l.lanes.insert(lane_key(lane), Size::Expanded);
                        }
                    });
                },
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "chevrons-down".to_string() } }
            }
            span {
                class: "kanban__reset-lanes",
                style: ICON_BUTTON,
                title: "Put every lane back to its own size",
                onclick: move || kr.relook(|l| l.lanes.clear()),
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "restore".to_string() } }
            }
        }
    }
}

/// One lane: a bar with its name and count when collapsed; otherwise its
/// name's cell (on a board with named lanes), its own header row when the
/// board's lanes differ, and a cell per column.
fn render_lane(__scope: &mut RenderScope, k: &Kanban, shown: LaneShown, judge: SortJudge) -> NodeHandle {
    let lane = shown.lane.clone();
    let (board, metanode) = (k.board, lane.metanode);
    if shown.size == Size::Collapsed {
        let ke = k.clone();
        let l2 = lane.clone();
        let name = lane.label.clone().unwrap_or_else(|| "Other cards".to_string());
        return rsx! {
            div {
                class: "kanban__lane-bar",
                style: format!("display: flex; align-items: center; gap: 8px; padding: 6px 10px; cursor: pointer; font-weight: 600; font-size: 13px; {CELL_BG}"),
                title: "Show this lane",
                onclick: move || ke.set_size(&l2, Size::Minimized),
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "plus".to_string() } }
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "layout-kanban".to_string() } }
                span { {name} }
                span { style: LINE_STYLE, {move || format!("({})", lane_count(board, metanode))} }
            }
        };
    }
    let label = if shown.laned { Some(lane_label(__scope, k, &shown)) } else { None };
    let header = if shown.aligned { None } else { Some(header_row(__scope, k, Some(lane.clone()), judge.clone())) };
    let cells = cells_row(__scope, k, &shown, judge);
    rsx! {
        div {
            class: "kanban__lane-row",
            style: ROW_STYLE,
            if let Some(label) = label.clone() {
                {label}
            }
            div {
                style: "display: flex; flex-direction: column; gap: 6px; min-width: 0;",
                if let Some(header) = header.clone() {
                    {header}
                }
                {cells}
            }
        }
    }
}

/// A lane's name cell: its name (the unnamed lane's is "Other cards"),
/// renamed in place, its count and people, and its size: show all or about
/// three cards a column, collapse.
fn lane_label(__scope: &mut RenderScope, k: &Kanban, shown: &LaneShown) -> NodeHandle {
    let lane = shown.lane.clone();
    let (board, metanode) = (k.board, lane.metanode);
    let seen = k.host.seen();
    let (kc, kr, kf, kf2, ks, kz) = (k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone());
    let (chosen, renaming) = (k.chosen, k.renaming);
    let node = lane.node;
    let color = lane.color.clone();
    let name = lane.label.clone();
    let name2 = name.clone().unwrap_or_default();
    let writable = lane.writable && node.is_some();
    let size = shown.size;
    let (l1, l2) = (lane.clone(), lane.clone());
    let people_here = node.map(|n| people(__scope, k, n));
    let (toggle_icon, toggle_tip, toggle_to) = match size {
        Size::Expanded => ("chevrons-up", "Show about three cards a column", Size::Minimized),
        _ => ("chevrons-down", "Show every card", Size::Expanded),
    };
    rsx! {
        div {
            class: "kanban__lane-label",
            id: node.map(dom_id).unwrap_or_default(),
            style: {|| format!(
                "{LANE_LABEL_STYLE} width: {LABEL_W}px;{}",
                if node.is_some() && chosen.get() == node.map(Chosen::Lane) { " box-shadow: inset 0 0 0 2px var(--rinch-primary-color); border-radius: 6px;" } else { "" },
            )},
            div {
                style: {|| format!(
                    "display: flex; align-items: center; gap: 6px; font-weight: 600; font-size: 13px; cursor: pointer; min-width: 0;{}",
                    color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default(),
                )},
                onclick: move || {
                    if let Some(node) = node {
                        let again = clicked_again(node);
                        kc.choose(Some(Chosen::Lane(node)));
                        if untracked(|| kc.renaming.get()) == Some(node) {
                            return;
                        }
                        if again && writable {
                            kc.start_rename(node, name2.clone());
                        } else {
                            kc.take_keyboard();
                        }
                    }
                },
                span { style: "display: inline-flex; width: 14px; height: 14px; flex-shrink: 0;", IconGlyph { name: "layout-kanban".to_string() } }
                if node.is_some() && renaming.get() == node {
                    input {
                        class: "kanban__field",
                        style: FIELD_STYLE,
                        ref: kf.field,
                        placeholder: "Lane name",
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
                        style: "overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                        {name.clone().unwrap_or_else(|| "Other cards".to_string())}
                    }
                }
            }
            div {
                style: "display: flex; align-items: center; gap: 2px;",
                span { style: format!("{LINE_STYLE} margin-right: 4px;"), {move || { let n = lane_count(board, metanode); format!("{n} card{}", if n == 1 { "" } else { "s" }) }} }
                span {
                    style: ICON_BUTTON,
                    title: toggle_tip,
                    onclick: move || ks.set_size(&l1, toggle_to),
                    span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: toggle_icon.to_string() } }
                }
                span {
                    style: ICON_BUTTON,
                    title: "Collapse this lane",
                    onclick: move || kz.set_size(&l2, Size::Collapsed),
                    span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "minus".to_string() } }
                }
            }
            if let Some(people_here) = people_here.clone() {
                {people_here}
            }
        }
    }
}

/// A lane's cells, one per column, stretched to the tallest so the last
/// (Done, where finished cards pile up) is never taller than the others.
fn cells_row(__scope: &mut RenderScope, k: &Kanban, shown: &LaneShown, judge: SortJudge) -> NodeHandle {
    let kc = k.clone();
    let lane = shown.lane.metanode;
    let (size, writable) = (shown.size, shown.lane.writable);
    rsx! {
        div {
            class: "kanban__cells",
            style: ROW_STYLE,
            for (column, last, folded) in lane_columns(&kc, lane) {
                div {
                    key: format!("{column:?} {last} {folded}"),
                    style: "display: flex; flex-shrink: 0;",
                    {cell(__scope, &kc, column.clone(), last, folded, size, writable, judge.clone())}
                }
            }
        }
    }
}

/// One column of one lane: its cards (a sortable list of their own), and on
/// hover, "Add card". The last column's list fills the cell the row's tallest
/// sets and scrolls inside it; a minimized lane's lists show about three
/// cards. A folded column is a narrow strip with its name and count.
#[allow(clippy::too_many_arguments)]
fn cell(__scope: &mut RenderScope, k: &Kanban, column: board::Column, last: bool, folded: bool, size: Size, lane_writable: bool, judge: SortJudge) -> NodeHandle {
    let addr = column.addr;
    let name = column.label.clone();
    if folded {
        let kf = k.clone();
        let (tip, text) = (format!("Unfold {name}"), format!("{name} ({})", column.cards.len()));
        return rsx! {
            div {
                class: "kanban__cell kanban__cell--folded",
                style: format!("width: {FOLDED_W}px; min-height: 64px; position: relative; cursor: pointer; overflow: hidden; {CELL_BG}"),
                title: tip,
                onclick: move || kf.toggle_fold(&name),
                span {
                    style: "position: absolute; top: 10px; left: 30px; transform: rotate(90deg); transform-origin: top left; white-space: nowrap; font-size: 12px; font-weight: 600; color: var(--rinch-color-dimmed);",
                    {text}
                }
            }
        };
    }
    let cards: Vec<NodeHandle> = column.cards.iter().map(|card| render_card(__scope, k, card.clone())).collect();
    let can_add = column.writable;
    let (kd, ka, kenter, kleave) = (k.clone(), k.clone(), k.clone(), k.clone());
    let hovered = k.hovered;
    let list_style = if last {
        "position: absolute; top: 8px; left: 8px; right: 8px; bottom: 34px; overflow-y: auto;".to_string()
    } else if size == Size::Minimized {
        format!("max-height: {MINIMIZED_MAX}px; overflow-y: auto;")
    } else {
        String::new()
    };
    rsx! {
        div {
            class: "kanban__cell",
            data-kanban-column: key_of(addr),
            style: format!("width: {COLUMN_W}px; min-height: {}px; position: relative; display: flex; flex-direction: column; padding: 8px 8px 34px 8px; box-sizing: border-box; {CELL_BG}", if last { 110 } else { 72 }),
            onmouseenter: move || kenter.hovered.set(Some(addr)),
            onmouseleave: move || {
                if untracked(|| kleave.hovered.get()) == Some(addr) {
                    kleave.hovered.set(None);
                }
            },
            div {
                style: list_style,
                SortableList {
                    group: CARDS,
                    id: key_of(addr),
                    label: column.label.clone(),
                    class: "kanban__cards",
                    style: format!("{CARD_LIST_STYLE}{SORT_THEME}"),
                    // A read-only board takes no drags; a read-only column on a
                    // writable board still judges a drop into it, so the caret says
                    // why not ("You can read this, not change it.").
                    disabled: !lane_writable,
                    lift_keys: lift_keys(),
                    // Trello's feel (Joe, 2026-11-04): the card follows the
                    // pointer, and the list opens a gap where it lands.
                    indicator: SortIndicator::Gap,
                    source: SortSource::Collapse,
                    // The app's own picture of the item under the pointer: a copy of
                    // a plugin's elements does not keep its look across the host.
                    preview: SortPreview::Snapshot,
                    judge: Some(judge),
                    ondrop: move |m: SortMove| kd.drop_move(m),
                    {cards}
                }
            }
            if can_add && hovered.get() == Some(addr) {
                div {
                    class: "kanban__add-card",
                    style: "position: absolute; left: 50%; bottom: 6px; transform: translateX(-50%); display: flex; align-items: center; gap: 4px; padding: 3px 10px; font-size: 12px; border-radius: 4px; cursor: pointer; white-space: nowrap; color: var(--rinch-color-body); background: var(--rinch-color-text); box-shadow: 0 1px 3px rgba(0,0,0,0.25);",
                    onclick: { let k = ka.clone(); move || k.add(addr, true) },
                    span { style: "display: inline-flex; width: 12px; height: 12px;", IconGlyph { name: "plus".to_string() } }
                    "Add card"
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
fn render_card(__scope: &mut RenderScope, k: &Kanban, card: board::Card) -> NodeHandle {
    let addr = card.addr;
    let seen = k.host.seen();
    let (kc, kr, kd, kf, kf2, kenter, kleave) = (k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone());
    let chosen = k.chosen;
    let renaming = k.renaming;
    let hovered = k.hovered_card;
    let (color, background) = (card.color.clone(), card.background.clone());
    let label = card.label.clone();
    let writable = card.writable;
    let (icon, subtasks, label_color) = (card.icon.clone(), card.subtasks, card.color.clone());
    let item_label = card.label.clone();
    let show_icon = icon != DEFAULT_CARD_ICON;
    let card = rsx! {
        div {
            class: "kanban__card",
            id: dom_id(addr),
            style: {|| {
                let leader = seen.following().and_then(|key| seen.people_in(addr).into_iter().find(|p| p.key == key).map(|p| p.color));
                card_style(chosen.get() == Some(Chosen::Card(addr)), background.as_deref().map(|b| row_background(b, seen.dark())), leader)
            }},
            onclick: move || kc.click_card(addr, rinch::core::get_modifier_state()),
            onmouseenter: move || kenter.hovered_card.set(Some(addr)),
            onmouseleave: move || {
                if untracked(|| kleave.hovered_card.get()) == Some(addr) {
                    kleave.hovered_card.set(None);
                }
            },
            div {
                style: "display: flex; align-items: flex-start; gap: 8px; font-size: 14px; line-height: 1.45;",
                // A grip says the card drags (the whole card does).
                if writable {
                    span {
                        class: "kanban__grip",
                        style: "display: inline-flex; width: 14px; height: 14px; margin-top: 3px; margin-left: -4px; flex-shrink: 0; color: var(--rinch-color-dimmed); opacity: 0.6;",
                        IconGlyph { name: "grip-vertical".to_string() }
                    }
                }
                // A plain note's icon says nothing on a card; any other does.
                if show_icon {
                    span {
                        style: {let color = color.clone(); move || format!("display: inline-flex; width: 14px; height: 14px; margin-top: 3px; flex-shrink: 0;{}", color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default())},
                        IconGlyph { name: icon.clone() }
                    }
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
                // The trash can, while the pointer is on the card or it is
                // chosen; its slot is always there so the label never reflows.
                if writable {
                    span {
                        class: "kanban__trash-slot",
                        style: "display: inline-flex; width: 16px; height: 16px; margin-top: 2px; flex-shrink: 0;",
                        if (hovered.get() == Some(addr) || chosen.get() == Some(Chosen::Card(addr))) && renaming.get() != Some(addr) {
                            span {
                                class: "kanban__trash",
                                title: "Delete (View > Recently Deleted puts it back)",
                                style: "display: inline-flex; width: 16px; height: 16px; cursor: pointer; color: var(--rinch-color-dimmed);",
                                onclick: { let k = kd.clone(); move || k.delete(addr) },
                                IconGlyph { name: "trash".to_string() }
                            }
                        }
                    }
                }
            }
            div {
                style: "display: flex; align-items: center; min-height: 0;",
                {people(__scope, k, addr)}
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

    #[test]
    fn the_look_is_remembered_as_json_and_read_back() {
        let mut look = Look::default();
        look.lanes.insert("top".into(), Size::Collapsed);
        look.lanes.insert("s/n".into(), Size::Expanded);
        look.folded.push("done".into());
        assert_eq!(Look::parse(&look.to_json()), look);
        // Anything else reads as the board's own look.
        assert_eq!(Look::parse("not json"), Look::default());
        assert_eq!(Look::parse(r#"{"lanes":{"top":"sideways"}}"#), Look::default());
    }
}

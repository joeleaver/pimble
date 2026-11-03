//! The kanban as data, and where a gesture puts a card or a column
//! (docs/KANBAN_CONTRACT.md section 2). Nothing here draws or calls the API,
//! so it is tested natively.
//!
//! The metanode's source is its subtree two levels deep, a forest in the
//! tree's own order: the first level is the columns, the second the cards.
//! A card's place in its column is its priority, so every move says where it
//! lands as a [`Place`] in the tree (first, last, after a sibling), never as
//! an index: that is what `plan` judges and what the server makes.
//!
//! Every way of moving ends in one [`Target`]: the menu's "Move to" a column,
//! the keyboard's steps ([`step_card`], [`step_column`]), and later a drop. rinch #2091 (sortable
//! lists, being built) is asked to answer a drop as a list and a place
//! (first, last, after a key); [`drop_target`] is where that answer becomes a
//! `Target`, so a drop goes through the same judgement and the same move as
//! the menu and the keys.

use pimble_plugin_sdk::pimble_api::{Addr, Forest};
use pimble_plugin_sdk::pimble_core::{Node, Place};

/// A card: a node of the second level.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub addr: Addr,
    pub label: String,
    /// A Tabler icon name: its own, or its type's.
    pub icon: String,
    /// Its text color and row color as stored (`#rrggbb`), drawn for the
    /// theme by `pimble_ui::appearance`.
    pub color: Option<String>,
    pub background: Option<String>,
    /// How many children it has: its subtasks.
    pub subtasks: usize,
    /// The person may change it.
    pub writable: bool,
}

/// A column: a node of the first level, and its cards in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub addr: Addr,
    pub label: String,
    pub color: Option<String>,
    pub writable: bool,
    pub cards: Vec<Card>,
}

/// The board: its columns in order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Board {
    pub metanode: Option<Addr>,
    /// The person may change the board itself (add and move columns).
    pub writable: bool,
    pub columns: Vec<Column>,
}

/// Where a card or column goes: under `parent`, at `place`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub parent: Addr,
    pub place: Place,
}

/// A step the keyboard asks for (Alt+Shift+arrows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
    Left,
    Right,
}

fn writable(node: &Node) -> bool {
    node.access.allows_write()
}

/// The board a metanode's forest draws. `label` names a node as the explorer
/// does (`pimble_plugin_sdk::display_label`).
pub fn board_of(forest: &Forest, label: impl Fn(&Node) -> String) -> Board {
    let columns = forest
        .tops()
        .map(|col| Column {
            addr: col.addr,
            label: label(&col.node),
            color: col.node.metadata.color().map(str::to_string),
            writable: writable(&col.node),
            cards: col
                .children
                .iter()
                .filter_map(|addr| forest.get(*addr))
                .map(|card| Card {
                    addr: card.addr,
                    label: label(&card.node),
                    icon: pimble_plugin_sdk::pimble_ui::appearance::node_icon(&card.node),
                    color: card.node.metadata.color().map(str::to_string),
                    background: card.node.metadata.background().map(str::to_string),
                    subtasks: card.node.children.len(),
                    writable: writable(&card.node),
                })
                .collect(),
        })
        .collect();
    Board { metanode: Some(forest.metanode), writable: writable(&forest.node), columns }
}

impl Board {
    /// The column holding card `card`, and the card's index in it.
    pub fn find_card(&self, card: Addr) -> Option<(usize, usize)> {
        self.columns.iter().enumerate().find_map(|(c, col)| col.cards.iter().position(|k| k.addr == card).map(|i| (c, i)))
    }

    pub fn card(&self, card: Addr) -> Option<&Card> {
        self.find_card(card).map(|(c, i)| &self.columns[c].cards[i])
    }

    pub fn column_index(&self, column: Addr) -> Option<usize> {
        self.columns.iter().position(|c| c.addr == column)
    }
}

/// The place that puts something at `index` of a list whose items are `ids`
/// once it has left it (`ids` without it): first, or after the one before.
fn place_at(ids: &[Addr], index: usize) -> Place {
    match index.checked_sub(1).and_then(|i| ids.get(i)) {
        None => Place::First,
        Some(before) => Place::After(before.node),
    }
}

/// "Move to ▸ `column`": the card goes to the bottom of that column, the
/// least pressing place, and stays above nothing it was not above before.
pub fn to_column(column: Addr) -> Target {
    Target { parent: column, place: Place::Last }
}

/// Where Alt+Shift+`step` takes card `card`: up or down one within its column,
/// or into the neighbor column at the same height (or its bottom, when it is
/// shorter). `None` at an edge.
pub fn step_card(board: &Board, card: Addr, step: Step) -> Option<Target> {
    let (c, i) = board.find_card(card)?;
    let column = &board.columns[c];
    let others: Vec<Addr> = column.cards.iter().map(|k| k.addr).filter(|a| *a != card).collect();
    match step {
        Step::Up => (i > 0).then(|| Target { parent: column.addr, place: place_at(&others, i - 1) }),
        Step::Down => (i + 1 < column.cards.len()).then(|| Target { parent: column.addr, place: place_at(&others, i + 1) }),
        Step::Left | Step::Right => {
            let to = if step == Step::Left { c.checked_sub(1)? } else { c + 1 };
            let next = board.columns.get(to)?;
            let ids: Vec<Addr> = next.cards.iter().map(|k| k.addr).collect();
            Some(Target { parent: next.addr, place: place_at(&ids, i.min(ids.len())) })
        }
    }
}

/// Where Alt+Shift+Left or Right takes column `column`: one place along.
pub fn step_column(board: &Board, column: Addr, step: Step) -> Option<Target> {
    let metanode = board.metanode?;
    let c = board.column_index(column)?;
    let others: Vec<Addr> = board.columns.iter().map(|k| k.addr).filter(|a| *a != column).collect();
    match step {
        Step::Left => (c > 0).then(|| Target { parent: metanode, place: place_at(&others, c - 1) }),
        Step::Right => (c + 1 < board.columns.len()).then(|| Target { parent: metanode, place: place_at(&others, c + 1) }),
        Step::Up | Step::Down => None,
    }
}

/// A drop (rinch #2091, when it lands) into list `list` (a column's cards,
/// or the board's columns when `list` is the metanode) at `place`, as the
/// drop names it: first, last, or after the item with that key. The same
/// [`Target`] a menu item or a key makes, so a drop is judged and moved by
/// the same function.
#[allow(dead_code)] // Called once rinch #2091 lands; the tests hold its shape.
pub fn drop_target(list: Addr, place: Place) -> Target {
    Target { parent: list, place }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_plugin_sdk::pimble_core::{NodeId, StoreId};

    fn board(shape: &[usize]) -> Board {
        let store = StoreId::new();
        let card = |n: usize| Card {
            addr: Addr::new(store, NodeId::new()),
            label: format!("card {n}"),
            icon: "file".into(),
            color: None,
            background: None,
            subtasks: 0,
            writable: true,
        };
        Board {
            metanode: Some(Addr::new(store, NodeId::new())),
            writable: true,
            columns: shape
                .iter()
                .enumerate()
                .map(|(c, n)| Column {
                    addr: Addr::new(store, NodeId::new()),
                    label: format!("column {c}"),
                    color: None,
                    writable: true,
                    cards: (0..*n).map(card).collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn keys_move_a_card_within_its_column_by_place_not_index() {
        let b = board(&[3, 2]);
        let col = &b.columns[0];
        let [a, m, z] = [col.cards[0].addr, col.cards[1].addr, col.cards[2].addr];
        // Up from the second: first.
        assert_eq!(step_card(&b, m, Step::Up), Some(Target { parent: col.addr, place: Place::First }));
        // Up from the third: after the first.
        assert_eq!(step_card(&b, z, Step::Up), Some(Target { parent: col.addr, place: Place::After(a.node) }));
        // Down from the first: after the second.
        assert_eq!(step_card(&b, a, Step::Down), Some(Target { parent: col.addr, place: Place::After(m.node) }));
        // Edges answer nothing.
        assert_eq!(step_card(&b, a, Step::Up), None);
        assert_eq!(step_card(&b, z, Step::Down), None);
        assert_eq!(step_card(&b, a, Step::Left), None);
    }

    #[test]
    fn keys_move_a_card_across_at_the_same_height_and_the_menu_to_the_bottom() {
        let b = board(&[3, 2, 0]);
        let (left, mid, right) = (&b.columns[0], &b.columns[1], &b.columns[2]);
        // The first card goes to the top of the next column.
        assert_eq!(step_card(&b, left.cards[0].addr, Step::Right), Some(Target { parent: mid.addr, place: Place::First }));
        // The third goes below the next column's last (it is shorter).
        assert_eq!(step_card(&b, left.cards[2].addr, Step::Right), Some(Target { parent: mid.addr, place: Place::After(mid.cards[1].addr.node) }));
        // Into an empty column: first.
        assert_eq!(step_card(&b, mid.cards[1].addr, Step::Right), Some(Target { parent: right.addr, place: Place::First }));
        assert_eq!(step_card(&b, mid.cards[0].addr, Step::Left), Some(Target { parent: left.addr, place: Place::First }));
        assert_eq!(to_column(right.addr), Target { parent: right.addr, place: Place::Last });
        assert_eq!(drop_target(mid.addr, Place::After(mid.cards[0].addr.node)).place, Place::After(mid.cards[0].addr.node));
    }

    #[test]
    fn a_column_moves_along_the_board() {
        let b = board(&[1, 1, 1]);
        let m = b.metanode.unwrap();
        let [c0, c1, c2] = [b.columns[0].addr, b.columns[1].addr, b.columns[2].addr];
        assert_eq!(step_column(&b, c1, Step::Left), Some(Target { parent: m, place: Place::First }));
        assert_eq!(step_column(&b, c1, Step::Right), Some(Target { parent: m, place: Place::After(c2.node) }));
        assert_eq!(step_column(&b, c0, Step::Left), None);
        assert_eq!(step_column(&b, c2, Step::Right), None);
        assert_eq!(step_column(&b, c0, Step::Up), None);
    }
}

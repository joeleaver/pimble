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
//! **Lanes** (section 3): a child that is itself a kanban is a lane, drawn
//! below the others from its own forest (its own source, read as its own
//! metanode); a mounted kanban is one too. The children that are not
//! kanbans form one unnamed lane at the top (open decision 3), which is the
//! whole board when there are no lanes. A lane's columns are its children.
//!
//! Every way of moving ends in one [`Target`]: the keyboard's steps
//! ([`step_card`], [`step_column`]) and a drop. rinch's
//! sortable lists (joeleaver/rinch#2093) answer a drop as the keys carried,
//! the list it lands in and the key of the item it lands after;
//! [`drop_of`] is where that answer becomes a `Target`, so a drop goes
//! through the same judgement and the same move as the menu and the keys.

use pimble_plugin_sdk::pimble_api::{Addr, Forest};
use pimble_plugin_sdk::pimble_core::{Node, NodeId, Place, StoreId};

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

/// A lane: a row of columns under one node. The unnamed lane's node is the
/// board's metanode; a named lane's is the kanban it is (a mount's source,
/// for a mounted one), whose own source found its columns.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    /// The node the lane's columns are children of, and the metanode a move
    /// landing in this lane is judged in.
    pub metanode: Addr,
    /// The lane's own node on the board (the mount, for a mounted lane);
    /// `None` for the unnamed lane.
    pub node: Option<Addr>,
    /// The lane's name; `None` for the unnamed lane.
    pub label: Option<String>,
    pub color: Option<String>,
    /// The person may change the lane (add and move its columns).
    pub writable: bool,
    pub columns: Vec<Column>,
}

/// The board: its lanes, top to bottom.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Board {
    pub metanode: Option<Addr>,
    /// The person may change the board itself (add lanes).
    pub writable: bool,
    pub lanes: Vec<Lane>,
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

/// The columns of a forest's tops, `skip` left out (the tops that are lanes).
fn columns_of(forest: &Forest, skip: &[Addr], label: &impl Fn(&Node) -> String) -> Vec<Column> {
    forest
        .tops()
        .filter(|col| !skip.contains(&col.addr))
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
        .collect()
}

/// The board a metanode's forest draws, with `lanes`: for each top that is a
/// kanban, the top's address on the board and that kanban's own forest, in
/// the tops' order. `label` names a node as the explorer does
/// (`pimble_plugin_sdk::display_label`).
pub fn board_of(forest: &Forest, lanes: &[(Addr, Forest)], label: impl Fn(&Node) -> String) -> Board {
    let tops: Vec<Addr> = lanes.iter().map(|(top, _)| *top).collect();
    let unnamed = columns_of(forest, &tops, &label);
    let mut out = Vec::new();
    // The unnamed lane: the whole board without lanes, and with them only
    // when some children are not kanbans.
    if lanes.is_empty() || !unnamed.is_empty() {
        out.push(Lane { metanode: forest.metanode, node: None, label: None, color: None, writable: writable(&forest.node), columns: unnamed });
    }
    for (top, lane) in lanes {
        // The lane's name and color are its node's on the board (the mount's
        // label for a mounted lane, as the explorer shows it).
        let on_board = forest.get(*top).map(|e| &e.node).unwrap_or(&lane.node);
        out.push(Lane {
            metanode: lane.metanode,
            node: Some(*top),
            label: Some(label(on_board)),
            color: on_board.metadata.color().map(str::to_string),
            writable: writable(&lane.node),
            columns: columns_of(lane, &[], &label),
        });
    }
    Board { metanode: Some(forest.metanode), writable: writable(&forest.node), lanes: out }
}

/// A column's name as lanes match it: case and spaces do not count.
fn same_name(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    norm(a) == norm(b)
}

impl Board {
    /// The lane, column and index of card `card`.
    pub fn find_card(&self, card: Addr) -> Option<(usize, usize, usize)> {
        self.lanes.iter().enumerate().find_map(|(l, lane)| {
            lane.columns.iter().enumerate().find_map(|(c, col)| col.cards.iter().position(|k| k.addr == card).map(|i| (l, c, i)))
        })
    }

    pub fn card(&self, card: Addr) -> Option<&Card> {
        self.find_card(card).map(|(l, c, i)| &self.lanes[l].columns[c].cards[i])
    }

    /// The lane and index of column `column`.
    pub fn find_column(&self, column: Addr) -> Option<(usize, usize)> {
        self.lanes.iter().enumerate().find_map(|(l, lane)| lane.columns.iter().position(|c| c.addr == column).map(|c| (l, c)))
    }

    pub fn column(&self, column: Addr) -> Option<&Column> {
        self.find_column(column).map(|(l, c)| &self.lanes[l].columns[c])
    }

    /// The named lane whose node on the board is `node`.
    pub fn lane_of_node(&self, node: Addr) -> Option<&Lane> {
        self.lanes.iter().find(|lane| lane.node == Some(node))
    }

    /// The lane a move landing under `parent` lands in: the lane whose node
    /// it is (a column moved along it), or the lane of the column it is.
    pub fn lane_for_parent(&self, parent: Addr) -> Option<&Lane> {
        self.lanes
            .iter()
            .find(|lane| lane.metanode == parent)
            .or_else(|| self.find_column(parent).map(|(l, _)| &self.lanes[l]))
    }

    /// Every card, in lane, column and card order.
    pub fn cards(&self) -> impl Iterator<Item = &Card> {
        self.lanes.iter().flat_map(|lane| lane.columns.iter().flat_map(|c| c.cards.iter()))
    }

    /// The column of lane `to` that lines up with column `c` of lane `from`:
    /// the one of the same name, or the one at the same place.
    pub fn matching_column(&self, from: usize, c: usize, to: usize) -> Option<usize> {
        let name = &self.lanes.get(from)?.columns.get(c)?.label;
        let lane = self.lanes.get(to)?;
        lane.columns.iter().position(|col| same_name(&col.label, name)).or_else(|| (c < lane.columns.len()).then_some(c)).or_else(|| lane.columns.len().checked_sub(1))
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

/// Where Alt+Shift+`step` takes card `card`: up or down one within its column,
/// into the neighbor column at the same height (or its bottom, when it is
/// shorter), and past a column's top or bottom into the lined-up column of
/// the lane above (its bottom) or below (its top). `None` at an edge.
pub fn step_card(board: &Board, card: Addr, step: Step) -> Option<Target> {
    let (l, c, i) = board.find_card(card)?;
    let lane = &board.lanes[l];
    let column = &lane.columns[c];
    let others: Vec<Addr> = column.cards.iter().map(|k| k.addr).filter(|a| *a != card).collect();
    match step {
        Step::Up if i > 0 => Some(Target { parent: column.addr, place: place_at(&others, i - 1) }),
        Step::Down if i + 1 < column.cards.len() => Some(Target { parent: column.addr, place: place_at(&others, i + 1) }),
        Step::Up => {
            let to = l.checked_sub(1)?;
            let col = &board.lanes[to].columns[board.matching_column(l, c, to)?];
            Some(Target { parent: col.addr, place: Place::Last })
        }
        Step::Down => {
            let col = &board.lanes.get(l + 1)?.columns[board.matching_column(l, c, l + 1)?];
            Some(Target { parent: col.addr, place: Place::First })
        }
        Step::Left | Step::Right => {
            let to = if step == Step::Left { c.checked_sub(1)? } else { c + 1 };
            let next = lane.columns.get(to)?;
            let ids: Vec<Addr> = next.cards.iter().map(|k| k.addr).collect();
            Some(Target { parent: next.addr, place: place_at(&ids, i.min(ids.len())) })
        }
    }
}

/// Where Alt+Shift+Left or Right takes column `column`: one place along its
/// lane.
pub fn step_column(board: &Board, column: Addr, step: Step) -> Option<Target> {
    let (l, c) = board.find_column(column)?;
    let lane = &board.lanes[l];
    let others: Vec<Addr> = lane.columns.iter().map(|k| k.addr).filter(|a| *a != column).collect();
    match step {
        Step::Left => (c > 0).then(|| Target { parent: lane.metanode, place: place_at(&others, c - 1) }),
        Step::Right => (c + 1 < lane.columns.len()).then(|| Target { parent: lane.metanode, place: place_at(&others, c + 1) }),
        Step::Up | Step::Down => None,
    }
}

/// A node's key in a sortable list (a card's, a column's, and a list's id):
/// its store and its id, so a column mounted from another store keeps its
/// cards' own addresses.
pub fn key_of(addr: Addr) -> String {
    format!("{}/{}", addr.store, addr.node)
}

/// The node a [`key_of`] key names.
pub fn addr_of(key: &str) -> Option<Addr> {
    let (store, node) = key.split_once('/')?;
    Some(Addr::new(StoreId::parse(store).ok()?, NodeId::parse(node).ok()?))
}

/// A drop of a sortable list: the nodes it carries (`keys`, in order) and
/// where they go, under the node whose list it lands in (`to_list`: a
/// column's cards, or a lane's columns when it is the lane's node), after
/// the item keyed `after`, or first when there is none (the top of the
/// list, an empty list included). The same [`Target`] a key makes, so a drop is judged and moved by the same function. `None` for a
/// key that names no node, which nothing on this board draws.
pub fn drop_of(keys: &[String], to_list: &str, after: Option<&str>) -> Option<(Vec<Addr>, Target)> {
    let nodes = keys.iter().map(|k| addr_of(k)).collect::<Option<Vec<Addr>>>()?;
    let parent = addr_of(to_list)?;
    let place = match after {
        Some(key) => Place::After(addr_of(key)?.node),
        None => Place::First,
    };
    (!nodes.is_empty()).then_some((nodes, Target { parent, place }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_plugin_sdk::pimble_core::{NodeId, StoreId};

    fn board(shape: &[usize]) -> Board {
        laned(&[shape])
    }

    /// A board of lanes, each a list of column sizes; the first lane is the
    /// unnamed one, the others named lanes with their own nodes.
    fn laned(lanes: &[&[usize]]) -> Board {
        let store = StoreId::new();
        let metanode = Addr::new(store, NodeId::new());
        let card = |n: usize| Card {
            addr: Addr::new(store, NodeId::new()),
            label: format!("card {n}"),
            icon: "file".into(),
            color: None,
            background: None,
            subtasks: 0,
            writable: true,
        };
        let columns = |shape: &[usize]| -> Vec<Column> {
            shape
                .iter()
                .enumerate()
                .map(|(c, n)| Column {
                    addr: Addr::new(store, NodeId::new()),
                    label: format!("Column {c}"),
                    color: None,
                    writable: true,
                    cards: (0..*n).map(card).collect(),
                })
                .collect()
        };
        let lanes = lanes
            .iter()
            .enumerate()
            .map(|(l, shape)| {
                let node = (l > 0).then(|| Addr::new(store, NodeId::new()));
                Lane {
                    metanode: node.unwrap_or(metanode),
                    node,
                    label: node.map(|_| format!("lane {l}")),
                    color: None,
                    writable: true,
                    columns: columns(shape),
                }
            })
            .collect();
        Board { metanode: Some(metanode), writable: true, lanes }
    }

    #[test]
    fn keys_move_a_card_within_its_column_by_place_not_index() {
        let b = board(&[3, 2]);
        let col = &b.lanes[0].columns[0];
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
    fn keys_move_a_card_across_at_the_same_height() {
        let b = board(&[3, 2, 0]);
        let cols = &b.lanes[0].columns;
        let (left, mid, right) = (&cols[0], &cols[1], &cols[2]);
        // The first card goes to the top of the next column.
        assert_eq!(step_card(&b, left.cards[0].addr, Step::Right), Some(Target { parent: mid.addr, place: Place::First }));
        // The third goes below the next column's last (it is shorter).
        assert_eq!(step_card(&b, left.cards[2].addr, Step::Right), Some(Target { parent: mid.addr, place: Place::After(mid.cards[1].addr.node) }));
        // Into an empty column: first.
        assert_eq!(step_card(&b, mid.cards[1].addr, Step::Right), Some(Target { parent: right.addr, place: Place::First }));
        assert_eq!(step_card(&b, mid.cards[0].addr, Step::Left), Some(Target { parent: left.addr, place: Place::First }));
    }

    #[test]
    fn a_drop_lands_after_the_item_it_names_or_first() {
        let b = board(&[2, 1, 0]);
        let cols = &b.lanes[0].columns;
        let (left, mid, right) = (&cols[0], &cols[1], &cols[2]);
        let card = left.cards[0].addr;
        assert_eq!(addr_of(&key_of(card)), Some(card));
        let keys = vec![key_of(card)];
        // After a card of another column.
        let after = key_of(mid.cards[0].addr);
        assert_eq!(drop_of(&keys, &key_of(mid.addr), Some(&after)), Some((vec![card], Target { parent: mid.addr, place: Place::After(mid.cards[0].addr.node) })));
        // The top of a column, and an empty column: first.
        assert_eq!(drop_of(&keys, &key_of(mid.addr), None).map(|(_, t)| t.place), Some(Place::First));
        assert_eq!(drop_of(&keys, &key_of(right.addr), None), Some((vec![card], Target { parent: right.addr, place: Place::First })));
        // A column along the board: a move under the metanode.
        let m = b.metanode.unwrap();
        assert_eq!(drop_of(&[key_of(right.addr)], &key_of(m), None), Some((vec![right.addr], Target { parent: m, place: Place::First })));
        // Nothing for a key that names no node.
        assert_eq!(drop_of(&["nonsense".into()], &key_of(mid.addr), None), None);
        assert_eq!(drop_of(&[], &key_of(mid.addr), None), None);
    }

    #[test]
    fn a_column_moves_along_the_board() {
        let b = board(&[1, 1, 1]);
        let m = b.metanode.unwrap();
        let cols = &b.lanes[0].columns;
        let [c0, c1, c2] = [cols[0].addr, cols[1].addr, cols[2].addr];
        assert_eq!(step_column(&b, c1, Step::Left), Some(Target { parent: m, place: Place::First }));
        assert_eq!(step_column(&b, c1, Step::Right), Some(Target { parent: m, place: Place::After(c2.node) }));
        assert_eq!(step_column(&b, c0, Step::Left), None);
        assert_eq!(step_column(&b, c2, Step::Right), None);
        assert_eq!(step_column(&b, c0, Step::Up), None);
    }

    #[test]
    fn a_card_steps_into_the_lined_up_column_of_the_lane_above_or_below() {
        // The unnamed lane, then two lanes; the last has its columns in
        // another order, so the step finds the same name, not the same place.
        let mut b = laned(&[&[1, 0], &[2, 1], &[0, 0]]);
        b.lanes[2].columns.swap(0, 1);
        let (top, mid, low) = (&b.lanes[0], &b.lanes[1], &b.lanes[2]);
        // Down from the bottom of a column: the top of the same column below.
        let card = top.columns[0].cards[0].addr;
        assert_eq!(step_card(&b, card, Step::Down), Some(Target { parent: mid.columns[0].addr, place: Place::First }));
        // Up from the top: the bottom of the same column above.
        let card = mid.columns[0].cards[0].addr;
        assert_eq!(step_card(&b, card, Step::Up), Some(Target { parent: top.columns[0].addr, place: Place::Last }));
        // Within a column a step stays a step.
        let card = mid.columns[0].cards[1].addr;
        assert_eq!(step_card(&b, card, Step::Up), Some(Target { parent: mid.columns[0].addr, place: Place::First }));
        // "Column 0" of the last lane is its second column now.
        assert_eq!(step_card(&b, card, Step::Down), Some(Target { parent: low.columns[1].addr, place: Place::First }));
        // The top lane's top and the last lane's bottom are edges.
        let card = top.columns[0].cards[0].addr;
        assert_eq!(step_card(&b, card, Step::Up), None);
    }

    #[test]
    fn a_move_is_judged_in_the_lane_it_lands_in() {
        let b = laned(&[&[1], &[1, 1]]);
        let lane = &b.lanes[1];
        // A card into a lane's column, and a column along a lane.
        assert_eq!(b.lane_for_parent(lane.columns[1].addr).map(|l| l.metanode), Some(lane.metanode));
        assert_eq!(b.lane_for_parent(lane.metanode).map(|l| l.metanode), Some(lane.metanode));
        assert_eq!(b.lane_for_parent(b.lanes[0].columns[0].addr).map(|l| l.metanode), b.metanode);
        // A column steps along its own lane, under the lane's node.
        let c0 = lane.columns[0].addr;
        assert_eq!(step_column(&b, c0, Step::Right), Some(Target { parent: lane.metanode, place: Place::After(lane.columns[1].addr.node) }));
    }

    #[test]
    fn column_names_match_without_case_or_spacing() {
        assert!(same_name("To  do", "to do"));
        assert!(!same_name("To do", "Done"));
    }
}

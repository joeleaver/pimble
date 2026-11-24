//! The board's keyboard: arrows choose, Alt+Shift+arrows move, and the
//! keys a focused card or column is lifted with.

use super::*;

impl Kanban {
    /// The board's keys: arrows choose, Alt+Shift+arrows move, Enter opens the chosen
    /// card beside the board (Ctrl+Enter in its place), F2 renames, Delete deletes, Escape lets go. Answers whether it
    /// took the key.
    pub(crate) fn key(&self, key: &KeyEventData) -> bool {
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
pub(crate) fn walk(board: &Board, chosen: Option<Chosen>, step: Step) -> Option<Chosen> {
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
pub(crate) fn walk_aligned(board: &Board, chosen: Option<Chosen>, step: Step) -> Option<Chosen> {
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

/// The keys that lift a focused card or column for a keyboard move: Space
/// only, so Enter stays "open the card", as everywhere else in Pimble.
pub(crate) fn lift_keys() -> Vec<String> {
    vec![" ".to_string()]
}

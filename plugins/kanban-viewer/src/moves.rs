//! Moving cards and columns: by a drag (the sortable lists' judgement and
//! drop) and by the keyboard, all ending in [`Kanban::move_to`].

use super::*;

impl Kanban {
    /// Move the column at place `from` of every lane to place `at` (counted
    /// without it) of its lane: an aligned board's header moved.
    pub(crate) fn columns_everywhere(&self, from: usize, at: usize) {
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
    pub(crate) fn move_to(&self, node: Addr, target: Target, confirmed: bool) {
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
    pub(crate) fn judge_drop(&self, m: &SortMove) -> SortVerdict {
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
    pub(crate) fn drop_move(&self, m: SortMove) {
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
    pub(crate) fn step(&self, step: Step) {
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
}

/// How the board's lists judge a drop ([`Kanban::judge_drop`]).
pub(crate) fn drop_judge(k: &Kanban) -> SortJudge {
    let k = k.clone();
    std::rc::Rc::new(move |m: &SortMove| k.judge_drop(m))
}

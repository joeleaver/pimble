//! What the board does that is not a move: reading the forest, choosing,
//! renaming, adding and deleting.

use super::*;

impl Kanban {
    /// Read the forest into the board, and each lane's own forest: the
    /// forests read, which say what a change must touch to be read again.
    pub(crate) async fn read(&self) -> Option<Vec<Forest>> {
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
    /// [`pimble_plugin_sdk::DOUBLE_CLICK`] renames it instead, as a double-click does a row of the
    /// explorer. The board keeps the keyboard.
    pub(crate) fn click_card(&self, card: Addr, modifiers: rinch::core::ModifierState) {
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
    pub(crate) fn choose(&self, chosen: Option<Chosen>) {
        if untracked(|| self.chosen.get()) == chosen {
            return;
        }
        self.chosen.set(chosen);
        self.host.focus(chosen.map(Chosen::addr));
    }

    /// Give the board the keyboard, so Alt+Shift+arrows reach it.
    pub(crate) fn take_keyboard(&self) {
        if let Some(root) = untracked(|| self.root.get()) {
            root.focus();
        }
    }

    pub(crate) fn say(&self, line: impl Into<String>) {
        self.status.set(line.into());
    }

    /// Start typing a name for `addr`, from what it is called now.
    pub(crate) fn start_rename(&self, addr: Addr, now: String) {
        self.draft.set(now);
        self.renaming.set(Some(addr));
        // The field takes the keyboard when it is drawn (`view`'s effect over
        // `field`): a new card's is drawn only once the board has read it.
    }

    /// Enter in the name field: rename through the one write, with the
    /// explicit-title rule (a name typed here is the node's name, not a
    /// guess from its text).
    pub(crate) fn commit_rename(&self) {
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

    pub(crate) fn cancel_rename(&self) {
        self.renaming.set(None);
        self.take_keyboard();
    }

    /// "+ Add card" (under `column`, last) or "+ Add column" (`column` is the
    /// metanode): a create, then its name typed in place.
    pub(crate) fn add(&self, under: Addr, card: bool) {
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
    pub(crate) fn add_column(&self, lanes: Vec<Addr>) {
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
    pub(crate) fn add_lane(&self) {
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
    pub(crate) fn delete(&self, card: Addr) {
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
}

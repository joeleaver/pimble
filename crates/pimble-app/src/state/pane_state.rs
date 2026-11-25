//! The four pane slots (docs/SPLIT_VIEW_CONTRACT.md "The model"): what each
//! holds, which has the focus, which panes show a node, and the layout saved
//! and restored.

use super::*;

/// Identifies the node open in a pane's editor: the CANONICAL pair, where an
/// edit would be written, even when the node was reached through a mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveEdit {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

/// What one pane slot holds (docs/SPLIT_VIEW_CONTRACT.md "The model"). The
/// slot exists for the app's life whether or not the tiling shows it; a
/// slot that is not shown holds nothing.
#[derive(Clone, Copy)]
pub struct PaneState {
    /// The tree value the pane was opened on (a node's, with any mount
    /// path, or a store row's), which is the tree's selected row while the
    /// pane has the focus.
    pub selected: Signal<Option<String>>,
    /// Whether the pane shows its editor (it holds a document) or the
    /// empty state.
    pub show_editor: Signal<bool>,
    /// The node open in the pane's editor. Its content lives in that
    /// editor's collab session; edits broadcast to the server live rather
    /// than being saved by wholesale replacement.
    pub active_edit: Signal<Option<ActiveEdit>>,
    /// A deep link's anchor waiting for its node's editor session in this
    /// pane: the editor resolves it (`NodeDoc::resolve_anchor`) and puts the
    /// caret there once the session holds the content
    /// (docs/LINKS_CONTRACT.md "Following a link").
    pub pending_anchor: Signal<Option<(StoreId, NodeId, pimble_core::Anchor)>>,
    /// A document the saved layout names for this pane, opened once its
    /// store is open and the node is known (docs/SPLIT_VIEW_CONTRACT.md
    /// "Persistence").
    pub restore: Signal<Option<(StoreId, NodeId)>>,
    /// What the pane has shown, for the explorer's back and forward arrows:
    /// this device's memory only, never stored or sent ([`crate::nav`]).
    pub history: Signal<crate::nav::History>,
    /// Whether the pane shows a board (docs/IMAGE_MARKUP_CONTRACT.md section
    /// 1, "Showing one") instead of its editor, which stays mounted, hidden.
    pub show_board: Signal<bool>,
    /// The board node the pane holds: its canonical pair, like
    /// `active_edit` for a document. Never both at once.
    pub board: Signal<Option<ActiveEdit>>,
    /// What the floating markup control shows: how many marks the board has
    /// and whether they are shown (`markup_visible`).
    pub board_marks: Signal<(usize, bool)>,
    /// What the board view shows over its surface and in its side panel
    /// (the selection, the layers, the text being typed, ...).
    pub board_ui: crate::boards::PaneUi,
    /// The board view shows over the pane's text, marking up a picture in it
    /// (docs/IMAGE_MARKUP_CONTRACT.md section 2, "Marking up"): `board`
    /// holds the note itself, and "Done" goes back to the text.
    pub markup: Signal<bool>,
    /// The picture in the pane's text the pointer is over, and what its
    /// floating controls show (section 5, "Showing and hiding markup").
    pub picture: Signal<Option<crate::text_boards::PictureHover>>,
    /// What the pane's text has selected, which the main toolbar follows
    /// (`crate::toolbar::which`).
    pub item: Signal<crate::toolbar::Item>,
    /// Whether the pane shows a sheet node's tables (docs/SHEETS_CONTRACT.md
    /// section 1) instead of its editor, which stays mounted, hidden.
    pub show_sheet: Signal<bool>,
    /// The sheet node the pane holds, like `board` for a board node.
    pub sheet: Signal<Option<ActiveEdit>>,
    /// What the sheet view shows: the computed tables, the cell being typed
    /// into, the column settings, the formula bar.
    pub sheet_ui: crate::sheets::SheetUi,
    /// Whether the pane shows a discussion (docs/DISCUSSIONS_CONTRACT.md)
    /// instead of its editor, which stays mounted, hidden.
    pub show_discussion: Signal<bool>,
    /// The discussion node the pane holds, like `sheet` for a sheet node.
    pub discussion: Signal<Option<ActiveEdit>>,
    /// What the discussion view shows: the thread, the message being typed.
    pub discussion_ui: crate::discussions::DiscussionUi,
    /// Whether the pane shows a metanode (docs/METANODES_CONTRACT.md)
    /// instead of its editor, which stays mounted, hidden.
    pub show_metanode: Signal<bool>,
    /// The metanode the pane holds, like `sheet` for a sheet node.
    pub metanode: Signal<Option<ActiveEdit>>,
    /// What the metanode's outline shows (`crate::metanodes`).
    pub metanode_ui: crate::metanodes::MetanodeUi,
    /// Whether the pane shows a theme node in the theme editor
    /// (docs/THEMES_CONTRACT.md section 9) instead of its editor, which
    /// stays mounted, hidden.
    pub show_theme: Signal<bool>,
    /// The theme node the pane holds, like `sheet` for a sheet node.
    pub theme: Signal<Option<ActiveEdit>>,
    /// What the theme editor shows beyond the theme: the switch to show only
    /// what it changes, and the scheme its sample is drawn in.
    pub theme_ui: crate::app::ThemeUi,
    /// The pane this one opens things beside (`pane_view::open_beside`: a
    /// kanban's card clicked), kept so that opening one after another shows
    /// each in the same place instead of splitting again. Forgotten when
    /// either pane closes.
    pub beside: Signal<Option<PaneId>>,
}

impl PaneState {
    pub(super) fn new() -> Self {
        Self {
            selected: Signal::new(None),
            show_editor: Signal::new(false),
            active_edit: Signal::new(None),
            pending_anchor: Signal::new(None),
            restore: Signal::new(None),
            history: Signal::new(crate::nav::History::default()),
            show_board: Signal::new(false),
            board: Signal::new(None),
            board_marks: Signal::new((0, true)),
            board_ui: crate::boards::PaneUi::new(),
            markup: Signal::new(false),
            picture: Signal::new(None),
            item: Signal::new(crate::toolbar::Item::Text),
            show_sheet: Signal::new(false),
            sheet: Signal::new(None),
            sheet_ui: crate::sheets::SheetUi::new(),
            show_discussion: Signal::new(false),
            discussion: Signal::new(None),
            discussion_ui: crate::discussions::DiscussionUi::new(),
            show_metanode: Signal::new(false),
            metanode: Signal::new(None),
            metanode_ui: crate::metanodes::MetanodeUi::new(),
            show_theme: Signal::new(false),
            theme: Signal::new(None),
            theme_ui: crate::app::ThemeUi::new(),
            beside: Signal::new(None),
        }
    }
}

impl AppStore {
    /// What pane `pane` holds.
    pub fn pane(&self, pane: PaneId) -> PaneState {
        self.panes[pane.index()]
    }

    /// The focused pane, untracked.
    pub fn focused(&self) -> PaneId {
        untracked(|| self.focused_pane.get())
    }

    /// The node open in the focused pane's editor, untracked: what "the
    /// active edit" means now that there are several panes.
    pub fn focused_edit(&self) -> Option<ActiveEdit> {
        untracked(|| self.pane(self.focused()).active_edit.get())
    }

    /// The panes whose editor has this node open, untracked.
    pub fn panes_editing(&self, store_id: StoreId, node_id: NodeId) -> Vec<PaneId> {
        let edit = Some(ActiveEdit { store_id, node_id });
        untracked(|| PaneId::ALL.into_iter().filter(|pane| self.pane(*pane).active_edit.get() == edit).collect())
    }

    /// The panes holding this board node, untracked.
    pub fn panes_boarding(&self, store_id: StoreId, node_id: NodeId) -> Vec<PaneId> {
        let edit = Some(ActiveEdit { store_id, node_id });
        untracked(|| PaneId::ALL.into_iter().filter(|pane| self.pane(*pane).board.get() == edit).collect())
    }

    /// The panes holding this sheet node, untracked.
    pub fn panes_sheeting(&self, store_id: StoreId, node_id: NodeId) -> Vec<PaneId> {
        let edit = Some(ActiveEdit { store_id, node_id });
        untracked(|| PaneId::ALL.into_iter().filter(|pane| self.pane(*pane).sheet.get() == edit).collect())
    }

    /// The panes holding this discussion node, untracked.
    pub fn panes_discussing(&self, store_id: StoreId, node_id: NodeId) -> Vec<PaneId> {
        let edit = Some(ActiveEdit { store_id, node_id });
        untracked(|| PaneId::ALL.into_iter().filter(|pane| self.pane(*pane).discussion.get() == edit).collect())
    }

    /// Whether any pane holds this node, in its editor, as a board, as a
    /// sheet or as a discussion, untracked: what a node's subscription is
    /// counted by.
    pub fn is_editing(&self, store_id: StoreId, node_id: NodeId) -> bool {
        !self.panes_editing(store_id, node_id).is_empty()
            || !self.panes_boarding(store_id, node_id).is_empty()
            || !self.panes_sheeting(store_id, node_id).is_empty()
            || !self.panes_discussing(store_id, node_id).is_empty()
            // And a discussion shown inline in a note's text.
            || crate::discussions::inline_holds(store_id, node_id)
            // A board embedded in a note's text holds its subscription too
            // (docs/IMAGE_MARKUP_CONTRACT.md section 2, "Showing").
            || crate::text_boards::embed_holds(store_id, node_id)
            // So does a note a sheet's formulas read (docs/SHEETS_CONTRACT.md
            // section 7).
            || crate::sheets::reads::holds(store_id, node_id)
            // And a sheet node embedded in a note's text.
            || crate::sheets::embed_holds(store_id, node_id)
    }

    /// Every node some pane holds (a document in its editor, a board or a
    /// sheet), each once, untracked.
    pub fn edited_nodes(&self) -> Vec<ActiveEdit> {
        let mut nodes: Vec<ActiveEdit> = Vec::new();
        for pane in PaneId::ALL {
            let held = untracked(|| {
                let p = self.pane(pane);
                p.active_edit.get().or_else(|| p.board.get()).or_else(|| p.sheet.get()).or_else(|| p.discussion.get())
            });
            if let Some(edit) = held {
                if !nodes.contains(&edit) {
                    nodes.push(edit);
                }
            }
        }
        nodes
    }

    /// The panes opened on this node (their tree value names it, through a
    /// mount or not), untracked: the ones a `NodeLoaded` for it is news to,
    /// and the ones that close when it is deleted.
    pub fn panes_showing(&self, store_id: StoreId, node_id: NodeId) -> Vec<PaneId> {
        untracked(|| {
            PaneId::ALL
                .into_iter()
                .filter(|pane| {
                    self.pane(*pane).selected.get().and_then(|value| parse_tree_value(&value)) == Some((store_id, Some(node_id)))
                })
                .collect()
        })
    }

    /// Open pane `pane` on a tree value, or on nothing. The tree's selected
    /// row follows when the pane has the focus.
    pub fn select_in_pane(&self, pane: PaneId, value: Option<String>) {
        self.pane(pane).selected.set(value.clone());
        if pane == self.focused() {
            self.selected_id.set(value);
        }
    }

    /// Empty pane `pane`: nothing selected, the empty state shown. The
    /// editor itself is left alone, and the next document opened there ends
    /// its session as it ends any other.
    pub fn clear_pane(&self, pane: PaneId) {
        self.select_in_pane(pane, None);
        self.pane(pane).show_editor.set(false);
        self.pane(pane).show_board.set(false);
        self.pane(pane).show_sheet.set(false);
        self.pane(pane).show_discussion.set(false);
        crate::metanodes::stop(*self, pane);
        crate::app::stop_theme_editor(*self, pane);
        self.pane(pane).restore.set(None);
        if pane == self.focused() {
            self.node_title.set(String::new());
        }
    }

    /// The layout as it is to be remembered (docs/SPLIT_VIEW_CONTRACT.md
    /// "Persistence"), tracked: the tiling, the focus, and for each pane
    /// shown the document its editor has open, or the one it is still
    /// waiting to open from the last layout (a store that is not open yet
    /// must not cost the pane its document).
    pub fn saved_panes(&self) -> crate::panes::SavedPanes {
        let tiling = self.tiling.get();
        let focused = self.focused_pane.get();
        let mut documents = Vec::new();
        for pane in tiling.panes() {
            let state = self.pane(pane);
            let held = state
                .active_edit
                .get()
                .or_else(|| state.board.get())
                .or_else(|| state.sheet.get())
                .or_else(|| state.discussion.get())
                .or_else(|| state.metanode.get())
                .or_else(|| state.theme.get())
                .map(|edit| (edit.store_id, edit.node_id))
                .or_else(|| state.restore.get());
            if let Some((store_id, node_id)) = held {
                documents.push(crate::panes::SavedDocument { pane, store_id, node_id });
            }
        }
        let beside = tiling.panes().into_iter().filter_map(|pane| Some((pane, self.pane(pane).beside.get()?))).collect();
        crate::panes::SavedPanes { tiling, focused, documents, beside }
    }

    /// Put back a remembered layout at start: the tiling and the focus at
    /// once, and each pane's document as one it opens when its store is
    /// open and the node is known (`events::restore_pane_documents`).
    pub fn restore_panes(&self, saved: crate::panes::SavedPanes) {
        let saved = saved.sanitized();
        for document in &saved.documents {
            self.pane(document.pane).restore.set(Some((document.store_id, document.node_id)));
        }
        for (pane, other) in &saved.beside {
            self.pane(*pane).beside.set(Some(*other));
        }
        self.tiling.set(saved.tiling);
        self.focused_pane.set(saved.focused);
    }

    /// Give pane `pane` the focus: the tree's selected row becomes its
    /// document. Answers whether the focus moved.
    pub fn focus_pane(&self, pane: PaneId) -> bool {
        if pane == self.focused() {
            return false;
        }
        self.focused_pane.set(pane);
        let value = untracked(|| self.pane(pane).selected.get());
        if untracked(|| self.selected_id.get()) != value {
            self.selected_id.set(value);
        }
        true
    }
}

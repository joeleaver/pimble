//! The toolbar's buttons as data: what each one does (`Cmd`), when it reads
//! as pressed (`ActiveCheck`), the labeled groups they sit in, the inserts it
//! shares with Edit > Insert, and running a button's command on a pane's
//! editor (`execute_cmd`).

use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use super::bump_toolbar;
use crate::editor::editor;
use crate::panes::PaneId;
use crate::rinch_editor::EditorHandle;
use crate::state::AppStore;
use pimble_crdt::text_roles::ROLE_ATTR;

#[derive(Clone)]
pub(super) enum Cmd {
    ToggleWrap(&'static str),
    SetBlock(&'static str),
    /// One of rinch's editor commands, by name.
    Run(&'static str),
    Undo,
    Redo,
    ClearFormatting,
    /// One of the inserts (`inserts`), into the focused pane's note.
    Insert(Insert),
    /// A discussion at the caret, drawn in the margin.
    Discuss,
    /// Makes the paragraph a subtitle, or an ordinary one again.
    Subtitle,
    /// The Text Color button: it opens its swatches (`render_color_btn`).
    TextColor,
}

/// What goes into a note's text at the caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Insert {
    Canvas,
    Sheet,
    SheetNode,
    Discussion,
    Table,
    Rule,
    Field,
    Picture,
}

/// One insert: its icon, its name in Edit > Insert, and its button's tooltip.
pub(crate) struct InsertItem {
    pub icon: TablerIcon,
    pub label: &'static str,
    pub tip: &'static str,
    pub what: Insert,
    /// A rule goes before it in the menu.
    pub divided: bool,
}

/// Edit > Insert and the toolbar's Insert group are this one list, in this
/// order, so they cannot drift apart. What New also makes (a canvas, a sheet,
/// a discussion, a canvas from a picture) has New's name, icon and place in the order
/// (`crate::creating::kinds`; Joe, 2026-11-06).
pub(crate) fn inserts() -> Vec<InsertItem> {
    let item = |icon, label, tip, what, divided| InsertItem { icon, label, tip, what, divided };
    vec![
        item(TablerIcon::Chalkboard, "Canvas", "Insert Canvas", Insert::Canvas, false),
        item(TablerIcon::Table, "Sheet", "Insert Sheet", Insert::Sheet, false),
        item(TablerIcon::TableShare, "Sheet from a Node...", "Insert Sheet from a Node...", Insert::SheetNode, false),
        item(TablerIcon::Messages, "Discussion", "Insert Discussion", Insert::Discussion, false),
        item(TablerIcon::TablePlus, "Table", "Insert Table", Insert::Table, true),
        item(TablerIcon::SeparatorHorizontal, "Horizontal Rule", "Insert Horizontal Rule", Insert::Rule, false),
        item(TablerIcon::Forms, "Field", "Add Field", Insert::Field, false),
        item(TablerIcon::Photo, "Canvas from Picture...", "Insert Canvas from Picture...", Insert::Picture, true),
    ]
}

/// Put `what` into the focused pane's note at its caret: Edit > Insert and
/// the toolbar both run this. With no note there, or one this device may only
/// read, it says so.
pub(crate) fn insert(store: AppStore, what: Insert) {
    match what {
        Insert::Canvas => crate::text_boards::insert_board(store),
        Insert::Sheet => crate::sheets::in_text::insert_sheet(store),
        Insert::SheetNode => crate::sheets::in_text::insert_sheet_node(store),
        Insert::Discussion => crate::discussions::insert_inline(store),
        Insert::Field => crate::properties::add_field(store),
        Insert::Picture => crate::pictures::insert_image(store),
        Insert::Table | Insert::Rule => {
            let pane = store.focused();
            let Some(active) = untracked(|| store.pane(pane).active_edit.get()) else {
                crate::events::show_notice(store, "Open a note to put it in.".to_string());
                return;
            };
            if !store.node_access(active.store_id, active.node_id).allows_write() {
                crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
                return;
            }
            let h = editor(pane);
            h.command(if what == Insert::Table { "insertTable" } else { "insertHorizontalRule" });
            h.focus();
            bump_toolbar(pane);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum ActiveCheck {
    Mark(&'static str),
    Block(&'static str),
    Subtitle,
    None,
}

pub(super) struct BtnDef {
    pub(super) icon: TablerIcon,
    pub(super) tooltip: &'static str,
    pub(super) cmd: Cmd,
    pub(super) active_check: ActiveCheck,
}

/// A labeled run of buttons (Joe, 2026-10-20: group them in a way that makes
/// sense, with category labels).
pub(super) struct Group {
    pub(super) label: &'static str,
    pub(super) buttons: Vec<BtnDef>,
    /// Shown only while the caret is in a table: the row and column buttons
    /// do nothing anywhere else.
    pub(super) in_table_only: bool,
    /// Usable by a commenter too, who writes discussions and nothing else.
    pub(super) for_commenters: bool,
}

pub(super) fn button_groups() -> Vec<Group> {
    let b = |icon, tooltip, cmd, active_check| BtnDef { icon, tooltip, cmd, active_check };
    vec![
        Group {
            label: "Text",
            buttons: vec![
                b(TablerIcon::Bold, "Bold (Ctrl+B)", Cmd::ToggleWrap("strong"), ActiveCheck::Mark("strong")),
                b(TablerIcon::Italic, "Italic (Ctrl+I)", Cmd::ToggleWrap("em"), ActiveCheck::Mark("em")),
                b(TablerIcon::Underline, "Underline (Ctrl+U)", Cmd::ToggleWrap("u"), ActiveCheck::Mark("u")),
                b(TablerIcon::Strikethrough, "Strikethrough", Cmd::ToggleWrap("s"), ActiveCheck::Mark("s")),
                b(TablerIcon::Code, "Inline Code", Cmd::ToggleWrap("code"), ActiveCheck::Mark("code")),
                b(TablerIcon::TextColor, "Text Color", Cmd::TextColor, ActiveCheck::None),
                b(TablerIcon::ClearFormatting, "Clear Formatting", Cmd::ClearFormatting, ActiveCheck::None),
            ],
            in_table_only: false,
            for_commenters: false,
        },
        // Everything here is inside rinch's collaboration scope (CLAUDE.md
        // "Collaboration shape"). Task lists are not, so they have no button.
        Group {
            label: "Paragraph",
            buttons: vec![
                b(TablerIcon::H1, "Heading 1", Cmd::SetBlock("h1"), ActiveCheck::Block("h1")),
                b(TablerIcon::H2, "Heading 2", Cmd::SetBlock("h2"), ActiveCheck::Block("h2")),
                b(TablerIcon::H3, "Heading 3", Cmd::SetBlock("h3"), ActiveCheck::Block("h3")),
                b(TablerIcon::TextCaption, "Subtitle", Cmd::Subtitle, ActiveCheck::Subtitle),
                b(TablerIcon::List, "Bullet List", Cmd::SetBlock("ul"), ActiveCheck::Block("ul")),
                b(TablerIcon::ListNumbers, "Numbered List", Cmd::SetBlock("ol"), ActiveCheck::Block("ol")),
                b(TablerIcon::Blockquote, "Block Quote (Ctrl+Shift+B)", Cmd::SetBlock("blockquote"), ActiveCheck::Block("blockquote")),
                b(TablerIcon::SourceCode, "Code Block", Cmd::SetBlock("pre"), ActiveCheck::Block("pre")),
            ],
            in_table_only: false,
            for_commenters: false,
        },
        // The Edit menu's inserts, at the caret, plus the text's own table and rule.
        Group {
            label: "Insert",
            buttons: inserts().into_iter().map(|i| b(i.icon, i.tip, Cmd::Insert(i.what), ActiveCheck::None)).collect(),
            in_table_only: false,
            for_commenters: false,
        },
        Group {
            label: "Table",
            buttons: vec![
                b(TablerIcon::RowInsertBottom, "Add Row Below", Cmd::Run("addRowAfter"), ActiveCheck::None),
                b(TablerIcon::ColumnInsertRight, "Add Column to the Right", Cmd::Run("addColumnAfter"), ActiveCheck::None),
                b(TablerIcon::RowRemove, "Delete Row", Cmd::Run("deleteRow"), ActiveCheck::None),
                b(TablerIcon::ColumnRemove, "Delete Column", Cmd::Run("deleteColumn"), ActiveCheck::None),
                b(TablerIcon::TableMinus, "Delete Table", Cmd::Run("deleteTable"), ActiveCheck::None),
            ],
            in_table_only: true,
            for_commenters: false,
        },
        // docs/DISCUSSIONS_CONTRACT.md section 6: a discussion at the caret,
        // in the margin.
        Group {
            label: "Discuss",
            buttons: vec![b(TablerIcon::MessagePlus, "Start a Discussion Here", Cmd::Discuss, ActiveCheck::None)],
            in_table_only: false,
            for_commenters: true,
        },
        Group {
            label: "History",
            buttons: vec![
                b(TablerIcon::ArrowBackUp, "Undo (Ctrl+Z)", Cmd::Undo, ActiveCheck::None),
                b(TablerIcon::ArrowForwardUp, "Redo (Ctrl+Shift+Z)", Cmd::Redo, ActiveCheck::None),
            ],
            in_table_only: false,
            for_commenters: false,
        },
    ]
}

pub(super) fn mark_name(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "strong" => "bold",
        "em" => "italic",
        "u" => "underline",
        "s" => "strike",
        "code" => "code",
        _ => return None,
    })
}

/// The `level` of the heading the cursor is in, or `None` when the block at
/// the selection head is not a heading (or the selection spans blocks).
pub(super) fn heading_level(h: &EditorHandle) -> Option<i64> {
    let state = h.state();
    let resolved = state.doc.resolve(state.selection.head()).ok()?;
    let block = resolved.parent();
    if block.type_name() != "heading" {
        return None;
    }
    block.attrs().get_int("level")
}

/// Turn the textblock(s) at the selection into plain paragraphs, keeping
/// their inline marks and the alignment and indent of the block the cursor is
/// in: the "off" half of a heading or code-block button. The same transaction
/// step rinch's own block commands use, through the editor's one dispatch
/// path, so it is recorded and broadcast like any other edit.
pub(crate) fn set_plain_paragraph(h: &EditorHandle) -> bool {
    use rinch_editor_core::{AttrValue, Attrs};
    h.update(|state| {
        let paragraph = state.schema().node_type("paragraph")?.clone();
        let (from, to) = (state.selection.from().0, state.selection.to().0);
        let resolved = state.doc.resolve(state.selection.head()).ok()?;
        let block = resolved.parent();
        let mut kept: Vec<(&str, AttrValue)> = Vec::new();
        if let Some(align) = block.attrs().get_str("text_align") {
            kept.push(("text_align", AttrValue::from(align)));
        }
        if let Some(indent) = block.attrs().get_int("indent") {
            kept.push(("indent", AttrValue::Int(indent)));
        }
        let mut tr = state.tr();
        tr.set_block_type(from, to, paragraph, Attrs::from_iter(kept)).ok()?;
        tr.doc_changed().then_some(tr)
    })
}

pub(super) fn check_active_with(h: &EditorHandle, check: &ActiveCheck) -> bool {
    match check {
        ActiveCheck::Mark(tag) => mark_name(tag).map(|m| h.is_mark_active(m)).unwrap_or(false),
        ActiveCheck::Block(tag) => match *tag {
            "h1" => heading_level(h) == Some(1),
            "h2" => heading_level(h) == Some(2),
            "h3" => heading_level(h) == Some(3),
            "ul" => h.in_node_type("bullet_list"),
            "ol" => h.in_node_type("ordered_list"),
            "blockquote" => h.in_node_type("blockquote"),
            "pre" => h.current_block_type().as_deref() == Some("code_block"),
            _ => false,
        },
        ActiveCheck::Subtitle => crate::paragraph_roles::subtitle_active(h),
        ActiveCheck::None => false,
    }
}

pub(super) fn execute_cmd(store: AppStore, pane: PaneId, cmd: &Cmd) {
    let h = editor(pane);
    match cmd {
        // These act on the focused pane, which is the one the toolbar follows.
        Cmd::Insert(what) => {
            insert(store, *what);
            return;
        }
        Cmd::Discuss => {
            crate::discussions::start_here(store);
            return;
        }
        // Its own button opens the swatches; nothing runs from here.
        Cmd::TextColor => return,
        Cmd::ToggleWrap(tag) => {
            let name = match *tag {
                "strong" => "toggleBold",
                "em" => "toggleItalic",
                "u" => "toggleUnderline",
                "s" => "toggleStrike",
                "code" => "toggleCode",
                _ => return,
            };
            h.command(name);
        }
        Cmd::Subtitle => crate::paragraph_roles::toggle_subtitle(&h),
        Cmd::SetBlock(tag) => {
            // A block button is a toggle: pressed on a block that already is
            // what it sets, it goes back to a plain paragraph. rinch's
            // `setHeadingN`/`setCodeBlock` only ever set, and its
            // `setParagraph` is "clear formatting" (it strips bold and links
            // and lifts the block out of its list), which is not what turning
            // a heading off means.
            if matches!(*tag, "h1" | "h2" | "h3" | "pre") && check_active_with(&h, &ActiveCheck::Block(tag)) {
                set_plain_paragraph(&h);
                bump_toolbar(pane);
                return;
            }
            // And so is the quote button: inside a quote it lifts the block out.
            if *tag == "blockquote" && check_active_with(&h, &ActiveCheck::Block(tag)) {
                h.command("liftListItem");
                bump_toolbar(pane);
                return;
            }
            let name = match *tag {
                "h1" => "setHeading1",
                "h2" => "setHeading2",
                "h3" => "setHeading3",
                "ul" => "toggleBulletList",
                "ol" => "toggleOrderedList",
                "blockquote" => "wrapInBlockquote",
                "pre" => "setCodeBlock",
                _ => return,
            };
            h.command(name);
        }
        Cmd::Run(name) => {
            h.command(name);
        }
        Cmd::Undo => {
            h.command("undo");
        }
        Cmd::Redo => {
            h.command("redo");
        }
        Cmd::ClearFormatting => {
            h.command("setParagraph");
            // A subtitle is formatting too, and rinch keeps an app's
            // attributes on a paragraph that stays one.
            h.set_block_data_attr(ROLE_ATTR, None);
        }
    }
    bump_toolbar(pane);
}

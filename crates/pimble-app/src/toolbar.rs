//! Reactive editor toolbar for Pimble: one, above the panes, acting on and
//! showing the state of the focused pane's editor (docs/SPLIT_VIEW_CONTRACT.md,
//! "One toolbar", Joe 2026-10-08). It greys out while the focused pane holds
//! no document or one this device may only read.
//!
//! Built with proper rinch patterns (Signals, reactive closures) so that
//! button active-state and dropdowns update correctly — unlike the upstream
//! `render_toolbar` which renders once and never updates.

use std::cell::{Cell, RefCell};

use rinch::prelude::*;
use rinch_tabler_icons::{TablerIcon, TablerIconStyle, render_tabler_icon};

use crate::editor::editor;
use crate::panes::PaneId;
use crate::rinch_editor::EditorHandle;
use crate::state::AppStore;

thread_local! {
    /// Each mounted button's active state, for the focused pane, in
    /// `button_groups` order (see `fingerprint`). Written with `set_if_changed`, and each button's style
    /// reads only its own, so a keystroke that leaves the formatting at the
    /// caret alone writes nothing to the toolbar, and one that changes it
    /// restyles only the buttons that flipped. (Until 2026-09-22 every editor
    /// change bumped a version every button's style read: fifteen identical
    /// style writes and about a hundred restyled nodes, twice per keystroke.
    /// A `Memo` per button would not have helped: rinch's `Memo` notifies its
    /// readers whenever its inputs change, equal or not.)
    static BUTTON_ACTIVE: RefCell<Vec<Signal<bool>>> = const { RefCell::new(Vec::new()) };
    /// The pane the buttons show and act on: the focused one, kept in step by
    /// an effect in `render_pimble_toolbar`.
    static TARGET: Cell<PaneId> = const { Cell::new(PaneId::FIRST) };
    /// Whether the self-rescheduling watcher is running.
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    /// Whether the focused pane's caret is in a table: the Table group shows.
    static IN_TABLE: RefCell<Option<Signal<bool>>> = const { RefCell::new(None) };
}

/// How often the watcher re-reads the editor state. Cursor moves fire no
/// editor callback, so this is what keeps the buttons honest while the
/// caret travels; a handful of RefCell reads per tick, nothing more.
const WATCH_INTERVAL_MS: u32 = 120;

/// Re-read the buttons' active states now, if `pane` is the one they show.
/// Deferred via `run_on_main_thread` so it never runs while the editor's
/// RefCell is still mutably borrowed by the operation that triggered it (a
/// command, a change callback).
pub(crate) fn bump_toolbar(pane: PaneId) {
    run_on_main_thread(move || {
        if TARGET.with(Cell::get) == pane {
            refresh_active_bits(pane);
        }
    });
}

/// Store pane `pane`'s current active states in the buttons, notifying only
/// the buttons that flipped.
fn refresh_active_bits(pane: PaneId) {
    let signals = BUTTON_ACTIVE.with(|b| b.borrow().clone());
    if signals.is_empty() {
        return;
    }
    let now = fingerprint(pane);
    for (i, signal) in signals.iter().enumerate() {
        if signal.is_alive() {
            signal.set_if_changed(now & (1 << i) != 0);
        }
    }
    if let Some(in_table) = IN_TABLE.with(|t| *t.borrow()).filter(|s| s.is_alive()) {
        in_table.set_if_changed(editor(pane).in_node_type("table"));
    }
}

/// Every button's active state packed into bits, so a change anywhere is
/// one integer compare.
fn fingerprint(pane: PaneId) -> u64 {
    let h = editor(pane);
    let mut bits = 0u64;
    for (i, def) in button_groups().into_iter().flat_map(|g| g.buttons).enumerate() {
        if check_active_with(&h, &def.active_check) {
            bits |= 1 << i;
        }
    }
    bits
}

/// Start (idempotently) a light poll that bumps the toolbar only when its
/// active states actually changed. Called once from `render_pimble_toolbar`.
fn watch_toolbar() {
    if WATCHING.with(|w| w.replace(true)) {
        return;
    }
    fn tick() {
        refresh_active_bits(TARGET.with(Cell::get));
        set_timeout(WATCH_INTERVAL_MS, tick);
    }
    set_timeout(WATCH_INTERVAL_MS, tick);
}

// ── Toolbar button definitions ───────────────────────────────────────────

#[derive(Clone)]
enum Cmd {
    ToggleWrap(&'static str),
    SetBlock(&'static str),
    /// One of rinch's editor commands, by name.
    Run(&'static str),
    HorizontalRule,
    Undo,
    Redo,
    ClearFormatting,
    /// One of the Edit menu's inserts, into the focused pane's note.
    Insert(Insert),
}

/// The Edit menu's inserts, the same functions its items call (`menus.rs`).
#[derive(Clone, Copy)]
enum Insert {
    Image,
    Board,
    Sheet,
    SheetNode,
    Field,
}

#[derive(Clone, Copy)]
enum ActiveCheck {
    Mark(&'static str),
    Block(&'static str),
    None,
}

struct BtnDef {
    icon: TablerIcon,
    tooltip: &'static str,
    cmd: Cmd,
    active_check: ActiveCheck,
}

/// A labeled run of buttons (Joe, 2026-10-20: group them in a way that makes
/// sense, with category labels).
struct Group {
    label: &'static str,
    buttons: Vec<BtnDef>,
    /// Shown only while the caret is in a table: the row and column buttons
    /// do nothing anywhere else.
    in_table_only: bool,
}

fn button_groups() -> Vec<Group> {
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
                b(TablerIcon::ClearFormatting, "Clear Formatting", Cmd::ClearFormatting, ActiveCheck::None),
            ],
            in_table_only: false,
        },
        // Everything here is inside rinch's collaboration scope (CLAUDE.md
        // "Collaboration shape"). Task lists are not, so they have no button.
        Group {
            label: "Paragraph",
            buttons: vec![
                b(TablerIcon::H1, "Heading 1", Cmd::SetBlock("h1"), ActiveCheck::Block("h1")),
                b(TablerIcon::H2, "Heading 2", Cmd::SetBlock("h2"), ActiveCheck::Block("h2")),
                b(TablerIcon::H3, "Heading 3", Cmd::SetBlock("h3"), ActiveCheck::Block("h3")),
                b(TablerIcon::List, "Bullet List", Cmd::SetBlock("ul"), ActiveCheck::Block("ul")),
                b(TablerIcon::ListNumbers, "Numbered List", Cmd::SetBlock("ol"), ActiveCheck::Block("ol")),
                b(TablerIcon::Blockquote, "Block Quote (Ctrl+Shift+B)", Cmd::SetBlock("blockquote"), ActiveCheck::Block("blockquote")),
                b(TablerIcon::SourceCode, "Code Block", Cmd::SetBlock("pre"), ActiveCheck::Block("pre")),
            ],
            in_table_only: false,
        },
        // The Edit menu's inserts, at the caret, plus the text's own table and rule.
        Group {
            label: "Insert",
            buttons: vec![
                b(TablerIcon::Photo, "Insert Image...", Cmd::Insert(Insert::Image), ActiveCheck::None),
                b(TablerIcon::Chalkboard, "Insert Board...", Cmd::Insert(Insert::Board), ActiveCheck::None),
                b(TablerIcon::Table, "Insert Sheet", Cmd::Insert(Insert::Sheet), ActiveCheck::None),
                b(TablerIcon::TableShare, "Insert Sheet from a Node...", Cmd::Insert(Insert::SheetNode), ActiveCheck::None),
                b(TablerIcon::TablePlus, "Insert Table", Cmd::Run("insertTable"), ActiveCheck::None),
                b(TablerIcon::SeparatorHorizontal, "Horizontal Rule", Cmd::HorizontalRule, ActiveCheck::None),
                b(TablerIcon::Forms, "Add Field", Cmd::Insert(Insert::Field), ActiveCheck::None),
            ],
            in_table_only: false,
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
        },
        Group {
            label: "History",
            buttons: vec![
                b(TablerIcon::ArrowBackUp, "Undo (Ctrl+Z)", Cmd::Undo, ActiveCheck::None),
                b(TablerIcon::ArrowForwardUp, "Redo (Ctrl+Shift+Z)", Cmd::Redo, ActiveCheck::None),
            ],
            in_table_only: false,
        },
    ]
}

fn mark_name(tag: &str) -> Option<&'static str> {
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
fn heading_level(h: &EditorHandle) -> Option<i64> {
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
fn set_plain_paragraph(h: &EditorHandle) -> bool {
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

fn check_active_with(h: &EditorHandle, check: &ActiveCheck) -> bool {
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
        ActiveCheck::None => false,
    }
}

fn execute_cmd(store: AppStore, pane: PaneId, cmd: &Cmd) {
    let h = editor(pane);
    match cmd {
        // These act on the focused pane, which is the one the toolbar follows.
        Cmd::Insert(what) => {
            match what {
                Insert::Image => crate::pictures::insert_image(store),
                Insert::Board => crate::text_boards::insert_board(store),
                Insert::Sheet => crate::sheets::in_text::insert_sheet(store),
                Insert::SheetNode => crate::sheets::in_text::insert_sheet_node(store),
                Insert::Field => crate::fields::add_field(store),
            }
            return;
        }
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
        Cmd::HorizontalRule => {
            h.command("insertHorizontalRule");
        }
        Cmd::Undo => {
            h.command("undo");
        }
        Cmd::Redo => {
            h.command("redo");
        }
        Cmd::ClearFormatting => {
            h.command("setParagraph");
        }
    }
    bump_toolbar(pane);
}

// ── Styles ───────────────────────────────────────────────────────────────

const BTN_BASE: &str = "display: inline-flex; align-items: center; justify-content: center; \
    width: 30px; height: 30px; border-radius: 4px; cursor: pointer; transition: background 0.15s;";

fn btn_style(active: bool) -> String {
    if active {
        format!("{} border: 1px solid var(--pimble-toolbar-active-border); \
                 background: var(--pimble-toolbar-active-bg); \
                 color: var(--pimble-toolbar-active-fg);", BTN_BASE)
    } else {
        format!("{} border: 1px solid transparent; \
                 color: var(--pimble-toolbar-icon);", BTN_BASE)
    }
}

// ── Public render ────────────────────────────────────────────────────────

/// The toolbar, rendered once. `usable` says whether the focused pane holds a
/// document this device may write; while it does not, the buttons are dimmed
/// and do nothing.
pub(crate) fn render_pimble_toolbar(
    __scope: &mut RenderScope,
    store: AppStore,
    usable: impl Fn() -> bool + Copy + 'static,
) -> NodeHandle {
    let groups = button_groups();
    let count = groups.iter().map(|g| g.buttons.len()).sum();
    BUTTON_ACTIVE.with(|b| *b.borrow_mut() = (0..count).map(|_| Signal::new(false)).collect());
    let in_table = Signal::new(false);
    IN_TABLE.with(|t| *t.borrow_mut() = Some(in_table));
    // The buttons follow the focus: a click in another pane, or its editor
    // taking the keyboard, re-reads them from that pane's editor.
    let _ = rinch::Effect::new(move || {
        let pane = store.focused_pane.get();
        TARGET.with(|t| t.set(pane));
        untracked(|| refresh_active_bits(pane));
    });
    watch_toolbar();

    let toolbar = rsx! {
        div {
            class: "editor-toolbar pimble-toolbar",
            style: {move || if usable() { "" } else { "opacity: 0.4; pointer-events: none;" }},
        }
    };

    // Each button's index into `BUTTON_ACTIVE`, counted the way `fingerprint`
    // flattens the groups.
    let mut bit = 0u32;
    for group in groups {
        let in_table_only = group.in_table_only;
        let element = rsx! {
            div {
                class: "pimble-toolbar__group",
                style: {move || if in_table_only && !in_table.get() { "display: none;" } else { "" }},
            }
        };
        let row = rsx! { div { class: "pimble-toolbar__buttons" } };
        for btn_def in group.buttons {
            let btn = render_btn(__scope, store, bit, btn_def);
            row.append_child(&btn);
            bit += 1;
        }
        let label = rsx! { div { class: "pimble-toolbar__label", {group.label} } };
        element.append_child(&row);
        element.append_child(&label);
        toolbar.append_child(&element);
    }

    toolbar
}

fn render_btn(
    __scope: &mut RenderScope,
    store: AppStore,
    bit: u32,
    def: BtnDef,
) -> NodeHandle {
    let cmd = def.cmd;
    let icon_el = render_tabler_icon(__scope, def.icon, TablerIconStyle::Outline);
    // A button with no active state never restyles; the rest follow their
    // own signal only.
    let active = match def.active_check {
        ActiveCheck::None => None,
        _ => BUTTON_ACTIVE.with(|b| b.borrow().get(bit as usize).copied()),
    };

    rsx! {
        div {
            title: def.tooltip,
            style: {
                move || btn_style(active.is_some_and(|a| a.get()))
            },
            onclick: {
                let cmd = cmd.clone();
                move || execute_cmd(store, TARGET.with(Cell::get), &cmd)
            },
            span {
                style: "width: 18px; height: 18px; display: inline-flex; align-items: center; justify-content: center;",
                {icon_el}
            }
        }
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::rinch_editor::create_editor;
    use rinch_editor_core::{Pos, Selection};

    fn block_type(h: &EditorHandle) -> String {
        h.state().doc.child(0).type_name().to_string()
    }

    /// A heading button is a toggle: pressed on its own heading it gives a plain
    /// paragraph back, and the bold inside it is still bold (rinch's
    /// `setParagraph` would have stripped it: that one is "clear formatting").
    #[test]
    fn a_heading_button_pressed_again_gives_the_paragraph_back_and_keeps_its_marks() {
        let h = create_editor();
        assert!(h.load_html("<h2>Plan for <strong>Friday</strong></h2>"));
        h.set_selection(Selection::cursor(Pos(3)));
        assert_eq!(block_type(&h), "heading");
        assert!(check_active_with(&h, &ActiveCheck::Block("h2")));
        assert!(!check_active_with(&h, &ActiveCheck::Block("h1")));

        assert!(set_plain_paragraph(&h));
        assert_eq!(block_type(&h), "paragraph");
        assert!(!check_active_with(&h, &ActiveCheck::Block("h2")));
        let state = h.state();
        let block = state.doc.child(0);
        let friday = (0..block.child_count())
            .map(|i| block.child(i))
            .find(|n| n.text() == Some("Friday"))
            .expect("the bold run is still its own text node");
        assert!(friday.marks().iter().any(|m| m.type_name() == "bold"), "the bold must survive the toggle");

        // And it is a real toggle: nothing to turn off on a paragraph.
        assert!(!set_plain_paragraph(&h));
    }

    /// A refresh that finds the formatting at the caret unchanged notifies no
    /// button, and one that finds bold switched on notifies the bold button
    /// alone: a keystroke must not restyle the whole toolbar.
    #[test]
    fn a_refresh_wakes_only_the_buttons_whose_state_changed() {
        use std::rc::Rc;

        // A pane of its own: the buttons of the others are not this test's.
        let pane = PaneId::ALL[2];
        let h = editor(pane);
        assert!(h.load_html("<p><strong>bold</strong> plain</p>"));
        h.set_selection(Selection::cursor(Pos(8)));

        let count: usize = button_groups().iter().map(|g| g.buttons.len()).sum();
        let signals: Vec<Signal<bool>> = (0..count).map(|_| Signal::new(false)).collect();
        BUTTON_ACTIVE.with(|b| *b.borrow_mut() = signals.clone());
        let runs: Vec<Rc<Cell<u32>>> = (0..count).map(|_| Rc::new(Cell::new(0))).collect();
        let _effects: Vec<rinch::Effect> = signals
            .iter()
            .zip(&runs)
            .map(|(signal, runs)| {
                let (signal, runs) = (*signal, runs.clone());
                rinch::Effect::new(move || {
                    let _ = signal.get();
                    runs.set(runs.get() + 1);
                })
            })
            .collect();
        let snapshot = || runs.iter().map(|r| r.get()).collect::<Vec<_>>();
        let before = snapshot();

        refresh_active_bits(pane);
        refresh_active_bits(pane);
        assert_eq!(snapshot(), before, "nothing changed, so nothing is notified");

        h.set_selection(Selection::cursor(Pos(3)));
        refresh_active_bits(pane);
        let after = snapshot();
        let bold = 0; // the first button of the first group
        assert!(signals[bold].get(), "the caret is in bold text");
        for i in 0..count {
            let expected = before[i] + u32::from(i == bold);
            assert_eq!(after[i], expected, "button {i}");
        }
    }
}

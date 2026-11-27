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
use rinch_tabler_icons::{TablerIconStyle, render_tabler_icon};

use crate::editor::editor;
use crate::panes::PaneId;
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
    /// The text color at the focused pane's selection (the stored value,
    /// `None` for the default), and whether its caret is in a code block,
    /// which takes no color (docs/TEXT_COLOR_CONTRACT.md).
    static TEXT_COLOR: RefCell<Option<(Signal<Option<String>>, Signal<bool>)>> = const { RefCell::new(None) };
}

/// What a pane's text has selected, as the toolbar reads it
/// (docs/TOOLBAR_CONTRACT.md "Which toolbar"): written from the editor's own
/// selection callback (`item_changed`), never polled.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Item {
    /// A caret or a selection of text (a table's cells, a discussion or a
    /// web picture in the text included).
    #[default]
    Text,
    /// Exactly a canvas in the text: a picture, a canvas that began empty, or
    /// an embedded canvas node.
    Canvas(Picked),
    /// Exactly a sheet in the text (a table of the note's, or an embedded
    /// sheet node): the table it shows, when it shows one.
    Sheet(Option<String>),
}

/// A canvas selected in a pane's text: the pane, the position before the
/// atom, its `src`, its board (`data-board`, none before it has one) and, for
/// an embedded canvas node, that node.
#[derive(Debug, Clone, PartialEq)]
pub struct Picked {
    pub pane: PaneId,
    pub pos: usize,
    pub src: String,
    pub board: Option<String>,
    pub embed: Option<pimble_core::BoardUrl>,
}

/// Pane `pane`'s selection was set or its text changed: what it selects is
/// read again, a turn later (the editor is still busy with the change).
pub(crate) fn item_changed(store: AppStore, pane: PaneId) {
    run_on_main_thread(move || {
        let item = crate::text_boards::selected_item(store, pane);
        store.pane(pane).item.set_if_changed(item);
    });
}

/// Which toolbar the main toolbar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Which {
    /// The text groups: a note's caret or text selection, and every pane
    /// with nothing of its own to act on (no document, a discussion, a
    /// metanode), where they dim.
    Text,
    /// The canvas groups.
    Canvas,
    /// The sheet groups.
    Sheet,
}

/// The one rule choosing the main toolbar (docs/TOOLBAR_CONTRACT.md "Which
/// toolbar"): it shows the toolbar of what is selected in the focused pane.
/// Read reactively; `pane_view` shows the one it names.
pub(crate) fn which(store: AppStore) -> Which {
    let pane = store.focused_pane.get();
    let state = store.pane(pane);
    if state.show_discussion.get() {
        return Which::Text;
    }
    if state.show_board.get() {
        return Which::Canvas;
    }
    if state.show_sheet.get() {
        return Which::Sheet;
    }
    if state.show_metanode.get() || !state.show_editor.get() {
        return Which::Text;
    }
    // A note: a cell of a sheet in its text being typed in or chosen, else
    // what its editor's selection is.
    let ui = state.sheet_ui;
    if ui.editing.get().is_some() || ui.selected.get().is_some() {
        return Which::Sheet;
    }
    match state.item.get() {
        Item::Text => Which::Text,
        Item::Canvas(_) => Which::Canvas,
        Item::Sheet(_) => Which::Sheet,
    }
}

/// The canvas selected in pane `pane`'s text, read reactively.
pub(crate) fn picked(store: AppStore, pane: PaneId) -> Option<Picked> {
    match store.pane(pane).item.get() {
        Item::Canvas(p) => Some(p),
        _ => None,
    }
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
    if let Some((color, in_code)) = TEXT_COLOR.with(|t| *t.borrow()).filter(|(c, k)| c.is_alive() && k.is_alive()) {
        let state = editor(pane).state();
        color.set_if_changed(crate::text_color::current_color(&state));
        in_code.set_if_changed(crate::text_color::in_code_block(&state));
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

mod buttons;

use buttons::*;
pub(crate) use buttons::{insert, inserts, set_plain_paragraph};

/// The Block Quote button's toggle on `pane`'s editor: wrap the selection in
/// a quote, or lift it out of the one it is in. Ctrl+Shift+B runs this too
/// (crate::editor_keys), so the key and the button are one path. A note this
/// device may only read is left alone.
pub(crate) fn toggle_quote(store: AppStore, pane: PaneId) {
    if editor(pane).is_read_only() {
        return;
    }
    execute_cmd(store, pane, &Cmd::SetBlock("blockquote"));
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
/// and do nothing, but for a commenter's (`commentable`: a note this device
/// may comment on).
pub(crate) fn render_pimble_toolbar(
    __scope: &mut RenderScope,
    store: AppStore,
    usable: impl Fn() -> bool + Copy + 'static,
    commentable: impl Fn() -> bool + Copy + 'static,
) -> NodeHandle {
    let groups = button_groups();
    let count = groups.iter().map(|g| g.buttons.len()).sum();
    BUTTON_ACTIVE.with(|b| *b.borrow_mut() = (0..count).map(|_| Signal::new(false)).collect());
    let in_table = Signal::new(false);
    IN_TABLE.with(|t| *t.borrow_mut() = Some(in_table));
    let text_color = (Signal::new(None::<String>), Signal::new(false));
    TEXT_COLOR.with(|t| *t.borrow_mut() = Some(text_color));
    // The buttons follow the focus: a click in another pane, or its editor
    // taking the keyboard, re-reads them from that pane's editor.
    let _ = rinch::Effect::new(move || {
        let pane = store.focused_pane.get();
        TARGET.with(|t| t.set(pane));
        untracked(|| refresh_active_bits(pane));
    });
    watch_toolbar();

    // One row at any width (docs/TOOLBAR_CONTRACT.md "One row"): groups
    // fold into More, Insert first (Edit > Insert has it all), then Discuss,
    // History (Ctrl+Z), Table, Paragraph; Text last.
    let priority = |label: &str| match label {
        "Insert" => 1,
        "Discuss" => 2,
        "History" => 3,
        "Table" => 4,
        "Paragraph" => 5,
        _ => 6,
    };
    // Each button's index into `BUTTON_ACTIVE`, counted the way `fingerprint`
    // flattens the groups.
    let mut first_bit = 0u32;
    let mut slots = Vec::new();
    for (gi, group) in groups.into_iter().enumerate() {
        let (in_table_only, for_commenters) = (group.in_table_only, group.for_commenters);
        let bits = first_bit;
        first_bit += group.buttons.len() as u32;
        let build = move |__scope: &mut RenderScope| -> Vec<NodeHandle> {
            let group = button_groups().into_iter().nth(gi).expect("the group");
            let mut bit = bits;
            group
                .buttons
                .into_iter()
                .map(|btn_def| {
                    let btn = if matches!(btn_def.cmd, Cmd::TextColor) {
                        render_color_btn(__scope, store, btn_def, text_color)
                    } else {
                        render_btn(__scope, store, bit, btn_def)
                    };
                    bit += 1;
                    btn
                })
                .collect()
        };
        slots.push(crate::toolbar_fit::Slot::new(
            group.label,
            Some(priority(group.label)),
            move || !in_table_only || in_table.get(),
            move || usable() || (for_commenters && commentable()),
            build,
        ));
    }
    crate::toolbar_fit::render(__scope, "editor-toolbar pimble-toolbar", slots, || {})
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

/// The Text Color button (docs/TEXT_COLOR_CONTRACT.md section 4): the icon
/// over a bar in the color at the selection, as the current theme shows it,
/// opening "Default" and the palette, each drawn in the current theme's shade
/// and named in its tooltip. A swatch colors the selection (or what is typed
/// next at a caret) and closes; Escape or a click outside closes. Dimmed in a
/// code block, which takes no color.
fn render_color_btn(
    __scope: &mut RenderScope,
    store: AppStore,
    def: BtnDef,
    (current, in_code): (Signal<Option<String>>, Signal<bool>),
) -> NodeHandle {
    use crate::text_color::{shown_color, PALETTE};
    let open = Signal::new(false);
    let icon_el = render_tabler_icon(__scope, def.icon, TablerIconStyle::Outline);
    let dark = move || store.dark_mode.get();
    // "Default" first, then the palette: (label, stored color, shade per theme).
    let mut choices: Vec<(&'static str, Option<&'static str>, Option<(&'static str, &'static str)>)> = vec![("Default", None, None)];
    choices.extend(PALETTE.iter().map(|s| (s.label, Some(s.light), Some((s.light, s.dark)))));
    let swatches: Vec<NodeHandle> = choices
        .into_iter()
        .map(|(label, color, shades)| {
            let shade = move || match shades {
                Some((light, dark_shade)) => if dark() { dark_shade } else { light }.to_string(),
                None => "var(--rinch-color-text)".to_string(),
            };
            rsx! {
                div {
                    class: {move || {
                        let on = current.with(|c| match (c, color) {
                            (None, None) => true,
                            (Some(c), Some(stored)) => crate::text_color::palette_entry(c).is_some_and(|e| e.light == stored),
                            _ => false,
                        });
                        if on { "pimble-tc-swatch pimble-tc-swatch--on" } else { "pimble-tc-swatch" }
                    }},
                    title: label,
                    style: {move || format!("color: {};", shade())},
                    onclick: move || {
                        open.set(false);
                        crate::text_color::apply(store, color);
                    },
                    span { class: "pimble-tc-swatch__letter", "A" }
                    span { class: "pimble-tc-swatch__bar" }
                }
            }
        })
        .collect();

    rsx! {
        div {
            style: {move || if in_code.get() { "opacity: 0.4; pointer-events: none;" } else { "" }},
            Popover {
                opened_fn: move || open.get(),
                onclose: move || {
                    open.set(false);
                    editor(TARGET.with(Cell::get)).focus();
                },
                position: "bottom-start",
                close_on_click_outside: true,
                close_on_escape: true,
                z_index: 300,
                PopoverTarget {
                    div {
                        title: def.tooltip,
                        style: {move || btn_style(open.get())},
                        onclick: move || open.set(!untracked(|| open.get())),
                        span {
                            class: "pimble-tc-button",
                            {icon_el}
                            span {
                                class: "pimble-tc-button__bar",
                                style: {move || match current.get() {
                                    Some(c) => format!("background: {};", shown_color(&c, dark())),
                                    // No color here: the palette as a band, so the
                                    // button says "text color" at a glance (Joe).
                                    None => {
                                        let d = dark();
                                        let stops: Vec<&str> = PALETTE.iter().filter(|s| s.label != "Gray").map(|s| if d { s.dark } else { s.light }).collect();
                                        format!("background: linear-gradient(90deg, {});", stops.join(", "))
                                    }
                                }},
                            }
                        }
                    }
                }
                PopoverDropdown {
                    div {
                        class: "pimble-tc-swatches",
                        {swatches}
                    }
                }
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

    /// The one rule (docs/TOOLBAR_CONTRACT.md "Which toolbar"): the toolbar
    /// of what the focused pane has selected.
    #[test]
    fn the_toolbar_follows_the_selected_item() {
        let store = AppStore::new();
        let pane = store.focused();
        let state = store.pane(pane);
        assert_eq!(which(store), Which::Text, "an empty pane: the text groups, dimmed");
        state.show_editor.set(true);
        assert_eq!(which(store), Which::Text, "a caret in a note");
        let picked = Picked { pane, pos: 3, src: "pimble-blob:x".into(), board: None, embed: None };
        state.item.set(Item::Canvas(picked.clone()));
        assert_eq!(which(store), Which::Canvas, "a picture selected");
        state.item.set(Item::Canvas(Picked { embed: Some(pimble_core::BoardUrl::new(pimble_core::StoreId::new(), pimble_core::NodeId::new())), ..picked }));
        assert_eq!(which(store), Which::Canvas, "an embedded canvas selected");
        state.item.set(Item::Sheet(Some("t1".into())));
        assert_eq!(which(store), Which::Sheet, "a sheet in the text selected");
        state.item.set(Item::Text);
        state.sheet_ui.selected.set(Some(crate::sheets::CellKey { table: "t1".into(), row: None, col: "c".into() }));
        assert_eq!(which(store), Which::Sheet, "a cell of a sheet in the text");
        state.sheet_ui.selected.set(None);
        state.show_board.set(true);
        assert_eq!(which(store), Which::Canvas, "a canvas pane, or a canvas being edited");
        state.show_board.set(false);
        state.show_sheet.set(true);
        assert_eq!(which(store), Which::Sheet, "a sheet pane");
        state.show_sheet.set(false);
        state.show_discussion.set(true);
        assert_eq!(which(store), Which::Text, "a discussion pane: nothing of its own");
        state.show_discussion.set(false);
        state.show_metanode.set(true);
        assert_eq!(which(store), Which::Text, "a metanode: nothing of its own");
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

    /// The Block Quote button (and Ctrl+Shift+B, crate::editor_keys) wraps
    /// the paragraph in a quote, lifts it out when pressed again, and leaves a
    /// note this device may only read alone.
    #[test]
    fn the_quote_toggle_wraps_lifts_and_leaves_a_read_only_note_alone() {
        let store = AppStore::new();
        let pane = store.focused();
        let h = editor(pane);
        assert!(h.load_html("<p>quote me</p>"));
        h.set_selection(Selection::cursor(Pos(2)));
        toggle_quote(store, pane);
        assert_eq!(block_type(&h), "blockquote");
        toggle_quote(store, pane);
        assert_eq!(block_type(&h), "paragraph");

        h.set_read_only(true);
        toggle_quote(store, pane);
        assert_eq!(block_type(&h), "paragraph", "a read-only note is not changed");
        h.set_read_only(false);
    }
}

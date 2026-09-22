//! Reactive editor toolbar for Pimble.
//!
//! Built with proper rinch patterns (Signals, reactive closures) so that
//! button active-state and dropdowns update correctly — unlike the upstream
//! `render_toolbar` which renders once and never updates.

use std::cell::{Cell, RefCell};

use rinch::prelude::*;
use rinch_tabler_icons::{TablerIcon, TablerIconStyle, render_tabler_icon};

use crate::editor::editor;
use crate::rinch_editor::EditorHandle;

thread_local! {
    /// Each mounted button's active state, in `button_groups` order (see
    /// `fingerprint`). Written with `set_if_changed`, and each button's style
    /// reads only its own, so a keystroke that leaves the formatting at the
    /// caret alone writes nothing to the toolbar, and one that changes it
    /// restyles only the buttons that flipped. (Until 2026-09-22 every editor
    /// change bumped a version every button's style read: fifteen identical
    /// style writes and about a hundred restyled nodes, twice per keystroke.
    /// A `Memo` per button would not have helped: rinch's `Memo` notifies its
    /// readers whenever its inputs change, equal or not.)
    static BUTTON_ACTIVE: RefCell<Vec<Signal<bool>>> = const { RefCell::new(Vec::new()) };
    /// Whether the self-rescheduling watcher is running.
    static WATCHING: Cell<bool> = const { Cell::new(false) };
}

/// How often the watcher re-reads the editor state. Cursor moves fire no
/// editor callback, so this is what keeps the buttons honest while the
/// caret travels; a handful of RefCell reads per tick, nothing more.
const WATCH_INTERVAL_MS: u32 = 120;

/// Re-read the active states now. Deferred via `run_on_main_thread` so it
/// never runs while the editor's RefCell is still mutably borrowed by the
/// operation that triggered it (a command, a change callback).
pub(crate) fn bump_toolbar() {
    run_on_main_thread(refresh_active_bits);
}

/// Store the current active states, notifying only the buttons that flipped.
fn refresh_active_bits() {
    let signals = BUTTON_ACTIVE.with(|b| b.borrow().clone());
    if signals.is_empty() {
        return;
    }
    let now = fingerprint();
    for (i, signal) in signals.iter().enumerate() {
        if signal.is_alive() {
            signal.set_if_changed(now & (1 << i) != 0);
        }
    }
}

/// Every button's active state packed into bits, so a change anywhere is
/// one integer compare.
fn fingerprint() -> u64 {
    let h = editor();
    let mut bits = 0u64;
    for (i, def) in button_groups().into_iter().flatten().enumerate() {
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
        refresh_active_bits();
        set_timeout(WATCH_INTERVAL_MS, tick);
    }
    set_timeout(WATCH_INTERVAL_MS, tick);
}

// ── Toolbar button definitions ───────────────────────────────────────────

#[derive(Clone)]
enum Cmd {
    ToggleWrap(&'static str),
    SetBlock(&'static str),
    HorizontalRule,
    Undo,
    Redo,
    ClearFormatting,
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

fn button_groups() -> Vec<Vec<BtnDef>> {
    vec![
        // Inline formatting
        vec![
            BtnDef { icon: TablerIcon::Bold, tooltip: "Bold (Ctrl+B)", cmd: Cmd::ToggleWrap("strong"), active_check: ActiveCheck::Mark("strong") },
            BtnDef { icon: TablerIcon::Italic, tooltip: "Italic (Ctrl+I)", cmd: Cmd::ToggleWrap("em"), active_check: ActiveCheck::Mark("em") },
            BtnDef { icon: TablerIcon::Underline, tooltip: "Underline (Ctrl+U)", cmd: Cmd::ToggleWrap("u"), active_check: ActiveCheck::Mark("u") },
            BtnDef { icon: TablerIcon::Strikethrough, tooltip: "Strikethrough", cmd: Cmd::ToggleWrap("s"), active_check: ActiveCheck::Mark("s") },
            BtnDef { icon: TablerIcon::Code, tooltip: "Inline Code", cmd: Cmd::ToggleWrap("code"), active_check: ActiveCheck::Mark("code") },
        ],
        // Block structure
        vec![
            BtnDef { icon: TablerIcon::H1, tooltip: "Heading 1", cmd: Cmd::SetBlock("h1"), active_check: ActiveCheck::Block("h1") },
            BtnDef { icon: TablerIcon::H2, tooltip: "Heading 2", cmd: Cmd::SetBlock("h2"), active_check: ActiveCheck::Block("h2") },
            BtnDef { icon: TablerIcon::H3, tooltip: "Heading 3", cmd: Cmd::SetBlock("h3"), active_check: ActiveCheck::Block("h3") },
        ],
        // Lists & blocks. Bullet and ordered lists are inside rinch's
        // collaboration scope (flat text blocks, marks, and nested lists);
        // blockquote stays hidden because a blockquote in a collaborating
        // document fails loudly by design (CLAUDE.md "Collaboration shape").
        vec![
            BtnDef { icon: TablerIcon::List, tooltip: "Bullet List", cmd: Cmd::SetBlock("ul"), active_check: ActiveCheck::Block("ul") },
            BtnDef { icon: TablerIcon::ListNumbers, tooltip: "Ordered List", cmd: Cmd::SetBlock("ol"), active_check: ActiveCheck::Block("ol") },
            BtnDef { icon: TablerIcon::SourceCode, tooltip: "Code Block", cmd: Cmd::SetBlock("pre"), active_check: ActiveCheck::Block("pre") },
        ],
        // Insert & utility
        vec![
            BtnDef { icon: TablerIcon::SeparatorHorizontal, tooltip: "Horizontal Rule", cmd: Cmd::HorizontalRule, active_check: ActiveCheck::None },
            BtnDef { icon: TablerIcon::ClearFormatting, tooltip: "Clear Formatting", cmd: Cmd::ClearFormatting, active_check: ActiveCheck::None },
        ],
        // Undo/Redo
        vec![
            BtnDef { icon: TablerIcon::ArrowBackUp, tooltip: "Undo (Ctrl+Z)", cmd: Cmd::Undo, active_check: ActiveCheck::None },
            BtnDef { icon: TablerIcon::ArrowForwardUp, tooltip: "Redo (Ctrl+Shift+Z)", cmd: Cmd::Redo, active_check: ActiveCheck::None },
        ],
    ]
}

/// Map an inline HTML tag to the new editor's mark name.
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

fn execute_cmd(cmd: &Cmd) {
    let h = editor();
    match cmd {
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
                bump_toolbar();
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
    bump_toolbar();
}

// ── Styles ───────────────────────────────────────────────────────────────

const BTN_BASE: &str = "display: inline-flex; align-items: center; justify-content: center; \
    width: 30px; height: 30px; border-radius: 4px; cursor: pointer; transition: background 0.15s;";

fn btn_style(active: bool) -> String {
    if active {
        format!("{} border: 1px solid var(--rinch-primary-color-4); \
                 background: var(--rinch-color-dark-4); \
                 color: var(--rinch-primary-color-4);", BTN_BASE)
    } else {
        format!("{} border: 1px solid transparent; \
                 color: #909296;", BTN_BASE)
    }
}

// ── Public render ────────────────────────────────────────────────────────

pub(crate) fn render_pimble_toolbar(__scope: &mut RenderScope) -> NodeHandle {
    let groups = button_groups();
    let count = groups.iter().map(Vec::len).sum();
    BUTTON_ACTIVE.with(|b| *b.borrow_mut() = (0..count).map(|_| Signal::new(false)).collect());
    refresh_active_bits();
    watch_toolbar();

    let toolbar = rsx! {
        div {
            class: "editor-toolbar",
            style: "display: flex; flex-wrap: wrap; gap: 6px; align-items: center; \
                    padding: 6px 12px;",
        }
    };

    // Each button's index into `BUTTON_ACTIVE`, counted the way `fingerprint`
    // flattens the groups.
    let mut bit = 0u32;
    for (gi, group) in groups.into_iter().enumerate() {
        // Divider between groups
        if gi > 0 {
            let divider = rsx! {
                div {
                    style: "width: 1px; height: 20px; background: var(--rinch-color-dark-4); margin: 0 2px;",
                }
            };
            toolbar.append_child(&divider);
        }

        for btn_def in group {
            let btn = render_btn(__scope, bit, btn_def);
            toolbar.append_child(&btn);
            bit += 1;
        }
    }

    toolbar
}

fn render_btn(
    __scope: &mut RenderScope,
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
                move || execute_cmd(&cmd)
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

        let h = editor();
        assert!(h.load_html("<p><strong>bold</strong> plain</p>"));
        h.set_selection(Selection::cursor(Pos(8)));

        let count: usize = button_groups().iter().map(Vec::len).sum();
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

        refresh_active_bits();
        refresh_active_bits();
        assert_eq!(snapshot(), before, "nothing changed, so nothing is notified");

        h.set_selection(Selection::cursor(Pos(3)));
        refresh_active_bits();
        let after = snapshot();
        let bold = 0; // the first button of the first group
        assert!(signals[bold].get(), "the caret is in bold text");
        for i in 0..count {
            let expected = before[i] + u32::from(i == bold);
            assert_eq!(after[i], expected, "button {i}");
        }
    }
}

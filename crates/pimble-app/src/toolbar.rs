//! Reactive editor toolbar for Pimble.
//!
//! Built with proper rinch patterns (Signals, reactive closures) so that
//! button active-state and dropdowns update correctly — unlike the upstream
//! `render_toolbar` which renders once and never updates.

use std::cell::Cell;

use rinch::prelude::*;
use rinch_tabler_icons::{TablerIcon, TablerIconStyle, render_tabler_icon};

use crate::editor::editor;
use crate::rinch_editor::EditorHandle;

/// A Signal bumped whenever the toolbar's active states may have changed, so
/// the per-button style closures re-evaluate.
static TOOLBAR_VERSION: std::sync::OnceLock<Signal<u32>> = std::sync::OnceLock::new();

pub(crate) fn toolbar_version() -> Signal<u32> {
    *TOOLBAR_VERSION.get_or_init(|| Signal::new(0))
}

thread_local! {
    /// The last active-state fingerprint the watcher saw (see `watch_toolbar`).
    static LAST_FINGERPRINT: Cell<u64> = const { Cell::new(u64::MAX) };
    /// Whether the self-rescheduling watcher is running.
    static WATCHING: Cell<bool> = const { Cell::new(false) };
}

/// How often the watcher re-reads the editor state. Cursor moves fire no
/// editor callback, so this is what keeps the buttons honest while the
/// caret travels; a handful of RefCell reads per tick, nothing more.
const WATCH_INTERVAL_MS: u32 = 120;

/// Refresh the toolbar now. Deferred via `run_on_main_thread` so it never
/// runs while the editor's RefCell is still mutably borrowed by the
/// operation that triggered it (a command, a change callback).
pub(crate) fn bump_toolbar() {
    run_on_main_thread(|| {
        LAST_FINGERPRINT.with(|f| f.set(u64::MAX));
        toolbar_version().update(|v| *v = v.wrapping_add(1));
    });
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
        let now = fingerprint();
        let changed = LAST_FINGERPRINT.with(|f| {
            let changed = f.get() != now;
            f.set(now);
            changed
        });
        if changed {
            toolbar_version().update(|v| *v = v.wrapping_add(1));
        }
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

fn check_active(check: &ActiveCheck) -> bool {
    check_active_with(&editor(), check)
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
    let version = toolbar_version();
    let groups = button_groups();
    watch_toolbar();

    let toolbar = rsx! {
        div {
            class: "editor-toolbar",
            style: "display: flex; flex-wrap: wrap; gap: 6px; align-items: center; \
                    padding: 6px 12px;",
        }
    };

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
            let btn = render_btn(__scope, version, btn_def);
            toolbar.append_child(&btn);
        }
    }

    toolbar
}

fn render_btn(
    __scope: &mut RenderScope,
    version: Signal<u32>,
    def: BtnDef,
) -> NodeHandle {
    let check = def.active_check; // Copy
    let cmd = def.cmd;
    let icon_el = render_tabler_icon(__scope, def.icon, TablerIconStyle::Outline);

    rsx! {
        div {
            title: def.tooltip,
            style: {
                move || {
                    let _ = version.get();
                    btn_style(check_active(&check))
                }
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

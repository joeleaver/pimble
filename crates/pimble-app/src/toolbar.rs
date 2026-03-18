//! Reactive editor toolbar for Pimble.
//!
//! Built with proper rinch patterns (Signals, reactive closures) so that
//! button active-state and dropdowns update correctly — unlike the upstream
//! `render_toolbar` which renders once and never updates.

use rinch::prelude::*;
use rinch::core::ce::with_active_ce_api;
use rinch_tabler_icons::{TablerIcon, TablerIconStyle, render_tabler_icon};

/// A Signal bumped on every CE change/selection event so toolbar closures re-evaluate.
static TOOLBAR_VERSION: std::sync::OnceLock<Signal<u32>> = std::sync::OnceLock::new();

pub(crate) fn toolbar_version() -> Signal<u32> {
    *TOOLBAR_VERSION.get_or_init(|| Signal::new(0))
}

/// Call this after any CE mutation or cursor change to refresh toolbar state.
/// Deferred via run_on_main_thread so it doesn't fire while the CE API's
/// RefCell is still mutably borrowed by the operation that triggered the event.
pub(crate) fn bump_toolbar() {
    rinch::run_on_main_thread(|| {
        toolbar_version().update(|v| *v = v.wrapping_add(1));
    });
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
        // Lists & blocks
        vec![
            BtnDef { icon: TablerIcon::List, tooltip: "Bullet List", cmd: Cmd::SetBlock("ul"), active_check: ActiveCheck::Block("ul") },
            BtnDef { icon: TablerIcon::ListNumbers, tooltip: "Ordered List", cmd: Cmd::SetBlock("ol"), active_check: ActiveCheck::Block("ol") },
            BtnDef { icon: TablerIcon::Blockquote, tooltip: "Blockquote", cmd: Cmd::SetBlock("blockquote"), active_check: ActiveCheck::Block("blockquote") },
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

fn check_active(check: &ActiveCheck) -> bool {
    match check {
        ActiveCheck::Mark(tag) => {
            with_active_ce_api(|api| api.borrow().has_active_mark(tag)).unwrap_or(false)
        }
        ActiveCheck::Block(tag) => {
            let cur = with_active_ce_api(|api| api.borrow().cursor_block_tag()).flatten();
            cur.as_deref() == Some(*tag)
        }
        ActiveCheck::None => false,
    }
}

fn execute_cmd(cmd: &Cmd) {
    with_active_ce_api(|api| {
        let mut api = api.borrow_mut();
        match cmd {
            Cmd::ToggleWrap(tag) => api.toggle_wrap(tag),
            Cmd::SetBlock(tag) => api.set_block_type(tag),
            Cmd::HorizontalRule => {
                api.split_block();
                api.set_block_type("hr");
            }
            Cmd::Undo => api.undo(),
            Cmd::Redo => api.redo(),
            Cmd::ClearFormatting => api.clear_formatting(),
        }
    });
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

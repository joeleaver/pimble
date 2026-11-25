//! The Theme picker's preview page (docs/THEMES_CONTRACT.md section 9): a
//! note's blocks (`themes::preview`) drawn with the elements rinch's editor
//! draws them with, under an element carrying `data-pm-editor`, so rinch's
//! editor sheet and `document_theme.css` dress it exactly as they dress the
//! note in a pane. It takes no input: it is a picture of the note.
//!
//! Each element is made with `rsx!`, and children are appended to their
//! parent directly rather than embedded as a list: an embedded list arrives
//! inside a `display: contents` box, which the theme rules' `p + p` and
//! `[data-pm-editor] > p` selectors would see as a parent of its own, so the
//! preview would indent, space and measure differently from the note.

use rinch::core::dom::IntoNode;
use rinch::prelude::*;

use crate::state::AppStore;
use crate::themes::preview::{Block, Cell, Inline, Run, Script, MAX_PLACED_COLUMNS};

/// The page: the editor's element, in the app's scheme, holding `blocks`.
pub(super) fn page(__scope: &mut RenderScope, store: AppStore, blocks: &[Block]) -> NodeHandle {
    page_in(__scope, store.dark_mode, blocks)
}

/// The page in the scheme `dark` says (the theme editor's switch).
pub(super) fn page_in(__scope: &mut RenderScope, dark: Signal<bool>, blocks: &[Block]) -> NodeHandle {
    let page = rsx! {
        div {
            "data-pm-editor": "",
            "data-pm-theme": {move || if dark.get() { "dark" } else { "light" }},
        }
    };
    for b in blocks {
        let child = block(__scope, b);
        page.append_child(&child);
    }
    page
}

/// Appends `children` to `parent` and answers it.
fn fill(parent: NodeHandle, children: Vec<NodeHandle>) -> NodeHandle {
    for child in &children {
        parent.append_child(child);
    }
    parent
}

fn blocks(__scope: &mut RenderScope, list: &[Block]) -> Vec<NodeHandle> {
    list.iter().map(|b| block(__scope, b)).collect()
}

/// A textblock's own alignment as a style, as the editor writes it.
fn align_style(align: Option<&'static str>) -> String {
    align.map(|a| format!("text-align: {a};")).unwrap_or_default()
}

fn block(__scope: &mut RenderScope, b: &Block) -> NodeHandle {
    match b {
        Block::Paragraph { align, inlines } => {
            let style = align_style(*align);
            let el = rsx! { p { style: style } };
            let kids = inline_nodes(__scope, inlines);
            fill(el, kids)
        }
        Block::Heading { level, align, inlines } => {
            let style = align_style(*align);
            let el = match level {
                1 => rsx! { h1 { style: style } },
                2 => rsx! { h2 { style: style } },
                3 => rsx! { h3 { style: style } },
                4 => rsx! { h4 { style: style } },
                5 => rsx! { h5 { style: style } },
                _ => rsx! { h6 { style: style } },
            };
            let kids = inline_nodes(__scope, inlines);
            fill(el, kids)
        }
        Block::List { ordered, start, items } => {
            let el = match (ordered, *start) {
                (true, 1) => rsx! { ol {} },
                (true, n) => rsx! { ol { start: n.to_string() } },
                (false, _) => rsx! { ul {} },
            };
            let kids = items
                .iter()
                .map(|item| {
                    let li = rsx! { li {} };
                    let inner = blocks(__scope, item);
                    fill(li, inner)
                })
                .collect();
            fill(el, kids)
        }
        Block::Quote(inner) => {
            let el = rsx! { blockquote {} };
            let kids = blocks(__scope, inner);
            fill(el, kids)
        }
        Block::Code(text) => {
            let text = text.clone();
            rsx! { pre { {text} } }
        }
        Block::Table { columns, rows } => {
            // The editor's table is a grid whose column count it writes
            // inline (rinch-editor-view, `table`), its rows `display:
            // contents`, so the cells are the grid's items.
            let style = format!("grid-template-columns: repeat({columns}, minmax(0, 1fr));");
            let el = rsx! { table { "data-pm-type": "table", style: style } };
            let placed = *columns <= MAX_PLACED_COLUMNS;
            let kids = rows
                .iter()
                .map(|row| {
                    let tr = rsx! { tr {} };
                    let cells = row.iter().map(|c| cell(__scope, c, placed)).collect();
                    fill(tr, cells)
                })
                .collect();
            fill(el, kids)
        }
        Block::Rule => rsx! { hr {} },
    }
}

/// A cell on its rectangle in the table's grid, as the editor places it
/// (`grid-column`/`grid-row` lines), when the grid is narrow enough to place.
fn cell(__scope: &mut RenderScope, c: &Cell, placed: bool) -> NodeHandle {
    let style = match (placed, c.rect) {
        (true, Some(r)) => format!("grid-column: {} / {}; grid-row: {} / {};", r.left + 1, r.right + 1, r.top + 1, r.bottom + 1),
        _ => String::new(),
    };
    let el = if c.header { rsx! { th { style: style } } } else { rsx! { td { style: style } } };
    let kids = blocks(__scope, &c.blocks);
    fill(el, kids)
}

fn inline_nodes(__scope: &mut RenderScope, inlines: &[Inline]) -> Vec<NodeHandle> {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Text(r) => run(__scope, r),
            Inline::Break => rsx! { br {} },
        })
        .collect()
}

/// A run of text inside the elements its marks draw as, innermost first,
/// in the order the editor nests them. A link draws as a link and goes
/// nowhere: the preview is a picture.
fn run(__scope: &mut RenderScope, r: &Run) -> NodeHandle {
    let mut node = r.text.clone().into_node(__scope);
    let wrap = |outer: NodeHandle, node: &mut NodeHandle| {
        outer.append_child(node);
        *node = outer;
    };
    if r.code {
        wrap(rsx! { code {} }, &mut node);
    }
    match r.script {
        Script::Sub => wrap(rsx! { sub {} }, &mut node),
        Script::Super => wrap(rsx! { sup {} }, &mut node),
        Script::Normal => {}
    }
    if r.strike {
        wrap(rsx! { s {} }, &mut node);
    }
    if r.underline {
        wrap(rsx! { u {} }, &mut node);
    }
    if r.italic {
        wrap(rsx! { em {} }, &mut node);
    }
    if r.bold {
        wrap(rsx! { strong {} }, &mut node);
    }
    if let Some(color) = &r.color {
        let style = format!("color: {color};");
        wrap(rsx! { span { style: style } }, &mut node);
    }
    match &r.highlight {
        Some(Some(color)) => {
            let style = format!("background: {color};");
            wrap(rsx! { mark { style: style } }, &mut node)
        }
        Some(None) => wrap(rsx! { mark {} }, &mut node),
        None => {}
    }
    if r.link {
        wrap(rsx! { a {} }, &mut node);
    }
    node
}

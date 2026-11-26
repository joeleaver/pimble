//! The rows: the sortable list and each row's chevron, checkbox, icon, title,
//! progress, due date, people and trash can.

use pimble_plugin_sdk::pimble_ui::appearance::{display_color, row_background, IconGlyph};
use pimble_plugin_sdk::pimble_ui::presence::{more_label, PersonChip};
use pimble_plugin_sdk::pimble_ui::viewer as look;
use pimble_plugin_sdk::prelude::*;

use crate::outline::{key_of, Item};
use crate::{dom_id, due, Todo, INDENT, ITEMS};

const SLOT: &str = "display: inline-flex; width: 16px; height: 16px; flex-shrink: 0; margin-top: 2px;";

/// Who else is on `addr`: the app's own chips, up to three and "+n".
fn people(__scope: &mut RenderScope, t: &Todo, addr: Addr) -> NodeHandle {
    let (seen, follow) = (t.host.seen(), t.host.clone());
    rsx! {
        span {
            class: "pimble-pane__people",
            style: "display: inline-flex; align-items: center;",
            for person in seen.people_in(addr).into_iter().take(3) {
                PersonChip {
                    key: person.key.clone(),
                    person: person.clone(),
                    onfollow: { let f = follow.clone(); move |key: String| f.follow(&key) },
                }
            }
            span {
                class: "pimble-pane__more",
                style: {move || if seen.people_in(addr).len() > 3 { "" } else { "display: none;" }},
                {move || more_label(seen.people_in(addr).len().saturating_sub(3))}
            }
        }
    }
}

/// The rows, as one sortable list: a row drags whole (a press that never
/// moves 5 px stays a click), and a drop lands where [`drop_target`] says.
pub(crate) fn render_list(__scope: &mut RenderScope, t: &Todo, writable: bool) -> NodeHandle {
    let (td, tr) = (t.clone(), t.clone());
    let judge: SortJudge = {
        let t = t.clone();
        std::rc::Rc::new(move |m: &SortMove| t.judge_drop(m))
    };
    rsx! {
        SortableList {
            group: ITEMS,
            id: "todo".to_string(),
            label: "Todo list".to_string(),
            class: "todo__rows",
            style: format!("display: flex; flex-direction: column; gap: 4px; position: relative;{}", look::SORT_THEME),
            disabled: !writable,
            // A focused row lifts with its lift key, and the arrows carry it.
            keyboard: true,
            // Space checks an item off and Enter opens it, so neither lifts a
            // row; the keyboard moves rows with Alt+Shift+arrows and Tab.
            lift_keys: vec!["Insert".to_string()],
            indicator: SortIndicator::Gap,
            source: SortSource::Collapse,
            preview: SortPreview::Snapshot,
            judge: Some(judge),
            ondrop: move |m: SortMove| td.drop_move(m),
            for (key, item) in tr.shown() {
                SortableItem {
                    key: key.clone(),
                    value: key_of(item.addr),
                    disabled: !item.writable,
                    label: item.label.clone(),
                    class: "todo__row",
                    {render_item(__scope, &tr, item.clone())}
                }
            }
        }
    }
}

/// One row: its fold chevron, checkbox, icon, title, progress, due date, who
/// is on it, and its trash can.
fn render_item(__scope: &mut RenderScope, t: &Todo, item: Item) -> NodeHandle {
    let addr = item.addr;
    let seen = t.host.seen();
    let (tc, tf, tchk, tr, tfield, tfield2, td, tenter, tleave) = (t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone(), t.clone());
    let (chosen, renaming, hovered, look_now) = (t.chosen, t.renaming, t.hovered, t.look);
    let writable = item.writable;
    let done = item.done.is_some();
    let has_children = !item.children.is_empty();
    let (done_now, all) = untracked(|| t.outline.with(|o| o.progress(addr)));
    let indent = (item.depth.saturating_sub(1)) * INDENT;
    let (color, color2, background) = (item.color.clone(), item.color.clone(), item.background.clone());
    let show_icon = item.icon != look::PLAIN_ICON;
    let icon = item.icon.clone();
    let label = item.label.clone();
    let due = item.due.as_deref().map(|typed| due::due_of(typed, due::today(), done));
    let done_title = item.done.as_ref().map(|when| format!("Done {when}")).unwrap_or_else(|| if writable { "Check off".to_string() } else { String::new() });
    rsx! {
        div {
            class: "todo__item",
            id: dom_id(addr),
            style: {|| {
                let leader = seen.following().and_then(|key| seen.people_in(addr).into_iter().find(|p| p.key == key).map(|p| p.color));
                format!(
                    "display: flex; align-items: flex-start; gap: 6px; margin-left: {indent}px; font-size: 14px; line-height: 1.45; {}",
                    look::item_style(chosen.get() == Some(addr), background.as_deref().map(|b| row_background(b, seen.dark())), leader, (6, 10)),
                )
            }},
            onclick: move || tc.click_item(addr, rinch::core::get_modifier_state()),
            onmouseenter: move || tenter.hovered.set(Some(addr)),
            onmouseleave: move || {
                if untracked(|| tleave.hovered.get()) == Some(addr) {
                    tleave.hovered.set(None);
                }
            },
            // A grip says the row drags (the whole row does).
            if writable {
                span {
                    class: "todo__grip",
                    style: format!("{SLOT} margin-left: -4px; color: var(--rinch-color-dimmed); opacity: 0.6;"),
                    IconGlyph { name: "grip-vertical".to_string() }
                }
            }
            // The fold chevron's slot is always there, so titles line up.
            span {
                class: "todo__fold",
                style: format!("{SLOT} color: var(--rinch-color-dimmed);{}", if has_children { " cursor: pointer;" } else { "" }),
                title: if has_children { "Fold or unfold its sub-todos" } else { "" },
                onclick: move || if has_children { tf.toggle_fold(addr) },
                if has_children {
                    IconGlyph { name: if look_now.with(|l| l.folded.contains(&key_of(addr))) { "chevron-right".to_string() } else { "chevron-down".to_string() } }
                }
            }
            span {
                class: {if done { "todo__check todo__check--done" } else { "todo__check" }},
                style: format!("{SLOT}{}{}", if done { " color: var(--rinch-primary-color);" } else { " color: var(--rinch-color-dimmed);" }, if writable { " cursor: pointer;" } else { "" }),
                title: done_title,
                onclick: move || tchk.toggle_done(addr),
                IconGlyph { name: if done { "square-check".to_string() } else { "square".to_string() } }
            }
            if show_icon {
                span {
                    style: {let color = color.clone(); move || format!("{SLOT}{}", color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default())},
                    IconGlyph { name: icon.clone() }
                }
            }
            if renaming.get() == Some(addr) {
                input {
                    class: "todo__field",
                    style: look::FIELD_STYLE,
                    ref: tfield.field,
                    placeholder: "Item name",
                    value: {move || tfield.draft.get()},
                    oninput: move |v: String| tfield2.draft.set(v),
                    onkeydown: {
                        let t = tr.clone();
                        move |key: &KeyEventData| match key.key.as_str() {
                            "Enter" => { t.commit_rename(true); true }
                            "Escape" => { t.cancel_rename(); true }
                            _ => false,
                        }
                    },
                }
            } else {
                span {
                    class: "todo__label",
                    style: {
                        let color = color2.clone();
                        move || format!(
                            "flex: 1; overflow-wrap: anywhere;{}{}",
                            if done { " text-decoration: line-through; color: var(--rinch-color-dimmed);" } else { "" },
                            match (&color, done) { (Some(c), false) => format!(" color: {};", display_color(c, seen.dark())), _ => String::new() },
                        )
                    },
                    {label.clone()}
                }
            }
            if all > 0 {
                span { class: "todo__progress", style: format!("{} margin-top: 2px;", look::LINE_STYLE), {format!("{done_now}/{all}")} }
            }
            if let Some(due) = due.clone() {
                span {
                    class: if due.overdue { "todo__due todo__due--overdue" } else { "todo__due" },
                    style: format!("{} margin-top: 2px; white-space: nowrap;{}", look::LINE_STYLE, if due.overdue { " color: var(--rinch-color-red-6);" } else { "" }),
                    title: due.title.clone(),
                    {due.text.clone()}
                }
            }
            {people(__scope, t, addr)}
            if writable {
                span {
                    class: "todo__trash-slot",
                    style: SLOT,
                    if (hovered.get() == Some(addr) || chosen.get() == Some(addr)) && renaming.get() != Some(addr) {
                        span {
                            class: "todo__trash",
                            title: "Delete (View > Recently Deleted puts it back)",
                            style: "display: inline-flex; width: 16px; height: 16px; cursor: pointer; color: var(--rinch-color-dimmed);",
                            onclick: { let t = td.clone(); move || t.delete(addr) },
                            IconGlyph { name: "trash".to_string() }
                        }
                    }
                }
            }
        }
    }
}

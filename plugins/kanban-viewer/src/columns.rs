//! Columns: the header rows (dragged by their headers) and each lane's
//! cells, a column's cards as a sortable list.

use super::*;

/// A lane's columns as the board has them now, tracked, each with whether it
/// is folded.
pub(crate) fn lane_columns(k: &Kanban, lane: Addr) -> Vec<(board::Column, bool)> {
    let columns = k.board.with(|b| b.lanes.iter().find(|l| l.metanode == lane).map(|l| l.columns.clone()).unwrap_or_default());
    columns.into_iter().map(|c| {
        let folded = k.folded(&c.label);
        (c, folded)
    }).collect()
}

/// A header's columns, tracked: a lane's own (`lane`), or on an aligned board
/// the first lane's, each with its cards counted in every lane, and folded.
pub(crate) fn header_columns(k: &Kanban, lane: Option<Addr>) -> Vec<(board::Column, usize, bool)> {
    let columns: Vec<(board::Column, usize)> = k.board.with(|b| match lane {
        Some(lane) => b.lanes.iter().find(|l| l.metanode == lane).map(|l| l.columns.iter().map(|c| (c.clone(), c.cards.len())).collect()).unwrap_or_default(),
        None => b.lanes.first().map(|l| {
            l.columns.iter().enumerate().map(|(i, c)| (c.clone(), b.across(i).iter().map(|col| col.cards.len()).sum())).collect()
        }).unwrap_or_default(),
    });
    columns.into_iter().map(|(c, n)| {
        let folded = k.folded(&c.label);
        (c, n, folded)
    }).collect()
}

/// A row of column headers: the board's (`lane` `None`: one for every lane,
/// after a space the width of the lane names when there are any) or a lane's
/// own. A horizontal sortable list dragged by the headers, then "+ Add
/// column".
pub(crate) fn header_row(__scope: &mut RenderScope, k: &Kanban, lane: Option<Lane>, judge: SortJudge) -> NodeHandle {
    let (kd, kc, ka, kt) = (k.clone(), k.clone(), k.clone(), k.clone());
    let board = untracked(|| k.board.get());
    let first = board.lanes.first().cloned();
    let Some(owner) = lane.clone().or(first) else { return rsx! { span { style: "display: none;" } } };
    let spacer = lane.is_none() && board.lanes.iter().any(|l| l.node.is_some());
    let writable = owner.writable;
    let (list_lane, list_label) = (owner.metanode, owner.label.clone().unwrap_or_else(|| "Columns".to_string()));
    let shared = lane.is_none() && board.lanes.len() > 1;
    rsx! {
        div {
            class: "kanban__header-row",
            style: HEADER_ROW_STYLE,
            // The corner above the lane names holds the lane tools.
            if spacer {
                div {
                    class: "kanban__corner",
                    style: format!("width: {LABEL_W}px; flex-shrink: 0; display: flex; gap: 2px; align-items: center; padding: 2px 0;"),
                    if shared {
                        {lane_tools(__scope, &kt)}
                    }
                }
            }
            SortableList {
                group: COLUMNS,
                id: key_of(list_lane),
                axis: SortAxis::Horizontal,
                label: list_label,
                class: "kanban__column-list",
                style: format!("{COLUMN_LIST_STYLE}{SORT_THEME}"),
                disabled: !writable,
                lift_keys: lift_keys(),
                indicator: SortIndicator::Gap,
                source: SortSource::Collapse,
                // The app's own picture of the item under the pointer: a copy of
                // a plugin's elements does not keep its look across the host.
                preview: SortPreview::Snapshot,
                judge: Some(judge.clone()),
                ondrop: move |m: SortMove| kd.drop_move(m),
                for (column, count, folded) in header_columns(&kc, lane.as_ref().map(|l| l.metanode)) {
                    SortableItem {
                        key: format!("{column:?} {count} {folded}"),
                        value: key_of(column.addr),
                        handle: true,
                        disabled: folded || !(column.writable && writable),
                        label: column.label.clone(),
                        class: "kanban__column",
                        style: format!("width: {}px; flex-shrink: 0; position: relative;", if folded { FOLDED_W } else { COLUMN_W }),
                        {column_header(__scope, &kc, column.clone(), count, folded, writable)}
                    }
                }
            }
            if writable {
                div {
                    class: "kanban__add-column",
                    style: "flex-shrink: 0; padding: 4px 8px; font-size: 13px; color: var(--rinch-color-dimmed); cursor: pointer; white-space: nowrap;",
                    onclick: {
                        let k = ka.clone();
                        move || {
                            // On a shared header, a new column goes in every lane.
                            let lanes: Vec<Addr> = if shared { untracked(|| k.board.get()).lanes.iter().map(|l| l.metanode).collect() } else { vec![list_lane] };
                            k.add_column(lanes);
                        }
                    },
                    "+ Add column"
                }
            }
        }
    }
}

/// A column's header: its name (renamed in place; on a shared header, in
/// every lane), its count, its people, fold; what a column is dragged by. A
/// folded column's header is a narrow "+" that unfolds it.
pub(crate) fn column_header(__scope: &mut RenderScope, k: &Kanban, column: board::Column, count: usize, folded: bool, board_writable: bool) -> NodeHandle {
    let addr = column.addr;
    let name = column.label.clone();
    if folded {
        let kf = k.clone();
        let tip = format!("Unfold {name} ({count})");
        return rsx! {
            div {
                class: "kanban__header kanban__header--folded",
                style: format!("{ICON_BUTTON} width: {FOLDED_W}px; height: 28px; {CELL_BG}"),
                title: tip,
                onclick: move || kf.toggle_fold(&name),
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "plus".to_string() } }
            }
        };
    }
    let seen = k.host.seen();
    let color = column.color.clone();
    let (kc, kr, kf, kf2, kfold) = (k.clone(), k.clone(), k.clone(), k.clone(), k.clone());
    let (label, label2) = (column.label.clone(), column.label.clone());
    let writable = column.writable;
    let (chosen, renaming) = (k.chosen, k.renaming);
    let header = rsx! {
        div {
            class: "kanban__header",
            id: dom_id(addr),
            style: {|| format!(
                "{HEADER_STYLE}{}{}",
                color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default(),
                if chosen.get() == Some(Chosen::Column(addr)) { " box-shadow: inset 0 0 0 2px var(--rinch-primary-color);" } else { "" },
            )},
            onclick: move || {
                let again = clicked_again(addr);
                kc.choose(Some(Chosen::Column(addr)));
                if untracked(|| kc.renaming.get()) == Some(addr) {
                    return;
                }
                if again && writable {
                    kc.start_rename(addr, label2.clone());
                } else {
                    kc.take_keyboard();
                }
            },
            if renaming.get() == Some(addr) {
                input {
                    class: "kanban__field",
                    style: FIELD_STYLE,
                    ref: kf.field,
                    placeholder: "Column name",
                    value: {move || kf.draft.get()},
                    oninput: move |v: String| kf2.draft.set(v),
                    onkeydown: {
                        let k = kr.clone();
                        move |key: &KeyEventData| match key.key.as_str() {
                            "Enter" => { k.commit_rename(); true }
                            "Escape" => { k.cancel_rename(); true }
                            _ => false,
                        }
                    },
                }
            } else {
                span {
                    style: "overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                    title: if writable { "Double-click or F2 to rename" } else { "" },
                    {label.clone()}
                }
                span { style: LINE_STYLE, {count.to_string()} }
            }
            {people(__scope, k, addr)}
            span {
                style: ICON_BUTTON,
                title: "Fold this column",
                onclick: { let name = name.clone(); move || kfold.toggle_fold(&name) },
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "minus".to_string() } }
            }
        }
    };
    // A column is dragged by its header, when the person may move it.
    if board_writable && writable { rsx! { SortableHandle { {header} } } } else { header }
}

/// A lane's cells, one per column, stretched to the tallest: each grows with
/// its cards (the board scrolls), and a shorter one's empty foot is still its
/// list, a place to drop.
pub(crate) fn cells_row(__scope: &mut RenderScope, k: &Kanban, shown: &LaneShown, judge: SortJudge) -> NodeHandle {
    let kc = k.clone();
    let lane = shown.lane.metanode;
    let (size, writable) = (shown.size, shown.lane.writable);
    rsx! {
        div {
            class: "kanban__cells",
            style: ROW_STYLE,
            for (column, folded) in lane_columns(&kc, lane) {
                div {
                    key: format!("{column:?} {folded}"),
                    style: "display: flex; flex-shrink: 0;",
                    {cell(__scope, &kc, column.clone(), folded, size, writable, judge.clone())}
                }
            }
        }
    }
}

/// One column of one lane: its cards (a sortable list of their own), and on
/// hover, "Add card". The list is the whole cell, padding and foot included,
/// so a drag anywhere over a column (between cards, below the last, in the
/// empty foot of a shorter column) is over its list; a drag over anything
/// else in the cell would find no list and offer no place. It grows with its
/// cards; only a minimized lane caps it, at about three cards, and scrolls.
/// A folded column is a narrow strip with its name and count.
pub(crate) fn cell(__scope: &mut RenderScope, k: &Kanban, column: board::Column, folded: bool, size: Size, lane_writable: bool, judge: SortJudge) -> NodeHandle {
    let addr = column.addr;
    let name = column.label.clone();
    if folded {
        let kf = k.clone();
        let (tip, text) = (format!("Unfold {name}"), format!("{name} ({})", column.cards.len()));
        return rsx! {
            div {
                class: "kanban__cell kanban__cell--folded",
                style: format!("width: {FOLDED_W}px; min-height: 64px; position: relative; cursor: pointer; overflow: hidden; {CELL_BG}"),
                title: tip,
                onclick: move || kf.toggle_fold(&name),
                span {
                    style: "position: absolute; top: 10px; left: 30px; transform: rotate(90deg); transform-origin: top left; white-space: nowrap; font-size: 12px; font-weight: 600; color: var(--rinch-color-dimmed);",
                    {text}
                }
            }
        };
    }
    let cards: Vec<NodeHandle> = column.cards.iter().map(|card| render_card(__scope, k, card.clone())).collect();
    let can_add = column.writable;
    let (kd, ka, kenter, kleave) = (k.clone(), k.clone(), k.clone(), k.clone());
    let hovered = k.hovered;
    rsx! {
        div {
            class: "kanban__cell",
            data-kanban-column: key_of(addr),
            style: cell_style(size),
            onmouseenter: move || kenter.hovered.set(Some(addr)),
            onmouseleave: move || {
                if untracked(|| kleave.hovered.get()) == Some(addr) {
                    kleave.hovered.set(None);
                }
            },
            SortableList {
                group: CARDS,
                id: key_of(addr),
                label: column.label.clone(),
                class: "kanban__cards",
                style: format!("{}{SORT_THEME}", card_list_style(size)),
                // A read-only board takes no drags; a read-only column on a
                // writable board still judges a drop into it, so the caret says
                // why not ("You can read this, not change it.").
                disabled: !lane_writable,
                lift_keys: lift_keys(),
                // Trello's feel (Joe, 2026-11-04): the card follows the
                // pointer, and the list opens a gap where it lands.
                indicator: SortIndicator::Gap,
                source: SortSource::Collapse,
                // The app's own picture of the item under the pointer: a copy of
                // a plugin's elements does not keep its look across the host.
                preview: SortPreview::Snapshot,
                judge: Some(judge),
                ondrop: move |m: SortMove| kd.drop_move(m),
                {cards}
            }
            // Not while a card is carried: the button is the cell's, not the
            // list's, and a drag over it would find no place to drop.
            if can_add && hovered.get() == Some(addr) && sortable_drag().is_none() {
                div {
                    class: "kanban__add-card",
                    style: "position: absolute; left: 50%; bottom: 6px; transform: translateX(-50%); display: flex; align-items: center; gap: 4px; padding: 3px 10px; font-size: 12px; border-radius: 4px; cursor: pointer; white-space: nowrap; color: var(--rinch-color-body); background: var(--rinch-color-text); box-shadow: 0 1px 3px rgba(0,0,0,0.25);",
                    onclick: { let k = ka.clone(); move || k.add(addr, true) },
                    span { style: "display: inline-flex; width: 12px; height: 12px;", IconGlyph { name: "plus".to_string() } }
                    "Add card"
                }
            }
        }
    }
}

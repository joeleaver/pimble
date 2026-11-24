//! Lanes: a kanban child that is itself a kanban, drawn below as a row
//! of its own, with its name, count and size.

use super::*;

/// A lane as the board draws it now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LaneShown {
    /// The lane, without its columns.
    pub(crate) lane: Lane,
    pub(crate) size: Size,
    /// The board shares one header row.
    pub(crate) aligned: bool,
    /// The board has a column of lane names (it has named lanes).
    pub(crate) laned: bool,
}

impl LaneShown {
    /// What redraws the lane: never its cards, which its cells follow on
    /// their own (a card redrawn mid-drag would lose the drag).
    pub(crate) fn key(&self) -> String {
        let l = &self.lane;
        format!("{:?} {:?} {:?} {:?} {} {:?} {} {}", l.metanode, l.node, l.label, l.color, l.writable, self.size, self.aligned, self.laned)
    }
}

/// How many cards lane `lane` has, tracked.
pub(crate) fn lane_count(board: Signal<Board>, lane: Addr) -> usize {
    board.with(|b| b.lanes.iter().find(|l| l.metanode == lane).map_or(0, |l| l.columns.iter().map(|c| c.cards.len()).sum()))
}

/// The lanes as the board draws them, tracked: the board and the look.
pub(crate) fn lanes_shown(board: Signal<Board>, _look: Signal<Look>, k: &Kanban) -> Vec<LaneShown> {
    board.with(|b| {
        let (aligned, laned) = (b.aligned(), b.lanes.iter().any(|l| l.node.is_some()));
        // Without its columns: a `for` row is drawn again when its item
        // changes, and a lane's cells follow the board on their own.
        b.lanes.iter().map(|lane| LaneShown { lane: Lane { columns: Vec::new(), ..lane.clone() }, size: k.size_of(lane, b.lanes.len()), aligned, laned }).collect()
    })
}

/// What a board with lanes offers for all of them, as icons: every lane shown
/// whole, and each back to its own size.
pub(crate) fn lane_tools(__scope: &mut RenderScope, k: &Kanban) -> NodeHandle {
    let (ke, kr) = (k.clone(), k.clone());
    rsx! {
        span {
            style: "display: inline-flex; gap: 2px; align-items: center;",
            span {
                class: "kanban__expand-all",
                style: ICON_BUTTON,
                title: "Show every lane whole",
                onclick: move || {
                    let lanes = untracked(|| ke.board.get()).lanes;
                    ke.relook(|l| {
                        for lane in &lanes {
                            l.lanes.insert(lane_key(lane), Size::Expanded);
                        }
                    });
                },
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "chevrons-down".to_string() } }
            }
            span {
                class: "kanban__reset-lanes",
                style: ICON_BUTTON,
                title: "Put every lane back to its own size",
                onclick: move || kr.relook(|l| l.lanes.clear()),
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "restore".to_string() } }
            }
        }
    }
}

/// One lane: a bar with its name and count when collapsed; otherwise its
/// name's cell (on a board with named lanes), its own header row when the
/// board's lanes differ, and a cell per column.
pub(crate) fn render_lane(__scope: &mut RenderScope, k: &Kanban, shown: LaneShown, judge: SortJudge) -> NodeHandle {
    let lane = shown.lane.clone();
    let (board, metanode) = (k.board, lane.metanode);
    if shown.size == Size::Collapsed {
        let ke = k.clone();
        let l2 = lane.clone();
        let name = lane.label.clone().unwrap_or_else(|| "Other cards".to_string());
        return rsx! {
            div {
                class: "kanban__lane-bar",
                style: format!("display: flex; align-items: center; gap: 8px; padding: 6px 10px; cursor: pointer; font-weight: 600; font-size: 13px; {CELL_BG}"),
                title: "Show this lane",
                onclick: move || ke.set_size(&l2, Size::Minimized),
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "plus".to_string() } }
                span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "layout-kanban".to_string() } }
                span { {name} }
                span { style: LINE_STYLE, {move || format!("({})", lane_count(board, metanode))} }
            }
        };
    }
    let label = if shown.laned { Some(lane_label(__scope, k, &shown)) } else { None };
    let header = if shown.aligned { None } else { Some(header_row(__scope, k, Some(lane.clone()), judge.clone())) };
    let cells = cells_row(__scope, k, &shown, judge);
    rsx! {
        div {
            class: "kanban__lane-row",
            style: ROW_STYLE,
            if let Some(label) = label.clone() {
                {label}
            }
            div {
                style: "display: flex; flex-direction: column; gap: 6px; min-width: 0;",
                if let Some(header) = header.clone() {
                    {header}
                }
                {cells}
            }
        }
    }
}

/// A lane's name cell: its name (the unnamed lane's is "Other cards"),
/// renamed in place, its count and people, and its size: show all or about
/// three cards a column, collapse.
pub(crate) fn lane_label(__scope: &mut RenderScope, k: &Kanban, shown: &LaneShown) -> NodeHandle {
    let lane = shown.lane.clone();
    let (board, metanode) = (k.board, lane.metanode);
    let seen = k.host.seen();
    let (kc, kr, kf, kf2, ks, kz) = (k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone());
    let (chosen, renaming) = (k.chosen, k.renaming);
    let node = lane.node;
    let color = lane.color.clone();
    let name = lane.label.clone();
    let name2 = name.clone().unwrap_or_default();
    let writable = lane.writable && node.is_some();
    let size = shown.size;
    let (l1, l2) = (lane.clone(), lane.clone());
    let people_here = node.map(|n| people(__scope, k, n));
    let (toggle_icon, toggle_tip, toggle_to) = match size {
        Size::Expanded => ("chevrons-up", "Show about three cards a column", Size::Minimized),
        _ => ("chevrons-down", "Show every card", Size::Expanded),
    };
    rsx! {
        div {
            class: "kanban__lane-label",
            id: node.map(dom_id).unwrap_or_default(),
            style: {|| format!(
                "{LANE_LABEL_STYLE} width: {LABEL_W}px;{}",
                if node.is_some() && chosen.get() == node.map(Chosen::Lane) { " box-shadow: inset 0 0 0 2px var(--rinch-primary-color); border-radius: 6px;" } else { "" },
            )},
            div {
                style: {|| format!(
                    "display: flex; align-items: center; gap: 6px; font-weight: 600; font-size: 13px; cursor: pointer; min-width: 0;{}",
                    color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default(),
                )},
                onclick: move || {
                    if let Some(node) = node {
                        let again = clicked_again(node);
                        kc.choose(Some(Chosen::Lane(node)));
                        if untracked(|| kc.renaming.get()) == Some(node) {
                            return;
                        }
                        if again && writable {
                            kc.start_rename(node, name2.clone());
                        } else {
                            kc.take_keyboard();
                        }
                    }
                },
                span { style: "display: inline-flex; width: 14px; height: 14px; flex-shrink: 0;", IconGlyph { name: "layout-kanban".to_string() } }
                if node.is_some() && renaming.get() == node {
                    input {
                        class: "kanban__field",
                        style: FIELD_STYLE,
                        ref: kf.field,
                        placeholder: "Lane name",
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
                        {name.clone().unwrap_or_else(|| "Other cards".to_string())}
                    }
                }
            }
            div {
                style: "display: flex; align-items: center; gap: 2px;",
                span { style: format!("{LINE_STYLE} margin-right: 4px;"), {move || { let n = lane_count(board, metanode); format!("{n} card{}", if n == 1 { "" } else { "s" }) }} }
                span {
                    style: ICON_BUTTON,
                    title: toggle_tip,
                    onclick: move || ks.set_size(&l1, toggle_to),
                    span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: toggle_icon.to_string() } }
                }
                span {
                    style: ICON_BUTTON,
                    title: "Collapse this lane",
                    onclick: move || kz.set_size(&l2, Size::Collapsed),
                    span { style: "display: inline-flex; width: 14px; height: 14px;", IconGlyph { name: "minus".to_string() } }
                }
            }
            if let Some(people_here) = people_here.clone() {
                {people_here}
            }
        }
    }
}

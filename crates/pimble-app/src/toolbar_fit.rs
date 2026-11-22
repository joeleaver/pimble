//! One row, at any width (docs/TOOLBAR_CONTRACT.md "One row"): the text,
//! canvas and sheet toolbars are each a row of labeled groups laid out here.
//! The row never wraps. When the groups do not fit the toolbar's measured
//! width, first every group's label goes (the row gets shorter), then whole
//! groups fold, lowest priority first, into a trailing "More" dropdown that
//! lists them with their labels and the same buttons. Pinned groups (the
//! canvas's Edit/Done and the selection's own group) never fold.
//!
//! Widths are measured, never guessed: the row's own box, and each group's
//! box as it last showed (with and without its label), through rinch's
//! bounds signals on both the desktop and the browser.

use std::rc::Rc;

use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use crate::appearance::IconGlyph;

/// One group of a fitted toolbar.
pub(crate) struct Slot {
    /// Its label, under its buttons and in the More menu.
    pub label: Rc<dyn Fn() -> String>,
    /// Lower folds first; `None` never folds.
    pub priority: Option<u8>,
    /// Whether it shows at all now (read reactively).
    pub shown: Rc<dyn Fn() -> bool>,
    /// Whether it acts now: dimmed and inert when not (read reactively).
    pub live: Rc<dyn Fn() -> bool>,
    /// Its buttons: built once for the row and, for a group that can fold,
    /// once more for the More menu (the same actions, the same state).
    pub build: Rc<dyn Fn(&mut RenderScope) -> Vec<NodeHandle>>,
    /// More classes on its group element.
    pub class: &'static str,
}

impl Slot {
    /// A group with a fixed label.
    pub(crate) fn new(
        label: &'static str,
        priority: Option<u8>,
        shown: impl Fn() -> bool + 'static,
        live: impl Fn() -> bool + 'static,
        build: impl Fn(&mut RenderScope) -> Vec<NodeHandle> + 'static,
    ) -> Slot {
        Slot { label: Rc::new(move || label.to_string()), priority, shown: Rc::new(shown), live: Rc::new(live), build: Rc::new(build), class: "" }
    }
}

/// What a group is to the fit: its width with its label and without, its
/// priority (`None`: never folds), and whether it shows now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Measure {
    pub with_label: f32,
    pub without_label: f32,
    pub priority: Option<u8>,
    pub shown: bool,
}

/// The More button's width, which folding must leave room for.
pub(crate) const MORE_WIDTH: f32 = 46.0;
/// Room kept spare, so a group a pixel too wide does not flicker in and out.
const SLACK: f32 = 6.0;

/// How the groups fit `avail` pixels: whether labels show, and which groups
/// fold into More. Pure: the toolbar's rule, tested below.
pub(crate) fn fit(avail: f32, groups: &[Measure]) -> (bool, Vec<bool>) {
    let none = vec![false; groups.len()];
    if avail <= 0.0 {
        return (true, none);
    }
    let room = avail - SLACK;
    let with: f32 = groups.iter().filter(|g| g.shown).map(|g| g.with_label).sum();
    if with <= room {
        return (true, none);
    }
    let mut total: f32 = groups.iter().filter(|g| g.shown).map(|g| g.without_label).sum();
    if total <= room {
        return (false, none);
    }
    let mut order: Vec<usize> = (0..groups.len()).filter(|&i| groups[i].shown && groups[i].priority.is_some()).collect();
    order.sort_by_key(|&i| groups[i].priority);
    let mut folded = none;
    for i in order {
        if total + MORE_WIDTH <= room {
            break;
        }
        folded[i] = true;
        total -= groups[i].without_label;
    }
    (false, folded)
}

/// A fitted toolbar: `class` on its row, `slots` in order.
/// `on_press` hears every press on the row.
pub(crate) fn render(__scope: &mut RenderScope, class: &'static str, slots: Vec<Slot>, on_press: impl Fn() + 'static) -> NodeHandle {
    let n = slots.len();
    let labels_on = Signal::new(true);
    let folded: Signal<Vec<bool>> = Signal::new(vec![false; n]);
    let mut measures: Vec<(Signal<f32>, Signal<f32>, Signal<bool>, Option<u8>)> = Vec::new();
    let mut groups: Vec<NodeHandle> = Vec::new();
    let mut menu_rows: Vec<NodeHandle> = Vec::new();
    for (i, slot) in slots.into_iter().enumerate() {
        let shown = Signal::new(true);
        let live = Signal::new(true);
        let label = Signal::new(String::new());
        {
            let (s, l, t) = (slot.shown.clone(), slot.live.clone(), slot.label.clone());
            let _ = rinch::Effect::new(move || shown.set_if_changed(s()));
            let _ = rinch::Effect::new(move || live.set_if_changed(l()));
            let _ = rinch::Effect::new(move || label.set_if_changed(t()));
        }
        let buttons = (slot.build)(__scope);
        let is_folded = move || folded.with(|f| f.get(i).copied().unwrap_or(false));
        let class = format!("pimble-toolbar__group {}", slot.class);
        let group = rsx! {
            div {
                class: class,
                style: {move || if !shown.get() || is_folded() { "display: none;" } else if live.get() { "" } else { "opacity: 0.4; pointer-events: none;" }},
                div { class: "pimble-toolbar__buttons", {buttons} }
                div {
                    class: "pimble-toolbar__label",
                    style: {move || if labels_on.get() { "" } else { "display: none;" }},
                    {move || label.get()}
                }
            }
        };
        // Its width as it last showed, with its label and without.
        let bounds = group.bounds_signal();
        let with_label = Signal::new(0.0f32);
        let without_label = Signal::new(0.0f32);
        let _ = rinch::Effect::new(move || {
            let w = bounds.get().width;
            if w <= 0.0 || !shown.get() || is_folded() {
                return;
            }
            if labels_on.get() {
                with_label.set_if_changed(w);
                // Without its label it is no wider than with it.
                if untracked(|| without_label.get()) <= 0.0 || untracked(|| without_label.get()) > w {
                    without_label.set_if_changed(w);
                }
            } else {
                without_label.set_if_changed(w);
                if untracked(|| with_label.get()) < w {
                    with_label.set_if_changed(w);
                }
            }
        });
        measures.push((with_label, without_label, shown, slot.priority));
        groups.push(group);
        if slot.priority.is_some() {
            let copy = (slot.build)(__scope);
            menu_rows.push(rsx! {
                div {
                    class: "pimble-toolbar__more-row",
                    style: {move || if shown.get() && is_folded() { if live.get() { "" } else { "opacity: 0.4; pointer-events: none;" } } else { "display: none;" }},
                    span { class: "pimble-toolbar__more-label", {move || label.get()} }
                    div { class: "pimble-toolbar__buttons", {copy} }
                }
            });
        }
    }

    let more_open = Signal::new(false);
    let any_folded = move || folded.with(|f| f.iter().any(|x| *x));
    let more = rsx! {
        div {
            class: "pimble-toolbar__more",
            style: {move || if any_folded() { "" } else { "display: none;" }},
            Popover {
                opened_fn: move || more_open.get(),
                onclose: move || more_open.set(false),
                position: "bottom-end",
                close_on_click_outside: true,
                close_on_escape: true,
                z_index: 300,
                PopoverTarget {
                    div {
                        title: "More",
                        class: {move || if more_open.get() { "pimble-board-tool pimble-board-tool--active" } else { "pimble-board-tool" }},
                        onclick: move || more_open.set(!untracked(|| more_open.get())),
                        IconGlyph { name: TablerIcon::Dots.name().to_string() }
                    }
                }
                PopoverDropdown {
                    div { class: "pimble-toolbar__more-menu", {menu_rows} }
                }
            }
        }
    };

    let row = rsx! {
        div {
            class: class,
            onmousedown: move || on_press(),
            {groups}
            {more}
        }
    };
    let avail = row.bounds_signal();
    let _ = rinch::Effect::new(move || {
        let width = avail.get().width;
        let now: Vec<Measure> = measures
            .iter()
            .map(|(on, off, shown, priority)| Measure { with_label: on.get(), without_label: off.get(), priority: *priority, shown: shown.get() })
            .collect();
        let (labels, fold) = fit(width, &now);
        untracked(|| {
            labels_on.set_if_changed(labels);
            folded.set_if_changed(fold);
        });
    });
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(on: f32, off: f32, priority: Option<u8>) -> Measure {
        Measure { with_label: on, without_label: off, priority, shown: true }
    }

    #[test]
    fn labels_go_first_then_groups_fold_lowest_priority_first() {
        let groups = [g(100.0, 90.0, None), g(200.0, 190.0, Some(3)), g(150.0, 140.0, Some(1)), g(80.0, 70.0, Some(2))];
        // Everything fits: labels and all.
        assert_eq!(fit(1000.0, &groups), (true, vec![false; 4]));
        // 530 with labels, 490 without: labels go.
        assert_eq!(fit(520.0, &groups), (false, vec![false; 4]));
        // 490 does not fit 400: the lowest priority (1) folds, leaving 350 + More.
        assert_eq!(fit(410.0, &groups), (false, vec![false, false, true, false]));
        // Narrower still: priority 2 too.
        assert_eq!(fit(340.0, &groups), (false, vec![false, false, true, true]));
        // A pinned group never folds, however narrow.
        let (_, folded) = fit(50.0, &groups);
        assert!(!folded[0] && folded[1] && folded[2] && folded[3]);
    }

    #[test]
    fn a_hidden_group_takes_no_room_and_never_folds() {
        let mut groups = [g(300.0, 300.0, Some(1)), g(300.0, 300.0, Some(2))];
        groups[0].shown = false;
        assert_eq!(fit(320.0, &groups), (true, vec![false, false]));
        // Unmeasured (no room known yet): nothing folds.
        assert_eq!(fit(0.0, &groups), (true, vec![false, false]));
    }
}

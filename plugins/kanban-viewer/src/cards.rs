//! One card, and the chips of who else is on something.

use super::*;

/// A card's style: chosen or not, its row color, and the color of the person
/// the pane follows when they are on it.
pub(crate) fn card_style(chosen: bool, background: Option<String>, leader: Option<String>) -> String {
    format!("display: flex; flex-direction: column; gap: 4px; {}", viewer_look::item_style(chosen, background, leader, (10, 12)))
}

/// Who else is on `addr`: the app's own chips, up to three and "+n"; a click
/// follows them.
pub(crate) fn people(__scope: &mut RenderScope, k: &Kanban, addr: Addr) -> NodeHandle {
    let (seen, follow) = (k.host.seen(), k.host.clone());
    rsx! {
        span {
            class: "pimble-pane__people",
            style: "margin-left: auto;",
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

/// One card: its icon, label and subtasks in its colors, who is on it, and
/// while chosen, what can be done with it.
pub(crate) fn render_card(__scope: &mut RenderScope, k: &Kanban, card: board::Card) -> NodeHandle {
    let addr = card.addr;
    let seen = k.host.seen();
    let (kc, kr, kd, kf, kf2, kenter, kleave) = (k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone());
    let chosen = k.chosen;
    let renaming = k.renaming;
    let hovered = k.hovered_card;
    let (color, background) = (card.color.clone(), card.background.clone());
    let label = card.label.clone();
    let writable = card.writable;
    let (icon, subtasks, label_color) = (card.icon.clone(), card.subtasks, card.color.clone());
    let item_label = card.label.clone();
    let show_icon = icon != DEFAULT_CARD_ICON;
    let card = rsx! {
        div {
            class: "kanban__card",
            id: dom_id(addr),
            style: {|| {
                let leader = seen.following().and_then(|key| seen.people_in(addr).into_iter().find(|p| p.key == key).map(|p| p.color));
                card_style(chosen.get() == Some(Chosen::Card(addr)), background.as_deref().map(|b| row_background(b, seen.dark())), leader)
            }},
            onclick: move || kc.click_card(addr, rinch::core::get_modifier_state()),
            onmouseenter: move || kenter.hovered_card.set(Some(addr)),
            onmouseleave: move || {
                if untracked(|| kleave.hovered_card.get()) == Some(addr) {
                    kleave.hovered_card.set(None);
                }
            },
            div {
                style: "display: flex; align-items: flex-start; gap: 8px; font-size: 14px; line-height: 1.45;",
                // A grip says the card drags (the whole card does).
                if writable {
                    span {
                        class: "kanban__grip",
                        style: "display: inline-flex; width: 14px; height: 14px; margin-top: 3px; margin-left: -4px; flex-shrink: 0; color: var(--rinch-color-dimmed); opacity: 0.6;",
                        IconGlyph { name: "grip-vertical".to_string() }
                    }
                }
                // A plain note's icon says nothing on a card; any other does.
                if show_icon {
                    span {
                        style: {let color = color.clone(); move || format!("display: inline-flex; width: 14px; height: 14px; margin-top: 3px; flex-shrink: 0;{}", color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default())},
                        IconGlyph { name: icon.clone() }
                    }
                }
                if renaming.get() == Some(addr) {
                    input {
                        class: "kanban__field",
                        style: FIELD_STYLE,
                        ref: kf.field,
                        placeholder: "Card name",
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
                        class: "kanban__label",
                        style: {
                            let color = label_color.clone();
                            move || format!("flex: 1; overflow-wrap: anywhere;{}", color.as_deref().map(|c| format!(" color: {};", display_color(c, seen.dark()))).unwrap_or_default())
                        },
                        {label.clone()}
                    }
                }
                if subtasks > 0 {
                    span {
                        class: "kanban__subtasks",
                        style: "display: inline-flex; align-items: center; gap: 2px; font-size: 11px; color: var(--rinch-color-dimmed);",
                        title: format!("{} subtask{}", subtasks, if subtasks == 1 { "" } else { "s" }),
                        span { style: "display: inline-flex; width: 12px; height: 12px;", IconGlyph { name: "subtask".to_string() } }
                        {subtasks.to_string()}
                    }
                }
                // The trash can, while the pointer is on the card or it is
                // chosen; its slot is always there so the label never reflows.
                if writable {
                    span {
                        class: "kanban__trash-slot",
                        style: "display: inline-flex; width: 16px; height: 16px; margin-top: 2px; flex-shrink: 0;",
                        if (hovered.get() == Some(addr) || chosen.get() == Some(Chosen::Card(addr))) && renaming.get() != Some(addr) {
                            span {
                                class: "kanban__trash",
                                title: "Delete (View > Recently Deleted puts it back)",
                                style: "display: inline-flex; width: 16px; height: 16px; cursor: pointer; color: var(--rinch-color-dimmed);",
                                onclick: { let k = kd.clone(); move || k.delete(addr) },
                                IconGlyph { name: "trash".to_string() }
                            }
                        }
                    }
                }
            }
            div {
                style: "display: flex; align-items: center; min-height: 0;",
                {people(__scope, k, addr)}
            }
        }
    };
    // The whole card drags (a press that never moves 5 px stays a click);
    // a card the person may only read does not.
    rsx! {
        SortableItem {
            value: key_of(addr),
            disabled: !writable,
            label: item_label,
            class: "kanban__card-item",
            style: "position: relative;",
            {card}
        }
    }
}

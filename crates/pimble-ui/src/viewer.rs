//! How a metanode viewer looks: the kanban's look, shared by every viewer
//! plugin (docs/KANBAN_CONTRACT.md "The look", docs/TODO_CONTRACT.md,
//! docs/TIMELINE_CONTRACT.md) so they read as one family.
//!
//! A plugin cannot add a stylesheet, so all of it is inline styles: the
//! viewer's root sets [`surfaces`] (custom properties, inherited), its
//! panels take [`PANEL`], and the things on them (cards, rows) take
//! [`item_style`].

/// The viewer's surfaces, set on its root and inherited: a panel (a
/// kanban's column cell, a todo list's list) a step off the pane and each
/// item a step further, lighter in the dark theme and white in the light one,
/// so items stand on their panels.
pub fn surfaces(dark: bool) -> &'static str {
    if dark {
        " --viewer-panel: var(--rinch-color-dark-6); --viewer-panel-border: var(--rinch-color-dark-5); --viewer-item: var(--rinch-color-dark-5); --viewer-item-border: var(--rinch-color-dark-4);"
    } else {
        " --viewer-panel: var(--rinch-color-gray-1); --viewer-panel-border: var(--rinch-color-gray-3); --viewer-item: var(--rinch-color-white); --viewer-item-border: var(--rinch-color-gray-3);"
    }
}

/// A panel: a kanban's column cell, a todo list's list.
pub const PANEL: &str = "background: var(--viewer-panel); border: 1px solid var(--viewer-panel-border); border-radius: 8px;";

/// A small square button holding one icon.
pub const ICON_BUTTON: &str = "display: inline-flex; align-items: center; justify-content: center; width: 22px; height: 22px; border-radius: 4px; cursor: pointer; color: var(--rinch-color-dimmed);";

/// A name being typed in place: it takes the room left in its row.
pub const FIELD_STYLE: &str = "flex: 1 1 0; min-width: 0; width: 100%; box-sizing: border-box; font: inherit; padding: 2px 4px;";

/// A line of small dimmed text: a status, a count.
pub const LINE_STYLE: &str = "font-size: 12px; color: var(--rinch-color-dimmed);";

/// rinch's sortable lists' caret and refusal box in the app's own theme
/// colors, through the properties its stylesheet reads.
pub const SORT_THEME: &str = " --rinch-sortable-caret-color: var(--rinch-primary-color); --rinch-sortable-refused-color: var(--rinch-color-red-6); --rinch-sortable-reason-bg: var(--rinch-color-red-6); --rinch-sortable-reason-color: var(--rinch-color-white, #ffffff); --rinch-sortable-reason-font-size: 12px; --rinch-sortable-reason-radius: 4px; --rinch-sortable-reason-white-space: normal; --rinch-sortable-reason-max-width: 220px; --rinch-sortable-dragging-opacity: 0.45;";

/// The icon a plain note has (`appearance::node_icon`), which an item leaves
/// out: it says nothing there.
pub const PLAIN_ICON: &str = "file";

/// An item's frame (a card, a row): chosen or not, its row color, the color of
/// the person the pane follows when they are on it, and its padding
/// (`(vertical, horizontal)` pixels at the 1 px border). A wider border takes
/// its width from the padding, so choosing an item never moves or rewraps
/// what is in it.
pub fn item_style(chosen: bool, background: Option<String>, leader: Option<String>, padding: (u32, u32)) -> String {
    let (v, h) = padding;
    let (border, (v, h)) = match (leader, chosen) {
        (Some(color), _) => (format!("2px solid {color}"), (v.saturating_sub(1), h.saturating_sub(1))),
        (None, true) => ("2px solid var(--rinch-primary-color)".to_string(), (v.saturating_sub(1), h.saturating_sub(1))),
        (None, false) => ("1px solid var(--viewer-item-border)".to_string(), (v, h)),
    };
    let background = background.unwrap_or_else(|| "var(--viewer-item)".to_string());
    format!("padding: {v}px {h}px; border-radius: 6px; cursor: pointer; border: {border}; background: {background};")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_item_takes_its_border_from_its_padding() {
        let plain = item_style(false, None, None, (10, 12));
        let chosen = item_style(true, None, None, (10, 12));
        assert!(plain.contains("padding: 10px 12px;") && plain.contains("1px solid"), "{plain}");
        assert!(chosen.contains("padding: 9px 11px;") && chosen.contains("2px solid"), "{chosen}");
        let followed = item_style(false, Some("#123456".into()), Some("#aa3300".into()), (6, 10));
        assert!(followed.contains("2px solid #aa3300") && followed.contains("background: #123456"), "{followed}");
    }
}

//! A grouping row of the explorer: one kind's row ("Themes";
//! docs/THEMES_CONTRACT.md section 3.1a "Grouped by kind"). It is no node: it is drawn dimmed like the nodes it holds, opens
//! nothing, cannot be renamed, deleted, copied or dragged, and a right
//! press on it aims the explorer's menu at no row.

use rinch::prelude::*;
use rinch_tabler_icons::{render_tabler_icon, TablerIcon, TablerIconStyle};

use super::row_menu::ExplorerMenu;
use super::rows::{REVEAL_ROW, TREE_ROWS};
use crate::appearance::icon_by_name;
use crate::state::{AppStore, InternalGroup};

/// What the row says, and its tooltip.
pub(crate) fn group_words(group: &InternalGroup) -> (String, String) {
    (group.kind.name().to_string(), group.kind.here_text(group.count))
}

pub(super) fn internal_group_view(
    __scope: &mut RenderScope,
    store: AppStore,
    tree_state: UseTreeReturn,
    menu: ExplorerMenu,
    value: &str,
    group: &InternalGroup,
) -> NodeHandle {
    let icon = icon_by_name(group.kind.icon()).unwrap_or(TablerIcon::Folder);
    let icon_el = render_tabler_icon(__scope, icon, TablerIconStyle::Outline);
    let (name, tooltip) = group_words(group);
    let wrapper = rsx! {
        span {
            class: "rinch-tree__label pimble-tree__internal-group",
            style: "flex: 1; min-width: 0; display: inline-flex; align-items: center;",
            title: tooltip,
            span { class: "rinch-tree__icon", style: "opacity: 0.55;", {icon_el} }
            span {
                style: "cursor: default; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; opacity: 0.55; font-style: italic;",
                {name}
            }
        }
    };
    let row_value = value.to_string();
    TREE_ROWS.with(|rows| rows.borrow_mut().insert(row_value.clone(), wrapper.clone()));
    if REVEAL_ROW.with(|pending| pending.borrow().as_deref() == Some(row_value.as_str())) {
        REVEAL_ROW.with(|pending| pending.borrow_mut().take());
        wrapper.scroll_into_view();
    }
    rsx! {
        div {
            style: "flex: 1; min-width: 0; display: flex; align-items: center; border-radius: 2px;",
            onmousedown: move || {
                if rinch::core::get_click_context().button == rinch::core::MouseButton::Right {
                    // No row's menu: the group is not a node.
                    menu.aim(store, tree_state, None);
                }
            },
            {wrapper}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::internal::InternalKind;
    use pimble_core::{NodeId, StoreId};

    #[test]
    fn a_group_says_what_it_holds() {
        let group = |count| InternalGroup {
            store_id: StoreId::new(),
            parent_id: NodeId::new(),
            parent_row: String::new(),
            kind: InternalKind::Theme,
            suffix: String::new(),
            count,
        };
        assert_eq!(group_words(&group(2)), ("Themes".to_string(), "2 themes here.".to_string()));
        assert_eq!(group_words(&group(1)).1, "1 theme here.");
    }
}

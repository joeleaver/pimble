//! The board's styles and sizes.

use super::*;

pub(crate) const BOARD_STYLE: &str = "display: flex; flex-direction: column; gap: 8px; padding: 12px 16px; height: 100%; box-sizing: border-box; outline: none;";
/// The board's rows (the header row, then a row per lane), scrolled together
/// so columns stay lined up.
pub(crate) const GRID_STYLE: &str = "display: flex; flex-direction: column; gap: 10px; overflow: auto; flex: 1; min-height: 0; padding-bottom: 12px;";
/// A column's width, a folded column's, and the lane names' column.
pub(crate) const COLUMN_W: u32 = 280;
pub(crate) const FOLDED_W: u32 = 44;
pub(crate) const LABEL_W: u32 = 168;
/// A minimized lane shows about three cards a column (kobayban's cap): the
/// height its cards scroll in, inside the list's padding.
pub(crate) const MINIMIZED_MAX: u32 = 220;
/// A column cell's padding around its cards; the foot leaves room for "Add
/// card" below the last.
const LIST_PAD: u32 = 8;
const LIST_FOOT: u32 = 34;
/// The least a column is drawn, empty: room to drop a first card.
const CELL_MIN: u32 = 72;
pub(crate) const ROW_STYLE: &str = "display: flex; gap: 12px; align-items: stretch; flex-shrink: 0;";
pub(crate) const HEADER_ROW_STYLE: &str = "display: flex; gap: 12px; align-items: flex-end; flex-shrink: 0;";
// The sortable lists' layout is written here as well as in rinch's stylesheet,
// which is the app's: an app on a rinch from before sortable lists still lays
// the board out (only the caret and the refusal box need its stylesheet).
pub(crate) const COLUMN_LIST_STYLE: &str = "display: flex; flex-direction: row; gap: 12px; align-items: flex-end; flex-shrink: 0; position: relative;";
pub(crate) const CARD_LIST_STYLE: &str = "display: flex; flex-direction: column; gap: 8px; position: relative;";
pub(crate) const CELL_BG: &str = viewer_look::PANEL;
pub(crate) const LANE_LABEL_STYLE: &str = "display: flex; flex-direction: column; gap: 4px; padding: 6px 4px; flex-shrink: 0; box-sizing: border-box; min-width: 0;";

pub(crate) const HEADER_STYLE: &str = "display: flex; align-items: center; gap: 6px; font-weight: 600; font-size: 13px; padding: 2px 4px; border-radius: 4px; cursor: pointer;";
pub(crate) const DEFAULT_CARD_ICON: &str = viewer_look::PLAIN_ICON;

/// A column cell's style: a panel its card list fills ([`card_list_style`]),
/// as tall as its cards or the row's tallest, capped only in a minimized lane.
pub(crate) fn cell_style(size: Size) -> String {
    let cap = match size {
        Size::Minimized => format!(" max-height: {}px;", MINIMIZED_MAX + LIST_PAD + LIST_FOOT),
        Size::Expanded | Size::Collapsed => String::new(),
    };
    format!("width: {COLUMN_W}px; min-height: {CELL_MIN}px;{cap} position: relative; display: flex; flex-direction: column; box-sizing: border-box; {CELL_BG}")
}

/// What a column's cell adds while a dragged card would land in its list
/// (`list`, its id): lit in the accent, red where the drop would be refused.
/// The place on offer (`target`, which rinch's slot shows too) decides, else
/// the list under the pointer. Nothing for a column drag (its header row has
/// the slot) or a card bound for another column.
pub(crate) fn drop_lit(drag: Option<&SortDragState>, list: &str) -> &'static str {
    let Some(drag) = drag.filter(|d| d.group == CARDS) else { return "" };
    if drag.target.as_ref().or(drag.over.as_ref()).is_none_or(|t| t.list != list) {
        return "";
    }
    viewer_look::drop_panel(matches!(drag.verdict, Some(SortVerdict::Refuse(_))))
}

/// A column's card list: the whole of its cell (it grows to fill a cell the
/// row stretched), the cell's padding its own, so every point of the column
/// is over the list. In a minimized lane it scrolls inside the cell's cap.
pub(crate) fn card_list_style(size: Size) -> String {
    let scroll = match size {
        Size::Minimized => " min-height: 0; overflow-y: auto;",
        Size::Expanded | Size::Collapsed => "",
    };
    format!("{CARD_LIST_STYLE} flex: 1;{scroll} padding: {LIST_PAD}px {LIST_PAD}px {LIST_FOOT}px {LIST_PAD}px; box-sizing: border-box;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(over: Option<&str>, verdict: Option<SortVerdict>) -> SortDragState {
        SortDragState {
            group: CARDS.to_string(),
            keys: vec!["card".into()],
            source_key: "card".into(),
            from_list: "a".into(),
            keyboard: false,
            over: over.map(|list| SortTarget { list: list.into(), index: 0 }),
            target: over.map(|list| SortTarget { list: list.into(), index: 0 }),
            verdict,
        }
    }

    /// Joe (2026-11-24): nothing lit up where a dragged card would land. The
    /// cell under the pointer is lit, in red when the drop is refused, and
    /// no other is.
    #[test]
    fn the_cell_a_card_is_dragged_over_is_lit() {
        assert_eq!(drop_lit(None, "b"), "");
        let over_b = drag(Some("b"), Some(SortVerdict::Accept));
        assert_eq!(drop_lit(Some(&over_b), "b"), viewer_look::drop_panel(false));
        assert_eq!(drop_lit(Some(&over_b), "a"), "");
        // Over its own place: no move on offer, still where it lands.
        assert_eq!(drop_lit(Some(&drag(Some("a"), None)), "a"), viewer_look::drop_panel(false));
        let refused = drag(Some("b"), Some(SortVerdict::Refuse("You can read this, not change it.".into())));
        assert_eq!(drop_lit(Some(&refused), "b"), viewer_look::drop_panel(true));
        assert_eq!(drop_lit(Some(&drag(None, None)), "b"), "");
        // The offer kept while the pointer is off every list: still lit.
        let kept = SortDragState { over: None, ..over_b.clone() };
        assert_eq!(drop_lit(Some(&kept), "b"), viewer_look::drop_panel(false));
        let columns = SortDragState { group: COLUMNS.to_string(), ..over_b };
        assert_eq!(drop_lit(Some(&columns), "b"), "", "a column drag lights no cell");
        assert_ne!(viewer_look::drop_panel(false), viewer_look::drop_panel(true));
    }

    /// Joe's "Done" column (2026-11-23): eight cards in a window one card tall.
    /// Nothing but a minimized lane caps a column, and no column's list is
    /// taken out of the flow, where it would add no height to its cell.
    #[test]
    fn a_column_grows_with_its_cards_unless_its_lane_is_minimized() {
        for size in [Size::Expanded, Size::Collapsed] {
            assert!(!cell_style(size).contains("max-height"), "{size:?}");
            assert!(!card_list_style(size).contains("overflow"), "{size:?}");
        }
        assert!(cell_style(Size::Minimized).contains("max-height"));
        assert!(card_list_style(Size::Minimized).contains("overflow-y: auto"));
        for size in [Size::Expanded, Size::Minimized, Size::Collapsed] {
            assert!(!card_list_style(size).contains("absolute"));
            assert!(!cell_style(size).contains("padding"), "the cell's padding is the list's, so the list is every point of it");
            assert!(card_list_style(size).contains("flex: 1;"));
        }
    }
}

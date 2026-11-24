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
/// A minimized lane shows about three cards a column (kobayban's cap).
pub(crate) const MINIMIZED_MAX: u32 = 220;
pub(crate) const ROW_STYLE: &str = "display: flex; gap: 12px; align-items: stretch; flex-shrink: 0;";
pub(crate) const HEADER_ROW_STYLE: &str = "display: flex; gap: 12px; align-items: flex-end; flex-shrink: 0;";
// The sortable lists' layout is written here as well as in rinch's stylesheet,
// which is the app's: an app on a rinch from before sortable lists still lays
// the board out (only the caret and the refusal box need its stylesheet).
pub(crate) const COLUMN_LIST_STYLE: &str = "display: flex; flex-direction: row; gap: 12px; align-items: flex-end; flex-shrink: 0; position: relative;";
pub(crate) const CARD_LIST_STYLE: &str = "display: flex; flex-direction: column; gap: 8px; min-height: 28px; position: relative;";
pub(crate) const CELL_BG: &str = viewer_look::PANEL;
pub(crate) const LANE_LABEL_STYLE: &str = "display: flex; flex-direction: column; gap: 4px; padding: 6px 4px; flex-shrink: 0; box-sizing: border-box; min-width: 0;";

pub(crate) const HEADER_STYLE: &str = "display: flex; align-items: center; gap: 6px; font-weight: 600; font-size: 13px; padding: 2px 4px; border-radius: 4px; cursor: pointer;";
pub(crate) const DEFAULT_CARD_ICON: &str = viewer_look::PLAIN_ICON;

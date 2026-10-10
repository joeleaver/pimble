//! How a viewer chooses to see a board, none of it stored in the document:
//! every element in its author's color, every layer and the markup shown
//! (for export), and the titles and colors of the nodes cards link to (the
//! app knows them; the board stores only the link).

use std::collections::HashMap;
use std::sync::Arc;

use crate::model::Color;

/// What the app knows about the node a card links to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CardLabel {
    /// The node's title, shown on the card in place of the element's text.
    pub title: String,
    /// The node's color, filling the card's small icon square.
    pub color: Option<Color>,
}

/// A viewer's choices for drawing a board (see the module doc). Carried by
/// [`crate::BoardSnapshot`] through `update` and `rebuild`; changed with
/// [`crate::BoardSnapshot::with_options`] or [`crate::BoardView::set_options`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewOptions {
    /// Every element except pictures drawn in its author's color
    /// ([`author_color`]; a neutral grey when nobody is stamped), a fill
    /// that is set becoming the same color, translucent.
    pub color_by_author: bool,
    /// Every layer read as visible and the markup as shown ("with all
    /// markup", for export). The stored values are not changed.
    pub show_all: bool,
    /// By a card's link string (`Geom::Card::link`).
    pub card_labels: Arc<HashMap<String, CardLabel>>,
}

/// The color of elements nobody is stamped on, when coloring by author.
pub const UNAUTHORED_COLOR: Color = Color::rgb(0x9e, 0x9e, 0x9e);

/// Distinct colors, legible on a light and a dark board.
const AUTHOR_PALETTE: [Color; 10] = [
    Color::rgb(0xe5, 0x39, 0x35), // red
    Color::rgb(0x1e, 0x88, 0xe5), // blue
    Color::rgb(0x43, 0xa0, 0x47), // green
    Color::rgb(0x8e, 0x24, 0xaa), // purple
    Color::rgb(0xf4, 0x51, 0x1e), // deep orange
    Color::rgb(0x00, 0x89, 0x7b), // teal
    Color::rgb(0xd8, 0x1b, 0x60), // pink
    Color::rgb(0x39, 0x49, 0xab), // indigo
    Color::rgb(0x6d, 0x4c, 0x41), // brown
    Color::rgb(0x00, 0xac, 0xc1), // cyan
];

/// An author's color: a pick from a palette of ten by a hash of their id,
/// the same on every device and every run.
pub fn author_color(id: &str) -> Color {
    // FNV-1a: stable across platforms and Rust versions.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    AUTHOR_PALETTE[(h % AUTHOR_PALETTE.len() as u64) as usize]
}

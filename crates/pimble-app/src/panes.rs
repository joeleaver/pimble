//! The split view's model (docs/SPLIT_VIEW_CONTRACT.md "The model"): how the
//! area beside the explorer is tiled into one to four panes.
//!
//! Nothing here draws or knows about editors. A [`Tiling`] is a binary tree
//! whose leaves are pane slots; the app positions its four slots from
//! [`Tiling::rects`] and its three dividers from [`Tiling::dividers`], and
//! every change the person makes (split, close, drag) is one of the pure
//! functions below.

use serde::{Deserialize, Serialize};

/// How many panes the area holds at most.
pub const MAX_PANES: usize = 4;

/// The narrowest a divider leaves a pane, in pixels.
pub const PANE_MIN_WIDTH: f32 = 200.0;
/// The shortest a divider leaves a pane, in pixels.
pub const PANE_MIN_HEIGHT: f32 = 120.0;

/// A pane slot, `0..4`: stable for the pane's life, and reused once the pane
/// is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(u8);

impl PaneId {
    /// Every slot, in order.
    pub const ALL: [PaneId; MAX_PANES] = [PaneId(0), PaneId(1), PaneId(2), PaneId(3)];

    /// The first slot: the one pane of an unsplit area.
    pub const FIRST: PaneId = PaneId(0);

    /// The slot's index into a per-pane table.
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// The slot with this index, when there is one.
    pub fn from_index(index: usize) -> Option<PaneId> {
        PaneId::ALL.get(index).copied()
    }
}

impl Default for PaneId {
    fn default() -> Self {
        PaneId::FIRST
    }
}

/// Which way a split divides its space: `second` is right of, or below,
/// `first`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Right,
    Down,
}

/// A rectangle in fractions of the tiled area (`0.0..=1.0` each way).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The line between a split's two halves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Divider {
    /// Which split it drags: the split's place in the tiling, counted
    /// parents before children and `first` before `second`
    /// ([`Tiling::set_ratio`] takes it back).
    pub index: usize,
    pub direction: Direction,
    /// The line itself, in fractions of the area: no width for a `Right`
    /// split, no height for a `Down` one.
    pub rect: Rect,
    /// The space the split divides, which its ratio is a fraction of.
    pub space: Rect,
}

/// The tiling: a binary tree with at most four leaves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tiling {
    Pane(PaneId),
    Split { direction: Direction, ratio: f32, first: Box<Tiling>, second: Box<Tiling> },
}

/// The layout as it is remembered across restarts
/// (docs/SPLIT_VIEW_CONTRACT.md "Persistence"): `state.json`'s `panes` on the
/// desktop, the same JSON in `localStorage` in the browser.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SavedPanes {
    pub tiling: Tiling,
    #[serde(default)]
    pub focused: PaneId,
    /// The document each pane held: the canonical pair, where the note
    /// lives, however it was reached.
    #[serde(default)]
    pub documents: Vec<SavedDocument>,
}

/// One pane's document in a [`SavedPanes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDocument {
    pub pane: PaneId,
    pub store_id: pimble_core::StoreId,
    pub node_id: pimble_core::NodeId,
}

impl SavedPanes {
    /// What was read from disk, made safe to restore: a tiling that can be
    /// drawn, the focus on a pane it shows, and one document at most for
    /// each pane it shows.
    pub fn sanitized(self) -> SavedPanes {
        let tiling = self.tiling.sanitized();
        let shown = tiling.panes();
        let focused = if shown.contains(&self.focused) { self.focused } else { shown[0] };
        let mut documents: Vec<SavedDocument> = Vec::new();
        for document in self.documents {
            if shown.contains(&document.pane) && !documents.iter().any(|held| held.pane == document.pane) {
                documents.push(document);
            }
        }
        SavedPanes { tiling, focused, documents }
    }
}

impl Default for Tiling {
    fn default() -> Self {
        Tiling::Pane(PaneId::FIRST)
    }
}

/// The least and most a ratio may be whatever the area's size: a pane is
/// never dragged to nothing.
const RATIO_MIN: f32 = 0.05;
const RATIO_MAX: f32 = 0.95;

impl Tiling {
    /// The panes shown, in reading order (`first` before `second`).
    pub fn panes(&self) -> Vec<PaneId> {
        match self {
            Tiling::Pane(pane) => vec![*pane],
            Tiling::Split { first, second, .. } => {
                let mut panes = first.panes();
                panes.extend(second.panes());
                panes
            }
        }
    }

    /// Whether `pane` is shown.
    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Tiling::Pane(p) => *p == pane,
            Tiling::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    /// How many panes are shown.
    pub fn len(&self) -> usize {
        match self {
            Tiling::Pane(_) => 1,
            Tiling::Split { first, second, .. } => first.len() + second.len(),
        }
    }

    /// Whether another pane fits.
    pub fn can_split(&self) -> bool {
        self.len() < MAX_PANES
    }

    /// Halve `pane` in `direction` and answer the new pane, which takes the
    /// second half (right of, or below, `pane`). Refused (`None`) with four
    /// panes open or when `pane` is not shown.
    pub fn split(&mut self, pane: PaneId, direction: Direction) -> Option<PaneId> {
        if !self.can_split() || !self.contains(pane) {
            return None;
        }
        let shown = self.panes();
        let new = PaneId::ALL.into_iter().find(|p| !shown.contains(p))?;
        self.replace_leaf(pane, |leaf| Tiling::Split {
            direction,
            ratio: 0.5,
            first: Box::new(leaf),
            second: Box::new(Tiling::Pane(new)),
        });
        Some(new)
    }

    /// Close `pane`: the sibling it was split from takes its space. Answers
    /// the pane that now covers where it was (the nearest of the sibling's),
    /// or `None` when `pane` is the last one or is not shown, which changes
    /// nothing.
    pub fn close(&mut self, pane: PaneId) -> Option<PaneId> {
        match self {
            Tiling::Pane(_) => None,
            Tiling::Split { first, second, .. } => {
                let (closing_first, closing_second) =
                    (matches!(**first, Tiling::Pane(p) if p == pane), matches!(**second, Tiling::Pane(p) if p == pane));
                if closing_first || closing_second {
                    // The sibling's leaf that touched the closed pane: its
                    // first when it was after it, its last when before.
                    let sibling = if closing_first { (**second).clone() } else { (**first).clone() };
                    let panes = sibling.panes();
                    let neighbour = if closing_first { panes.first() } else { panes.last() }.copied();
                    *self = sibling;
                    return neighbour;
                }
                first.close(pane).or_else(|| second.close(pane))
            }
        }
    }

    /// Where each pane is, in fractions of the area, in reading order.
    pub fn rects(&self) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.walk(Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }, &mut 0, &mut |pane, rect| out.push((pane, rect)), &mut |_| {});
        out
    }

    /// Where `pane` is, when it is shown.
    pub fn rect_of(&self, pane: PaneId) -> Option<Rect> {
        self.rects().into_iter().find(|(p, _)| *p == pane).map(|(_, rect)| rect)
    }

    /// The line of every split, each with the split it drags.
    pub fn dividers(&self) -> Vec<Divider> {
        let mut out = Vec::new();
        self.walk(Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }, &mut 0, &mut |_, _| {}, &mut |divider| out.push(divider));
        out
    }

    /// Set the ratio of split `index` (a [`Divider::index`]), kept inside
    /// `0.05..=0.95`. Answers whether there is such a split.
    pub fn set_ratio(&mut self, index: usize, ratio: f32) -> bool {
        if !ratio.is_finite() {
            return false;
        }
        match self.split_mut(index, &mut 0) {
            Some(Tiling::Split { ratio: held, .. }) => {
                *held = ratio.clamp(RATIO_MIN, RATIO_MAX);
                true
            }
            _ => false,
        }
    }

    /// Set the ratio of split `index` as a drag does: kept where every pane
    /// on both sides stays at least [`PANE_MIN_WIDTH`] wide and
    /// [`PANE_MIN_HEIGHT`] tall in an area of `area_width` by `area_height`
    /// pixels. In an area too small for that the split stays where it is.
    pub fn drag_ratio(&mut self, index: usize, ratio: f32, area_width: f32, area_height: f32) -> bool {
        let Some(divider) = self.dividers().into_iter().find(|d| d.index == index) else { return false };
        let Some(Tiling::Split { direction, first, second, .. }) = self.split_mut(index, &mut 0) else { return false };
        let (extent, least_first, least_second) = match direction {
            Direction::Right => (divider.space.w * area_width, first.least_size().0, second.least_size().0),
            Direction::Down => (divider.space.h * area_height, first.least_size().1, second.least_size().1),
        };
        // Half a pixel of slack: the extent is a product of fractions, and a
        // space of exactly the least size must not be refused for rounding.
        if extent <= 0.0 || least_first + least_second > extent + 0.5 {
            return false;
        }
        let (least, most) = (least_first / extent, 1.0 - least_second / extent);
        let ratio = ratio.clamp(least.min(most), most.max(least));
        self.set_ratio(index, ratio)
    }

    /// The smallest this part of the tiling may be drawn, in pixels: its
    /// panes' least sizes added along each split.
    fn least_size(&self) -> (f32, f32) {
        match self {
            Tiling::Pane(_) => (PANE_MIN_WIDTH, PANE_MIN_HEIGHT),
            Tiling::Split { direction, first, second, .. } => {
                let ((w1, h1), (w2, h2)) = (first.least_size(), second.least_size());
                match direction {
                    Direction::Right => (w1 + w2, h1.max(h2)),
                    Direction::Down => (w1.max(w2), h1 + h2),
                }
            }
        }
    }

    /// A tiling read from disk, made safe to draw: it must name each slot at
    /// most once and no more than four, or it is the single pane; a ratio
    /// outside its bounds is brought back inside them.
    pub fn sanitized(mut self) -> Tiling {
        let mut panes = self.panes();
        let count = panes.len();
        panes.sort();
        panes.dedup();
        if count > MAX_PANES || panes.len() != count || panes.iter().any(|p| p.index() >= MAX_PANES) {
            return Tiling::default();
        }
        self.clamp_ratios();
        self
    }

    fn clamp_ratios(&mut self) {
        if let Tiling::Split { ratio, first, second, .. } = self {
            *ratio = if ratio.is_finite() { ratio.clamp(RATIO_MIN, RATIO_MAX) } else { 0.5 };
            first.clamp_ratios();
            second.clamp_ratios();
        }
    }

    fn replace_leaf(&mut self, pane: PaneId, with: impl FnOnce(Tiling) -> Tiling) {
        fn find(tiling: &mut Tiling, pane: PaneId) -> Option<&mut Tiling> {
            match tiling {
                Tiling::Pane(p) if *p == pane => Some(tiling),
                Tiling::Pane(_) => None,
                Tiling::Split { first, second, .. } => match find(first, pane) {
                    Some(found) => Some(found),
                    None => find(second, pane),
                },
            }
        }
        if let Some(leaf) = find(self, pane) {
            let held = std::mem::take(leaf);
            *leaf = with(held);
        }
    }

    /// The split numbered `index`, counting as [`Divider::index`] does.
    fn split_mut(&mut self, index: usize, next: &mut usize) -> Option<&mut Tiling> {
        if matches!(self, Tiling::Pane(_)) {
            return None;
        }
        if *next == index {
            return Some(self);
        }
        *next += 1;
        let Tiling::Split { first, second, .. } = self else { return None };
        match first.split_mut(index, next) {
            Some(found) => Some(found),
            None => second.split_mut(index, next),
        }
    }

    /// Visit every pane and every split's line inside `space`.
    fn walk(&self, space: Rect, next: &mut usize, pane: &mut impl FnMut(PaneId, Rect), divider: &mut impl FnMut(Divider)) {
        match self {
            Tiling::Pane(p) => pane(*p, space),
            Tiling::Split { direction, ratio, first, second } => {
                let index = *next;
                *next += 1;
                // The second half ends where the space ends, whatever the
                // rounding of the first: the halves tile it exactly.
                let (a, b, line) = match direction {
                    Direction::Right => {
                        let w = space.w * ratio;
                        (
                            Rect { w, ..space },
                            Rect { x: space.x + w, w: space.w - w, ..space },
                            Rect { x: space.x + w, w: 0.0, ..space },
                        )
                    }
                    Direction::Down => {
                        let h = space.h * ratio;
                        (
                            Rect { h, ..space },
                            Rect { y: space.y + h, h: space.h - h, ..space },
                            Rect { y: space.y + h, h: 0.0, ..space },
                        )
                    }
                };
                divider(Divider { index, direction: *direction, rect: line, space });
                first.walk(a, next, pane, divider);
                second.walk(b, next, pane, divider);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: [PaneId; 4] = PaneId::ALL;

    /// The panes' rectangles cover the area exactly: their areas add up to
    /// it, none overlaps another, and none leaves it.
    fn assert_tiles(tiling: &Tiling) {
        let rects = tiling.rects();
        let area: f32 = rects.iter().map(|(_, r)| r.w * r.h).sum();
        assert!((area - 1.0).abs() < 1e-5, "the panes cover {area} of the area: {tiling:?}");
        for (i, (_, a)) in rects.iter().enumerate() {
            assert!(a.x >= -1e-6 && a.y >= -1e-6 && a.x + a.w <= 1.0 + 1e-6 && a.y + a.h <= 1.0 + 1e-6, "{a:?} leaves the area");
            assert!(a.w > 0.0 && a.h > 0.0, "{a:?} is empty");
            for (_, b) in &rects[i + 1..] {
                let apart = a.x + a.w <= b.x + 1e-6 || b.x + b.w <= a.x + 1e-6 || a.y + a.h <= b.y + 1e-6 || b.y + b.h <= a.y + 1e-6;
                assert!(apart, "{a:?} and {b:?} overlap in {tiling:?}");
            }
        }
        assert_eq!(tiling.dividers().len(), rects.len() - 1, "one divider per split");
    }

    /// Every tiling of four panes reachable by splitting: each step splits
    /// any shown pane either way.
    fn every_tiling_of(count: usize) -> Vec<Tiling> {
        let mut level = vec![Tiling::default()];
        for _ in 1..count {
            let mut next = Vec::new();
            for tiling in &level {
                for pane in tiling.panes() {
                    for direction in [Direction::Right, Direction::Down] {
                        let mut split = tiling.clone();
                        assert!(split.split(pane, direction).is_some());
                        next.push(split);
                    }
                }
            }
            level = next;
        }
        level
    }

    #[test]
    fn a_split_halves_the_pane_and_the_new_one_is_right_of_or_below_it() {
        let mut tiling = Tiling::default();
        let new = tiling.split(P[0], Direction::Right).unwrap();
        assert_eq!(new, P[1]);
        assert_eq!(
            tiling.rects(),
            vec![(P[0], Rect { x: 0.0, y: 0.0, w: 0.5, h: 1.0 }), (P[1], Rect { x: 0.5, y: 0.0, w: 0.5, h: 1.0 })]
        );
        let below = tiling.split(P[1], Direction::Down).unwrap();
        assert_eq!(below, P[2]);
        assert_eq!(tiling.rect_of(P[1]), Some(Rect { x: 0.5, y: 0.0, w: 0.5, h: 0.5 }));
        assert_eq!(tiling.rect_of(P[2]), Some(Rect { x: 0.5, y: 0.5, w: 0.5, h: 0.5 }));
        assert_eq!(tiling.rect_of(P[0]), Some(Rect { x: 0.0, y: 0.0, w: 0.5, h: 1.0 }), "the untouched pane keeps its place");
    }

    #[test]
    fn every_way_to_four_panes_tiles_the_area_and_a_fifth_is_refused() {
        for count in 1..=MAX_PANES {
            let tilings = every_tiling_of(count);
            // 1, then 2 choices, then 2 panes x 2, then 3 panes x 2 each.
            assert_eq!(tilings.len(), [1, 2, 8, 48][count - 1]);
            for tiling in &tilings {
                assert_eq!(tiling.len(), count);
                let mut panes = tiling.panes();
                panes.sort();
                assert_eq!(panes, P[..count].to_vec(), "slots are handed out lowest first");
                assert_tiles(tiling);
            }
        }
        for mut tiling in every_tiling_of(MAX_PANES) {
            let before = tiling.clone();
            assert!(!tiling.can_split());
            for pane in PaneId::ALL {
                assert_eq!(tiling.split(pane, Direction::Right), None);
                assert_eq!(tiling.split(pane, Direction::Down), None);
            }
            assert_eq!(tiling, before, "a refused split changes nothing");
        }
    }

    #[test]
    fn a_split_of_a_pane_that_is_not_shown_is_refused() {
        let mut tiling = Tiling::default();
        assert_eq!(tiling.split(P[2], Direction::Right), None);
        assert_eq!(tiling, Tiling::default());
    }

    /// Every order of closing, from every tiling of four: each close leaves
    /// a tiling of the panes that remain, and the last pane stays.
    #[test]
    fn closing_in_every_order_gives_the_space_back_and_keeps_the_last_pane() {
        let orders: Vec<[usize; 4]> = {
            let mut all = Vec::new();
            for a in 0..4 {
                for b in 0..4 {
                    for c in 0..4 {
                        for d in 0..4 {
                            let order = [a, b, c, d];
                            let mut seen = order;
                            seen.sort();
                            if seen == [0, 1, 2, 3] {
                                all.push(order);
                            }
                        }
                    }
                }
            }
            all
        };
        assert_eq!(orders.len(), 24);
        for start in every_tiling_of(MAX_PANES) {
            for order in &orders {
                let mut tiling = start.clone();
                for (closed, &slot) in order.iter().enumerate() {
                    let pane = P[slot];
                    let neighbour = tiling.close(pane);
                    if closed == 3 {
                        assert_eq!(neighbour, None, "the last pane cannot be closed");
                        assert_eq!(tiling, Tiling::Pane(pane));
                    } else {
                        let neighbour = neighbour.expect("a pane beside others closes");
                        assert!(tiling.contains(neighbour) && !tiling.contains(pane));
                        assert_eq!(tiling.len(), 3 - closed);
                        assert_tiles(&tiling);
                    }
                }
            }
        }
    }

    #[test]
    fn a_closed_panes_space_goes_to_the_sibling_it_was_split_from() {
        // 0 | (1 over 2): closing 1 gives 2 the whole right half, and 0 is
        // not touched.
        let mut tiling = Tiling::default();
        tiling.split(P[0], Direction::Right);
        tiling.split(P[1], Direction::Down);
        assert_eq!(tiling.close(P[1]), Some(P[2]));
        assert_eq!(
            tiling.rects(),
            vec![(P[0], Rect { x: 0.0, y: 0.0, w: 0.5, h: 1.0 }), (P[2], Rect { x: 0.5, y: 0.0, w: 0.5, h: 1.0 })]
        );
        // Closing the left pane gives its half to the whole right side.
        let mut tiling = Tiling::default();
        tiling.split(P[0], Direction::Right);
        tiling.split(P[1], Direction::Down);
        assert_eq!(tiling.close(P[0]), Some(P[1]));
        assert_eq!(
            tiling.rects(),
            vec![(P[1], Rect { x: 0.0, y: 0.0, w: 1.0, h: 0.5 }), (P[2], Rect { x: 0.0, y: 0.5, w: 1.0, h: 0.5 })]
        );
        // A freed slot is the next one handed out.
        assert_eq!(tiling.split(P[2], Direction::Right), Some(P[0]));
        // A pane that is not shown closes nothing.
        let before = tiling.clone();
        assert_eq!(tiling.close(P[3]), None);
        assert_eq!(tiling, before);
    }

    #[test]
    fn dividers_lie_between_the_halves_and_name_their_split() {
        let mut tiling = Tiling::default();
        tiling.split(P[0], Direction::Right);
        tiling.split(P[1], Direction::Down);
        let dividers = tiling.dividers();
        assert_eq!(dividers.len(), 2);
        assert_eq!(dividers[0].index, 0);
        assert_eq!(dividers[0].direction, Direction::Right);
        assert_eq!(dividers[0].rect, Rect { x: 0.5, y: 0.0, w: 0.0, h: 1.0 });
        assert_eq!(dividers[1].index, 1);
        assert_eq!(dividers[1].direction, Direction::Down);
        assert_eq!(dividers[1].rect, Rect { x: 0.5, y: 0.5, w: 0.5, h: 0.0 });
        assert_eq!(dividers[1].space, Rect { x: 0.5, y: 0.0, w: 0.5, h: 1.0 });

        // Dragging the inner one moves only the two panes it divides.
        assert!(tiling.set_ratio(1, 0.25));
        assert_eq!(tiling.rect_of(P[0]), Some(Rect { x: 0.0, y: 0.0, w: 0.5, h: 1.0 }));
        assert_eq!(tiling.rect_of(P[1]), Some(Rect { x: 0.5, y: 0.0, w: 0.5, h: 0.25 }));
        assert_eq!(tiling.rect_of(P[2]), Some(Rect { x: 0.5, y: 0.25, w: 0.5, h: 0.75 }));
        assert_tiles(&tiling);
        assert!(!tiling.set_ratio(2, 0.5), "there is no third split");
    }

    #[test]
    fn ratios_clamp() {
        let mut tiling = Tiling::default();
        tiling.split(P[0], Direction::Right);
        assert!(tiling.set_ratio(0, -3.0));
        assert_eq!(tiling.rect_of(P[0]).unwrap().w, RATIO_MIN);
        assert!(tiling.set_ratio(0, 7.0));
        assert_eq!(tiling.rect_of(P[0]).unwrap().w, RATIO_MAX);
        assert!(!tiling.set_ratio(0, f32::NAN));
        assert_eq!(tiling.rect_of(P[0]).unwrap().w, RATIO_MAX, "a ratio that is no number changes nothing");
        assert_tiles(&tiling);
    }

    #[test]
    fn a_drag_keeps_every_pane_at_least_200_wide_and_120_tall() {
        // 0 | (1 | 2) in an area 1000 wide: the outer divider leaves 200 for
        // pane 0 and 400 for the two panes right of it.
        let mut tiling = Tiling::default();
        tiling.split(P[0], Direction::Right);
        tiling.split(P[1], Direction::Right);
        assert!(tiling.drag_ratio(0, 0.0, 1000.0, 600.0));
        assert!((tiling.rect_of(P[0]).unwrap().w - 0.2).abs() < 1e-6);
        assert!(tiling.drag_ratio(0, 1.0, 1000.0, 600.0));
        assert!((tiling.rect_of(P[0]).unwrap().w - 0.6).abs() < 1e-6);
        // The inner one divides the 400 px that leaves: no room to move.
        assert!(tiling.drag_ratio(1, 0.9, 1000.0, 600.0));
        assert!((tiling.rect_of(P[1]).unwrap().w - 0.2).abs() < 1e-4);
        assert!((tiling.rect_of(P[2]).unwrap().w - 0.2).abs() < 1e-4);

        // Stacked panes keep 120 px each.
        let mut tiling = Tiling::default();
        tiling.split(P[0], Direction::Down);
        assert!(tiling.drag_ratio(0, 0.0, 1000.0, 600.0));
        assert!((tiling.rect_of(P[0]).unwrap().h - 0.2).abs() < 1e-6);
        assert!(tiling.drag_ratio(0, 1.0, 1000.0, 600.0));
        assert!((tiling.rect_of(P[1]).unwrap().h - 0.2).abs() < 1e-6);

        // An area too small for both halves leaves the split where it is.
        let before = tiling.clone();
        assert!(!tiling.drag_ratio(0, 0.3, 1000.0, 200.0));
        assert_eq!(tiling, before);
        assert!(!tiling.drag_ratio(5, 0.3, 1000.0, 600.0));
    }

    #[test]
    fn serde_round_trip() {
        for tiling in every_tiling_of(MAX_PANES).into_iter().chain(every_tiling_of(2)).chain([Tiling::default()]) {
            let mut tiling = tiling;
            tiling.set_ratio(0, 0.3);
            let json = serde_json::to_string(&tiling).unwrap();
            let back: Tiling = serde_json::from_str(&json).unwrap();
            assert_eq!(back, tiling);
            assert_eq!(back.clone().sanitized(), tiling, "a tiling this module made is left as it is");
        }
        let mut tiling = Tiling::default();
        tiling.split(P[0], Direction::Down);
        assert_eq!(
            serde_json::to_value(&tiling).unwrap(),
            serde_json::json!({"split": {"direction": "down", "ratio": 0.5, "first": {"pane": 0}, "second": {"pane": 1}}})
        );
    }

    #[test]
    fn a_saved_layout_round_trips_and_is_made_safe_to_restore() {
        let (store_id, node_id, other) = (pimble_core::StoreId::new(), pimble_core::NodeId::new(), pimble_core::NodeId::new());
        let mut tiling = Tiling::default();
        tiling.split(P[0], Direction::Right);
        tiling.set_ratio(0, 0.3);
        let saved = SavedPanes {
            tiling,
            focused: P[1],
            documents: vec![
                SavedDocument { pane: P[0], store_id, node_id },
                SavedDocument { pane: P[1], store_id, node_id },
            ],
        };
        let json = serde_json::to_string(&saved).unwrap();
        let back: SavedPanes = serde_json::from_str(&json).unwrap();
        assert_eq!(back, saved, "the same note in two panes is two entries");
        assert_eq!(back.clone().sanitized(), saved);

        // A pane the tiling does not show holds nothing, a pane is named
        // once, and the focus is on a pane that is shown.
        let mut odd = saved.clone();
        odd.focused = P[3];
        odd.documents.push(SavedDocument { pane: P[2], store_id, node_id: other });
        odd.documents.push(SavedDocument { pane: P[0], store_id, node_id: other });
        let safe = odd.sanitized();
        assert_eq!(safe.focused, P[0]);
        assert_eq!(safe.documents, saved.documents);

        // A file from before the split view has no `panes`; one with only a
        // tiling is the tiling with nothing in it.
        let bare: SavedPanes = serde_json::from_value(serde_json::json!({"tiling": {"pane": 0}})).unwrap();
        assert_eq!(bare, SavedPanes::default());
    }

    #[test]
    fn a_tiling_read_from_disk_is_made_safe_to_draw() {
        let twice: Tiling = serde_json::from_value(serde_json::json!(
            {"split": {"direction": "right", "ratio": 0.5, "first": {"pane": 1}, "second": {"pane": 1}}}
        ))
        .unwrap();
        assert_eq!(twice.sanitized(), Tiling::default(), "a slot named twice");
        let unknown: Tiling = serde_json::from_value(serde_json::json!(
            {"split": {"direction": "right", "ratio": 0.5, "first": {"pane": 0}, "second": {"pane": 9}}}
        ))
        .unwrap();
        assert_eq!(unknown.sanitized(), Tiling::default(), "a slot there is not");
        let wild: Tiling = serde_json::from_value(serde_json::json!(
            {"split": {"direction": "right", "ratio": 40.0, "first": {"pane": 0}, "second": {"pane": 2}}}
        ))
        .unwrap();
        let safe = wild.sanitized();
        assert_eq!(safe.rect_of(P[0]).unwrap().w, RATIO_MAX);
        assert_tiles(&safe);
    }
}

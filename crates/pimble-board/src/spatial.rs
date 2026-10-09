//! The spatial index (contract section 8): a uniform grid of board-unit
//! cells over the visible elements' bounds, rebuilt with every snapshot, so a
//! view of a board of thousands of elements visits only what it touches. An
//! element covering very many cells, and every global one (a spotlight),
//! is kept in a list that every query returns.

use std::collections::HashMap;

use crate::model::Rect;
use crate::snapshot::Element;

/// A cell's side, in board units.
const CELL: f32 = 512.0;
/// An element covering more cells than this is not put in cells.
const MAX_CELLS: i64 = 256;

#[derive(Debug, Default)]
pub(crate) struct Grid {
    cells: HashMap<(i32, i32), Vec<u32>>,
    always: Vec<u32>,
}

fn cell_range(r: &Rect) -> (i32, i32, i32, i32) {
    let c = |v: f32| (v / CELL).floor().clamp(i32::MIN as f32 / 2.0, i32::MAX as f32 / 2.0) as i32;
    (c(r.x), c(r.y), c(r.right()), c(r.bottom()))
}

impl Grid {
    pub(crate) fn build(elements: &[Element]) -> Grid {
        let mut grid = Grid::default();
        for (i, e) in elements.iter().enumerate() {
            if !e.visible || !e.has_extent || !e.bounds.is_finite() {
                continue;
            }
            if e.global {
                grid.always.push(i as u32);
                continue;
            }
            let (x0, y0, x1, y1) = cell_range(&e.bounds);
            let count = (x1 as i64 - x0 as i64 + 1) * (y1 as i64 - y0 as i64 + 1);
            if count > MAX_CELLS {
                grid.always.push(i as u32);
                continue;
            }
            for cy in y0..=y1 {
                for cx in x0..=x1 {
                    grid.cells.entry((cx, cy)).or_default().push(i as u32);
                }
            }
        }
        grid
    }

    /// Visible elements whose bounds meet `rect` (and every global one), in
    /// z-order.
    pub(crate) fn query(&self, rect: &Rect, elements: &[Element]) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        let (x0, y0, x1, y1) = cell_range(rect);
        let cells = (x1 as i64 - x0 as i64 + 1) * (y1 as i64 - y0 as i64 + 1);
        if cells > self.cells.len() as i64 {
            // A view wider than the occupied cells: walk the cells instead.
            for list in self.cells.values() {
                out.extend(list.iter().map(|i| *i as usize));
            }
        } else {
            for cy in y0..=y1 {
                for cx in x0..=x1 {
                    if let Some(list) = self.cells.get(&(cx, cy)) {
                        out.extend(list.iter().map(|i| *i as usize));
                    }
                }
            }
        }
        out.extend(self.always.iter().map(|i| *i as usize));
        out.sort_unstable();
        out.dedup();
        out.retain(|i| {
            let e = &elements[*i];
            e.global || e.bounds.intersects(rect)
        });
        out
    }
}

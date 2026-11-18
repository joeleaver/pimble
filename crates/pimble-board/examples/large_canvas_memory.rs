//! What a view of a large canvas holds while a paint layer grows by remote
//! edits (docs/DRAWING_CONTRACT.md "Large canvases", "Memory"): a
//! 1,920 × 1,080 `BoardView` fitted to a 11.8 × 11 km canvas, 50 edits of
//! 500 single-point dabs each made on another copy of the board and applied
//! here as the app applies them (`Board::watch`, `BoardView::update`, a
//! frame), then a thumbnail of the layer. Live heap bytes are counted by
//! the allocator.
//!
//!   cargo run -p pimble-board --release --example large_canvas_memory

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use pimble_board::{brush, render_paint_layer, Board, BoardView, Color, NewStroke, NoPictures, Rect, Stamp};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if new > l.size() {
            let now = LIVE.fetch_add(new - l.size(), Ordering::Relaxed) + new - l.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(l.size() - new, Ordering::Relaxed);
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

fn mb(n: usize) -> String {
    format!("{:.0} MB", n as f64 / (1 << 20) as f64)
}

fn main() {
    let stamp = Stamp::new(None, "2026-11-18T10:00:00.000Z");
    let painter = Board::standalone();
    let (_, first) = painter.edit(stamp, |t| {
        t.create()?;
        t.paint_layer_for(None)
    })
    .into_result()
    .unwrap();
    let layer = painter.snapshot().layers.iter().find(|l| l.kind == pimble_board::LayerKind::Paint).unwrap().id.clone();
    let mut doc = pimble_crdt::NodeDoc::new();
    doc.apply_update(&first.unwrap()).unwrap();
    let here = Board::of_node(&doc);
    let watch = here.watch().unwrap();
    let mut view = BoardView::new(1920, 1080, Color::WHITE);
    view.set_snapshot(here.snapshot());
    view.fit(&Rect::new(0.0, 0.0, 11800.0, 11000.0), 20.0);
    let _ = view.frame(&NoPictures);
    println!("start: live {}", mb(LIVE.load(Ordering::Relaxed)));
    let mut seed = 7u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as f32 / (1u64 << 31) as f32
    };
    for batch in 1..=50 {
        let (_, update) = painter
            .edit(stamp, |t| {
                for _ in 0..500 {
                    let (x, y) = (next() * 11800.0, next() * 11000.0);
                    t.add_stroke(NewStroke::new(&layer, &brush::ROUND, Color::rgb(40, 120, 40)).with_size(20.0).with_points(vec![[x, y, 1.0, 0.0]]))?;
                }
                Ok(())
            })
            .into_result()
            .unwrap();
        doc.apply_update(&update.unwrap()).unwrap();
        view.update(&here, &watch.take());
        let _ = view.frame(&NoPictures);
        if batch % 10 == 0 {
            println!(
                "{:>5} dabs: live {}, peak {}, paint cache {}",
                batch * 500,
                mb(LIVE.load(Ordering::Relaxed)),
                mb(PEAK.load(Ordering::Relaxed)),
                mb(view.paint_cache().memory_bytes())
            );
        }
    }
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    let thumb = render_paint_layer(view.snapshot(), &layer, &NoPictures, 48.0 / 11800.0, None);
    drop(thumb);
    println!(
        "thumbnail: live {}, peak {}, paint cache {}",
        mb(LIVE.load(Ordering::Relaxed)),
        mb(PEAK.load(Ordering::Relaxed)),
        mb(view.paint_cache().memory_bytes())
    );
}

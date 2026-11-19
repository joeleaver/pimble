//! A board read once and resolved for drawing: z-order, effective layers and
//! parents, world transforms (a member moves with its frame or group), step
//! numbers (derived, contract section 3), routed connectors, text layout,
//! bounds, visibility, a fingerprint per element for the raster cache, and a
//! spatial index. Built from the document, never stored.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use tiny_skia::{Path, Transform as Affine};

use crate::clip::Clip;
use crate::doc::{Board, Changes, RawBoard, RawElement};
use crate::paint_ops::{points_bounds, settings_hash, PaintBody, PaintOp, RawPaintOp};
use crate::tiles::PaintCache;
use crate::layer_tree::LayerTree;
use crate::model::*;
use crate::options::{author_color, CardLabel, ViewOptions, UNAUTHORED_COLOR};
use crate::spatial::Grid;
use crate::text::{self, TextLayout};

/// The offset of a drop shadow, in board units (tiny-skia has no blur, so a
/// shadow is the shape's fill, offset and translucent).
pub const SHADOW_OFFSET: f32 = 4.0;
/// A sticky note's padding as a multiple of its font size.
pub const STICKY_PADDING: f32 = 0.6;
/// A step badge's radius as a multiple of its font size.
pub const STEP_RADIUS: f32 = 0.75;

/// One layer, resolved (docs/DRAWING_CONTRACT.md section 1): a vector
/// layer, a paint layer or a group, read in the tree
/// [`crate::layer_tree::resolve`] makes.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub id: String,
    pub name: String,
    /// Its order among its siblings.
    pub order: String,
    /// Its own flag; [`Layer::effective_visible`] says whether it shows.
    pub visible: bool,
    /// Its own flag; [`Layer::effective_locked`] says whether it is locked.
    pub locked: bool,
    pub opacity: f32,
    /// How many elements it holds (none for a group); a paint layer: how
    /// many paint operations.
    pub count: usize,
    /// What it is for (`doc` module, "Meaning").
    pub note: Option<String>,
    pub fields: Vec<(String, String)>,
    pub kind: LayerKind,
    /// Its blend mode as read: the kind's default when it names none.
    pub blend: Blend,
    /// The group it is read inside (a dangling or cyclic `parent` reads as
    /// top level), by id.
    pub parent: Option<String>,
    /// 0 at top level, 1 inside a top-level group, and so on.
    pub depth: usize,
    /// It and every group above it are visible.
    pub effective_visible: bool,
    /// It or a group above it is locked.
    pub effective_locked: bool,
    /// The layer whose own lock locks it (itself, or the nearest locked
    /// group above it).
    pub locked_by: Option<String>,
    /// Drawn into a buffer of its own and then blended (a blend that is not
    /// normal, opacity below 1; a group that does not pass through at full
    /// opacity; a clip in effect).
    pub isolated: bool,
    /// The element that clips it, when that clip is in effect
    /// ([`crate::clip`]: a closed element of this canvas, not inside this
    /// layer).
    pub clip: Option<String>,
    /// The element its `clip` key names, as stored, in effect or not.
    pub clip_named: Option<String>,
}

/// One element, as stored and as resolved for drawing.
#[derive(Debug)]
pub struct Element {
    pub raw: RawElement,
    /// The layer it is drawn in: its own, or the top layer when its layer
    /// was deleted while it was being added.
    pub layer: String,
    /// Its parent when that names an element (and makes no cycle).
    pub parent: Option<String>,
    pub style: ResolvedStyle,
    /// Derived step number (steps only): per layer, by `created_at` then id.
    pub step_number: Option<u32>,
    /// Its own geometry's extent before any transform (the pivot of its
    /// rotation and scale).
    pub pivot_bounds: Rect,
    /// Element space to board space: its parent's world transform, then its
    /// own.
    pub world: Affine,
    /// Everything it paints, in element space (strokes' widths, heads,
    /// labels and shadows included).
    pub local_bounds: Rect,
    /// `local_bounds` through `world`, axis-aligned, in board units. A
    /// connector's is in board units directly.
    pub bounds: Rect,
    /// Drawn: its layer and every group above it are visible, and it is a
    /// picture or the markup shows.
    pub visible: bool,
    /// Its style's opacity. A layer's opacity and blend apply to the layer
    /// as a whole, composited by the renderer.
    pub opacity: f32,
    /// The index (into [`BoardSnapshot::layers`]) of the layer it is drawn
    /// in.
    pub layer_index: Option<usize>,
    /// A connector's two ends, routed, in board units.
    pub route: Option<(Point, Point)>,
    /// Text laid out at the element's font size (kinds with text, and a
    /// step's number).
    pub layout: Option<Arc<TextLayout>>,
    /// The box text is laid out in, element space.
    pub text_box: Option<Rect>,
    /// A sticky note's rectangle, grown to fit its text.
    pub grown: Option<Rect>,
    /// Paints outside its bounds (a spotlight dims the whole board).
    pub global: bool,
    /// Has anything to show: false for an empty group, a stroke with no
    /// points yet, a kind this version does not know.
    pub has_extent: bool,
    /// A card's label from [`ViewOptions::card_labels`], when the app gave
    /// one for its link.
    pub card: Option<CardLabel>,
    /// Changes whenever anything about how it looks changes.
    pub fingerprint: u64,
    pub(crate) text_path: OnceLock<Option<Path>>,
    /// Its text set along another element (a text whose `along` names a
    /// path, line or stroke: [`crate::along`]); its `world` is then the
    /// identity and its boxes are the placed glyphs', in board units.
    pub along: Option<Arc<crate::along::Placed>>,
    /// The clips of its layer and of every group above it: it shows (and
    /// is hit) only inside all of them.
    pub(crate) clips: Vec<Arc<Clip>>,
    /// A scatter's symbol and its chunks' bounds
    /// (docs/DRAWING_WAVE6_CONTRACT.md section 2).
    pub scatter: Option<Arc<crate::scatter::ResolvedScatter>>,
}

impl Element {
    pub fn id(&self) -> &str {
        &self.raw.id
    }

    pub fn kind(&self) -> &Kind {
        &self.raw.kind
    }

    /// Whether board point `p` is inside every clip it is drawn inside (its
    /// layer's and its groups'): outside them it shows nothing and is not
    /// hit.
    pub fn inside_clips(&self, p: Point) -> bool {
        self.clips.iter().all(|c| c.contains(p))
    }

    /// Whether it draws inside a clip.
    pub fn is_clipped(&self) -> bool {
        !self.clips.is_empty()
    }

    /// Its bounds cut to its clips' (what it can show at most); `None` when
    /// its clips leave nothing of it.
    pub fn shown_bounds(&self) -> Option<Rect> {
        self.clips.iter().try_fold(self.bounds, |b, c| b.intersect(&c.bounds))
    }

    /// The glyph outlines of its text in element space, built once.
    pub(crate) fn text_path(&self) -> Option<&Path> {
        self.text_path
            .get_or_init(|| {
                let layout = self.layout.as_ref()?;
                let tb = self.text_box?;
                layout.path(Point::new(tb.x, tb.y), tb.w, self.style.align)
            })
            .as_ref()
    }
}

/// A board ready to draw. See the module doc.
#[derive(Debug)]
pub struct BoardSnapshot {
    pub exists: bool,
    pub version: i64,
    pub page: Option<Page>,
    pub markup_visible: bool,
    pub edited_by: Option<Author>,
    pub edited_at: Option<String>,
    /// Every layer entry, groups included, in draw order bottom to top:
    /// depth first, a group right after the layers it holds (so read in
    /// reverse, a group comes before its layers, as a panel lists them).
    pub layers: Vec<Layer>,
    /// Bottom to top: by layer order, then element order, ties by id.
    pub elements: Vec<Element>,
    /// The symbols defined in the canvas (docs/DRAWING_WAVE6_CONTRACT.md
    /// section 2), by when they were made.
    pub symbols: Vec<Arc<crate::scatter::SymbolDef>>,
    /// Element index by a hash of its id (checked against the id on lookup).
    index: HashMap<u64, usize>,
    grid: Grid,
    options: ViewOptions,
    /// The layers' tree; its entries are `layers`, index for index.
    tree: LayerTree,
    /// Per layer (as `layers`), the isolated layers it draws through,
    /// outermost first: the buffers the renderer opens.
    pub(crate) chains: Vec<Vec<usize>>,
    /// Per layer (as `layers`), a hash of how it is composited and where it
    /// is in the draw order.
    pub(crate) composite: Vec<u64>,
    /// Per layer (as `layers`), its clip when one is in effect.
    pub(crate) clips: Vec<Option<Arc<Clip>>>,
    /// Every paint operation, by layer id, then order, then id
    /// (docs/DRAWING_CONTRACT.md section 3).
    pub paint: Vec<PaintOp>,
    /// Per paint layer id: its operations' range in `paint` and the union of
    /// their bounds.
    paint_index: HashMap<String, (std::ops::Range<usize>, Option<Rect>)>,
    /// The tiles paint layers are drawn from: kept by every snapshot this
    /// one is brought up to date into.
    paint_cache: Arc<PaintCache>,
}

/// One paint operation as a raster cache follows it ([`crate::BoardView`]):
/// a key, a fingerprint of everything that changes its pixels (its layer's
/// compositing and visibility included), the same without its points, how
/// many points it has, and where it reaches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaintMark {
    pub key: u64,
    pub fingerprint: u64,
    pub look: u64,
    pub points: usize,
    pub bounds: Rect,
    /// Its index in [`BoardSnapshot::paint`].
    pub index: usize,
}

impl BoardSnapshot {
    /// A board that does not exist: nothing to draw.
    pub fn empty() -> Self {
        Self::build(None)
    }

    pub fn build(raw: Option<RawBoard>) -> Self {
        Self::build_with(raw, ViewOptions::default())
    }

    /// [`BoardSnapshot::build`] seen with `options`.
    pub fn build_with(raw: Option<RawBoard>, options: ViewOptions) -> Self {
        let Some(raw) = raw else {
            return BoardSnapshot {
                exists: false,
                version: crate::doc::VERSION,
                page: None,
                markup_visible: true,
                edited_by: None,
                edited_at: None,
                layers: Vec::new(),
                elements: Vec::new(),
                symbols: Vec::new(),
                index: HashMap::new(),
                grid: Grid::default(),
                options,
                tree: LayerTree::default(),
                chains: Vec::new(),
                composite: Vec::new(),
                clips: Vec::new(),
                paint: Vec::new(),
                paint_index: HashMap::new(),
                paint_cache: PaintCache::new(),
            };
        };
        Resolver::run(raw, HashMap::new(), None, options)
    }

    /// The viewer's choices this snapshot is drawn with.
    pub fn options(&self) -> &ViewOptions {
        &self.options
    }

    /// This board read again from `board` with other view options. Nothing
    /// is reused (options change rarely, and a card's title may change with
    /// them).
    pub fn with_options(self, options: ViewOptions, board: &Board) -> Self {
        let cache = self.paint_cache;
        Self::build_with(board.read(), options).with_paint_cache(cache)
    }

    /// [`BoardSnapshot::build`], keeping what this snapshot already worked
    /// out (text layout, glyph outlines) for every element that has not
    /// changed.
    pub fn rebuild(self, raw: Option<RawBoard>) -> Self {
        let options = self.options;
        let cache = self.paint_cache.clone();
        let Some(raw) = raw else { return Self::build_with(None, options).with_paint_cache(cache) };
        let mut prev: HashMap<String, Element> = self.elements.into_iter().map(|e| (e.raw.id.clone(), e)).collect();
        let mut reuse = HashMap::new();
        for e in &raw.elements {
            if let Some(old) = prev.remove(&e.id) {
                if old.raw == *e {
                    reuse.insert(e.id.clone(), Reuse::of(old));
                }
            }
        }
        Resolver::run(raw, reuse, None, options).with_paint_cache(cache)
    }

    /// Bring this snapshot up to date with `board` given what changed
    /// ([`Board::watch`]): only the changed elements are read again, and
    /// the others keep their text layout. What depends on other elements
    /// (z-order, members' positions, connectors, step numbers, the index)
    /// is resolved again for all of them.
    pub fn update(self, board: &Board, changes: &Changes) -> Self {
        if changes.is_empty() && self.exists {
            return self;
        }
        // A symbol being edited is drawn alone, from its definition: read
        // again whole.
        if changes.all || !self.exists || self.options.editing_symbol.is_some() {
            return self.rebuild(board.read());
        }
        let paint = if changes.paint.is_empty() { None } else { Some(board.read_paint_some(&changes.paint)) };
        if !changes.board && changes.elements.is_empty() {
            // Only paint changed (a stroke being drawn): nothing about the
            // elements needs resolving again.
            let mut next = self;
            let ops = merge_paint(std::mem::take(&mut next.paint), paint.unwrap_or_default());
            next.set_paint(ops);
            return next;
        }
        let options = self.options;
        let cache = self.paint_cache.clone();
        let old_paint = self.paint;
        let Some((mut raw, changed)) = board.read_some(&changes.elements) else { return Self::build_with(None, options).with_paint_cache(cache) };
        let mut reuse = HashMap::new();
        let mut fresh: HashMap<String, Option<RawElement>> = changed.into_iter().collect();
        let mut elements = Vec::with_capacity(self.elements.len() + fresh.len());
        for old in self.elements {
            match fresh.remove(&old.raw.id) {
                None => {
                    // Unchanged: keep it as it was read.
                    let id = old.raw.id.clone();
                    let (raw_el, r) = Reuse::split(old);
                    reuse.insert(id, r);
                    elements.push(raw_el);
                }
                Some(Some(new)) => {
                    if new == old.raw {
                        reuse.insert(new.id.clone(), Reuse::of(old));
                    }
                    elements.push(new);
                }
                Some(None) => {}
            }
        }
        // Elements this snapshot did not have.
        elements.extend(fresh.into_values().flatten());
        raw.elements = elements;
        let ops = merge_paint(old_paint, paint.unwrap_or_default());
        Resolver::run(raw, reuse, Some(ops), options).with_paint_cache(cache)
    }

    // ── Paint ────────────────────────────────────────────────────────────

    /// This snapshot drawing its paint layers from `cache` (the app's, with
    /// a disk store; or the cache of the view it is shown in).
    pub fn with_paint_cache(mut self, cache: Arc<PaintCache>) -> Self {
        self.paint_cache = cache;
        self
    }

    /// The cache its paint layers are drawn from.
    pub fn paint_cache(&self) -> &Arc<PaintCache> {
        &self.paint_cache
    }

    pub(crate) fn set_paint_cache(&mut self, cache: Arc<PaintCache>) {
        self.paint_cache = cache;
    }

    /// The operations of paint layer `layer`, bottom to top.
    pub fn paint_ops(&self, layer: &str) -> &[PaintOp] {
        match self.paint_index.get(layer) {
            Some((range, _)) => &self.paint[range.clone()],
            None => &[],
        }
    }

    /// The union of where the operations of paint layer `layer` reach.
    pub fn paint_bounds(&self, layer: &str) -> Option<Rect> {
        self.paint_index.get(layer).and_then(|(_, b)| *b)
    }

    /// Paint operation `id`.
    pub fn paint_op(&self, id: &str) -> Option<&PaintOp> {
        self.paint.iter().find(|o| o.raw.id == id)
    }

    /// Whether layer `at` (an index into `layers`) is a paint layer that
    /// shows: it and its groups shown and the markup shown, or the viewer
    /// showing everything.
    pub fn paint_shown(&self, at: usize) -> bool {
        let Some(l) = self.layers.get(at) else { return false };
        let show_all = self.options.show_all;
        l.kind == LayerKind::Paint && (l.effective_visible || show_all) && (self.markup_visible || show_all)
    }

    /// The paint layers that show and reach `region`, as indices into
    /// `layers`, in draw order.
    pub fn paint_layers_in(&self, region: &Rect) -> Vec<usize> {
        (0..self.layers.len())
            .filter(|at| self.paint_shown(*at) && self.paint_bounds(&self.layers[*at].id).is_some_and(|b| b.intersects(region)))
            .collect()
    }

    /// Every paint operation as a raster cache follows it (see [`PaintMark`]).
    pub fn paint_marks(&self) -> Vec<PaintMark> {
        let at_of: HashMap<&str, usize> = self.layers.iter().enumerate().map(|(i, l)| (l.id.as_str(), i)).collect();
        self.paint
            .iter()
            .enumerate()
            .filter_map(|(index, op)| {
                let bounds = op.bounds?;
                let at = *at_of.get(op.raw.layer.as_str())?;
                let mut h = Fx::default();
                self.composite.get(at).hash(&mut h);
                self.paint_shown(at).hash(&mut h);
                op.raw.order.hash(&mut h);
                let (look, points) = match &op.raw.body {
                    PaintBody::Stroke(s) => {
                        settings_hash(&s.settings, s.color, s.blend).hash(&mut h);
                        (h.finish(), s.points.len())
                    }
                    // A smudge or blur that only grew changes only where its
                    // new points reach, unless what is below it changed.
                    PaintBody::Retouch(r) => {
                        settings_hash(&r.settings, Color::TRANSPARENT, crate::brush::StrokeBlend::Normal).hash(&mut h);
                        r.effect.hash(&mut h);
                        r.strength.to_bits().hash(&mut h);
                        op.below.hash(&mut h);
                        (h.finish(), r.points.len())
                    }
                    _ => {
                        op.effective.hash(&mut h);
                        (h.finish(), 0)
                    }
                };
                let mut f = Fx::default();
                look.hash(&mut f);
                op.effective.hash(&mut f);
                Some(PaintMark { key: id_hash(&op.raw.id), fingerprint: f.finish(), look, points, bounds, index })
            })
            .collect()
    }

    /// Where the points of stroke `index` (in `paint`) from point `from` on
    /// reach: what an appended stroke changed.
    pub fn paint_tail_bounds(&self, index: usize, from: usize) -> Option<Rect> {
        let op = self.paint.get(index)?;
        match &op.raw.body {
            PaintBody::Stroke(s) => points_bounds(&s.points[from.saturating_sub(1).min(s.points.len())..]).map(|r| r.inflate(s.settings.reach())),
            PaintBody::Retouch(r) => points_bounds(&r.points[from.saturating_sub(1).min(r.points.len())..])
                .map(|b| b.inflate(r.settings.reach() + crate::paint_ops::RETOUCH_MARGIN + crate::paint_ops::BLUR_MAX_KERNEL_PX as f32 / crate::brush::PX_PER_UNIT)),
            _ => op.bounds,
        }
    }

    /// The `pimble-blob:` URLs the board draws: its pictures' and its paint
    /// layers' pixels.
    pub fn picture_srcs(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let elements = self.elements.iter().filter_map(|e| match &e.raw.geom {
            Geom::Picture(p) => Some(p.src.clone()),
            _ => None,
        });
        let paint = self.paint.iter().filter_map(|o| match &o.raw.body {
            PaintBody::Pixels(p) => Some(p.src.clone()),
            _ => None,
        });
        for src in elements.chain(paint) {
            if seen.insert(src.clone()) {
                out.push(src);
            }
        }
        out
    }

    /// Take `ops` as this snapshot's paint operations: sorted, indexed, and
    /// the paint layers' counts set.
    fn set_paint(&mut self, ops: Vec<PaintOp>) {
        let (ops, index) = index_paint(ops);
        self.paint = ops;
        self.paint_index = index;
        for l in &mut self.layers {
            if l.kind == LayerKind::Paint {
                l.count = self.paint_index.get(&l.id).map_or(0, |(r, _)| r.len());
            }
        }
    }

    /// Symbol `id`'s definition, resolved.
    pub fn symbol(&self, id: &str) -> Option<&Arc<crate::scatter::SymbolDef>> {
        self.symbols.iter().find(|s| s.id == id)
    }

    pub fn get(&self, id: &str) -> Option<&Element> {
        self.index.get(&id_hash(id)).map(|i| &self.elements[*i]).filter(|e| e.raw.id == id)
    }

    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// The layers' tree (entries index for index with `layers`).
    pub fn layer_tree(&self) -> &LayerTree {
        &self.tree
    }

    /// The layers group `id` holds, bottom to top (the top-level layers
    /// when `None`).
    pub fn children_of(&self, id: Option<&str>) -> Vec<&Layer> {
        let list = match id {
            None => &self.tree.roots,
            Some(id) => match self.layers.iter().position(|l| l.id == id) {
                Some(at) => &self.tree.entries[at].children,
                None => return Vec::new(),
            },
        };
        list.iter().map(|at| &self.layers[*at]).collect()
    }

    /// The top vector layer (where elements whose layer is gone are drawn).
    pub fn top_layer(&self) -> Option<&Layer> {
        self.tree.fallback().map(|at| &self.layers[at])
    }

    /// Where new elements go when nothing names a layer: the top vector
    /// layer that is not locked and not inside a hidden group
    /// ([`crate::layer_tree::LayerTree::default_target`]).
    pub fn default_layer(&self) -> Option<&Layer> {
        self.tree.default_target().map(|at| &self.layers[at])
    }

    /// How many elements the markup toggle hides (contract section 5:
    /// "Markup · 12"): every element that is not a picture or a group.
    pub fn markup_count(&self) -> usize {
        self.elements.iter().filter(|e| !e.raw.kind.is_picture() && !matches!(e.raw.kind, Kind::Group)).count()
    }

    /// The distinct authors of its elements (contract section 6:
    /// attribution shows only with two or more).
    pub fn authors(&self) -> Vec<Author> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in &self.elements {
            if let Some(a) = &e.raw.author {
                if seen.insert(a.id.clone()) {
                    out.push(a.clone());
                }
            }
        }
        out
    }

    /// The visible extent (contract section 1): the union of the bounds of
    /// every element the document shows, stroke widths and shadows
    /// included; `None` when nothing shows. Not yet trimmed to pixels: that
    /// is [`crate::render_extent`].
    pub fn visible_extent(&self) -> Option<Rect> {
        let paint = (0..self.layers.len())
            .filter(|at| self.paint_shown(*at))
            .filter_map(|at| self.paint_bounds(&self.layers[at].id).and_then(|b| self.clip_bounds(at, b)));
        self.elements
            .iter()
            .filter(|e| e.visible && e.has_extent && !matches!(e.raw.kind, Kind::Group))
            .filter_map(|e| e.shown_bounds())
            .chain(paint)
            .filter(|b| b.is_finite())
            .reduce(|a, b| a.union(&b))
    }

    /// `bounds` of something layer `at` draws, cut to the clips it draws
    /// inside (its own and its groups'); `None` when they leave nothing.
    pub fn clip_bounds(&self, at: usize, bounds: Rect) -> Option<Rect> {
        if self.clips.is_empty() {
            return Some(bounds);
        }
        std::iter::once(at)
            .chain(self.tree.ancestors(at))
            .filter_map(|a| self.clips.get(a).and_then(|c| c.as_ref()))
            .try_fold(bounds, |b, c| b.intersect(&c.bounds))
    }

    /// The clip in effect on layer `at` (an index into `layers`).
    pub fn clip_of(&self, at: usize) -> Option<&Clip> {
        self.clips.get(at).and_then(|c| c.as_deref())
    }

    /// What a board renders as when embedded or exported: its page when it
    /// has one, else its visible extent.
    pub fn render_rect(&self) -> Option<Rect> {
        match &self.page {
            Some(page) => Some(page.rect.normalized()),
            None => self.visible_extent(),
        }
    }

    /// The indices (into `elements`, so in z-order) of the visible elements
    /// whose bounds meet `rect`, plus every visible global one.
    pub fn query(&self, rect: &Rect) -> Vec<usize> {
        self.grid.query(rect, &self.elements)
    }

    /// The text on the board, element by element (for search and the MCP):
    /// (element id, kind, text), skipping empty text.
    pub fn texts(&self) -> Vec<(String, Kind, String)> {
        self.elements
            .iter()
            .filter_map(|e| {
                let t = e.raw.text.as_deref()?.trim();
                (!t.is_empty()).then(|| (e.raw.id.clone(), e.raw.kind.clone(), t.to_string()))
            })
            .collect()
    }

    /// The topmost visible element under `p` (by bounds), for hit testing.
    pub fn hit(&self, p: Point) -> Option<&Element> {
        let r = Rect::new(p.x, p.y, 0.001, 0.001);
        self.query(&r).into_iter().rev().map(|i| &self.elements[i]).find(|e| !e.global && e.bounds.contains(p) && e.inside_clips(p))
    }
}

/// What an unchanged element's last resolution can lend the next one.
struct Reuse {
    layout: Option<Arc<TextLayout>>,
    text_box: Option<Rect>,
    grown: Option<Rect>,
    step_number: Option<u32>,
    text_path: Option<Path>,
}

impl Reuse {
    fn of(e: Element) -> Reuse {
        Self::split(e).1
    }

    fn split(e: Element) -> (RawElement, Reuse) {
        let connector = matches!(e.raw.kind, Kind::Connector);
        // A connector's label and a text set along a path are placed in
        // board units from other elements: never lent.
        let text_path = if connector || e.along.is_some() { None } else { e.text_path.into_inner().flatten() };
        let reuse = Reuse { layout: e.layout, text_box: e.text_box, grown: e.grown, step_number: e.step_number, text_path };
        (e.raw, reuse)
    }
}

struct Resolver {
    reuse: HashMap<String, Reuse>,
    layers: Vec<Layer>,
    /// Per element, the layer it is drawn in (index into `layers`).
    homes: Vec<Option<usize>>,
    /// Per layer, a hash of how it is composited (its chain's ids,
    /// opacities and blends) and its place in the draw order, folded into
    /// its elements' fingerprints so the raster cache redraws them.
    composite: Vec<u64>,
    raws: Vec<RawElement>,
    by_id: HashMap<u64, usize>,
    options: ViewOptions,
    symbols: HashMap<String, Arc<crate::scatter::SymbolDef>>,
}

impl Resolver {
    fn run(raw: RawBoard, reuse: HashMap<String, Reuse>, paint: Option<Vec<PaintOp>>, options: ViewOptions) -> BoardSnapshot {
        let raw = match options.editing_symbol.as_deref() {
            Some(symbol) => isolate(raw, symbol),
            None => raw,
        };
        let RawBoard { version, page, markup_visible, edited_by, edited_at, layers: raw_layers, elements, paint: raw_paint, symbols: raw_symbols } = raw;
        // Symbols first: a scatter's extent is its symbol's, placed. Each is
        // resolved as a board of its own, in one layer; a symbol holds no
        // scatter, so this never recurses further.
        let symbol_options = ViewOptions { editing_symbol: None, show_all: true, ..options.clone() };
        let symbols: Vec<Arc<crate::scatter::SymbolDef>> = raw_symbols
            .into_iter()
            .map(|sym| {
                let (created_at, author) = (sym.created_at.clone(), sym.author.clone());
                let inner = RawBoard {
                    version,
                    page: None,
                    markup_visible: true,
                    edited_by: None,
                    edited_at: None,
                    layers: vec![symbol_layer(&sym.id, &sym.name)],
                    elements: sym.elements.into_iter().filter(|e| e.kind != Kind::Scatter).collect(),
                    paint: Vec::new(),
                    symbols: Vec::new(),
                };
                let resolved = Resolver::run(inner, HashMap::new(), Some(Vec::new()), symbol_options.clone());
                Arc::new(crate::scatter::SymbolDef::of(sym.id, sym.name, created_at, author, resolved.elements))
            })
            .collect();
        let symbol_index: HashMap<String, Arc<crate::scatter::SymbolDef>> = symbols.iter().map(|s| (s.id.clone(), s.clone())).collect();
        let paint = paint.unwrap_or_else(|| raw_paint.into_iter().map(PaintOp::of).collect());
        let tree = crate::layer_tree::resolve(&raw_layers);
        let layers: Vec<Layer> = tree
            .entries
            .iter()
            .map(|e| {
                let l = &raw_layers[e.index];
                Layer {
                    id: l.id.clone(),
                    name: l.name.clone(),
                    order: l.order.clone(),
                    visible: l.visible,
                    locked: l.locked,
                    opacity: l.opacity,
                    count: 0,
                    note: l.note.clone(),
                    fields: l.fields.clone(),
                    kind: e.kind.clone(),
                    blend: e.blend,
                    parent: e.parent.map(|p| raw_layers[tree.entries[p].index].id.clone()),
                    depth: e.depth,
                    effective_visible: e.effective_visible,
                    effective_locked: e.effective_locked,
                    locked_by: e.locked_by.map(|b| raw_layers[tree.entries[b].index].id.clone()),
                    isolated: e.isolated,
                    clip: None,
                    clip_named: l.clip.clone(),
                }
            })
            .collect();
        let mut tree = tree;
        let mut chains: Vec<Vec<usize>> = (0..layers.len()).map(|at| tree.chain(at)).collect();
        let composite = composite_of(&chains, &layers, &[]);

        // z-order: by the drawing order of the layers, then element order.
        let mut ranked: Vec<(Option<usize>, RawElement)> =
            elements.into_iter().map(|e| (tree.home_of(&raw_layers, &e.layer), e)).collect();
        ranked.sort_by(|(ra, a), (rb, b)| ra.cmp(rb).then_with(|| a.order.cmp(&b.order)).then_with(|| a.id.cmp(&b.id)));
        let homes: Vec<Option<usize>> = ranked.iter().map(|(h, _)| *h).collect();
        let raws: Vec<RawElement> = ranked.into_iter().map(|(_, e)| e).collect();
        let by_id: HashMap<u64, usize> = raws.iter().enumerate().map(|(i, e)| (id_hash(&e.id), i)).collect();
        let mut r = Resolver { reuse, layers, homes, composite, raws, by_id, options, symbols: symbol_index };
        let mut snapshot_elements = r.resolve(markup_visible);
        let index: HashMap<u64, usize> = snapshot_elements.iter().enumerate().map(|(i, e)| (id_hash(&e.raw.id), i)).collect();
        let mut composite = std::mem::take(&mut r.composite);
        // Clips (docs/DRAWING_WAVE6_CONTRACT.md section 1): read once the
        // elements are placed, since a clip is an element's outline where
        // it is drawn. A clip in effect gives its layer a buffer, and its
        // outline goes into the fingerprints of everything it clips.
        let mut clips: Vec<Option<Arc<Clip>>> = vec![None; r.layers.len()];
        for at in 0..r.layers.len() {
            let Some(id) = r.layers[at].clip_named.as_deref() else { continue };
            let Some(e) = index.get(&id_hash(id)).map(|i| &snapshot_elements[*i]).filter(|e| e.raw.id == id) else { continue };
            // A clip drawn inside the layer or group it clips reads as none.
            if e.layer_index.is_none_or(|home| tree.is_within(home, at)) {
                continue;
            }
            if let Some(clip) = Clip::of(e) {
                clips[at] = Some(Arc::new(clip));
            }
        }
        if clips.iter().any(Option::is_some) {
            for (at, clip) in clips.iter().enumerate() {
                if let Some(clip) = clip {
                    tree.entries[at].isolated = true;
                    r.layers[at].isolated = true;
                    r.layers[at].clip = Some(clip.element.clone());
                }
            }
            chains = (0..r.layers.len()).map(|at| tree.chain(at)).collect();
            let next = composite_of(&chains, &r.layers, &clips);
            // The clips each layer draws inside: its own and its groups'.
            let within: Vec<Vec<Arc<Clip>>> = (0..r.layers.len())
                .map(|at| std::iter::once(at).chain(tree.ancestors(at)).filter_map(|a| clips[a].clone()).collect())
                .collect();
            for e in &mut snapshot_elements {
                if let Some(l) = e.layer_index {
                    e.fingerprint ^= composite[l] ^ next[l];
                    if let Some(sc) = e.scatter.as_mut().and_then(Arc::get_mut) {
                        sc.look ^= composite[l] ^ next[l];
                    }
                    e.clips = within[l].clone();
                }
            }
            composite = next;
        }
        let grid = Grid::build(&snapshot_elements);
        let mut snapshot = BoardSnapshot {
            exists: true,
            version,
            page,
            markup_visible,
            edited_by,
            edited_at,
            layers: r.layers,
            elements: snapshot_elements,
            symbols,
            index,
            grid,
            options: r.options,
            tree,
            chains,
            composite,
            clips,
            paint: Vec::new(),
            paint_index: HashMap::new(),
            paint_cache: PaintCache::new(),
        };
        snapshot.set_paint(paint);
        snapshot
    }

    /// The index of the element `id` names.
    fn find(&self, id: &str) -> Option<usize> {
        self.by_id.get(&id_hash(id)).copied().filter(|i| self.raws[*i].id == id)
    }

    fn resolve(&mut self, markup_visible: bool) -> Vec<Element> {
        let n = self.raws.len();

        // Effective layers (as indices into `layers`), counts, parents (a
        // parent that names nothing, or makes a cycle, reads as none).
        let layer_of: Vec<Option<usize>> = std::mem::take(&mut self.homes);
        for l in layer_of.iter().flatten() {
            self.layers[*l].count += 1;
        }
        let mut parent: Vec<Option<usize>> =
            self.raws.iter().map(|e| e.parent.as_deref().and_then(|p| self.find(p))).collect();
        for i in 0..n {
            // Walk up; a walk longer than n is a cycle: cut this element's link.
            let mut at = parent[i];
            let mut steps = 0;
            while let Some(p) = at {
                if p == i || steps > n {
                    parent[i] = None;
                    break;
                }
                at = parent[p];
                steps += 1;
            }
        }
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, p) in parent.iter().enumerate() {
            if let Some(p) = p {
                children[*p].push(i);
            }
        }

        // Step numbers, per layer, by created_at then id.
        let mut step_number: Vec<Option<u32>> = vec![None; n];
        let mut steps: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
        for (i, e) in self.raws.iter().enumerate() {
            if e.kind == Kind::Step {
                steps.entry(layer_of[i]).or_default().push(i);
            }
        }
        for list in steps.values_mut() {
            list.sort_by(|a, b| {
                let (ea, eb) = (&self.raws[*a], &self.raws[*b]);
                ea.created_at.cmp(&eb.created_at).then_with(|| ea.id.cmp(&eb.id))
            });
            for (k, i) in list.iter().enumerate() {
                step_number[*i] = Some(k as u32 + 1);
            }
        }

        let by_author = self.options.color_by_author;
        let styles: Vec<ResolvedStyle> = self
            .raws
            .iter()
            .map(|e| {
                let mut s = e.style.resolved(&e.kind);
                if by_author && !e.kind.is_picture() {
                    let c = e.author.as_ref().map_or(UNAUTHORED_COLOR, |a| author_color(&a.id));
                    s.stroke = c;
                    s.fill = s.fill.map(|_| Color { a: 0x40, ..c });
                }
                s
            })
            .collect();
        let mut cards: Vec<Option<CardLabel>> = self
            .raws
            .iter()
            .map(|e| match &e.geom {
                Geom::Card { link, .. } if e.kind == Kind::Card => self.options.card_labels.get(link).cloned(),
                _ => None,
            })
            .collect();

        // Text layout and each element's own extent, children before
        // parents (a group's extent is its members').
        let mut layouts: Vec<Option<Arc<TextLayout>>> = Vec::with_capacity(n);
        let mut text_boxes: Vec<Option<Rect>> = Vec::with_capacity(n);
        let mut grown: Vec<Option<Rect>> = Vec::with_capacity(n);
        let mut text_paths: Vec<Option<Path>> = Vec::with_capacity(n);
        let mut pivot: Vec<Option<Rect>> = vec![None; n];
        let mut local: Vec<Option<Rect>> = vec![None; n];
        for i in 0..n {
            let reuse = self.reuse.remove(&self.raws[i].id).filter(|r| r.step_number == step_number[i]);
            match reuse {
                Some(r) => {
                    layouts.push(r.layout);
                    text_boxes.push(r.text_box);
                    grown.push(r.grown);
                    text_paths.push(r.text_path);
                }
                None => {
                    let title = cards[i].as_ref().map(|c| c.title.as_str());
                    let (layout, tb, g) = lay_out(&self.raws[i], &styles[i], step_number[i], title);
                    layouts.push(layout.map(Arc::new));
                    text_boxes.push(tb);
                    grown.push(g);
                    text_paths.push(None);
                }
            }
        }
        let mut order_post = Vec::with_capacity(n);
        {
            // Post-order over the parent forest.
            let mut visited = vec![false; n];
            for root in 0..n {
                if parent[root].is_some() {
                    continue;
                }
                let mut stack = vec![(root, false)];
                while let Some((i, expanded)) = stack.pop() {
                    if expanded {
                        order_post.push(i);
                        continue;
                    }
                    if visited[i] {
                        continue;
                    }
                    visited[i] = true;
                    stack.push((i, true));
                    for c in &children[i] {
                        stack.push((*c, false));
                    }
                }
            }
        }
        let mut scatters: Vec<Option<Arc<crate::scatter::ResolvedScatter>>> = self
            .raws
            .iter()
            .map(|e| {
                let raw = e.scatter.as_ref().filter(|_| e.kind == Kind::Scatter)?;
                Some(Arc::new(crate::scatter::ResolvedScatter::resolve(raw, self.symbols.get(&raw.symbol).cloned())))
            })
            .collect();
        for &i in &order_post {
            let e = &self.raws[i];
            let ink_below = layouts[i].as_ref().map_or(0.0, |l| l.ink_below());
            let scatter_extent = scatters[i].as_ref().map(|sc| sc.extent().map(|r| (r, r)));
            let extent = match scatter_extent {
                Some(extent) => extent,
                None => own_extent(e, &styles[i], text_boxes[i], grown[i], ink_below),
            }
            .or_else(|| {
                if !matches!(e.kind, Kind::Group) {
                    return None;
                }
                // A group's extent: its members', in its own space.
                let mut acc: Option<Rect> = None;
                for c in &children[i] {
                    if let Some(cl) = local[*c] {
                        let m = own_affine(&self.raws[*c].transform, pivot[*c].unwrap_or(cl));
                        let b = map_rect(&m, &cl);
                        acc = Some(acc.map_or(b, |a| a.union(&b)));
                    }
                }
                acc.map(|r| (r, r))
            });
            if let Some((p, mut l)) = extent {
                // A halo reaches past the glyphs.
                if let (Some(h), Some(tb)) = (styles[i].halo, text_boxes[i]) {
                    if !matches!(e.kind, Kind::Connector) {
                        l = l.union(&Rect::new(tb.x, tb.y, tb.w, tb.h + ink_below).inflate(h.width));
                    }
                }
                pivot[i] = Some(p);
                local[i] = Some(l);
            }
        }

        // World transforms, parents before children.
        let mut world: Vec<Option<Affine>> = vec![None; n];
        for &i in order_post.iter().rev() {
            let own = own_affine(&self.raws[i].transform, pivot[i].unwrap_or_default());
            let w = match parent[i] {
                Some(p) => world[p].unwrap_or_default().pre_concat(own),
                None => own,
            };
            world[i] = Some(w);
        }

        let mut bounds: Vec<Rect> = (0..n)
            .map(|i| map_rect(&world[i].unwrap_or_default(), &local[i].unwrap_or_default()))
            .collect();

        // Connectors, routed against the others' bounds.
        let mut routes: Vec<Option<(Point, Point)>> = vec![None; n];
        for i in 0..n {
            let Geom::Connector { from, to } = &self.raws[i].geom else { continue };
            let target = |end: &End| -> Option<Rect> {
                match end {
                    End::Element { id, .. } => self
                        .find(id)
                        .filter(|j| *j != i && self.raws[*j].kind != Kind::Connector)
                        .map(|j| bounds[j]),
                    End::Point(_) => None,
                }
            };
            let (ta, tb) = (target(from), target(to));
            let center_or = |t: Option<Rect>, end: &End| t.map(|r| r.center()).unwrap_or(end.last_point());
            let (ca, cb) = (center_or(ta, from), center_or(tb, to));
            let a = route_end(from, ta, cb);
            let b = route_end(to, tb, ca);
            routes[i] = Some((a, b));
            let s = &styles[i];
            let mut r = Rect::from_points(a, b).inflate(s.width / 2.0 + head_size(s.width));
            if let (Some(layout), Some(_)) = (&layouts[i], text_boxes[i]) {
                let mid = Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
                let label = Rect::new(mid.x - layout.width / 2.0, mid.y - layout.height / 2.0, layout.width, layout.height)
                    .inflate(s.font_size * 0.3);
                r = r.union(&label);
                text_boxes[i] = Some(Rect::new(mid.x - layout.width / 2.0, mid.y - layout.height / 2.0, layout.width, layout.height));
            }
            bounds[i] = r;
            world[i] = Some(Affine::identity());
        }

        // Assemble.
        let mut out = Vec::with_capacity(n);
        let parent_ids: Vec<Option<String>> = parent.iter().map(|p| p.map(|p| self.raws[p].id.clone())).collect();
        let raws = std::mem::take(&mut self.raws);
        for (i, raw) in raws.into_iter().enumerate() {
            let (layer, layer_visible, composite) = match layer_of[i] {
                Some(l) => (self.layers[l].id.clone(), self.layers[l].effective_visible, self.composite[l]),
                None => (raw.layer.clone(), true, 0),
            };
            let show_all = self.options.show_all;
            let known = !matches!(raw.kind, Kind::Other(_));
            let visible = known && (layer_visible || show_all) && (markup_visible || show_all || raw.kind.is_picture());
            let style = styles[i].clone();
            let opacity = style.opacity;
            let global = matches!(raw.kind, Kind::Spotlight);
            let text_path = OnceLock::new();
            if let Some(path) = text_paths[i].take() {
                let _ = text_path.set(Some(path));
            }
            let mut e = Element {
                layer,
                parent: parent_ids[i].clone(),
                style,
                step_number: step_number[i],
                pivot_bounds: pivot[i].unwrap_or_default(),
                world: world[i].unwrap_or_default(),
                local_bounds: local[i].unwrap_or_default(),
                bounds: bounds[i],
                visible,
                opacity,
                layer_index: layer_of[i],
                route: routes[i],
                layout: layouts[i].take(),
                text_box: text_boxes[i],
                grown: grown[i],
                global,
                has_extent: local[i].is_some() || routes[i].is_some(),
                card: cards[i].take(),
                fingerprint: 0,
                text_path,
                along: None,
                clips: Vec::new(),
                scatter: scatters[i].take(),
                raw,
            };
            let (print, look) = fingerprint_and_look(&e);
            e.fingerprint = print ^ composite;
            if let Some(sc) = e.scatter.as_mut().and_then(Arc::get_mut) {
                sc.look = look ^ composite;
            }
            out.push(e);
        }

        // Text set along a path (docs/DRAWING_WAVE6_CONTRACT.md section 4):
        // placed once every element is where it is drawn, since the line it
        // follows is another element's, through that one's world. Naming
        // nothing, or anything but a line, reads as plain text.
        let along: Vec<(usize, String)> = out
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!((&e.raw.kind, &e.raw.geom), (Kind::Text, Geom::Rect(_))))
            .filter_map(|(i, e)| e.style.along.clone().map(|a| (i, a)))
            .collect();
        if !along.is_empty() {
            let at: HashMap<&str, usize> = out.iter().enumerate().map(|(i, e)| (e.raw.id.as_str(), i)).collect();
            let placed: Vec<(usize, crate::along::Placed)> = along
                .iter()
                .filter_map(|(i, target)| {
                    let j = *at.get(target.as_str())?;
                    if j == *i {
                        return None;
                    }
                    let line = crate::along::polyline_of(&out[j])?;
                    let layout = out[*i].layout.clone()?;
                    crate::along::place(target, &layout, &out[*i].style, line).map(|p| (*i, p))
                })
                .collect();
            for (i, p) in placed {
                let composite = layer_of[i].map_or(0, |l| self.composite[l]);
                let e = &mut out[i];
                let halo = e.style.halo.map_or(0.0, |h| h.width);
                e.world = Affine::identity();
                e.pivot_bounds = p.bounds;
                e.local_bounds = p.bounds.inflate(halo);
                e.bounds = e.local_bounds;
                e.text_box = Some(p.bounds);
                e.text_path = OnceLock::new();
                let _ = e.text_path.set(p.path.clone());
                let hash = p.hash;
                e.along = Some(Arc::new(p));
                e.fingerprint = fingerprint(e) ^ composite ^ hash;
            }
        }
        out
    }
}

/// Per layer, a hash of how it is composited (its chain's ids, opacities,
/// blends and clips) and its place in the draw order, folded into its
/// elements' fingerprints and its paint marks so the raster cache redraws
/// them when any of it changes.
fn composite_of(chains: &[Vec<usize>], layers: &[Layer], clips: &[Option<Arc<Clip>>]) -> Vec<u64> {
    chains
        .iter()
        .enumerate()
        .map(|(at, chain)| {
            let mut h = Fx::default();
            at.hash(&mut h);
            for c in chain {
                let l = &layers[*c];
                l.id.hash(&mut h);
                l.opacity.to_bits().hash(&mut h);
                l.blend.hash(&mut h);
                if let Some(Some(clip)) = clips.get(*c) {
                    clip.hash.hash(&mut h);
                }
            }
            h.finish()
        })
        .collect()
}

/// Arrow head length for a stroke width.
pub(crate) fn head_size(width: f32) -> f32 {
    (width * 4.0).max(10.0)
}

/// The text an element shows, its layout and its box (element space), and
/// a sticky note's grown rectangle.
fn lay_out(
    e: &RawElement,
    s: &ResolvedStyle,
    step: Option<u32>,
    card_title: Option<&str>,
) -> (Option<TextLayout>, Option<Rect>, Option<Rect>) {
    let text = e.text.as_deref().unwrap_or("");
    match (&e.kind, &e.geom) {
        (Kind::Step, Geom::Anchor(c)) => {
            let n = step.unwrap_or(0).to_string();
            let l = text::layout_styled(&n, s.font_size, None, &s.text);
            let tb = Rect::new(c.x - l.width / 2.0, c.y - l.height / 2.0, l.width, l.height);
            (Some(l), Some(tb), None)
        }
        (Kind::Text, Geom::Rect(r)) => {
            let wrap = (r.w > 0.0).then_some(r.w);
            let l = text::layout_styled(text, s.font_size, wrap, &s.text);
            let w = if r.w > 0.0 { r.w } else { l.width };
            let tb = Rect::new(r.x, r.y, w, l.height.max(r.h));
            (Some(l), Some(tb), None)
        }
        (Kind::Sticky, Geom::Rect(r)) => {
            let pad = s.font_size * STICKY_PADDING;
            let inner_w = (r.w - 2.0 * pad).max(1.0);
            let l = text::layout_styled(text, s.font_size, Some(inner_w), &s.text);
            let h = r.h.max(l.height + 2.0 * pad);
            let grown = Rect::new(r.x, r.y, r.w, h);
            let tb = Rect::new(r.x + pad, r.y + pad, inner_w, l.height);
            (Some(l), Some(tb), Some(grown))
        }
        (Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star, Geom::Rect(r)) if !text.is_empty() => {
            let pad = s.font_size * 0.4;
            let inner_w = (r.w - 2.0 * pad).max(1.0);
            let l = text::layout_styled(text, s.font_size, Some(inner_w), &s.text);
            let tb = Rect::new(r.x + pad, r.y + (r.h - l.height) / 2.0, inner_w, l.height);
            (Some(l), Some(tb), None)
        }
        (Kind::Callout, Geom::Callout { rect: r, .. }) if !text.is_empty() => {
            let pad = s.font_size * 0.5;
            let inner_w = (r.w - 2.0 * pad).max(1.0);
            let l = text::layout_styled(text, s.font_size, Some(inner_w), &s.text);
            (Some(l), Some(Rect::new(r.x + pad, r.y + pad, inner_w, r.h - 2.0 * pad)), None)
        }
        (Kind::Frame, Geom::Rect(r)) if !text.is_empty() => {
            let l = text::layout_styled(text, s.font_size, None, &s.text);
            let tb = Rect::new(r.x, r.y - l.height - s.font_size * 0.2, l.width, l.height);
            (Some(l), Some(tb), None)
        }
        (Kind::Card, Geom::Card { rect: r, link }) => {
            let title = card_title.unwrap_or(if text.is_empty() { link.as_str() } else { text });
            let pad = s.font_size * 0.6;
            let icon = s.font_size * 1.2;
            let inner_w = (r.w - 3.0 * pad - icon).max(1.0);
            let l = text::layout_styled(title, s.font_size, Some(inner_w), &s.text);
            let tb = Rect::new(r.x + 2.0 * pad + icon, r.y + (r.h - l.height) / 2.0, inner_w, l.height);
            (Some(l), Some(tb), None)
        }
        (Kind::Connector, _) if !text.is_empty() => {
            let l = text::layout_styled(text, s.font_size, None, &s.text);
            (Some(l), Some(Rect::default()), None)
        }
        _ => (None, None, None),
    }
}

/// An element's own extent in element space: (the pivot rectangle, what it
/// paints). `None` for a group (its extent is its members') or a kind with
/// nothing to show.
fn own_extent(e: &RawElement, s: &ResolvedStyle, text_box: Option<Rect>, grown: Option<Rect>, ink_below: f32) -> Option<(Rect, Rect)> {
    // Half the stroke, and more where a miter or a square cap reaches out.
    let half = s.stroke_reach();
    let shadow = |r: Rect| if s.shadow { r.union(&Rect::new(r.x + SHADOW_OFFSET, r.y + SHADOW_OFFSET, r.w, r.h)) } else { r };
    Some(match (&e.kind, &e.geom) {
        (Kind::Pen | Kind::Highlighter, _) => {
            let mut it = e.points.iter().filter(|p| p[0].is_finite() && p[1].is_finite());
            let first = it.next()?;
            let mut r = Rect::new(first[0], first[1], 0.0, 0.0);
            for p in it {
                r = r.union(&Rect::new(p[0], p[1], 0.0, 0.0));
            }
            // Pressure widens a pen where it was pressed harder.
            (r, r.inflate((crate::paint::stroke_max_width(&e.kind, &e.points, s.width, s.dash) / 2.0).max(0.5)))
        }
        (Kind::Group | Kind::Other(_), _) => return None,
        (Kind::Path, Geom::Path(p)) => {
            let r = p.bounds()?;
            (r, shadow(r.inflate(half)))
        }
        (_, Geom::Picture(p)) => {
            let r = p.rect.normalized();
            (r, shadow(r))
        }
        (_, Geom::Segment { a, b }) => {
            let r = Rect::from_points(*a, *b);
            let heads = if s.arrow_heads != ArrowHeads::None { head_size(s.width) * 0.6 } else { 0.0 };
            (r, r.inflate(half + heads))
        }
        (Kind::Step, Geom::Anchor(c)) => {
            let rad = s.font_size * STEP_RADIUS;
            let r = Rect::new(c.x - rad, c.y - rad, 2.0 * rad, 2.0 * rad);
            (r, shadow(r))
        }
        (Kind::Text, Geom::Rect(_)) => {
            let tb = text_box?;
            // Descenders hang below the last line's box.
            (tb, Rect::new(tb.x, tb.y, tb.w, tb.h + ink_below))
        }
        (Kind::Sticky, Geom::Rect(r)) => {
            let g = grown.unwrap_or(*r).normalized();
            (g, shadow(g))
        }
        (Kind::Frame, Geom::Rect(r)) => {
            let r = r.normalized();
            let painted = text_box.map_or(r, |tb| r.union(&Rect::new(tb.x, tb.y, tb.w, tb.h + ink_below)));
            (r, painted.inflate(half))
        }
        (Kind::Spotlight, Geom::Rect(r)) => {
            let r = r.normalized();
            (r, r)
        }
        (_, Geom::Rect(r)) => {
            let r = r.normalized();
            (r, shadow(r.inflate(half)))
        }
        (_, Geom::Callout { rect, tip }) => {
            let r = rect.normalized();
            let painted = r.union(&Rect::new(tip.x, tip.y, 0.0, 0.0)).inflate(half);
            (r, shadow(painted))
        }
        (_, Geom::Card { rect, .. }) => {
            let r = rect.normalized();
            (r, shadow(r.inflate(half)))
        }
        // Connectors are placed in board units when routed.
        (_, Geom::Connector { from, to }) => {
            let r = Rect::from_points(from.last_point(), to.last_point());
            (r, r)
        }
        _ => return None,
    })
}

/// The extent an element's rotation and scale turn about (its own geometry
/// before any transform), for the kinds whose extent needs no text layout:
/// what [`crate::BoardTxn::convert_to_path`] keeps an element in place by.
pub(crate) fn pivot_of(e: &RawElement, s: &ResolvedStyle) -> Option<Rect> {
    own_extent(e, s, None, None, 0.0).map(|(p, _)| p)
}

/// The matrix of a stored transform about `pivot`'s center.
pub(crate) fn own_affine(t: &Transform, pivot: Rect) -> Affine {
    if t.is_identity() {
        return Affine::identity();
    }
    let c = pivot.center();
    Affine::from_translate(t.tx + c.x, t.ty + c.y)
        .pre_concat(Affine::from_rotate(t.rot))
        .pre_concat(Affine::from_scale(t.sx, t.sy))
        .pre_concat(Affine::from_translate(-c.x, -c.y))
}

/// `r` through `m`, as the axis-aligned box of its four corners.
pub(crate) fn map_rect(m: &Affine, r: &Rect) -> Rect {
    if m.is_identity() {
        return *r;
    }
    let mut pts = [
        tiny_skia::Point::from_xy(r.x, r.y),
        tiny_skia::Point::from_xy(r.right(), r.y),
        tiny_skia::Point::from_xy(r.x, r.bottom()),
        tiny_skia::Point::from_xy(r.right(), r.bottom()),
    ];
    m.map_points(&mut pts);
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in pts {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// Where a connector end sits: a free point; a named side's midpoint; or
/// where the line toward the other end leaves the element's box.
fn route_end(end: &End, target: Option<Rect>, toward: Point) -> Point {
    let Some(r) = target else { return end.last_point() };
    if let End::Element { side: Some(side), .. } = end {
        let c = r.center();
        return match side {
            Side::Top => Point::new(c.x, r.y),
            Side::Right => Point::new(r.right(), c.y),
            Side::Bottom => Point::new(c.x, r.bottom()),
            Side::Left => Point::new(r.x, c.y),
        };
    }
    let c = r.center();
    let (dx, dy) = (toward.x - c.x, toward.y - c.y);
    if dx == 0.0 && dy == 0.0 {
        return c;
    }
    let tx = if dx != 0.0 { (r.w / 2.0) / dx.abs() } else { f32::INFINITY };
    let ty = if dy != 0.0 { (r.h / 2.0) / dy.abs() } else { f32::INFINITY };
    let t = tx.min(ty).min(1.0);
    Point::new(c.x + dx * t, c.y + dy * t)
}

/// A hash of an element id, for maps that would otherwise copy every id.
pub(crate) fn id_hash(id: &str) -> u64 {
    let mut h = Fx::default();
    id.hash(&mut h);
    h.finish()
}

/// A small fast hasher (rustc's "Fx"): fingerprints hash every point of
/// every stroke on every snapshot, and nothing here faces untrusted keys
/// that could be chosen to collide.
#[derive(Default)]
pub(crate) struct Fx(u64);

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.write_u64(u64::from_le_bytes(c.try_into().unwrap()));
        }
        for b in chunks.remainder() {
            self.write_u64(*b as u64);
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn write_u32(&mut self, v: u32) {
        self.write_u64(v as u64);
    }
    fn write_u8(&mut self, v: u8) {
        self.write_u64(v as u64);
    }
    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
}

fn fingerprint(e: &Element) -> u64 {
    fingerprint_and_look(e).0
}

/// The fingerprint, and the same without a scatter's placements (what
/// stays while placements are only appended).
fn fingerprint_and_look(e: &Element) -> (u64, u64) {
    let mut h = Fx::default();
    let r = &e.raw;
    let f = |h: &mut Fx, v: f32| h.write_u32(v.to_bits());
    let rect = |h: &mut Fx, r: &Rect| {
        for v in [r.x, r.y, r.w, r.h] {
            v.to_bits().hash(h);
        }
    };
    r.kind.as_str().hash(&mut h);
    match &r.geom {
        Geom::None | Geom::Stroke => 0u8.hash(&mut h),
        Geom::Scatter => 2u8.hash(&mut h),
        Geom::Rect(x) => rect(&mut h, x),
        Geom::Picture(p) => {
            rect(&mut h, &p.rect);
            p.src.hash(&mut h);
            (p.px_w, p.px_h).hash(&mut h);
            if let Some(c) = &p.crop {
                rect(&mut h, c);
            }
        }
        Geom::Segment { a, b } => {
            for v in [a.x, a.y, b.x, b.y] {
                f(&mut h, v);
            }
        }
        Geom::Anchor(a) => {
            f(&mut h, a.x);
            f(&mut h, a.y);
        }
        Geom::Callout { rect: x, tip } => {
            rect(&mut h, x);
            f(&mut h, tip.x);
            f(&mut h, tip.y);
        }
        Geom::Card { rect: x, link } => {
            rect(&mut h, x);
            link.hash(&mut h);
        }
        // A connector's look is its route, hashed below.
        Geom::Connector { .. } => 1u8.hash(&mut h),
        Geom::Path(p) => {
            p.rule.hash(&mut h);
            for sp in &p.subpaths {
                (sp.closed, sp.anchors.len()).hash(&mut h);
                for a in &sp.anchors {
                    for v in [a.p.x, a.p.y, a.h_in.x, a.h_in.y, a.h_out.x, a.h_out.y] {
                        f(&mut h, v);
                    }
                    a.mode.hash(&mut h);
                }
            }
        }
    }
    r.text.hash(&mut h);
    let s = &e.style;
    (s.stroke, s.fill, s.align, s.shadow, s.arrow_heads, s.dash).hash(&mut h);
    (s.cap, s.join, s.sides, s.points).hash(&mut h);
    for v in [s.width, s.opacity, s.font_size, s.miter, s.radius, s.inner] {
        f(&mut h, v);
    }
    for g in [&s.fill_gradient, &s.stroke_gradient] {
        match g {
            None => 0u8.hash(&mut h),
            Some(g) => {
                g.kind.hash(&mut h);
                for v in [g.from.x, g.from.y, g.to.x, g.to.y] {
                    f(&mut h, v);
                }
                for stop in &g.stops {
                    f(&mut h, stop.offset);
                    stop.color.hash(&mut h);
                }
            }
        }
    }
    s.text.font.hash(&mut h);
    f(&mut h, s.text.tracking);
    if let Some(halo) = s.halo {
        halo.color.hash(&mut h);
        f(&mut h, halo.width);
    }
    if let Some(pattern) = &s.dash_pattern {
        pattern.len().hash(&mut h);
        for v in pattern {
            f(&mut h, *v);
        }
    }
    for v in [e.world.sx, e.world.kx, e.world.ky, e.world.sy, e.world.tx, e.world.ty, e.opacity] {
        f(&mut h, v);
    }
    e.visible.hash(&mut h);
    e.step_number.hash(&mut h);
    e.card.hash(&mut h);
    if let Some((a, b)) = e.route {
        for v in [a.x, a.y, b.x, b.y] {
            f(&mut h, v);
        }
    }
    r.points.len().hash(&mut h);
    for p in &r.points {
        for v in p {
            f(&mut h, *v);
        }
    }
    s.tint.hash(&mut h);
    let Some(sc) = &r.scatter else {
        let print = h.finish();
        return (print, print);
    };
    sc.symbol.hash(&mut h);
    e.scatter.as_ref().and_then(|s| s.def.as_ref()).map(|d| d.hash).hash(&mut h);
    let look = h.finish();
    sc.placements.len().hash(&mut h);
    for p in &sc.placements {
        for v in [p.x, p.y, p.scale, p.rotation] {
            f(&mut h, v);
        }
        p.tint.hash(&mut h);
    }
    (h.finish(), look)
}

/// The layer a symbol's elements are resolved in (and shown in while it is
/// edited): its key as an id, its name.
pub fn symbol_layer(symbol: &str, name: &str) -> crate::doc::RawLayer {
    crate::doc::RawLayer {
        id: crate::scatter::symbol_key(symbol),
        name: name.to_string(),
        order: "a".into(),
        visible: true,
        locked: false,
        opacity: 1.0,
        note: None,
        fields: Vec::new(),
        kind: LayerKind::Vector,
        blend: None,
        parent: None,
        clip: None,
    }
}

/// The board as seen while symbol `symbol` is edited: its elements alone,
/// in symbol space, in one layer named after it (new elements drawn there go
/// into the symbol: [`crate::BoardTxn::add_element`]). Unchanged when the
/// canvas has no such symbol.
fn isolate(mut raw: RawBoard, symbol: &str) -> RawBoard {
    let Some(sym) = raw.symbols.iter().find(|s| s.id == symbol) else { return raw };
    let layer = symbol_layer(&sym.id, &sym.name);
    raw.elements = sym
        .elements
        .iter()
        .filter(|e| e.kind != Kind::Scatter)
        .cloned()
        .map(|mut e| {
            e.layer = layer.id.clone();
            e
        })
        .collect();
    raw.layers = vec![layer];
    raw.paint = Vec::new();
    raw
}

/// The paint operations sorted (by layer, order, id) and indexed by layer.
#[allow(clippy::type_complexity)]
fn index_paint(mut ops: Vec<PaintOp>) -> (Vec<PaintOp>, HashMap<String, (std::ops::Range<usize>, Option<Rect>)>) {
    ops.sort_by(|a, b| a.raw.layer.cmp(&b.raw.layer).then_with(|| a.raw.order.cmp(&b.raw.order)).then_with(|| a.raw.id.cmp(&b.raw.id)));
    let mut index: HashMap<String, (std::ops::Range<usize>, Option<Rect>)> = HashMap::new();
    let mut start = 0;
    while start < ops.len() {
        let layer = ops[start].raw.layer.clone();
        let mut end = start;
        let mut bounds: Option<Rect> = None;
        while end < ops.len() && ops[end].raw.layer == layer {
            if let Some(b) = ops[end].bounds {
                bounds = Some(bounds.map_or(b, |a| a.union(&b)));
            }
            end += 1;
        }
        crate::paint_ops::settle_layer(&mut ops[start..end]);
        index.insert(layer, (start..end, bounds));
        start = end;
    }
    (ops, index)
}

/// `old` with the operations re-read in `changed` put in (or taken out).
fn merge_paint(old: Vec<PaintOp>, changed: Vec<(String, Option<RawPaintOp>)>) -> Vec<PaintOp> {
    if changed.is_empty() {
        return old;
    }
    let mut fresh: HashMap<String, Option<RawPaintOp>> = changed.into_iter().collect();
    let mut out = Vec::with_capacity(old.len() + fresh.len());
    for op in old {
        match fresh.remove(&op.raw.id) {
            None => out.push(op),
            Some(Some(raw)) if raw == op.raw => out.push(op),
            Some(Some(raw)) => out.push(PaintOp::of(raw)),
            Some(None) => {}
        }
    }
    out.extend(fresh.into_values().flatten().map(PaintOp::of));
    out
}

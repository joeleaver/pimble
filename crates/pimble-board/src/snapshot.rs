//! A board read once and resolved for drawing: z-order, effective layers and
//! parents, world transforms (a member moves with its frame or group), step
//! numbers (derived, contract section 3), routed connectors, text layout,
//! bounds, visibility, a fingerprint per element for the raster cache, and a
//! spatial index. Built from the document, never stored.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use tiny_skia::{Path, Transform as Affine};

use crate::doc::{Board, Changes, RawBoard, RawElement, RawLayer};
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

/// One layer, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub id: String,
    pub name: String,
    pub order: String,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    /// How many elements it holds.
    pub count: usize,
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
    /// Drawn: its layer is visible, and it is a picture or the markup shows.
    pub visible: bool,
    /// Style opacity times layer opacity.
    pub opacity: f32,
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
}

impl Element {
    pub fn id(&self) -> &str {
        &self.raw.id
    }

    pub fn kind(&self) -> &Kind {
        &self.raw.kind
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
    /// Bottom to top.
    pub layers: Vec<Layer>,
    /// Bottom to top: by layer order, then element order, ties by id.
    pub elements: Vec<Element>,
    /// Element index by a hash of its id (checked against the id on lookup).
    index: HashMap<u64, usize>,
    grid: Grid,
    options: ViewOptions,
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
                index: HashMap::new(),
                grid: Grid::default(),
                options,
            };
        };
        Resolver::run(raw, HashMap::new(), options)
    }

    /// The viewer's choices this snapshot is drawn with.
    pub fn options(&self) -> &ViewOptions {
        &self.options
    }

    /// This board read again from `board` with other view options. Nothing
    /// is reused (options change rarely, and a card's title may change with
    /// them).
    pub fn with_options(self, options: ViewOptions, board: &Board) -> Self {
        Self::build_with(board.read(), options)
    }

    /// [`BoardSnapshot::build`], keeping what this snapshot already worked
    /// out (text layout, glyph outlines) for every element that has not
    /// changed.
    pub fn rebuild(self, raw: Option<RawBoard>) -> Self {
        let options = self.options;
        let Some(raw) = raw else { return Self::build_with(None, options) };
        let mut prev: HashMap<String, Element> = self.elements.into_iter().map(|e| (e.raw.id.clone(), e)).collect();
        let mut reuse = HashMap::new();
        for e in &raw.elements {
            if let Some(old) = prev.remove(&e.id) {
                if old.raw == *e {
                    reuse.insert(e.id.clone(), Reuse::of(old));
                }
            }
        }
        Resolver::run(raw, reuse, options)
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
        if changes.all || !self.exists {
            return self.rebuild(board.read());
        }
        let options = self.options;
        let Some((mut raw, changed)) = board.read_some(&changes.elements) else { return Self::build_with(None, options) };
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
        Resolver::run(raw, reuse, options)
    }

    pub fn get(&self, id: &str) -> Option<&Element> {
        self.index.get(&id_hash(id)).map(|i| &self.elements[*i]).filter(|e| e.raw.id == id)
    }

    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// The top layer (where new elements go by default).
    pub fn top_layer(&self) -> Option<&Layer> {
        self.layers.last()
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
        self.elements
            .iter()
            .filter(|e| e.visible && e.has_extent && !matches!(e.raw.kind, Kind::Group))
            .map(|e| e.bounds)
            .filter(|b| b.is_finite())
            .reduce(|a, b| a.union(&b))
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
        self.query(&r).into_iter().rev().map(|i| &self.elements[i]).find(|e| !e.global && e.bounds.contains(p))
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
        let text_path = if connector { None } else { e.text_path.into_inner().flatten() };
        let reuse = Reuse { layout: e.layout, text_box: e.text_box, grown: e.grown, step_number: e.step_number, text_path };
        (e.raw, reuse)
    }
}

struct Resolver {
    reuse: HashMap<String, Reuse>,
    layers: Vec<Layer>,
    layer_rank: HashMap<String, usize>,
    raws: Vec<RawElement>,
    by_id: HashMap<u64, usize>,
    options: ViewOptions,
}

impl Resolver {
    fn run(raw: RawBoard, reuse: HashMap<String, Reuse>, options: ViewOptions) -> BoardSnapshot {
        let RawBoard { version, page, markup_visible, edited_by, edited_at, layers: raw_layers, elements } = raw;
        let mut raw_layers: Vec<RawLayer> = raw_layers;
        raw_layers.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
        let layers: Vec<Layer> = raw_layers
            .into_iter()
            .map(|l| Layer { id: l.id, name: l.name, order: l.order, visible: l.visible, locked: l.locked, opacity: l.opacity, count: 0 })
            .collect();
        let layer_rank: HashMap<String, usize> = layers.iter().enumerate().map(|(i, l)| (l.id.clone(), i)).collect();
        let top = layers.len().saturating_sub(1);

        // z-order.
        let mut ranked: Vec<(usize, RawElement)> =
            elements.into_iter().map(|e| (layer_rank.get(&e.layer).copied().unwrap_or(top), e)).collect();
        ranked.sort_by(|(ra, a), (rb, b)| ra.cmp(rb).then_with(|| a.order.cmp(&b.order)).then_with(|| a.id.cmp(&b.id)));
        let raws: Vec<RawElement> = ranked.into_iter().map(|(_, e)| e).collect();
        let by_id: HashMap<u64, usize> = raws.iter().enumerate().map(|(i, e)| (id_hash(&e.id), i)).collect();
        let mut r = Resolver { reuse, layers, layer_rank, raws, by_id, options };
        let snapshot_elements = r.resolve(markup_visible);
        let index = snapshot_elements.iter().enumerate().map(|(i, e)| (id_hash(&e.raw.id), i)).collect();
        let grid = Grid::build(&snapshot_elements);
        BoardSnapshot {
            exists: true,
            version,
            page,
            markup_visible,
            edited_by,
            edited_at,
            layers: r.layers,
            elements: snapshot_elements,
            index,
            grid,
            options: r.options,
        }
    }

    /// The index of the element `id` names.
    fn find(&self, id: &str) -> Option<usize> {
        self.by_id.get(&id_hash(id)).copied().filter(|i| self.raws[*i].id == id)
    }

    fn resolve(&mut self, markup_visible: bool) -> Vec<Element> {
        let n = self.raws.len();
        let top = self.layers.len().checked_sub(1);

        // Effective layers (as indices into `layers`), counts, parents (a
        // parent that names nothing, or makes a cycle, reads as none).
        let layer_of: Vec<Option<usize>> =
            self.raws.iter().map(|e| self.layer_rank.get(&e.layer).copied().or(top)).collect();
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
        for &i in &order_post {
            let e = &self.raws[i];
            let extent = own_extent(e, &styles[i], text_boxes[i], grown[i]).or_else(|| {
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
            if let Some((p, l)) = extent {
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
            let (layer, layer_visible, layer_opacity) = match layer_of[i] {
                Some(l) => (self.layers[l].id.clone(), self.layers[l].visible, self.layers[l].opacity),
                None => (raw.layer.clone(), true, 1.0),
            };
            let show_all = self.options.show_all;
            let known = !matches!(raw.kind, Kind::Other(_));
            let visible = known && (layer_visible || show_all) && (markup_visible || show_all || raw.kind.is_picture());
            let style = styles[i].clone();
            let opacity = style.opacity * layer_opacity;
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
                route: routes[i],
                layout: layouts[i].take(),
                text_box: text_boxes[i],
                grown: grown[i],
                global,
                has_extent: local[i].is_some() || routes[i].is_some(),
                card: cards[i].take(),
                fingerprint: 0,
                text_path,
                raw,
            };
            e.fingerprint = fingerprint(&e);
            out.push(e);
        }
        out
    }
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
            let l = text::layout(&n, s.font_size, None);
            let tb = Rect::new(c.x - l.width / 2.0, c.y - l.height / 2.0, l.width, l.height);
            (Some(l), Some(tb), None)
        }
        (Kind::Text, Geom::Rect(r)) => {
            let wrap = (r.w > 0.0).then_some(r.w);
            let l = text::layout(text, s.font_size, wrap);
            let w = if r.w > 0.0 { r.w } else { l.width };
            let tb = Rect::new(r.x, r.y, w, l.height.max(r.h));
            (Some(l), Some(tb), None)
        }
        (Kind::Sticky, Geom::Rect(r)) => {
            let pad = s.font_size * STICKY_PADDING;
            let inner_w = (r.w - 2.0 * pad).max(1.0);
            let l = text::layout(text, s.font_size, Some(inner_w));
            let h = r.h.max(l.height + 2.0 * pad);
            let grown = Rect::new(r.x, r.y, r.w, h);
            let tb = Rect::new(r.x + pad, r.y + pad, inner_w, l.height);
            (Some(l), Some(tb), Some(grown))
        }
        (Kind::Rect | Kind::Ellipse | Kind::Diamond, Geom::Rect(r)) if !text.is_empty() => {
            let pad = s.font_size * 0.4;
            let inner_w = (r.w - 2.0 * pad).max(1.0);
            let l = text::layout(text, s.font_size, Some(inner_w));
            let tb = Rect::new(r.x + pad, r.y + (r.h - l.height) / 2.0, inner_w, l.height);
            (Some(l), Some(tb), None)
        }
        (Kind::Callout, Geom::Callout { rect: r, .. }) if !text.is_empty() => {
            let pad = s.font_size * 0.5;
            let inner_w = (r.w - 2.0 * pad).max(1.0);
            let l = text::layout(text, s.font_size, Some(inner_w));
            (Some(l), Some(Rect::new(r.x + pad, r.y + pad, inner_w, r.h - 2.0 * pad)), None)
        }
        (Kind::Frame, Geom::Rect(r)) if !text.is_empty() => {
            let l = text::layout(text, s.font_size, None);
            let tb = Rect::new(r.x, r.y - l.height - s.font_size * 0.2, l.width, l.height);
            (Some(l), Some(tb), None)
        }
        (Kind::Card, Geom::Card { rect: r, link }) => {
            let title = card_title.unwrap_or(if text.is_empty() { link.as_str() } else { text });
            let pad = s.font_size * 0.6;
            let icon = s.font_size * 1.2;
            let inner_w = (r.w - 3.0 * pad - icon).max(1.0);
            let l = text::layout(title, s.font_size, Some(inner_w));
            let tb = Rect::new(r.x + 2.0 * pad + icon, r.y + (r.h - l.height) / 2.0, inner_w, l.height);
            (Some(l), Some(tb), None)
        }
        (Kind::Connector, _) if !text.is_empty() => {
            let l = text::layout(text, s.font_size, None);
            (Some(l), Some(Rect::default()), None)
        }
        _ => (None, None, None),
    }
}

/// An element's own extent in element space: (the pivot rectangle, what it
/// paints). `None` for a group (its extent is its members') or a kind with
/// nothing to show.
fn own_extent(e: &RawElement, s: &ResolvedStyle, text_box: Option<Rect>, grown: Option<Rect>) -> Option<(Rect, Rect)> {
    let half = s.width / 2.0;
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
            (tb, tb)
        }
        (Kind::Sticky, Geom::Rect(r)) => {
            let g = grown.unwrap_or(*r).normalized();
            (g, shadow(g))
        }
        (Kind::Frame, Geom::Rect(r)) => {
            let r = r.normalized();
            let painted = text_box.map_or(r, |tb| r.union(&tb));
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
struct Fx(u64);

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
    }
    r.text.hash(&mut h);
    let s = &e.style;
    (s.stroke, s.fill, s.align, s.shadow, s.arrow_heads, s.dash).hash(&mut h);
    for v in [s.width, s.opacity, s.font_size] {
        f(&mut h, v);
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
    h.finish()
}

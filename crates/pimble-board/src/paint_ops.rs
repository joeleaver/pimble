//! Paint operations as stored and read (docs/DRAWING_CONTRACT.md section 3,
//! "Paint is stored as operations, not pixels"). A paint layer is the
//! ordered list of the operations naming it; every device replays the list
//! to get the image ([`crate::tiles`]).
//!
//! ```text
//! board: Map {
//!   ...
//!   "paint:<op id>": Map {
//!     layer: String, order: String,          // fractional, within the layer
//!     kind: "stroke" | "pixels" | ...,        // fill, smudge, blur, move: wave 5
//!     author: Any { id, name }?, created_at: String,
//!     // stroke
//!     preset: String, color: "#rrggbb(aa)", blend: "normal" | "erase",
//!     brush: Any { size, hardness, spacing, flow, opacity,
//!                  pressure_size: [at0, at1, gamma], pressure_opacity: [...],
//!                  tilt_size: [...], tilt_opacity: [...],
//!                  jitter_size, jitter_opacity, jitter_position, grain, wet_edge },
//!     points: Array<Buffer>,                  // chunks of little-endian f32
//!                                             // quadruples (x, y, pressure, tilt)
//!     // pixels
//!     src: "pimble-blob:...", rect: Any { x, y, w, h },
//!   },
//! }
//! ```
//!
//! **Each operation is its own key of the board map** (`paint:<op id>`),
//! where the contract drew one `paint` map of them: a map made on first use
//! would be made by two devices painting their first strokes while apart,
//! and yrs keeps only one of the two maps (and its strokes), the rule this
//! crate's `board:<id>` keys and node fields already follow. A board made
//! before painting needs nothing new; an older app ignores the keys.
//!
//! A kind this version does not know is read as [`PaintBody::Other`] and
//! skipped when drawing: the layer draws the rest.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::brush::{self, BrushPoint, BrushSettings, Preset, StrokeBlend};
use crate::model::{Author, Color, Rect};
use crate::snapshot::Fx;

/// The prefix of a paint operation's key in the board map.
pub const PAINT_PREFIX: &str = "paint:";

/// The board-map key of operation `id`.
pub fn paint_key(id: &str) -> String {
    format!("{PAINT_PREFIX}{id}")
}

/// A brush stroke: what it was drawn with and its points.
#[derive(Debug, Clone, PartialEq)]
pub struct StrokeOp {
    /// The preset's name as it was when drawn ("round"); a name this version
    /// does not know is kept and drawn with the stored settings.
    pub preset: String,
    pub settings: BrushSettings,
    pub color: Color,
    pub blend: StrokeBlend,
    pub points: Vec<BrushPoint>,
}

/// A picture laid over a rectangle of the layer (Flatten, Merge Down,
/// paste: wave 5 makes them; wave 4 stores and draws them).
#[derive(Debug, Clone, PartialEq)]
pub struct PixelsOp {
    /// A `pimble-blob:` URL.
    pub src: String,
    /// Where it lies, board units.
    pub rect: Rect,
}

/// What an operation does, by kind.
#[derive(Debug, Clone, PartialEq)]
pub enum PaintBody {
    Stroke(Arc<StrokeOp>),
    Pixels(PixelsOp),
    /// A kind this version does not know (fill, smudge, blur and move until
    /// wave 5): kept, skipped when drawing.
    Other(String),
}

impl PaintBody {
    pub fn kind(&self) -> &str {
        match self {
            PaintBody::Stroke(_) => "stroke",
            PaintBody::Pixels(_) => "pixels",
            PaintBody::Other(k) => k,
        }
    }
}

/// An operation as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct RawPaintOp {
    pub id: String,
    pub layer: String,
    pub order: String,
    pub body: PaintBody,
    pub author: Option<Author>,
    pub created_at: String,
}

impl RawPaintOp {
    pub fn kind(&self) -> &str {
        self.body.kind()
    }

    /// The board rectangle its paint can reach (`None` for a stroke with no
    /// points and a kind this version does not draw).
    pub fn bounds(&self) -> Option<Rect> {
        match &self.body {
            PaintBody::Stroke(s) => points_bounds(&s.points).map(|r| r.inflate(s.settings.reach())),
            PaintBody::Pixels(p) => {
                let r = p.rect.normalized();
                (r.w > 0.0 && r.h > 0.0 && r.is_finite()).then_some(r)
            }
            PaintBody::Other(_) => None,
        }
    }
}

/// The bounding box of points' positions (non-finite ones skipped).
pub(crate) fn points_bounds(points: &[BrushPoint]) -> Option<Rect> {
    let mut it = points.iter().filter(|p| p[0].is_finite() && p[1].is_finite());
    let first = it.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first[0], first[1], first[0], first[1]);
    for p in it {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// A hash of everything about an operation that changes its pixels.
pub(crate) fn fingerprint(op: &RawPaintOp) -> u64 {
    let mut h = Fx::default();
    op.id.hash(&mut h);
    op.layer.hash(&mut h);
    op.order.hash(&mut h);
    match &op.body {
        PaintBody::Stroke(s) => {
            1u8.hash(&mut h);
            settings_hash(&s.settings, s.color, s.blend).hash(&mut h);
            s.points.len().hash(&mut h);
            for p in &s.points {
                for v in p {
                    h.write_u32(v.to_bits());
                }
            }
        }
        PaintBody::Pixels(p) => {
            2u8.hash(&mut h);
            p.src.hash(&mut h);
            for v in [p.rect.x, p.rect.y, p.rect.w, p.rect.h] {
                h.write_u32(v.to_bits());
            }
        }
        PaintBody::Other(k) => {
            3u8.hash(&mut h);
            k.hash(&mut h);
        }
    }
    h.finish()
}

/// A hash of what a stroke is drawn with, points aside.
pub(crate) fn settings_hash(s: &BrushSettings, color: Color, blend: StrokeBlend) -> u64 {
    let mut h = Fx::default();
    color.hash(&mut h);
    blend.hash(&mut h);
    for v in settings_values(s) {
        h.write_u32(v.to_bits());
    }
    h.finish()
}

fn settings_values(s: &BrushSettings) -> [f32; 22] {
    [
        s.size,
        s.hardness,
        s.spacing,
        s.flow,
        s.opacity,
        s.pressure_size.at0,
        s.pressure_size.at1,
        s.pressure_size.gamma,
        s.pressure_opacity.at0,
        s.pressure_opacity.at1,
        s.pressure_opacity.gamma,
        s.tilt_size.at0,
        s.tilt_size.at1,
        s.tilt_size.gamma,
        s.tilt_opacity.at0,
        s.tilt_opacity.at1,
        s.tilt_opacity.gamma,
        s.jitter.size,
        s.jitter.opacity,
        s.jitter.position,
        s.grain,
        s.wet_edge,
    ]
}

/// A stroke to start ([`crate::BoardTxn::begin_stroke`]).
#[derive(Debug, Clone, PartialEq)]
pub struct NewStroke {
    /// A paint layer's id ([`crate::BoardTxn::paint_layer_for`] picks one by
    /// the contract's rule).
    pub layer: String,
    pub preset: String,
    pub settings: BrushSettings,
    pub color: Color,
    pub blend: StrokeBlend,
    /// The first points, if any (more come with
    /// [`crate::BoardTxn::append_stroke`]).
    pub points: Vec<BrushPoint>,
}

impl NewStroke {
    /// A stroke of `preset` in `color` into `layer`.
    pub fn new(layer: impl Into<String>, preset: &Preset, color: Color) -> NewStroke {
        NewStroke {
            layer: layer.into(),
            preset: preset.name.to_string(),
            settings: preset.settings,
            color,
            blend: StrokeBlend::Normal,
            points: Vec::new(),
        }
    }

    /// The eraser: the same brush, removing.
    pub fn erasing(mut self) -> NewStroke {
        self.blend = StrokeBlend::Erase;
        self
    }

    pub fn with_size(mut self, size: f32) -> NewStroke {
        self.settings = self.settings.with_size(size);
        self
    }

    pub fn with_settings(mut self, settings: BrushSettings) -> NewStroke {
        self.settings = settings.sanitized();
        self
    }

    pub fn with_points(mut self, points: Vec<BrushPoint>) -> NewStroke {
        self.points = points;
        self
    }
}

/// The preset a stroke names, when this version has it.
pub fn preset_of(stroke: &StrokeOp) -> Option<&'static Preset> {
    brush::preset(&stroke.preset)
}

pub(crate) fn settings_fields(s: &BrushSettings) -> Vec<(&'static str, SettingValue)> {
    vec![
        ("size", SettingValue::N(s.size)),
        ("hardness", SettingValue::N(s.hardness)),
        ("spacing", SettingValue::N(s.spacing)),
        ("flow", SettingValue::N(s.flow)),
        ("opacity", SettingValue::N(s.opacity)),
        ("pressure_size", SettingValue::Curve(s.pressure_size)),
        ("pressure_opacity", SettingValue::Curve(s.pressure_opacity)),
        ("tilt_size", SettingValue::Curve(s.tilt_size)),
        ("tilt_opacity", SettingValue::Curve(s.tilt_opacity)),
        ("jitter_size", SettingValue::N(s.jitter.size)),
        ("jitter_opacity", SettingValue::N(s.jitter.opacity)),
        ("jitter_position", SettingValue::N(s.jitter.position)),
        ("grain", SettingValue::N(s.grain)),
        ("wet_edge", SettingValue::N(s.wet_edge)),
    ]
}

/// One stored brush setting: a number or a curve `[at0, at1, gamma]`.
pub(crate) enum SettingValue {
    N(f32),
    Curve(brush::Curve),
}

/// The settings read from stored fields over `base` (the named preset's,
/// so a field a later version adds reads as the preset's value here).
pub(crate) fn settings_from(base: BrushSettings, get: impl Fn(&str) -> Option<Vec<f32>>) -> BrushSettings {
    let mut s = base;
    let n = |k: &str, v: &mut f32| {
        if let Some(x) = get(k).and_then(|x| x.first().copied()) {
            *v = x;
        }
    };
    let c = |k: &str, v: &mut brush::Curve| {
        if let Some(x) = get(k) {
            if x.len() >= 3 {
                *v = brush::Curve::new(x[0], x[1], x[2]);
            }
        }
    };
    n("size", &mut s.size);
    n("hardness", &mut s.hardness);
    n("spacing", &mut s.spacing);
    n("flow", &mut s.flow);
    n("opacity", &mut s.opacity);
    c("pressure_size", &mut s.pressure_size);
    c("pressure_opacity", &mut s.pressure_opacity);
    c("tilt_size", &mut s.tilt_size);
    c("tilt_opacity", &mut s.tilt_opacity);
    n("jitter_size", &mut s.jitter.size);
    n("jitter_opacity", &mut s.jitter.opacity);
    n("jitter_position", &mut s.jitter.position);
    n("grain", &mut s.grain);
    n("wet_edge", &mut s.wet_edge);
    s.sanitized()
}

pub(crate) fn pack_brush_points(points: &[BrushPoint]) -> Vec<u8> {
    let mut out = Vec::with_capacity(points.len() * 16);
    for p in points {
        for v in p {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

pub(crate) fn unpack_brush_points(bytes: &[u8], out: &mut Vec<BrushPoint>) {
    for c in bytes.chunks_exact(16) {
        let f = |i: usize| f32::from_le_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
        out.push([f(0), f(4), f(8), f(12)]);
    }
}

/// A paint operation resolved for drawing: what it is, where it reaches,
/// and its fingerprint.
#[derive(Debug, Clone)]
pub struct PaintOp {
    pub raw: RawPaintOp,
    pub bounds: Option<Rect>,
    pub fingerprint: u64,
}

impl PaintOp {
    pub fn of(raw: RawPaintOp) -> PaintOp {
        let bounds = raw.bounds();
        let fingerprint = fingerprint(&raw);
        PaintOp { raw, bounds, fingerprint }
    }

    pub fn id(&self) -> &str {
        &self.raw.id
    }
}

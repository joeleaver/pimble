//! The brush engine (docs/DRAWING_CONTRACT.md section 3, "Painting"): brush
//! settings and presets as data, the dabs a stroke's points make, and how
//! dabs are stamped into a tile and the stroke laid onto it. Pure and
//! deterministic: the same points, settings and op id give the same pixels
//! on every replay on one device (floating point between the desktop and
//! the browser may differ in the last bit, which the contract accepts).
//!
//! **How a stroke is drawn** (per tile, [`crate::tiles`] does the walking):
//!
//! 1. The points (board units, with pressure and tilt) are walked into dabs
//!    ([`DabWalker`]): one at the first point, then one every
//!    `spacing × diameter` along the polyline, pressure and tilt interpolated
//!    between points. A dab's diameter is `size × pressure_size(p) ×
//!    tilt_size(t)`, its alpha `pressure_opacity(p) × tilt_opacity(t)`, both
//!    then jittered by a generator seeded from the op id and the dab's index,
//!    so appending points never changes a dab already made.
//! 2. Dabs are stamped into the stroke's own **coverage mask** (f32 per
//!    pixel): each pixel moves toward the dab's alpha times its falloff by
//!    `flow` (`m += (target - m) × flow` when the target is higher), so a
//!    stroke crossing itself does not darken past what its pressure asks
//!    for, and a low flow builds up as an airbrush does.
//! 3. The mask is laid onto the layer's pixels (premultiplied RGBA8) with
//!    alpha `opacity × wet(m) × grain(x, y)`: normal paints the color,
//!    erase removes.
//!
//! The mask is f32 so a soft, low-flow brush (the airbrush, 0.06 per dab)
//! accumulates without the banding u8 steps would leave; the layer is u8
//! premultiplied because that is what tiny-skia composites and what a tile
//! costs in memory and on disk (see "Spike: brush engine" in the contract).

use std::fmt;

use crate::model::{Blend, Color};

/// Pixels per board unit in a paint layer (Joe's decision 4).
pub const PX_PER_UNIT: f32 = 2.0;

/// A response curve: `at0 + (at1 - at0) × t^gamma` for an input `t` in 0 to 1
/// (pressure, or tilt where 0 is upright and 1 is flat). A non-finite input
/// reads as 1 (full pressure, flat tilt is never assumed: see
/// [`BrushPoint`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve {
    pub at0: f32,
    pub at1: f32,
    pub gamma: f32,
}

impl Curve {
    /// 1 whatever the input.
    pub const FLAT: Curve = Curve { at0: 1.0, at1: 1.0, gamma: 1.0 };

    pub const fn new(at0: f32, at1: f32, gamma: f32) -> Curve {
        Curve { at0, at1, gamma }
    }

    pub fn at(&self, t: f32) -> f32 {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 1.0 };
        let g = if self.gamma.is_finite() { self.gamma.clamp(0.05, 20.0) } else { 1.0 };
        self.at0 + (self.at1 - self.at0) * t.powf(g)
    }

    /// The largest value it gives.
    pub fn max(&self) -> f32 {
        self.at0.max(self.at1)
    }

    fn sanitized(self) -> Curve {
        let f = |v: f32, d: f32| if v.is_finite() { v.clamp(0.0, 10.0) } else { d };
        Curve { at0: f(self.at0, 1.0), at1: f(self.at1, 1.0), gamma: if self.gamma.is_finite() { self.gamma.clamp(0.05, 20.0) } else { 1.0 } }
    }
}

/// Random variation per dab, each a fraction: of the size (a dab is up to
/// that much smaller), of the alpha, and of the diameter (how far a dab may
/// stray from the line).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Jitter {
    pub size: f32,
    pub opacity: f32,
    pub position: f32,
}

/// Everything a stroke is drawn with besides its color and points. Stored
/// whole with every stroke (`brush`), so a preset changed by a later version
/// never redraws an old painting differently.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushSettings {
    /// The dab's diameter at full pressure, in board units.
    pub size: f32,
    /// 0 a soft falloff from the center, 1 a hard edge (one pixel of
    /// antialiasing is always kept).
    pub hardness: f32,
    /// The distance between dabs as a fraction of the dab's diameter.
    pub spacing: f32,
    /// How far each dab moves a pixel toward its alpha (0 to 1).
    pub flow: f32,
    /// The stroke's alpha at full coverage (0 to 1).
    pub opacity: f32,
    pub pressure_size: Curve,
    pub pressure_opacity: Curve,
    pub tilt_size: Curve,
    pub tilt_opacity: Curve,
    pub jitter: Jitter,
    /// Paper grain (0 none to 1 strong): the alpha is modulated by a texture
    /// fixed to the board, so grain lines up across strokes and tiles.
    pub grain: f32,
    /// Watercolor's wet edge (0 none to 1 strong): the middle of a stroke
    /// paler than its rim.
    pub wet_edge: f32,
}

impl Default for BrushSettings {
    fn default() -> Self {
        ROUND.settings
    }
}

impl BrushSettings {
    /// Every value finite and in range (a stored stroke is read through
    /// this, so a malformed one still draws something bounded).
    pub fn sanitized(self) -> BrushSettings {
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        BrushSettings {
            size: f(self.size, MIN_SIZE, MAX_SIZE, 8.0),
            hardness: f(self.hardness, 0.0, 1.0, 0.8),
            spacing: f(self.spacing, 0.02, 4.0, 0.15),
            flow: f(self.flow, 0.0, 1.0, 1.0),
            opacity: f(self.opacity, 0.0, 1.0, 1.0),
            pressure_size: self.pressure_size.sanitized(),
            pressure_opacity: self.pressure_opacity.sanitized(),
            tilt_size: self.tilt_size.sanitized(),
            tilt_opacity: self.tilt_opacity.sanitized(),
            jitter: Jitter {
                size: f(self.jitter.size, 0.0, 1.0, 0.0),
                opacity: f(self.jitter.opacity, 0.0, 1.0, 0.0),
                position: f(self.jitter.position, 0.0, 4.0, 0.0),
            },
            grain: f(self.grain, 0.0, 1.0, 0.0),
            wet_edge: f(self.wet_edge, 0.0, 1.0, 0.0),
        }
    }

    /// The farthest any dab's paint reaches from its point, in board units
    /// (its largest radius plus its largest stray).
    pub fn reach(&self) -> f32 {
        let d = self.size * self.pressure_size.max() * self.tilt_size.max();
        d * 0.5 + d * self.jitter.position + 1.0 / PX_PER_UNIT
    }

    /// The same settings at another size (the context bar's size, `[` and
    /// `]`).
    pub fn with_size(mut self, size: f32) -> BrushSettings {
        self.size = size;
        self.sanitized()
    }
}

/// The smallest and largest brush, in board units.
pub const MIN_SIZE: f32 = 0.5;
pub const MAX_SIZE: f32 = 2000.0;

/// How a stroke meets the layer's pixels: paint, or the eraser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StrokeBlend {
    #[default]
    Normal,
    Erase,
}

impl StrokeBlend {
    pub fn as_str(&self) -> &'static str {
        match self {
            StrokeBlend::Normal => "normal",
            StrokeBlend::Erase => "erase",
        }
    }

    /// Anything but "erase" reads as normal.
    pub fn parse(s: &str) -> StrokeBlend {
        if s == "erase" {
            StrokeBlend::Erase
        } else {
            StrokeBlend::Normal
        }
    }
}

impl fmt::Display for StrokeBlend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A brush preset: data, named in the stroke (`preset`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    /// Stored in the stroke and taken by the MCP ("round", "highlighter").
    pub name: &'static str,
    /// What a person reads ("Round", "Watercolor").
    pub label: &'static str,
    pub settings: BrushSettings,
    /// The color it starts with when the person has picked none.
    pub color: Color,
    /// The blend a paint layer made for it alone takes: multiply for the
    /// highlighter ("Highlights"), normal for the rest.
    pub layer_blend: Blend,
}

const NO_JITTER: Jitter = Jitter { size: 0.0, opacity: 0.0, position: 0.0 };
const INK_COLOR: Color = Color::rgb(0x21, 0x25, 0x29);

pub const ROUND: Preset = Preset {
    name: "round",
    label: "Round",
    settings: BrushSettings {
        size: 8.0,
        hardness: 0.8,
        spacing: 0.12,
        flow: 1.0,
        opacity: 1.0,
        pressure_size: Curve::new(0.25, 1.0, 1.0),
        pressure_opacity: Curve::new(0.6, 1.0, 1.0),
        tilt_size: Curve::FLAT,
        tilt_opacity: Curve::FLAT,
        jitter: NO_JITTER,
        grain: 0.0,
        wet_edge: 0.0,
    },
    color: INK_COLOR,
    layer_blend: Blend::Normal,
};

pub const PENCIL: Preset = Preset {
    name: "pencil",
    label: "Pencil",
    settings: BrushSettings {
        size: 2.0,
        hardness: 0.9,
        spacing: 0.15,
        flow: 0.8,
        opacity: 0.9,
        pressure_size: Curve::new(0.6, 1.0, 1.0),
        pressure_opacity: Curve::new(0.15, 1.0, 1.5),
        // Laid flat, a pencil shades: wider and fainter.
        tilt_size: Curve::new(1.0, 4.0, 2.0),
        tilt_opacity: Curve::new(1.0, 0.45, 1.0),
        jitter: Jitter { size: 0.0, opacity: 0.1, position: 0.0 },
        grain: 0.55,
        wet_edge: 0.0,
    },
    color: Color::rgb(0x34, 0x3a, 0x40),
    layer_blend: Blend::Normal,
};

pub const INK: Preset = Preset {
    name: "ink",
    label: "Ink",
    settings: BrushSettings {
        size: 6.0,
        hardness: 0.95,
        spacing: 0.08,
        flow: 1.0,
        opacity: 1.0,
        pressure_size: Curve::new(0.1, 1.0, 1.4),
        pressure_opacity: Curve::FLAT,
        tilt_size: Curve::FLAT,
        tilt_opacity: Curve::FLAT,
        jitter: NO_JITTER,
        grain: 0.0,
        wet_edge: 0.0,
    },
    color: Color::rgb(0x10, 0x10, 0x18),
    layer_blend: Blend::Normal,
};

pub const AIRBRUSH: Preset = Preset {
    name: "airbrush",
    label: "Airbrush",
    settings: BrushSettings {
        size: 60.0,
        hardness: 0.0,
        spacing: 0.08,
        flow: 0.06,
        opacity: 1.0,
        pressure_size: Curve::new(0.7, 1.0, 1.0),
        pressure_opacity: Curve::new(0.0, 1.0, 1.0),
        tilt_size: Curve::new(1.0, 1.6, 1.0),
        tilt_opacity: Curve::FLAT,
        jitter: NO_JITTER,
        grain: 0.0,
        wet_edge: 0.0,
    },
    color: Color::rgb(0x4d, 0xab, 0xf7),
    layer_blend: Blend::Normal,
};

pub const MARKER: Preset = Preset {
    name: "marker",
    label: "Marker",
    settings: BrushSettings {
        size: 14.0,
        hardness: 0.85,
        spacing: 0.1,
        flow: 1.0,
        opacity: 0.75,
        pressure_size: Curve::new(0.85, 1.0, 1.0),
        pressure_opacity: Curve::FLAT,
        tilt_size: Curve::FLAT,
        tilt_opacity: Curve::FLAT,
        jitter: NO_JITTER,
        grain: 0.0,
        wet_edge: 0.0,
    },
    color: Color::rgb(0xe8, 0x59, 0x0c),
    layer_blend: Blend::Normal,
};

pub const CHARCOAL: Preset = Preset {
    name: "charcoal",
    label: "Charcoal",
    settings: BrushSettings {
        size: 12.0,
        hardness: 0.4,
        spacing: 0.1,
        flow: 0.6,
        opacity: 0.95,
        pressure_size: Curve::new(0.5, 1.0, 1.0),
        pressure_opacity: Curve::new(0.2, 1.0, 1.0),
        tilt_size: Curve::new(1.0, 3.0, 1.5),
        tilt_opacity: Curve::new(1.0, 0.6, 1.0),
        jitter: Jitter { size: 0.25, opacity: 0.3, position: 0.12 },
        grain: 0.85,
        wet_edge: 0.0,
    },
    color: Color::rgb(0x2b, 0x2b, 0x2b),
    layer_blend: Blend::Normal,
};

pub const WATERCOLOR: Preset = Preset {
    name: "watercolor",
    label: "Watercolor",
    settings: BrushSettings {
        size: 30.0,
        hardness: 0.3,
        spacing: 0.1,
        flow: 0.35,
        opacity: 0.6,
        pressure_size: Curve::new(0.6, 1.0, 1.0),
        pressure_opacity: Curve::new(0.4, 1.0, 1.0),
        tilt_size: Curve::FLAT,
        tilt_opacity: Curve::FLAT,
        jitter: Jitter { size: 0.15, opacity: 0.1, position: 0.05 },
        grain: 0.25,
        wet_edge: 0.8,
    },
    color: Color::rgb(0x22, 0x8b, 0xe6),
    layer_blend: Blend::Normal,
};

pub const HIGHLIGHTER: Preset = Preset {
    name: "highlighter",
    label: "Highlighter",
    settings: BrushSettings {
        size: 18.0,
        hardness: 0.9,
        spacing: 0.1,
        flow: 1.0,
        opacity: 1.0,
        pressure_size: Curve::FLAT,
        pressure_opacity: Curve::FLAT,
        tilt_size: Curve::FLAT,
        tilt_opacity: Curve::FLAT,
        jitter: NO_JITTER,
        grain: 0.0,
        wet_edge: 0.0,
    },
    color: Color::rgb(0xff, 0xeb, 0x3b),
    layer_blend: Blend::Multiply,
};

/// Every preset, in the order the context bar offers them.
pub const PRESETS: [Preset; 8] = [ROUND, PENCIL, INK, AIRBRUSH, MARKER, CHARCOAL, WATERCOLOR, HIGHLIGHTER];

/// The preset named `name` (case does not matter).
pub fn preset(name: &str) -> Option<&'static Preset> {
    let name = name.trim();
    PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name) || p.label.eq_ignore_ascii_case(name))
}

/// One point of a stroke: x and y in board units, pressure 0 to 1 (1 for an
/// input without pressure: a brush's size is its size at full pressure),
/// tilt 0 upright to 1 lying flat (0 for an input without tilt). Stored as
/// four little-endian f32 per point, appended in chunks as it is drawn.
pub type BrushPoint = [f32; 4];

/// One stamp of the brush, in board units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dab {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    /// The alpha it moves pixels toward (before falloff and flow).
    pub alpha: f32,
}

/// Walks a stroke's points into dabs, and can be fed more points later: the
/// dabs it made stay the dabs it would make from all the points at once.
#[derive(Debug, Clone)]
pub struct DabWalker {
    settings: BrushSettings,
    seed: u64,
    last: Option<BrushPoint>,
    /// Distance along the polyline from `last` to the next dab.
    until_next: f32,
    /// Dabs made so far (the jitter generator's index).
    made: u64,
    /// Points fed so far.
    fed: usize,
}

impl DabWalker {
    /// `seed` is [`seed_of`] the op id.
    pub fn new(settings: &BrushSettings, seed: u64) -> DabWalker {
        DabWalker { settings: settings.sanitized(), seed, last: None, until_next: 0.0, made: 0, fed: 0 }
    }

    /// Points fed so far.
    pub fn fed(&self) -> usize {
        self.fed
    }

    /// Walk `points` (the ones after those fed already), pushing the dabs
    /// they make onto `out`.
    pub fn feed(&mut self, points: &[BrushPoint], out: &mut Vec<Dab>) {
        for p in points {
            self.fed += 1;
            if !(p[0].is_finite() && p[1].is_finite()) {
                continue;
            }
            let Some(last) = self.last else {
                self.emit(p, out);
                self.last = Some(*p);
                continue;
            };
            let (dx, dy) = (p[0] - last[0], p[1] - last[1]);
            let len = (dx * dx + dy * dy).sqrt();
            if len <= 0.0 {
                continue;
            }
            let mut along = 0.0;
            // A bound against a pathological spacing: never more than one
            // dab per quarter pixel.
            while self.until_next <= len - along {
                along += self.until_next;
                let t = along / len;
                let q = [last[0] + dx * t, last[1] + dy * t, lerp(last[2], p[2], t), lerp(last[3], p[3], t)];
                self.emit(&q, out);
            }
            self.until_next -= len - along;
            self.last = Some(*p);
        }
    }

    fn emit(&mut self, p: &BrushPoint, out: &mut Vec<Dab>) {
        let s = &self.settings;
        let (pressure, tilt) = (pressure_of(p[2]), tilt_of(p[3]));
        let diameter = (s.size * s.pressure_size.at(pressure) * s.tilt_size.at(tilt)).max(0.0);
        let mut rng = SplitMix(self.seed ^ self.made.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        self.made += 1;
        let (r1, r2, r3, r4) = (rng.unit(), rng.unit(), rng.unit(), rng.unit());
        let radius = (0.5 * diameter * (1.0 - s.jitter.size * r1)).max(0.35 / PX_PER_UNIT);
        let alpha = (s.pressure_opacity.at(pressure) * s.tilt_opacity.at(tilt) * (1.0 - s.jitter.opacity * r2)).clamp(0.0, 1.0);
        let stray = s.jitter.position * diameter;
        out.push(Dab { x: p[0] + (r3 * 2.0 - 1.0) * stray, y: p[1] + (r4 * 2.0 - 1.0) * stray, radius, alpha });
        // The next dab: a fraction of this one's diameter on, never closer
        // than a quarter pixel.
        self.until_next = (s.spacing * diameter).max(0.25 / PX_PER_UNIT);
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn pressure_of(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn tilt_of(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Every dab `points` make with `settings`, seeded with `seed`.
pub fn dabs(settings: &BrushSettings, seed: u64, points: &[BrushPoint]) -> Vec<Dab> {
    let mut out = Vec::new();
    DabWalker::new(settings, seed).feed(points, &mut out);
    out
}

/// The jitter seed of an op id: the same on every device.
pub fn seed_of(op_id: &str) -> u64 {
    // FNV-1a, fixed forever: a stored stroke's dabs depend on it.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in op_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

// ── Stamping and laying down ─────────────────────────────────────────────

/// A rectangle of pixels in a tile, `x0..x1` by `y0..y1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PxRect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl PxRect {
    pub const EMPTY: PxRect = PxRect { x0: u32::MAX, y0: u32::MAX, x1: 0, y1: 0 };

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn union(&mut self, o: PxRect) {
        if o.is_empty() {
            return;
        }
        self.x0 = self.x0.min(o.x0);
        self.y0 = self.y0.min(o.y0);
        self.x1 = self.x1.max(o.x1);
        self.y1 = self.y1.max(o.y1);
    }
}

/// Stamp `dab` into `mask` (`size` × `size` pixels whose pixel (0, 0) is the
/// board's pixel (`ox`, `oy`) at [`PX_PER_UNIT`]). Answers the pixels it
/// touched.
pub(crate) fn stamp(mask: &mut [f32], size: u32, ox: i64, oy: i64, dab: &Dab, hardness: f32, flow: f32) -> PxRect {
    let cx = dab.x * PX_PER_UNIT - ox as f32;
    let cy = dab.y * PX_PER_UNIT - oy as f32;
    let r = dab.radius * PX_PER_UNIT;
    if !(cx.is_finite() && cy.is_finite() && r > 0.0) || dab.alpha <= 0.0 || flow <= 0.0 {
        return PxRect::EMPTY;
    }
    let x0 = (cx - r).floor().max(0.0);
    let y0 = (cy - r).floor().max(0.0);
    let x1 = (cx + r).ceil().min(size as f32);
    let y1 = (cy + r).ceil().min(size as f32);
    if x1 <= x0 || y1 <= y0 {
        return PxRect::EMPTY;
    }
    let (x0, y0, x1, y1) = (x0 as u32, y0 as u32, x1 as u32, y1 as u32);
    // The hard core, and the falloff to the rim: at least a pixel of it.
    let inner = (hardness.clamp(0.0, 1.0) * r).min(r - 1.0).max(0.0);
    let soft = (r - inner).max(1e-3);
    let r2 = r * r;
    let inner2 = inner * inner;
    let w = size as usize;
    for py in y0..y1 {
        let dy = py as f32 + 0.5 - cy;
        let dy2 = dy * dy;
        if dy2 >= r2 {
            continue;
        }
        // Only the pixels of this row inside the circle.
        let half = (r2 - dy2).sqrt();
        let sx0 = ((cx - half - 0.5).floor().max(x0 as f32)) as u32;
        let sx1 = ((cx + half + 0.5).ceil().min(x1 as f32)) as u32;
        let row = &mut mask[py as usize * w..py as usize * w + w];
        for px in sx0..sx1 {
            let dx = px as f32 + 0.5 - cx;
            let d2 = dx * dx + dy2;
            if d2 >= r2 {
                continue;
            }
            let target = if d2 <= inner2 {
                dab.alpha
            } else {
                let t = (d2.sqrt() - inner) / soft;
                dab.alpha * (1.0 - t * t * (3.0 - 2.0 * t))
            };
            let m = &mut row[px as usize];
            if target > *m {
                *m += (target - *m) * flow;
            }
        }
    }
    PxRect { x0, y0, x1, y1 }
}

/// What a stroke lays down: color (straight), blend and the settings its
/// alpha is shaped by.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Lay {
    pub color: Color,
    pub blend: StrokeBlend,
    pub opacity: f32,
    pub grain: f32,
    pub wet_edge: f32,
}

/// Lay the stroke's `mask` onto `tile` (premultiplied RGBA8, `size` square)
/// within `rect`: `tile = base ∘ stroke` where `base` is the tile's pixels
/// before the stroke (`None`: the tile itself, laid down in place).
pub(crate) fn lay(tile: &mut [u8], base: Option<&[u8]>, mask: &[f32], size: u32, ox: i64, oy: i64, rect: PxRect, how: &Lay) {
    if rect.is_empty() {
        return;
    }
    let w = size as usize;
    let ca = how.color.a as f32 / 255.0;
    let (cr, cg, cb) = (how.color.r as f32, how.color.g as f32, how.color.b as f32);
    for py in rect.y0..rect.y1 {
        let row = py as usize * w;
        for px in rect.x0..rect.x1 {
            let i = row + px as usize;
            let m = mask[i];
            let src = match base {
                Some(b) => [b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]],
                None => [tile[i * 4], tile[i * 4 + 1], tile[i * 4 + 2], tile[i * 4 + 3]],
            };
            if m <= 0.0 {
                if base.is_some() {
                    tile[i * 4..i * 4 + 4].copy_from_slice(&src);
                }
                continue;
            }
            let mut a = how.opacity * m;
            if how.wet_edge > 0.0 {
                a *= 1.0 - how.wet_edge * 0.55 * m * m * m;
            }
            if how.grain > 0.0 {
                a *= 1.0 - how.grain * (1.0 - grain_at(ox + px as i64, oy + py as i64));
            }
            let out = match how.blend {
                StrokeBlend::Normal => {
                    let sa = (a * ca).clamp(0.0, 1.0);
                    let k = 1.0 - sa;
                    [
                        cr * sa + src[0] as f32 * k,
                        cg * sa + src[1] as f32 * k,
                        cb * sa + src[2] as f32 * k,
                        255.0 * sa + src[3] as f32 * k,
                    ]
                }
                StrokeBlend::Erase => {
                    let k = 1.0 - a.clamp(0.0, 1.0);
                    [src[0] as f32 * k, src[1] as f32 * k, src[2] as f32 * k, src[3] as f32 * k]
                }
            };
            for c in 0..4 {
                tile[i * 4 + c] = (out[c] + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// The paper texture at a board pixel, 0 to 1: value noise on a 3-pixel
/// lattice mixed with a per-pixel speck, fixed to the board so it lines up
/// across tiles and strokes.
pub(crate) fn grain_at(gx: i64, gy: i64) -> f32 {
    const CELL: i64 = 3;
    let (cx, cy) = (gx.div_euclid(CELL), gy.div_euclid(CELL));
    let (fx, fy) = ((gx.rem_euclid(CELL)) as f32 / CELL as f32, (gy.rem_euclid(CELL)) as f32 / CELL as f32);
    let v = |x: i64, y: i64| hash2(x, y);
    let top = lerp(v(cx, cy), v(cx + 1, cy), fx);
    let bottom = lerp(v(cx, cy + 1), v(cx + 1, cy + 1), fx);
    let smooth = lerp(top, bottom, fy);
    0.65 * smooth + 0.35 * hash2(gx ^ 0x5bd1, gy ^ 0x1b87)
}

fn hash2(x: i64, y: i64) -> f32 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    (h >> 40) as f32 / (1u64 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize, step: f32) -> Vec<BrushPoint> {
        (0..n).map(|i| [i as f32 * step, 10.0, 1.0, 0.0]).collect()
    }

    #[test]
    fn appending_points_never_changes_the_dabs_already_made() {
        for p in PRESETS {
            let pts = line(80, 1.7);
            let whole = dabs(&p.settings, seed_of("op"), &pts);
            let mut walker = DabWalker::new(&p.settings, seed_of("op"));
            let mut parts = Vec::new();
            for chunk in pts.chunks(7) {
                walker.feed(chunk, &mut parts);
            }
            assert_eq!(whole, parts, "{}", p.name);
            assert!(!whole.is_empty());
        }
    }

    #[test]
    fn spacing_follows_the_diameter_and_jitter_follows_the_op() {
        let s = ROUND.settings;
        let d = dabs(&s, 1, &line(2, 100.0));
        // One at the start, then every 0.12 × 8 units.
        let step = 0.12 * 8.0;
        assert_eq!(d.len(), (100.0 / step) as usize + 1);
        assert!((d[1].x - d[0].x - step).abs() < 1e-3);
        let c = CHARCOAL.settings;
        assert_eq!(dabs(&c, seed_of("a"), &line(5, 10.0)), dabs(&c, seed_of("a"), &line(5, 10.0)));
        assert_ne!(dabs(&c, seed_of("a"), &line(5, 10.0)), dabs(&c, seed_of("b"), &line(5, 10.0)));
    }

    #[test]
    fn pressure_drives_size_and_alpha_through_the_curves() {
        let s = INK.settings;
        let light = dabs(&s, 1, &[[0.0, 0.0, 0.1, 0.0]])[0];
        let full = dabs(&s, 1, &[[0.0, 0.0, 1.0, 0.0]])[0];
        assert!(light.radius < full.radius * 0.3);
        assert!((full.radius - 3.0).abs() < 1e-4);
        let p = PENCIL.settings;
        let upright = dabs(&p, 1, &[[0.0, 0.0, 1.0, 0.0]])[0];
        let flat = dabs(&p, 1, &[[0.0, 0.0, 1.0, 1.0]])[0];
        assert!(flat.radius > upright.radius * 3.0);
        assert!(flat.alpha < upright.alpha);
        // No pressure reads as full.
        assert_eq!(dabs(&s, 1, &[[0.0, 0.0, f32::NAN, f32::NAN]])[0].radius, full.radius);
    }

    #[test]
    fn a_stroke_crossing_itself_does_not_darken_past_its_alpha() {
        let mut mask = vec![0.0f32; 64 * 64];
        let dab = Dab { x: 16.0, y: 16.0, radius: 8.0, alpha: 0.5 };
        for _ in 0..20 {
            stamp(&mut mask, 64, 0, 0, &dab, 0.9, 1.0);
        }
        assert!((mask[32 * 64 + 32] - 0.5).abs() < 1e-6);
        let mut soft = vec![0.0f32; 64 * 64];
        stamp(&mut soft, 64, 0, 0, &dab, 0.9, 0.1);
        assert!((soft[32 * 64 + 32] - 0.05).abs() < 1e-6);
    }

    #[test]
    fn presets_are_found_by_name_or_label() {
        assert_eq!(preset("Highlighter").unwrap().layer_blend, Blend::Multiply);
        assert_eq!(preset("watercolor").unwrap().name, "watercolor");
        assert!(preset("crayon").is_none());
        for p in PRESETS {
            assert_eq!(p.settings, p.settings.sanitized(), "{}", p.name);
        }
    }
}

//! Units and measurements on a canvas (docs/MEASURE_CONTRACT.md): the one
//! place lengths are parsed, formatted and converted.
//!
//! A canvas may carry one [`Scale`] (the board map's `scale` key, written
//! whole): the unit measurements are shown in and how many board units make
//! one of it. Nothing measured is ever stored as text: labels are computed
//! from geometry when drawn, so a change of scale relabels everything.
//!
//! **Paper.** A board unit at 100% is one CSS pixel, 1/96 inch. A ratio
//! `1:N` means one board unit is `N/96` inches in the world; `a = b` (`1 in =
//! 10 ft`) means `96 * a_in` board units are `b`. Both reduce to
//! [`Scale::per_unit`].

use std::fmt;
use std::hash::{Hash, Hasher};

use tiny_skia::{Path, PathBuilder, Transform as Affine};

use crate::model::*;
use crate::snapshot::{Element, Fx};

/// Board units in one paper inch (a CSS pixel is 1/96 inch).
pub const BOARD_PER_INCH: f64 = 96.0;
/// Meters in an inch.
const INCH_M: f64 = 0.0254;

/// A unit a canvas measures in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Unit {
    Mm,
    Cm,
    #[default]
    M,
    Km,
    In,
    Ft,
    Yd,
    Mi,
}

impl Unit {
    pub const ALL: [Unit; 8] = [Unit::Mm, Unit::Cm, Unit::M, Unit::Km, Unit::In, Unit::Ft, Unit::Yd, Unit::Mi];

    /// The stored name (`"mm"`, `"ft"`, ...), also its short symbol.
    pub fn as_str(&self) -> &'static str {
        match self {
            Unit::Mm => "mm",
            Unit::Cm => "cm",
            Unit::M => "m",
            Unit::Km => "km",
            Unit::In => "in",
            Unit::Ft => "ft",
            Unit::Yd => "yd",
            Unit::Mi => "mi",
        }
    }

    /// A unit as a person may write it: the symbol, the word (singular or
    /// plural, either spelling of meter), or a prime (`'` feet, `"` inches).
    pub fn parse(s: &str) -> Option<Unit> {
        let s = s.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "mm" | "millimeter" | "millimeters" | "millimetre" | "millimetres" => Unit::Mm,
            "cm" | "centimeter" | "centimeters" | "centimetre" | "centimetres" => Unit::Cm,
            "m" | "meter" | "meters" | "metre" | "metres" => Unit::M,
            "km" | "kilometer" | "kilometers" | "kilometre" | "kilometres" => Unit::Km,
            "in" | "inch" | "inches" | "\"" | "″" => Unit::In,
            "ft" | "foot" | "feet" | "'" | "′" => Unit::Ft,
            "yd" | "yard" | "yards" => Unit::Yd,
            "mi" | "mile" | "miles" => Unit::Mi,
            _ => return None,
        })
    }

    /// Its name for a picker ("Feet").
    pub fn label(&self) -> &'static str {
        match self {
            Unit::Mm => "Millimeters",
            Unit::Cm => "Centimeters",
            Unit::M => "Meters",
            Unit::Km => "Kilometers",
            Unit::In => "Inches",
            Unit::Ft => "Feet",
            Unit::Yd => "Yards",
            Unit::Mi => "Miles",
        }
    }

    /// Meters in one of it.
    pub fn meters(&self) -> f64 {
        match self {
            Unit::Mm => 0.001,
            Unit::Cm => 0.01,
            Unit::M => 1.0,
            Unit::Km => 1000.0,
            Unit::In => INCH_M,
            Unit::Ft => 12.0 * INCH_M,
            Unit::Yd => 36.0 * INCH_M,
            Unit::Mi => 63_360.0 * INCH_M,
        }
    }

    pub fn is_metric(&self) -> bool {
        matches!(self, Unit::Mm | Unit::Cm | Unit::M | Unit::Km)
    }

    /// The precision used when a scale names none: metric, decimals (mm 0,
    /// cm 1, m 2, km 3); imperial, the smallest inch fraction as a
    /// denominator (16 for inches, 1 for feet and yards); miles, decimals
    /// (2).
    pub fn default_precision(&self) -> u8 {
        match self {
            Unit::Mm => 0,
            Unit::Cm => 1,
            Unit::M => 2,
            Unit::Km => 3,
            Unit::In => 16,
            Unit::Ft | Unit::Yd => 1,
            Unit::Mi => 2,
        }
    }

    /// The precisions a picker offers.
    pub fn precisions(&self) -> &'static [u8] {
        match self {
            Unit::In | Unit::Ft | Unit::Yd => &[1, 2, 4, 8, 16],
            _ => &[0, 1, 2, 3],
        }
    }

    /// A precision as a person reads it ("1/16″", "0.01").
    pub fn precision_label(&self, p: u8) -> String {
        match self {
            Unit::In | Unit::Ft | Unit::Yd => {
                if p <= 1 {
                    "1″".into()
                } else {
                    format!("1/{p}″")
                }
            }
            _ => {
                if p == 0 {
                    "1".into()
                } else {
                    format!("0.{}1", "0".repeat(p as usize - 1))
                }
            }
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A canvas's scale (stored as the board map's `scale`, written whole).
#[derive(Debug, Clone, PartialEq)]
pub struct Scale {
    /// The unit measurements are shown in.
    pub unit: Unit,
    /// Board units in one `unit`: the one number everything is computed
    /// from. Always finite and positive in a valid scale.
    pub per_unit: f64,
    /// How the person wrote it (`1/4" = 1'`, `1:100`), shown back; never
    /// parsed again.
    pub label: Option<String>,
    /// Decimals (metric, miles) or the smallest inch fraction's denominator
    /// (inches, feet, yards); absent: [`Unit::default_precision`].
    pub precision: Option<u8>,
    /// Measuring switched off (Joe, 2026-11-22: "artists won't want it"):
    /// the scale is kept, and nothing measures until it is switched on.
    /// Stored as the map's `off`; absent is on.
    pub off: bool,
}

impl Scale {
    pub fn new(unit: Unit, per_unit: f64) -> Self {
        Scale { unit, per_unit, label: None, precision: None, off: false }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        let label = label.into();
        self.label = (!label.trim().is_empty()).then(|| label.trim().to_string());
        self
    }

    pub fn with_precision(mut self, precision: Option<u8>) -> Self {
        self.precision = precision;
        self
    }

    /// Finite and positive.
    pub fn is_valid(&self) -> bool {
        self.per_unit.is_finite() && self.per_unit > 0.0
    }

    /// The precision in effect (see [`Unit::default_precision`]), clamped
    /// to what the unit offers.
    pub fn precision(&self) -> u8 {
        let p = self.precision.unwrap_or_else(|| self.unit.default_precision());
        match self.unit {
            Unit::In | Unit::Ft | Unit::Yd => [1u8, 2, 4, 8, 16].into_iter().rev().find(|d| *d <= p.max(1)).unwrap_or(1),
            _ => p.min(3),
        }
    }

    /// Board units in a meter.
    pub fn board_per_meter(&self) -> f64 {
        self.per_unit / self.unit.meters()
    }

    /// A length in board units, in meters.
    pub fn to_meters(&self, board: f64) -> f64 {
        board / self.board_per_meter()
    }

    /// A length in meters, in board units.
    pub fn to_board(&self, meters: f64) -> f64 {
        meters * self.board_per_meter()
    }

    /// A length in board units as a person reads it (`12′ 6″`, `3.50 m`).
    pub fn length(&self, board: f64) -> String {
        format_length(self.to_meters(board), self.unit, self.precision())
    }

    /// An area in square board units as a person reads it (`120 sq ft`).
    pub fn area(&self, board_sq: f64) -> String {
        let k = self.board_per_meter();
        format_area(board_sq / (k * k), self.unit, self.precision())
    }

    /// A length typed by a person (see [`parse_length`]), in board units.
    pub fn parse_board(&self, s: &str) -> Option<f64> {
        parse_length(s, self.unit).map(|m| self.to_board(m))
    }

    /// What one paper inch is in the world, written in this scale's unit
    /// ("10 ft", "2.54 m"), and one unit at 100% in pixels: the preview
    /// line of the Scale dialog ("1 in on paper = 10′; 1 ft = 9.6 px at
    /// 100%").
    pub fn describe(&self) -> String {
        let inch_world = self.to_meters(BOARD_PER_INCH);
        let paper = if self.unit.is_metric() {
            // One centimeter of paper reads better for metric work.
            format!("1 cm on paper = {}", format_length(self.to_meters(BOARD_PER_INCH / 2.54), self.unit, self.precision().max(2)))
        } else {
            format!("1 in on paper = {}", format_length(inch_world, self.unit, self.precision().max(1)))
        };
        format!("{paper}; 1 {} = {} px at 100%", self.unit, trim_number(self.per_unit, 3))
    }

    /// What the scale is called on a button: the person's own words, or a
    /// ratio for metric work and an equation for imperial.
    pub fn name(&self) -> String {
        if let Some(l) = &self.label {
            return l.clone();
        }
        if self.unit.is_metric() {
            // 1 board unit = 1/96 in of paper; the world is to_meters(1).
            let n = self.to_meters(1.0) / (INCH_M / BOARD_PER_INCH);
            format!("1:{}", group_thousands(&trim_number(n, 2)))
        } else {
            format!("1 in = {}", format_length(self.to_meters(BOARD_PER_INCH), self.unit, self.precision()))
        }
    }

    pub(crate) fn hash_into(&self, h: &mut impl Hasher) {
        self.unit.hash(h);
        self.per_unit.to_bits().hash(h);
        self.precision().hash(h);
    }
}

/// `v` with at most `decimals` decimals, trailing zeros dropped.
fn trim_number(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// `12345.6` as `12,345.6`.
fn group_thousands(s: &str) -> String {
    let (sign, rest) = s.strip_prefix('-').map_or(("", s), |r| ("-", r));
    let (int, frac) = rest.split_once('.').map_or((rest, None), |(i, f)| (i, Some(f)));
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    match frac {
        Some(f) => format!("{sign}{out}.{f}"),
        None => format!("{sign}{out}"),
    }
}

/// `v` with exactly `decimals` decimals and thousands grouped.
fn fixed(v: f64, decimals: u8) -> String {
    // Halves round away from zero (as people round), with a nudge past
    // binary fractions just under a half (12.25 is 12.2499... in f64).
    let k = 10f64.powi(decimals as i32);
    let r = ((v * k) + v.signum() * 1e-9 * (v * k).abs().max(1.0)).round() / k;
    group_thousands(&format!("{r:.*}", decimals as usize))
}

/// A whole and a fraction of inches, rounded to `1/den`: `6`, `6 1/2`, `3/8`.
fn inches_text(inches: f64, den: u8) -> (String, bool) {
    let den = den.max(1) as i64;
    let total = (inches * den as f64).round() as i64;
    let (whole, mut num) = (total / den, total % den);
    let mut d = den;
    while num > 0 && num % 2 == 0 && d % 2 == 0 {
        num /= 2;
        d /= 2;
    }
    let text = match (whole, num) {
        (w, 0) => w.to_string(),
        (0, n) => format!("{n}/{d}"),
        (w, n) => format!("{w} {n}/{d}"),
    };
    (text, total == 0)
}

/// A length in meters as a person reads it in `unit` at `precision` (see
/// [`Scale::precision`]): metric and miles as decimals (`3.50 m`,
/// `0.35 mi`), inches with fractions (`3 3/8″`), feet and yards as feet and
/// inches (`12′ 6″`, `12′`, `6″`).
pub fn format_length(meters: f64, unit: Unit, precision: u8) -> String {
    if !meters.is_finite() {
        return "?".into();
    }
    let sign = if meters < 0.0 { "-" } else { "" };
    let m = meters.abs();
    match unit {
        Unit::In => {
            let (t, _) = inches_text(m / INCH_M, precision);
            format!("{sign}{t}″")
        }
        Unit::Ft | Unit::Yd => {
            let den = precision.max(1) as f64;
            let inches = (m / INCH_M * den).round() / den;
            let feet = (inches / 12.0).floor();
            let rest = inches - feet * 12.0;
            let (t, zero) = inches_text(rest, precision);
            match (feet as i64, zero) {
                (0, _) => format!("{sign}{t}″"),
                (f, true) => format!("{sign}{}′", group_thousands(&f.to_string())),
                (f, false) => format!("{sign}{}′ {t}″", group_thousands(&f.to_string())),
            }
        }
        _ => format!("{sign}{} {}", fixed(m / unit.meters(), precision.min(3)), unit.as_str()),
    }
}

/// An imperial area's number: a decimal under 100 (dropped when it is 0).
fn sq_number(v: f64) -> String {
    let t = fixed(v, if v < 100.0 { 1 } else { 0 });
    t.strip_suffix(".0").map(str::to_string).unwrap_or(t)
}

/// An area in square meters as a person reads it: `11.15 m²`, `120 sq ft`,
/// `144 sq in`, `2.10 sq mi`.
pub fn format_area(sq_meters: f64, unit: Unit, precision: u8) -> String {
    if !sq_meters.is_finite() {
        return "?".into();
    }
    let a = sq_meters.abs();
    let per = |u: Unit| a / (u.meters() * u.meters());
    match unit {
        Unit::In => {
            let v = per(Unit::In);
            format!("{} sq in", sq_number(v))
        }
        Unit::Ft => {
            let v = per(Unit::Ft);
            format!("{} sq ft", sq_number(v))
        }
        Unit::Yd => {
            let v = per(Unit::Yd);
            format!("{} sq yd", sq_number(v))
        }
        Unit::Mi => format!("{} sq mi", fixed(per(Unit::Mi), precision.min(3))),
        u => format!("{} {}²", fixed(per(u), precision.min(3)), u.as_str()),
    }
}

/// Typographic primes and quotes as the ASCII the parsers read.
fn normalize(s: &str) -> String {
    s.replace(['′', '’', '‘'], "'").replace(['″', '“', '”'], "\"").replace("''", "\"").replace('×', "x")
}

/// A number: `12`, `12.5`, `.5`, `3/8`, `1 1/2`, `1-1/2`. Advances `i`.
fn read_number(c: &[char], i: &mut usize) -> Option<f64> {
    let skip_ws = |i: &mut usize| {
        while *i < c.len() && c[*i].is_whitespace() {
            *i += 1;
        }
    };
    let read_plain = |i: &mut usize| -> Option<f64> {
        let start = *i;
        while *i < c.len() && (c[*i].is_ascii_digit() || c[*i] == '.' || c[*i] == ',') {
            *i += 1;
        }
        let s: String = c[start..*i].iter().filter(|ch| **ch != ',').collect();
        if s.is_empty() || s == "." {
            *i = start;
            return None;
        }
        s.parse::<f64>().ok()
    };
    let start = *i;
    let first = read_plain(i)?;
    // A fraction right after: `3/8`.
    if *i < c.len() && c[*i] == '/' {
        *i += 1;
        let Some(den) = read_plain(i).filter(|d| *d != 0.0) else {
            *i = start;
            return None;
        };
        return Some(first / den);
    }
    // A mixed number: `1 1/2` or `1-1/2`.
    let save = *i;
    let mut j = *i;
    if j < c.len() && c[j] == '-' {
        j += 1;
    } else {
        skip_ws(&mut j);
    }
    if j > save && j < c.len() && c[j].is_ascii_digit() {
        let mut k = j;
        if let Some(num) = read_plain(&mut k) {
            if k < c.len() && c[k] == '/' && first.fract() == 0.0 {
                k += 1;
                if let Some(den) = read_plain(&mut k).filter(|d| *d != 0.0) {
                    *i = k;
                    return Some(first + num / den);
                }
            }
        }
    }
    *i = save;
    Some(first)
}

/// A unit name or prime at `i`, if any. Advances `i`.
fn read_unit(c: &[char], i: &mut usize) -> Option<Unit> {
    while *i < c.len() && c[*i].is_whitespace() {
        *i += 1;
    }
    if *i >= c.len() {
        return None;
    }
    if c[*i] == '\'' {
        *i += 1;
        return Some(Unit::Ft);
    }
    if c[*i] == '"' {
        *i += 1;
        return Some(Unit::In);
    }
    let start = *i;
    let mut j = *i;
    while j < c.len() && (c[j].is_alphabetic() || c[j] == '.') {
        j += 1;
    }
    let word: String = c[start..j].iter().collect::<String>().trim_end_matches('.').to_string();
    let unit = Unit::parse(&word)?;
    *i = j;
    Some(unit)
}

/// A length typed by a person and the first unit it names: `12'6"`,
/// `12' 6`, `12'-6"`, `12 ft 6 in`, `3.5 m`, `3/8"`, `1 1/2 in`, `150`
/// (in `default`). `None` when it cannot be read whole.
pub fn parse_length_unit(s: &str, default: Unit) -> Option<(f64, Option<Unit>)> {
    let c: Vec<char> = normalize(s).trim().chars().collect();
    let mut i = 0;
    let mut total = 0.0;
    let mut first: Option<Unit> = None;
    let mut last: Option<Unit> = None;
    let mut terms = 0;
    loop {
        while i < c.len() && (c[i].is_whitespace() || (terms > 0 && c[i] == '-')) {
            i += 1;
        }
        if i >= c.len() {
            break;
        }
        let n = read_number(&c, &mut i)?;
        let named = read_unit(&c, &mut i);
        let unit = match named {
            Some(u) => u,
            // `12' 6` is 12 feet 6 inches; a number alone is in `default`.
            None if last == Some(Unit::Ft) => Unit::In,
            None if last == Some(Unit::M) => Unit::Cm,
            None if terms == 0 => default,
            None => return None,
        };
        total += n * unit.meters();
        first = first.or(named);
        last = named;
        terms += 1;
    }
    (terms > 0 && total.is_finite()).then_some((total, first))
}

/// A length typed by a person, in meters (see [`parse_length_unit`]).
pub fn parse_length(s: &str, default: Unit) -> Option<f64> {
    parse_length_unit(s, default).map(|(m, _)| m)
}

/// Refusal: a scale that cannot be read.
pub const SCALE_UNREADABLE: &str = "Write a scale as a ratio (1:100) or as paper = world (1 in = 10 ft, 1/4\" = 1').";
/// Refusal: a scale that reduces to nothing.
pub const SCALE_NOT_POSITIVE: &str = "Both sides of a scale must be more than zero.";

/// A scale typed by a person: a ratio (`1:100`, `1 : 24,000`, shown in
/// `default`) or paper = world (`1 in = 10 ft`, `1/4" = 1'`,
/// `1 cm = 2.5 km`; shown in the world side's first unit, `default` when it
/// names none). The text is kept as the label.
pub fn parse_scale(s: &str, default: Unit) -> Result<Scale, String> {
    let text = normalize(s);
    let text = text.trim();
    if let Some((a, b)) = text.split_once(':') {
        let num = |t: &str| t.trim().replace(',', "").parse::<f64>().ok();
        let (Some(a), Some(b)) = (num(a), num(b)) else { return Err(SCALE_UNREADABLE.into()) };
        if !(a > 0.0 && b > 0.0) {
            return Err(SCALE_NOT_POSITIVE.into());
        }
        let n = b / a;
        // One board unit (1/96 in of paper) is n/96 in of world.
        let per_unit = default.meters() / (n * INCH_M / BOARD_PER_INCH);
        return Ok(Scale::new(default, per_unit).with_label(s));
    }
    if let Some((paper, world)) = text.split_once('=') {
        let paper_default = if default.is_metric() { Unit::Cm } else { Unit::In };
        let Some(p) = parse_length(paper, paper_default) else { return Err(SCALE_UNREADABLE.into()) };
        let Some((w, wu)) = parse_length_unit(world, default) else { return Err(SCALE_UNREADABLE.into()) };
        if !(p > 0.0 && w > 0.0) {
            return Err(SCALE_NOT_POSITIVE.into());
        }
        let unit = wu.unwrap_or(default);
        let board = p / INCH_M * BOARD_PER_INCH;
        let per_unit = board / (w / unit.meters());
        return Ok(Scale::new(unit, per_unit).with_label(s));
    }
    Err(SCALE_UNREADABLE.into())
}

/// A scale everyone uses, offered in the Scale dialog.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    /// Architectural, Engineering, Metric, Map.
    pub group: &'static str,
    /// What it is written as (and parsed from).
    pub text: &'static str,
    /// The unit it is shown in.
    pub unit: Unit,
}

impl Preset {
    pub fn scale(&self) -> Scale {
        parse_scale(self.text, self.unit).expect("every preset parses")
    }
}

const fn preset(group: &'static str, text: &'static str, unit: Unit) -> Preset {
    Preset { group, text, unit }
}

/// The presets, by group (docs/MEASURE_CONTRACT.md section 2).
pub const PRESETS: &[Preset] = &[
    preset("Architectural", "1/16\" = 1'", Unit::Ft),
    preset("Architectural", "1/8\" = 1'", Unit::Ft),
    preset("Architectural", "1/4\" = 1'", Unit::Ft),
    preset("Architectural", "1/2\" = 1'", Unit::Ft),
    preset("Architectural", "1\" = 1'", Unit::Ft),
    preset("Engineering", "1\" = 10'", Unit::Ft),
    preset("Engineering", "1\" = 20'", Unit::Ft),
    preset("Engineering", "1\" = 50'", Unit::Ft),
    preset("Engineering", "1\" = 100'", Unit::Ft),
    preset("Metric", "1:1", Unit::Cm),
    preset("Metric", "1:5", Unit::Cm),
    preset("Metric", "1:10", Unit::Cm),
    preset("Metric", "1:20", Unit::Cm),
    preset("Metric", "1:50", Unit::M),
    preset("Metric", "1:100", Unit::M),
    preset("Metric", "1:200", Unit::M),
    preset("Metric", "1:500", Unit::M),
    preset("Metric", "1:1000", Unit::M),
    preset("Map", "1:10,000", Unit::M),
    preset("Map", "1:24,000", Unit::Mi),
    preset("Map", "1:25,000", Unit::Km),
    preset("Map", "1:50,000", Unit::Km),
    preset("Map", "1:100,000", Unit::Km),
];

/// The scale that makes `board` board units measure `meters`, in `unit`.
pub fn scale_from_length(board: f64, meters: f64, unit: Unit) -> Option<Scale> {
    (board.is_finite() && meters.is_finite() && board > 0.0 && meters > 0.0).then(|| Scale::new(unit, board / (meters / unit.meters())))
}

/// Board units per pixel of a scan at `dpi` that was drawn at `drawn`, on a
/// canvas at `canvas` (Size to Scale): one pixel is `1/dpi` paper inches,
/// which `drawn` says is so much of the world, which `canvas` draws in so
/// many board units.
pub fn picture_units_per_pixel(dpi: f64, drawn: &Scale, canvas: &Scale) -> Option<f64> {
    if !(dpi.is_finite() && dpi > 0.0 && drawn.is_valid() && canvas.is_valid()) {
        return None;
    }
    let paper_board = BOARD_PER_INCH / dpi;
    Some(canvas.to_board(drawn.to_meters(paper_board)))
}

// ── Grid in units ────────────────────────────────────────────────────────

/// The nice real steps an imperial grid takes, in inches.
const IMPERIAL_STEPS: &[f64] = &[
    1.0 / 16.0,
    1.0 / 8.0,
    0.25,
    0.5,
    1.0,
    3.0,
    6.0,
    12.0,
    24.0,
    60.0,
    120.0,
    300.0,
    600.0,
    1200.0,
    15_840.0,
    31_680.0,
    63_360.0,
];

/// Grid lines at most this close on screen, pixels.
pub const GRID_MIN_PX: f32 = 16.0;
/// Grid lines at most this far apart on screen, pixels.
pub const GRID_MAX_PX: f32 = 60.0;

/// The grid's step in board units for `scale` at `zoom` (screen pixels per
/// board unit): the smallest nice real step at least [`GRID_MIN_PX`] apart
/// on screen (metric 1, 2, 5 × 10ⁿ of the unit; imperial 1/16″ ... 1 mi).
pub fn grid_step(scale: &Scale, zoom: f32) -> f32 {
    let zoom = if zoom.is_finite() && zoom > 0.0 { zoom as f64 } else { 1.0 };
    let min_board = GRID_MIN_PX as f64 / zoom;
    let k = scale.board_per_meter();
    let candidates: Vec<f64> = if scale.unit.is_metric() {
        (-6..=9).flat_map(|e| [1.0, 2.0, 5.0].map(|m| m * 10f64.powi(e) * scale.unit.meters())).collect()
    } else {
        IMPERIAL_STEPS.iter().map(|i| i * INCH_M).collect()
    };
    let pick = candidates.iter().map(|m| m * k).find(|b| *b >= min_board).unwrap_or_else(|| candidates.last().unwrap() * k);
    pick as f32
}

/// A grid step in board units as a person reads it (`1′`, `50 cm`).
pub fn grid_label(scale: &Scale, step: f32) -> String {
    let m = scale.to_meters(step as f64);
    if scale.unit.is_metric() {
        // The smallest metric unit that writes it without a fraction.
        let unit = [Unit::Mm, Unit::Cm, Unit::M, Unit::Km]
            .into_iter()
            .rev()
            .find(|u| m / u.meters() >= 1.0 - 1e-9)
            .unwrap_or(Unit::Mm);
        format!("{} {}", trim_number(m / unit.meters(), 3), unit)
    } else if m >= Unit::Mi.meters() * 0.25 - 1e-9 && scale.unit == Unit::Mi {
        format!("{} mi", trim_number(m / Unit::Mi.meters(), 2))
    } else {
        format_length(m, Unit::Ft, 16)
    }
}

// ── Measurements of an element ──────────────────────────────────────────

/// What an element measures, in board units (lengths) and square board
/// units (areas), after its transform: a rotated or scaled shape measures
/// as seen. Each is `None` when the kind has no such measure.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Measures {
    pub length: Option<f32>,
    /// Degrees, 0 east, counter-clockwise (as drafting reads it), 0..360.
    pub angle: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub perimeter: Option<f32>,
    pub area: Option<f32>,
}

impl Measures {
    pub fn is_empty(&self) -> bool {
        *self == Measures::default()
    }
}

fn map_pt(m: &Affine, p: Point) -> Point {
    let mut q = [tiny_skia::Point::from_xy(p.x, p.y)];
    m.map_points(&mut q);
    Point::new(q[0].x, q[0].y)
}

fn dist(a: Point, b: Point) -> f32 {
    ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
}

/// The angle of `a` to `b` as drafting reads it: 0 east, counter-clockwise
/// (the board's y points down), 0..360.
pub fn drafting_angle(a: Point, b: Point) -> f32 {
    // `+ 0.0` turns a negative zero (a line drawn due east) into 0.
    let deg = (-(b.y - a.y)).atan2(b.x - a.x).to_degrees() + 0.0;
    let deg = if deg < 0.0 { deg + 360.0 } else { deg };
    if deg >= 359.9995 {
        0.0
    } else {
        deg
    }
}

fn polyline_length(pts: &[Point], closed: bool) -> f32 {
    let mut l: f32 = pts.windows(2).map(|w| dist(w[0], w[1])).sum();
    if closed && pts.len() > 2 {
        l += dist(pts[pts.len() - 1], pts[0]);
    }
    l
}

/// Signed area (shoelace), positive clockwise on screen (y down).
fn signed_area(pts: &[Point]) -> f32 {
    if pts.len() < 3 {
        return 0.0;
    }
    let mut s = 0.0f64;
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        s += a.x as f64 * b.y as f64 - b.x as f64 * a.y as f64;
    }
    (s / 2.0) as f32
}

fn inside(p: Point, pts: &[Point]) -> bool {
    let mut c = false;
    let n = pts.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b) = (pts[i], pts[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            c = !c;
        }
        j = i;
    }
    c
}

/// The filled area of closed rings by the fill rule: even-odd, rings inside
/// an odd number of others are holes; nonzero, a ring inside another turning
/// the other way is a hole, one turning the same way adds nothing.
fn rings_area(rings: &[Vec<Point>], rule: crate::path::PathRule) -> f32 {
    let mut total = 0.0;
    for (i, r) in rings.iter().enumerate() {
        if r.len() < 3 {
            continue;
        }
        let a = signed_area(r);
        let containers: Vec<usize> = (0..rings.len()).filter(|j| *j != i && rings[*j].len() >= 3 && inside(r[0], &rings[*j])).collect();
        total += match rule {
            crate::path::PathRule::EvenOdd => {
                if containers.len() % 2 == 0 {
                    a.abs()
                } else {
                    -a.abs()
                }
            }
            _ => match containers.iter().min_by(|x, y| signed_area(&rings[**x]).abs().total_cmp(&signed_area(&rings[**y]).abs())) {
                None => a.abs(),
                Some(c) if signed_area(&rings[*c]).signum() != a.signum() => -a.abs(),
                Some(_) => 0.0,
            },
        };
    }
    total.max(0.0)
}

/// The outline an element is measured around, in board units, when it is a
/// closed shape: (rings, fill rule). Straight-sided shapes give their
/// corners.
fn outline(e: &Element) -> Option<(Vec<Vec<Point>>, crate::path::PathRule)> {
    let w = e.world;
    let ring = |pts: Vec<Point>| pts.into_iter().map(|p| map_pt(&w, p)).collect::<Vec<_>>();
    let rect_ring = |r: &Rect| {
        let r = r.normalized();
        ring(vec![Point::new(r.x, r.y), Point::new(r.right(), r.y), Point::new(r.right(), r.bottom()), Point::new(r.x, r.bottom())])
    };
    let nonzero = crate::path::PathRule::NonZero;
    Some(match (&e.raw.kind, &e.raw.geom) {
        (Kind::Rect, Geom::Rect(r)) if e.style.radius <= 0.0 => (vec![rect_ring(r)], nonzero),
        (Kind::Rect | Kind::Diamond | Kind::Polygon | Kind::Star | Kind::Ellipse, Geom::Rect(r)) => {
            let p = crate::path::shape(&e.raw.kind, &r.normalized(), &e.style)?;
            let rings: Vec<Vec<Point>> = p.flatten(0.1).into_iter().map(|(pts, _)| ring(pts)).collect();
            (rings, nonzero)
        }
        (Kind::Frame | Kind::Sticky, Geom::Rect(r)) => (vec![rect_ring(&e.grown.unwrap_or(*r))], nonzero),
        (Kind::Picture, Geom::Picture(p)) => (vec![rect_ring(&p.rect)], nonzero),
        (Kind::Card, Geom::Card { rect, .. }) => (vec![rect_ring(rect)], nonzero),
        (Kind::Callout, Geom::Callout { rect, .. }) => (vec![rect_ring(rect)], nonzero),
        (Kind::Path, Geom::Path(p)) => {
            let flat = p.flatten(0.1);
            if flat.is_empty() || flat.iter().any(|(_, closed)| !closed) {
                return None;
            }
            (flat.into_iter().map(|(pts, _)| ring(pts)).collect(), p.rule)
        }
        _ => return None,
    })
}

/// The open line an element draws, in board units: a line's ends, a
/// connector's route, a stroke's points, an open path's first subpath.
fn open_line(e: &Element) -> Option<Vec<Point>> {
    let w = e.world;
    match (&e.raw.kind, &e.raw.geom) {
        (Kind::Line | Kind::Arrow | Kind::Dimension, Geom::Segment { a, b }) => Some(vec![map_pt(&w, *a), map_pt(&w, *b)]),
        (Kind::Connector, _) => e.route.map(|(a, b)| vec![a, b]),
        (Kind::Pen | Kind::Highlighter, _) => {
            let pts: Vec<Point> = e.raw.points.iter().filter(|p| p[0].is_finite() && p[1].is_finite()).map(|p| map_pt(&w, Point::new(p[0], p[1]))).collect();
            (pts.len() >= 2).then_some(pts)
        }
        (Kind::Path, Geom::Path(p)) => {
            let flat = p.flatten(0.1);
            if flat.is_empty() || flat.iter().all(|(_, closed)| *closed) {
                return None;
            }
            Some(flat.into_iter().filter(|(_, c)| !c).flat_map(|(pts, _)| ring(&w, pts)).collect())
        }
        _ => None,
    }
}

fn ring(w: &Affine, pts: Vec<Point>) -> Vec<Point> {
    pts.into_iter().map(|p| map_pt(w, p)).collect()
}

/// The box an element is measured as (rectangle-like kinds): its own
/// rectangle before the transform.
fn measured_box(e: &Element) -> Option<Rect> {
    Some(
        match (&e.raw.kind, &e.raw.geom) {
            (Kind::Sticky, Geom::Rect(r)) => e.grown.unwrap_or(*r),
            (Kind::Rect | Kind::Ellipse | Kind::Diamond | Kind::Polygon | Kind::Star | Kind::Frame, Geom::Rect(r)) => *r,
            (Kind::Picture, Geom::Picture(p)) => p.rect,
            (Kind::Card, Geom::Card { rect, .. }) => *rect,
            (Kind::Callout, Geom::Callout { rect, .. }) => *rect,
            (Kind::Path, Geom::Path(p)) => p.bounds()?,
            _ => return None,
        }
        .normalized(),
    )
}

/// The box's corners through the element's world: top-left, top-right,
/// bottom-right, bottom-left.
fn box_corners(e: &Element, r: &Rect) -> [Point; 4] {
    let w = e.world;
    [
        map_pt(&w, Point::new(r.x, r.y)),
        map_pt(&w, Point::new(r.right(), r.y)),
        map_pt(&w, Point::new(r.right(), r.bottom())),
        map_pt(&w, Point::new(r.x, r.bottom())),
    ]
}

/// What `e` measures (docs/MEASURE_CONTRACT.md section 3).
pub fn of(e: &Element) -> Measures {
    let mut m = Measures::default();
    match &e.raw.kind {
        Kind::Line | Kind::Arrow | Kind::Dimension | Kind::Connector => {
            if let Some(pts) = open_line(e) {
                m.length = Some(dist(pts[0], pts[1]));
                m.angle = Some(drafting_angle(pts[0], pts[1]));
            }
            return m;
        }
        Kind::Pen | Kind::Highlighter => {
            m.length = open_line(e).map(|pts| polyline_length(&pts, false));
            return m;
        }
        Kind::Path => {
            if let Some(pts) = open_line(e) {
                m.length = Some(polyline_length(&pts, false));
                return m;
            }
        }
        _ => {}
    }
    if let Some(r) = measured_box(e) {
        let c = box_corners(e, &r);
        m.width = Some(dist(c[0], c[1]));
        m.height = Some(dist(c[1], c[2]));
    }
    if e.raw.kind == Kind::Ellipse {
        if let (Some(w), Some(h)) = (m.width, m.height) {
            let (a, b) = (w as f64 / 2.0, h as f64 / 2.0);
            let p = std::f64::consts::PI * (3.0 * (a + b) - ((3.0 * a + b) * (a + 3.0 * b)).sqrt());
            m.perimeter = Some(p as f32);
            m.area = Some((std::f64::consts::PI * a * b) as f32);
        }
        return m;
    }
    if let Some((rings, rule)) = outline(e) {
        m.perimeter = Some(rings.iter().map(|r| polyline_length(r, true)).sum());
        m.area = Some(rings_area(&rings, rule));
    }
    m
}

// ── Measurements drawn on the canvas ────────────────────────────────────

/// A dimension's or a shape's labels' default size, board units.
pub const LABEL_SIZE: f32 = 12.0;
/// The color dimensions and labels of shapes are drawn in when the element
/// names none for them.
pub const LABEL_COLOR: Color = Color::rgb(0x33, 0x33, 0x33);
/// The halo behind labels.
pub const LABEL_HALO: Color = Color::WHITE;
/// [`LABEL_COLOR`] and [`LABEL_HALO`] over a dark background.
pub const LABEL_COLOR_DARK: Color = Color::rgb(0xe4, 0xe4, 0xe4);
pub const LABEL_HALO_DARK: Color = Color::rgb(0x1e, 0x1e, 0x1e);

/// What an element draws to show its measurements, in board units (never
/// stored; built by the snapshot when the canvas has a scale, or for a
/// dimension always).
#[derive(Debug, Clone)]
pub struct Marks {
    /// Dimension lines, extension lines and ticks, stroked.
    pub lines: Option<Path>,
    pub line_width: f32,
    /// Arrow heads, filled.
    pub heads: Option<Path>,
    /// The labels' glyph outlines, filled over a halo.
    pub labels: Option<Path>,
    pub label_color: Color,
    pub halo: f32,
    /// The halo's color.
    pub halo_color: Color,
    pub color: Color,
    /// What the labels say, in order (for tests and the MCP).
    pub texts: Vec<String>,
    /// The labels' turned boxes (hit testing a dimension).
    pub quads: Vec<[Point; 4]>,
    /// Segments a dimension is hit along.
    pub segments: Vec<(Point, Point)>,
    pub bounds: Rect,
    pub hash: u64,
}

impl Marks {
    /// The marks as drawn over a dark background: what is in the default
    /// colors reads light over a dark halo; a color the element names stays.
    pub fn on_dark(&mut self) {
        if self.label_color == LABEL_COLOR {
            self.label_color = LABEL_COLOR_DARK;
        }
        if self.color == LABEL_COLOR {
            self.color = LABEL_COLOR_DARK;
        }
        self.halo_color = LABEL_HALO_DARK;
        self.hash ^= 0x9e37_79b9_7f4a_7c15;
    }
}

/// A label laid out at `size`, centered on `at`, turned to `angle_deg`
/// (clockwise, screen): its glyph outlines and its turned box.
fn label_path(text: &str, size: f32, at: Point, angle_deg: f32) -> Option<(Path, [Point; 4])> {
    let layout = crate::text::layout(text, size, None);
    if layout.is_empty() {
        return None;
    }
    let (w, h) = (layout.width, layout.height);
    let glyphs = layout.path(Point::new(-w / 2.0, -h / 2.0), w, Align::Left)?;
    let m = Affine::from_translate(at.x, at.y).pre_concat(Affine::from_rotate(angle_deg));
    let path = glyphs.transform(m)?;
    let corners = [Point::new(-w / 2.0, -h / 2.0), Point::new(w / 2.0, -h / 2.0), Point::new(w / 2.0, h / 2.0), Point::new(-w / 2.0, h / 2.0)];
    Some((path, corners.map(|p| map_pt(&m, p))))
}

/// The angle (degrees, clockwise on screen) text along `a`→`b` reads at,
/// kept upright, and that direction's "up" normal.
fn upright(a: Point, b: Point) -> (f32, Point) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    let (mut ux, mut uy) = (dx / len, dy / len);
    // Reading leftward or straight down: read it the other way.
    if ux < -1e-4 || (ux.abs() <= 1e-4 && uy > 0.0) {
        ux = -ux;
        uy = -uy;
    }
    (uy.atan2(ux).to_degrees(), Point::new(uy, -ux))
}

struct Builder {
    lines: PathBuilder,
    heads: PathBuilder,
    labels: PathBuilder,
    texts: Vec<String>,
    quads: Vec<[Point; 4]>,
    segments: Vec<(Point, Point)>,
    bounds: Option<Rect>,
}

impl Builder {
    fn new() -> Self {
        Builder { lines: PathBuilder::new(), heads: PathBuilder::new(), labels: PathBuilder::new(), texts: Vec::new(), quads: Vec::new(), segments: Vec::new(), bounds: None }
    }

    fn grow(&mut self, p: Point) {
        let r = Rect::new(p.x, p.y, 0.0, 0.0);
        self.bounds = Some(self.bounds.map_or(r, |b| b.union(&r)));
    }

    fn line(&mut self, a: Point, b: Point) {
        self.lines.move_to(a.x, a.y);
        self.lines.line_to(b.x, b.y);
        self.grow(a);
        self.grow(b);
    }

    fn label(&mut self, text: String, size: f32, at: Point, angle: f32) {
        if let Some((path, quad)) = label_path(&text, size, at, angle) {
            self.labels.push_path(&path);
            for p in quad {
                self.grow(p);
            }
            self.quads.push(quad);
        }
        self.texts.push(text);
    }

    /// A label along the edge `a`→`b`, outside it (away from `inner`).
    fn edge_label(&mut self, text: String, size: f32, a: Point, b: Point, inner: Point) {
        let (angle, up) = upright(a, b);
        let mid = Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        let away = if (mid.x - inner.x) * up.x + (mid.y - inner.y) * up.y >= 0.0 { 1.0 } else { -1.0 };
        let h = size * 1.2;
        let off = h / 2.0 + size * 0.35;
        self.label(text, size, Point::new(mid.x + up.x * off * away, mid.y + up.y * off * away), angle);
    }

    fn finish(self, line_width: f32, color: Color, label_color: Color, halo: f32, scale: Option<&Scale>) -> Option<Marks> {
        let bounds = self.bounds?.inflate(line_width + halo + 1.0);
        let mut h = Fx::default();
        scale.map(|s| s.hash_into(&mut h));
        self.texts.hash(&mut h);
        for q in &self.quads {
            for p in q {
                p.x.to_bits().hash(&mut h);
                p.y.to_bits().hash(&mut h);
            }
        }
        for (a, b) in &self.segments {
            for v in [a.x, a.y, b.x, b.y] {
                v.to_bits().hash(&mut h);
            }
        }
        for v in [bounds.x, bounds.y, bounds.w, bounds.h] {
            v.to_bits().hash(&mut h);
        }
        color.hash(&mut h);
        label_color.hash(&mut h);
        line_width.to_bits().hash(&mut h);
        Some(Marks {
            lines: self.lines.finish(),
            line_width,
            heads: self.heads.finish(),
            labels: self.labels.finish(),
            label_color,
            halo,
            halo_color: LABEL_HALO,
            color,
            texts: self.texts,
            quads: self.quads,
            segments: self.segments,
            bounds,
            hash: h.finish(),
        })
    }
}

/// A dimension's label: its text with `{}` replaced by the length, or the
/// length alone; without a scale the length in board units.
pub fn dimension_text(text: Option<&str>, length: f32, scale: Option<&Scale>) -> String {
    let value = match scale {
        Some(s) => s.length(length as f64),
        None => trim_number(length as f64, 1),
    };
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) if t.contains("{}") => t.replace("{}", &value),
        Some(t) => t.to_string(),
        None => value,
    }
}

/// Whether a dimension is drawn with ticks (architectural) rather than
/// arrows: `arrow_heads` set to `none` asks for ticks and any other value
/// for arrows; absent, imperial canvases tick and metric ones (and those
/// with no scale) use arrows.
pub fn dimension_ticks(style: &Style, scale: Option<&Scale>) -> bool {
    match style.arrow_heads {
        Some(ArrowHeads::None) => true,
        Some(_) => false,
        None => scale.is_some_and(|s| !s.unit.is_metric()),
    }
}

/// What `e` draws to show measurements: a dimension always (its lines and
/// its length); a shape with `dims` or `area` on, only with a scale.
pub(crate) fn marks(e: &Element, scale: Option<&Scale>) -> Option<Marks> {
    if !e.visible || !e.has_extent {
        return None;
    }
    let s = &e.style;
    if e.raw.kind == Kind::Dimension {
        let Geom::Segment { a, b } = &e.raw.geom else { return None };
        let (a, b) = (map_pt(&e.world, *a), map_pt(&e.world, *b));
        let len = dist(a, b);
        let color = s.stroke;
        let width = s.width.max(0.25);
        let size = s.font_size;
        let mut out = Builder::new();
        let (ux, uy) = if len > 1e-6 { ((b.x - a.x) / len, (b.y - a.y) / len) } else { (1.0, 0.0) };
        // The normal a positive offset moves along: "up" for a line drawn
        // east.
        let n = Point::new(uy, -ux);
        let d = s.dim_offset;
        let sign = if d < 0.0 { -1.0 } else { 1.0 };
        let gap = (size * 0.25).min(d.abs());
        let over = size * 0.3;
        let (da, db) = (Point::new(a.x + n.x * d, a.y + n.y * d), Point::new(b.x + n.x * d, b.y + n.y * d));
        if d.abs() > 1e-3 {
            for p in [a, b] {
                out.line(Point::new(p.x + n.x * gap * sign, p.y + n.y * gap * sign), Point::new(p.x + n.x * (d + over * sign), p.y + n.y * (d + over * sign)));
            }
        }
        out.line(da, db);
        out.segments.push((da, db));
        out.segments.push((a, da));
        out.segments.push((b, db));
        if dimension_ticks(&e.raw.style, scale) {
            // A short slash at 45° through each end.
            let t = size * 0.35;
            let (tx, ty) = ((ux + n.x) * std::f32::consts::FRAC_1_SQRT_2 * t, (uy + n.y) * std::f32::consts::FRAC_1_SQRT_2 * t);
            for p in [da, db] {
                out.line(Point::new(p.x - tx, p.y - ty), Point::new(p.x + tx, p.y + ty));
            }
        } else if len > 1e-3 {
            let head = (size * 0.6).max(width * 3.0);
            for (tip, toward) in [(da, db), (db, da)] {
                // Tip on the extension line, pointing out of the dimension.
                let base = Point::new(tip.x + (toward.x - tip.x) / len * head, tip.y + (toward.y - tip.y) / len * head);
                let half = head * 0.35;
                out.heads.move_to(tip.x, tip.y);
                out.heads.line_to(base.x + n.x * half, base.y + n.y * half);
                out.heads.line_to(base.x - n.x * half, base.y - n.y * half);
                out.heads.close();
            }
        }
        let text = dimension_text(e.raw.text.as_deref(), len, scale);
        let (angle, up) = upright(da, db);
        let mid = Point::new((da.x + db.x) / 2.0, (da.y + db.y) / 2.0);
        let off = size * 0.6 + size * 0.25 + width / 2.0;
        out.label(text, size, Point::new(mid.x + up.x * off, mid.y + up.y * off), angle);
        return out.finish(width, color, color, size * 0.15, scale);
    }
    let scale = scale?;
    if !(s.dims || s.area) {
        return None;
    }
    let size = LABEL_SIZE;
    let mut out = Builder::new();
    let label = |board: f32| scale.length(board as f64);
    if s.dims {
        if let Some(line) = open_line(e) {
            // A line's length along its middle.
            let total = polyline_length(&line, false);
            let mut acc = 0.0;
            for w in line.windows(2) {
                let l = dist(w[0], w[1]);
                if acc + l >= total / 2.0 || l <= 0.0 {
                    let (angle, up) = upright(w[0], w[1]);
                    let t = if l > 0.0 { (total / 2.0 - acc) / l } else { 0.5 };
                    let mid = Point::new(w[0].x + (w[1].x - w[0].x) * t, w[0].y + (w[1].y - w[0].y) * t);
                    let off = size * 1.15 + s.width / 2.0;
                    out.label(label(total), size, Point::new(mid.x + up.x * off, mid.y + up.y * off), angle);
                    break;
                }
                acc += l;
            }
        } else if matches!(e.raw.kind, Kind::Diamond | Kind::Polygon | Kind::Star) {
            if let Some((rings, _)) = outline(e) {
                for r in &rings {
                    let c = centroid(r);
                    for i in 0..r.len() {
                        let (p, q) = (r[i], r[(i + 1) % r.len()]);
                        out.edge_label(label(dist(p, q)), size, p, q, c);
                    }
                }
            }
        } else if e.raw.kind != Kind::Ellipse && e.raw.kind != Kind::Path {
            if let Some(r) = measured_box(e) {
                let c = box_corners(e, &r);
                let center = centroid(&c);
                out.edge_label(label(dist(c[0], c[1])), size, c[0], c[1], center);
                out.edge_label(label(dist(c[1], c[2])), size, c[1], c[2], center);
            }
        }
    }
    if s.area {
        let m = of(e);
        if let (Some(a), Some(r)) = (m.area, measured_box(e)) {
            let c = box_corners(e, &r);
            let mut at = centroid(&c);
            // Under any text the shape holds.
            if let (Some(layout), Some(tb)) = (&e.layout, e.text_box) {
                if !layout.is_empty() {
                    let bottom = map_pt(&e.world, Point::new(tb.center().x, tb.bottom()));
                    let center = map_pt(&e.world, tb.center());
                    let (dx, dy) = (bottom.x - center.x, bottom.y - center.y);
                    let l = (dx * dx + dy * dy).sqrt().max(1e-6);
                    at = Point::new(bottom.x + dx / l * size * 0.8, bottom.y + dy / l * size * 0.8);
                }
            }
            out.label(scale.area(a as f64), size, at, 0.0);
        }
    }
    out.finish(0.0, LABEL_COLOR, LABEL_COLOR, size * 0.15, Some(scale))
}

fn centroid(pts: &[Point]) -> Point {
    let n = pts.len().max(1) as f32;
    Point::new(pts.iter().map(|p| p.x).sum::<f32>() / n, pts.iter().map(|p| p.y).sum::<f32>() / n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6 * b.abs().max(1.0)
    }

    #[test]
    fn units_parse_and_print() {
        for u in Unit::ALL {
            assert_eq!(Unit::parse(u.as_str()), Some(u));
        }
        assert_eq!(Unit::parse("Feet"), Some(Unit::Ft));
        assert_eq!(Unit::parse("metres"), Some(Unit::M));
        assert_eq!(Unit::parse("″"), Some(Unit::In));
        assert_eq!(Unit::parse("parsecs"), None);
    }

    #[test]
    fn lengths_parse() {
        let ft = |s: &str| parse_length(s, Unit::Ft).map(|m| m / Unit::Ft.meters());
        let near = |v: Option<f64>, want: f64| v.is_some_and(|v| (v - want).abs() < 1e-9);
        assert!(near(ft("12'6\""), 12.5));
        assert!(near(ft("12′ 6″"), 12.5));
        assert!(near(ft("12' 6"), 12.5));
        assert!(near(ft("12'-6\""), 12.5));
        assert!(near(ft("12 ft 6 in"), 12.5));
        assert!(near(ft("12 feet 6 inches"), 12.5));
        assert!(near(ft("150"), 150.0));
        assert!(near(ft("3/8\""), 3.0 / 8.0 / 12.0));
        assert!(near(ft("1 1/2\""), 1.5 / 12.0));
        assert!(near(ft("1-1/2 in"), 1.5 / 12.0));
        assert!(near(ft("1,250"), 1250.0));
        let m = |s: &str| parse_length(s, Unit::M);
        assert!(near(m("3.5 m"), 3.5));
        assert!(near(m("350 cm"), 3.5));
        assert!(near(m("3 m 20"), 3.2), "metres then a bare number is centimeters");
        assert!(near(m(".5"), 0.5));
        assert!(near(m("2 km"), 2000.0));
        for bad in ["", "abc", "12 parsecs", "3/0", "1 2", "--3"] {
            assert_eq!(parse_length(bad, Unit::M), None, "{bad:?}");
        }
        assert_eq!(parse_length_unit("12 ft 6 in", Unit::M).map(|(_, u)| u), Some(Some(Unit::Ft)));
        assert_eq!(parse_length_unit("12", Unit::M).map(|(_, u)| u), Some(None));
    }

    #[test]
    fn lengths_format() {
        let ft = |v: f64, p: u8| format_length(v * Unit::Ft.meters(), Unit::Ft, p);
        assert_eq!(ft(12.5, 1), "12′ 6″");
        assert_eq!(ft(12.0, 1), "12′");
        assert_eq!(ft(0.5, 1), "6″");
        assert_eq!(ft(12.0 + 6.5 / 12.0, 2), "12′ 6 1/2″");
        assert_eq!(ft(12.0 + 6.49 / 12.0, 1), "12′ 6″");
        assert_eq!(ft(11.0 + 11.9 / 12.0, 1), "12′", "rounding up carries into the feet");
        assert_eq!(ft(1234.0, 1), "1,234′");
        assert_eq!(format_length(3.0 / 8.0 * INCH_M, Unit::In, 16), "3/8″");
        assert_eq!(format_length(3.375 * INCH_M, Unit::In, 16), "3 3/8″");
        assert_eq!(format_length(3.5, Unit::M, 2), "3.50 m");
        assert_eq!(format_length(0.0125, Unit::Mm, 0), "13 mm");
        assert_eq!(format_length(1234.5, Unit::Km, 3), "1.235 km");
        assert_eq!(format_length(1609.344 * 0.35, Unit::Mi, 2), "0.35 mi");
        assert_eq!(format_length(12345.0, Unit::M, 0), "12,345 m");
    }

    #[test]
    fn lengths_round_trip() {
        for u in Unit::ALL {
            for v in [0.25, 1.0, 3.5, 12.5, 100.0, 1234.0] {
                let m = v * u.meters();
                let p = u.default_precision();
                let text = format_length(m, u, p);
                let back = parse_length(&text, u).unwrap_or_else(|| panic!("{text} parses"));
                // Within the precision shown.
                let step = match u {
                    Unit::In | Unit::Ft | Unit::Yd => INCH_M / p.max(1) as f64,
                    _ => u.meters() * 10f64.powi(-(p as i32)),
                };
                assert!((back - m).abs() <= step / 2.0 + 1e-9, "{u} {v}: {text} read back as {back}");
                assert_eq!(format_length(back, u, p), text, "printing what was read prints the same");
            }
        }
    }

    #[test]
    fn areas_format() {
        let sqft = Unit::Ft.meters() * Unit::Ft.meters();
        assert_eq!(format_area(120.0 * sqft, Unit::Ft, 1), "120 sq ft");
        assert_eq!(format_area(12.25 * sqft, Unit::Ft, 1), "12.3 sq ft");
        assert_eq!(format_area(40.0 * sqft, Unit::Ft, 1), "40 sq ft");
        assert_eq!(format_area(11.15, Unit::M, 2), "11.15 m²");
        assert_eq!(format_area(2.0e6, Unit::Km, 3), "2.000 km²");
        assert_eq!(format_area(1500.0 * sqft, Unit::Ft, 1), "1,500 sq ft");
    }

    #[test]
    fn scales_parse() {
        let s = parse_scale("1/4\" = 1'", Unit::M).unwrap();
        assert_eq!(s.unit, Unit::Ft);
        assert!(close(s.per_unit, 24.0), "{}", s.per_unit);
        assert_eq!(s.label.as_deref(), Some("1/4\" = 1'"));
        let s = parse_scale("1 in = 10 ft", Unit::M).unwrap();
        assert!(close(s.per_unit, 9.6));
        assert_eq!(s.length(96.0), "10′");
        let s = parse_scale("1 cm = 2.5 km", Unit::M).unwrap();
        assert_eq!(s.unit, Unit::Km);
        assert!(close(s.to_meters(96.0 / 2.54), 2500.0));
        let s = parse_scale("1:100", Unit::M).unwrap();
        assert_eq!(s.unit, Unit::M);
        // One paper inch (96 units) is 100 inches.
        assert!(close(s.to_meters(96.0), 2.54));
        let s = parse_scale("1 : 24,000", Unit::Mi).unwrap();
        assert!(close(s.to_meters(96.0), 24_000.0 * INCH_M));
        assert!(parse_scale("1:0", Unit::M).is_err());
        assert!(parse_scale("big", Unit::M).is_err());
        assert!(parse_scale("0 in = 1 ft", Unit::M).is_err());
        for p in PRESETS {
            assert!(p.scale().is_valid(), "{}", p.text);
            assert_eq!(p.scale().unit, p.unit);
        }
    }

    #[test]
    fn scales_describe() {
        let s = parse_scale("1 in = 10 ft", Unit::Ft).unwrap();
        assert_eq!(s.describe(), "1 in on paper = 10′; 1 ft = 9.6 px at 100%");
        assert_eq!(Scale::new(Unit::M, 96.0 / 2.54).name(), "1:100");
        assert_eq!(Scale::new(Unit::Ft, 9.6).name(), "1 in = 10′");
    }

    #[test]
    fn scale_from_a_length() {
        let s = scale_from_length(240.0, 10.0 * Unit::Ft.meters(), Unit::Ft).unwrap();
        assert!(close(s.per_unit, 24.0));
        assert!(scale_from_length(0.0, 1.0, Unit::M).is_none());
    }

    #[test]
    fn pictures_to_scale() {
        // A scan at 100 dpi of a plan drawn at 1/4" = 1': 25 pixels are a
        // foot; on a canvas at 1" = 10' a foot is 9.6 units.
        let drawn = parse_scale("1/4\" = 1'", Unit::Ft).unwrap();
        let canvas = parse_scale("1 in = 10 ft", Unit::Ft).unwrap();
        let k = picture_units_per_pixel(100.0, &drawn, &canvas).unwrap();
        assert!(close(k * 25.0, 9.6), "{k}");
        assert!(picture_units_per_pixel(0.0, &drawn, &canvas).is_none());
    }

    #[test]
    fn grids_are_nice() {
        let s = parse_scale("1 in = 10 ft", Unit::Ft).unwrap();
        for zoom in [0.1f32, 0.5, 1.0, 2.0, 8.0] {
            let step = grid_step(&s, zoom);
            let px = step * zoom;
            assert!(px >= GRID_MIN_PX - 1e-3, "{zoom}: {px}");
            assert!(px <= GRID_MAX_PX * 2.6, "{zoom}: {px}");
        }
        assert_eq!(grid_label(&s, grid_step(&s, 1.0)), "2′", "a foot is 9.6 px, two are 19.2");
        let m = parse_scale("1:100", Unit::M).unwrap();
        let step = grid_step(&m, 1.0);
        assert_eq!(grid_label(&m, step), "50 cm");
    }

    #[test]
    fn angles_are_drafting() {
        assert_eq!(drafting_angle(Point::new(0.0, 0.0), Point::new(10.0, 0.0)), 0.0);
        assert!((drafting_angle(Point::new(0.0, 0.0), Point::new(0.0, -10.0)) - 90.0).abs() < 1e-4);
        assert!((drafting_angle(Point::new(0.0, 0.0), Point::new(-10.0, 0.0)) - 180.0).abs() < 1e-4);
        assert!((drafting_angle(Point::new(0.0, 0.0), Point::new(0.0, 10.0)) - 270.0).abs() < 1e-4);
    }

    #[test]
    fn areas_by_rule() {
        let sq = |x: f32, y: f32, s: f32, cw: bool| {
            let mut v = vec![Point::new(x, y), Point::new(x + s, y), Point::new(x + s, y + s), Point::new(x, y + s)];
            if !cw {
                v.reverse();
            }
            v
        };
        let outer = sq(0.0, 0.0, 10.0, true);
        let hole_ccw = sq(2.0, 2.0, 2.0, false);
        let hole_cw = sq(2.0, 2.0, 2.0, true);
        assert_eq!(rings_area(&[outer.clone(), hole_ccw.clone()], crate::path::PathRule::NonZero), 96.0);
        assert_eq!(rings_area(&[outer.clone(), hole_cw.clone()], crate::path::PathRule::NonZero), 100.0);
        assert_eq!(rings_area(&[outer, hole_cw], crate::path::PathRule::EvenOdd), 96.0);
    }
}

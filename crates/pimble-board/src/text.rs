//! Board text: a small bundled font set (docs/DRAWING_WAVE6_CONTRACT.md
//! section 4, "Fonts"), so a board renders the same on every device
//! (contract section 8): Noto Sans and Noto Serif in regular, bold, italic
//! and bold italic, and Noto Sans Condensed in regular and bold, all under
//! the SIL Open Font License 1.1 (`fonts/OFL.txt`). Glyphs are filled as
//! tiny-skia paths from the fonts' outlines, so text rotates, scales and
//! blends like any other shape, and a glyph's outline is built once per
//! process.
//!
//! Noto Sans Regular is the full font, as it always was, so older boards
//! draw exactly as before; the other faces are subset to Latin (Basic,
//! Latin-1, Extended A and B and Additional), IPA, combining marks,
//! punctuation, currency, letterlike symbols, number forms and arrows, with
//! their layout tables dropped (`fonts/subset.py`). A character a face lacks
//! is drawn from Noto Sans Regular, so Greek in a serif label still reads.
//! Condensed italic is the condensed face slanted (there is no condensed
//! italic bundled). Every face shares Noto Sans's vertical metrics (1000
//! units to the em, ascender 1069, descender 293).
//!
//! Layout is deliberately simple: no shaping, no kerning (the faces keep
//! theirs in GPOS, which this does not read), letter spacing (`tracking`),
//! greedy word wrap, a `.notdef` box for a character no face has.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tiny_skia::{Path, PathBuilder, Transform};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::model::{Align, Point};

/// The bundled faces, by index (the `face` of a [`Glyph`]). Index 0 is the
/// fallback for every other face.
static FACE_BYTES: [&[u8]; FACE_COUNT] = [
    include_bytes!("../fonts/NotoSans-Regular.ttf"),
    include_bytes!("../fonts/NotoSans-Bold.ttf"),
    include_bytes!("../fonts/NotoSans-Italic.ttf"),
    include_bytes!("../fonts/NotoSans-BoldItalic.ttf"),
    include_bytes!("../fonts/NotoSerif-Regular.ttf"),
    include_bytes!("../fonts/NotoSerif-Bold.ttf"),
    include_bytes!("../fonts/NotoSerif-Italic.ttf"),
    include_bytes!("../fonts/NotoSerif-BoldItalic.ttf"),
    include_bytes!("../fonts/NotoSansCondensed-Regular.ttf"),
    include_bytes!("../fonts/NotoSansCondensed-Bold.ttf"),
];
const FACE_COUNT: usize = 10;

/// The line height, as a multiple of the font size.
pub const LINE_HEIGHT: f32 = 1.25;

/// The slant of a face drawn oblique for want of an italic (condensed
/// italic): about 12 degrees.
const SLANT: f32 = 0.21;

/// A font family a text element names in its style's `font` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontFamily {
    /// Noto Sans (what every board drew before wave 6).
    #[default]
    Sans,
    /// Noto Serif.
    Serif,
    /// Noto Sans Condensed.
    Condensed,
}

impl FontFamily {
    pub const ALL: [FontFamily; 3] = [FontFamily::Sans, FontFamily::Serif, FontFamily::Condensed];

    pub fn as_str(&self) -> &'static str {
        match self {
            FontFamily::Sans => "sans",
            FontFamily::Serif => "serif",
            FontFamily::Condensed => "condensed",
        }
    }

    /// `None` for a value this version does not know (it reads as sans).
    pub fn parse(s: &str) -> Option<FontFamily> {
        Some(match s {
            "sans" => FontFamily::Sans,
            "serif" => FontFamily::Serif,
            "condensed" => FontFamily::Condensed,
            _ => return None,
        })
    }

    /// The name a person reads.
    pub fn label(&self) -> &'static str {
        match self {
            FontFamily::Sans => "Sans",
            FontFamily::Serif => "Serif",
            FontFamily::Condensed => "Condensed",
        }
    }
}

/// A family, bold or not, italic or not: one bundled face (and a slant for
/// condensed italic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FontChoice {
    pub family: FontFamily,
    pub bold: bool,
    pub italic: bool,
}

impl FontChoice {
    /// The face it draws with, and whether that face is slanted.
    fn face(&self) -> (u8, bool) {
        let style = self.bold as u8 + 2 * self.italic as u8;
        match self.family {
            FontFamily::Sans => (style, false),
            FontFamily::Serif => (4 + style, false),
            FontFamily::Condensed => (8 + self.bold as u8, self.italic),
        }
    }
}

/// How a text is set: its font and its letter spacing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TextStyle {
    pub font: FontChoice,
    /// Extra space after every character, in em (0 is the font's own
    /// spacing; negative tightens).
    pub tracking: f32,
}

struct Font {
    face: Face<'static>,
    glyphs: Mutex<HashMap<u16, Option<Arc<Path>>>>,
}

fn font(index: u8) -> &'static Font {
    static FONTS: [OnceLock<Font>; FACE_COUNT] = [const { OnceLock::new() }; FACE_COUNT];
    let i = (index as usize).min(FACE_COUNT - 1);
    FONTS[i].get_or_init(|| Font {
        face: Face::parse(FACE_BYTES[i], 0).expect("the bundled fonts parse"),
        glyphs: Mutex::new(HashMap::new()),
    })
}

/// The shared vertical metrics, from Noto Sans Regular: units per em,
/// ascender, descender (below the baseline, positive), cap height.
fn metrics() -> (f32, f32, f32, f32) {
    let f = &font(0).face;
    let upm = f.units_per_em() as f32;
    (upm, f.ascender() as f32, -(f.descender() as f32), f.capital_height().map_or(upm * 0.714, |c| c as f32))
}

/// How far below the top of a text box its first baseline sits, at
/// `font_size` (the line's half leading plus the font's ascender).
pub fn first_baseline(font_size: f32) -> f32 {
    let (upm, ascender, _, _) = metrics();
    (font_size * LINE_HEIGHT - font_size) / 2.0 + ascender * font_size / upm
}

/// The ascender, descender and cap height at `font_size`, in board units.
pub(crate) fn vertical(font_size: f32) -> (f32, f32, f32) {
    let (upm, ascender, descender, cap) = metrics();
    let k = font_size / upm;
    (ascender * k, descender * k, cap * k)
}

/// One laid-out glyph: the face it is drawn from, its id there, its x
/// offset in the line and its advance (without the tracking), in board
/// units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub face: u8,
    pub id: u16,
    pub x: f32,
    pub adv: f32,
    /// A space (what a line's width leaves out at its end).
    pub space: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub glyphs: Vec<Glyph>,
    pub width: f32,
}

/// Text laid out at a font size: lines top to bottom, in board units.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLayout {
    pub lines: Vec<Line>,
    pub font_size: f32,
    /// The widest line.
    pub width: f32,
    /// Every line's height together.
    pub height: f32,
    /// How it was set.
    pub style: TextStyle,
}

impl TextLayout {
    pub fn line_height(&self) -> f32 {
        self.font_size * LINE_HEIGHT
    }

    /// How far ink reaches below the last line's box: descenders (g, y, p)
    /// hang past it, since the baseline sits the ascender below the line's
    /// top. What paints must be counted, or a render cut to an element's
    /// bounds clips them.
    pub fn ink_below(&self) -> f32 {
        let (_, descender, _) = vertical(self.font_size);
        (first_baseline(self.font_size) + descender - self.line_height()).max(0.0)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.glyphs.is_empty())
    }

    /// Whether its glyphs are drawn slanted (condensed italic).
    fn slanted(&self) -> bool {
        self.style.font.face().1
    }

    /// The matrix that draws glyph outlines (font units, y up) at this
    /// layout's size with its baseline origin at (0, 0), y down.
    pub(crate) fn glyph_matrix(&self) -> Transform {
        let (upm, ..) = metrics();
        let scale = self.font_size / upm;
        let kx = if self.slanted() { scale * SLANT } else { 0.0 };
        Transform::from_row(scale, 0.0, kx, -scale, 0.0, 0.0)
    }

    /// The outline of every glyph, placed in a box whose top-left is
    /// `origin` and whose width is `box_width` (for alignment), as one path
    /// in board units. `None` when there is nothing to fill.
    pub fn path(&self, origin: Point, box_width: f32, align: Align) -> Option<Path> {
        let line_height = self.line_height();
        let baseline0 = first_baseline(self.font_size);
        let glyph = self.glyph_matrix();
        let mut builder = PathBuilder::new();
        for (i, line) in self.lines.iter().enumerate() {
            let dx = align_offset(align, box_width, line.width);
            let baseline = origin.y + baseline0 + i as f32 * line_height;
            for g in &line.glyphs {
                let Some(outline) = glyph_outline(g.face, g.id) else { continue };
                let ts = Transform::from_translate(origin.x + dx + g.x, baseline).pre_concat(glyph);
                if let Some(placed) = (*outline).clone().transform(ts) {
                    builder.push_path(&placed);
                }
            }
        }
        builder.finish()
    }

    /// Each line's box (x offset by alignment, top, width, height), for
    /// drawing text too small to read as bars ("greeking").
    pub fn line_boxes(&self, origin: Point, box_width: f32, align: Align) -> Vec<crate::model::Rect> {
        let lh = self.line_height();
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.width > 0.0)
            .map(|(i, line)| {
                let dx = align_offset(align, box_width, line.width);
                crate::model::Rect::new(origin.x + dx, origin.y + i as f32 * lh + lh * 0.3, line.width, lh * 0.45)
            })
            .collect()
    }
}

/// Where a line of `width` starts in a box of `box_width`.
pub(crate) fn align_offset(align: Align, box_width: f32, width: f32) -> f32 {
    match align {
        Align::Left => 0.0,
        Align::Center => (box_width - width) / 2.0,
        Align::Right => box_width - width,
    }
}

/// Lay `text` out at `font_size` in Noto Sans Regular with no letter
/// spacing (see [`layout_styled`]).
pub fn layout(text: &str, font_size: f32, wrap: Option<f32>) -> TextLayout {
    layout_styled(text, font_size, wrap, &TextStyle::default())
}

/// The face a character is drawn from in face `face` (its own, or Noto
/// Sans Regular when it lacks it), and its glyph there (`.notdef` when no
/// face has it).
fn glyph_for(c: char, face: u8) -> (u8, GlyphId) {
    if let Some(id) = font(face).face.glyph_index(c) {
        return (face, id);
    }
    if face != 0 {
        if let Some(id) = font(0).face.glyph_index(c) {
            return (0, id);
        }
    }
    (face, GlyphId(0))
}

/// Lay `text` out at `font_size` in `style`, wrapping at `wrap` board units
/// when given (a word wider than that breaks between characters). Every
/// `\n` starts a line; an empty text is one empty line. Tracking is added
/// after every character but the last of a line, and wrapping measures
/// with it.
pub fn layout_styled(text: &str, font_size: f32, wrap: Option<f32>, style: &TextStyle) -> TextLayout {
    let (upm, ..) = metrics();
    let scale = font_size / upm;
    let (face, _) = style.font.face();
    let tracking = if style.tracking.is_finite() { style.tracking * font_size } else { 0.0 };
    let glyph = |c: char| -> Glyph {
        let (f, id) = glyph_for(c, face);
        let adv = font(f).face.glyph_hor_advance(id).unwrap_or(0) as f32 * scale;
        Glyph { face: f, id: id.0, x: 0.0, adv, space: c.is_whitespace() }
    };
    let wrap = wrap.filter(|w| *w > 0.0);
    let mut lines = Vec::new();
    // `pen` is where the next glyph goes (the last glyph's tracking
    // included); a line's width is where its last ink ends.
    let new_line = || (Line { glyphs: Vec::new(), width: 0.0 }, 0.0f32);
    for paragraph in text.split('\n') {
        let (mut line, mut pen) = new_line();
        // Where the current word started.
        let mut word_start = 0usize;
        for c in paragraph.chars() {
            if c == '\r' {
                continue;
            }
            let mut g = glyph(c);
            if g.space {
                g.x = pen;
                line.glyphs.push(g);
                pen += g.adv + tracking;
                word_start = line.glyphs.len();
                continue;
            }
            if let Some(w) = wrap {
                if pen + g.adv > w && !line.glyphs.is_empty() {
                    if word_start > 0 {
                        // Move the current word to a new line.
                        let word: Vec<Glyph> = line.glyphs.drain(word_start..).collect();
                        let start_x = word.first().map(|g| g.x).unwrap_or(pen);
                        line.width = ink_width(&line.glyphs);
                        lines.push(std::mem::replace(&mut line, new_line().0));
                        pen = 0.0;
                        for mut moved in word {
                            moved.x -= start_x;
                            pen = moved.x + moved.adv + tracking;
                            line.glyphs.push(moved);
                        }
                        word_start = 0;
                    } else {
                        // One word wider than the line: break it here.
                        line.width = ink_width(&line.glyphs);
                        lines.push(std::mem::replace(&mut line, new_line().0));
                        pen = 0.0;
                    }
                    if pen + g.adv > w && !line.glyphs.is_empty() {
                        line.width = ink_width(&line.glyphs);
                        lines.push(std::mem::replace(&mut line, new_line().0));
                        pen = 0.0;
                    }
                }
            }
            g.x = pen;
            line.glyphs.push(g);
            pen += g.adv + tracking;
        }
        // Trailing spaces and the last tracking do not widen a line.
        line.width = ink_width(&line.glyphs);
        lines.push(line);
    }
    let width = lines.iter().map(|l| l.width).fold(0.0, f32::max);
    let height = lines.len() as f32 * font_size * LINE_HEIGHT;
    TextLayout { lines, font_size, width, height, style: *style }
}

/// Where the last glyph that is not a space ends (0 for none).
fn ink_width(glyphs: &[Glyph]) -> f32 {
    glyphs.iter().rev().find(|g| !g.space).map_or(0.0, |g| (g.x + g.adv).max(0.0))
}

/// The outline of glyph `id` of face `face`, in font units (y up), built
/// once per process.
pub(crate) fn glyph_outline(face: u8, id: u16) -> Option<Arc<Path>> {
    let f = font(face);
    let mut cache = f.glyphs.lock().unwrap();
    cache
        .entry(id)
        .or_insert_with(|| {
            let mut b = Outline(PathBuilder::new());
            f.face.outline_glyph(GlyphId(id), &mut b)?;
            b.0.finish().map(Arc::new)
        })
        .clone()
}

struct Outline(PathBuilder);

impl OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_and_wraps() {
        let one = layout("Hello world", 20.0, None);
        assert_eq!(one.lines.len(), 1);
        assert!(one.width > 80.0 && one.width < 140.0, "{}", one.width);
        let wrapped = layout("Hello world", 20.0, Some(70.0));
        assert_eq!(wrapped.lines.len(), 2);
        assert!(wrapped.width <= 70.0, "{}", wrapped.width);
        assert_eq!(wrapped.lines[1].glyphs[0].x, 0.0);
        let lines = layout("a\nb\n", 10.0, None);
        assert_eq!(lines.lines.len(), 3);
        assert_eq!(lines.height, 3.0 * 12.5);
        // A word wider than the wrap breaks between characters.
        let long = layout("abcdefghijklmnop", 20.0, Some(50.0));
        assert!(long.lines.len() > 2);
        assert!(long.lines.iter().all(|l| l.width <= 50.0 + 0.01));
        assert!(one.path(Point::new(0.0, 0.0), one.width, Align::Left).is_some());
        assert!(layout("", 10.0, None).path(Point::default(), 0.0, Align::Left).is_none());
    }

    fn styled(family: FontFamily, bold: bool, italic: bool) -> TextStyle {
        TextStyle { font: FontChoice { family, bold, italic }, tracking: 0.0 }
    }

    #[test]
    fn every_face_parses_and_draws() {
        for family in FontFamily::ALL {
            for bold in [false, true] {
                for italic in [false, true] {
                    let l = layout_styled("Braynard River", 20.0, None, &styled(family, bold, italic));
                    assert!(l.width > 50.0, "{family:?} {bold} {italic}: {}", l.width);
                    assert!(l.path(Point::default(), l.width, Align::Left).is_some());
                    // Latin letters come from the face itself.
                    let (face, slant) = FontChoice { family, bold, italic }.face();
                    assert!(l.lines[0].glyphs.iter().all(|g| g.face == face));
                    assert_eq!(slant, family == FontFamily::Condensed && italic);
                }
            }
        }
    }

    #[test]
    fn faces_differ_where_they_should() {
        let w = |s: TextStyle| layout_styled("Mountain Pass", 20.0, None, &s).width;
        let sans = w(TextStyle::default());
        assert!(w(styled(FontFamily::Sans, true, false)) > sans, "bold is wider");
        assert!(w(styled(FontFamily::Condensed, false, false)) < sans * 0.9, "condensed is narrower");
        assert_ne!(w(styled(FontFamily::Serif, false, false)), sans);
        // Unknown names read as sans.
        assert_eq!(FontFamily::parse("comic"), None);
        assert_eq!(FontFamily::default(), FontFamily::Sans);
        assert_eq!(layout("Mountain Pass", 20.0, None), layout_styled("Mountain Pass", 20.0, None, &TextStyle::default()));
    }

    #[test]
    fn a_character_a_face_lacks_comes_from_noto_sans() {
        // Greek is not in the subset serif: it is drawn from Noto Sans.
        let l = layout_styled("Aα", 20.0, None, &styled(FontFamily::Serif, false, false));
        let g = &l.lines[0].glyphs;
        assert_eq!(g[0].face, 4);
        assert_eq!(g[1].face, 0);
        assert_ne!(g[1].id, 0, "Noto Sans has alpha");
    }

    #[test]
    fn tracking_spreads_letters() {
        let plain = layout("ABCD", 20.0, None);
        let wide = layout_styled("ABCD", 20.0, None, &TextStyle { tracking: 0.5, ..TextStyle::default() });
        // Three gaps of half an em (no tracking after the last letter).
        assert!((wide.width - plain.width - 3.0 * 10.0).abs() < 0.01, "{} {}", wide.width, plain.width);
        assert!((wide.lines[0].glyphs[1].x - plain.lines[0].glyphs[1].x - 10.0).abs() < 0.01);
        let tight = layout_styled("ABCD", 20.0, None, &TextStyle { tracking: -0.1, ..TextStyle::default() });
        assert!((plain.width - tight.width - 6.0).abs() < 0.01);
        // Wrapping measures with it: wide letters wrap sooner.
        let w = layout("AB CD EF", 20.0, None).width;
        assert_eq!(layout("AB CD EF", 20.0, Some(w + 1.0)).lines.len(), 1);
        assert!(layout_styled("AB CD EF", 20.0, Some(w + 1.0), &TextStyle { tracking: 0.5, ..TextStyle::default() }).lines.len() > 1);
    }
}

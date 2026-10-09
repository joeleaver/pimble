//! Board text: one bundled font (Noto Sans Regular, SIL Open Font License
//! 1.1, `fonts/OFL.txt`), so a board renders the same on every device
//! (contract section 8). Glyphs are filled as tiny-skia paths from the font's
//! outlines, so text rotates, scales and blends like any other shape, and a
//! glyph's outline is built once per process.
//!
//! Layout is deliberately simple: one font, no shaping, no kerning (Noto
//! Sans keeps its kerning in GPOS, which this does not read), greedy word
//! wrap, a `.notdef` box for a character the font lacks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tiny_skia::{Path, PathBuilder, Transform};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::model::{Align, Point};

static FONT_BYTES: &[u8] = include_bytes!("../fonts/NotoSans-Regular.ttf");

/// The line height, as a multiple of the font size.
pub const LINE_HEIGHT: f32 = 1.25;

struct Font {
    face: Face<'static>,
    units_per_em: f32,
    ascender: f32,
    glyphs: Mutex<HashMap<u16, Option<Arc<Path>>>>,
}

fn font() -> &'static Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| {
        let face = Face::parse(FONT_BYTES, 0).expect("the bundled font parses");
        Font {
            units_per_em: face.units_per_em() as f32,
            ascender: face.ascender() as f32,
            face,
            glyphs: Mutex::new(HashMap::new()),
        }
    })
}

/// One laid-out glyph: its id and its x offset in the line, in board units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub id: u16,
    pub x: f32,
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
}

impl TextLayout {
    pub fn line_height(&self) -> f32 {
        self.font_size * LINE_HEIGHT
    }

    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.glyphs.is_empty())
    }

    /// The outline of every glyph, placed in a box whose top-left is
    /// `origin` and whose width is `box_width` (for alignment), as one path
    /// in board units. `None` when there is nothing to fill.
    pub fn path(&self, origin: Point, box_width: f32, align: Align) -> Option<Path> {
        let f = font();
        let scale = self.font_size / f.units_per_em;
        let line_height = self.line_height();
        // The baseline sits the ascender below the line's top, with the
        // extra line height split above and below.
        let baseline0 = (line_height - self.font_size) / 2.0 + f.ascender * scale;
        let mut builder = PathBuilder::new();
        let mut cache = f.glyphs.lock().unwrap();
        for (i, line) in self.lines.iter().enumerate() {
            let dx = match align {
                Align::Left => 0.0,
                Align::Center => (box_width - line.width) / 2.0,
                Align::Right => box_width - line.width,
            };
            let baseline = origin.y + baseline0 + i as f32 * line_height;
            for glyph in &line.glyphs {
                let outline = cache.entry(glyph.id).or_insert_with(|| glyph_outline(&f.face, glyph.id)).clone();
                let Some(outline) = outline else { continue };
                let ts = Transform::from_row(scale, 0.0, 0.0, -scale, origin.x + dx + glyph.x, baseline);
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
                let dx = match align {
                    Align::Left => 0.0,
                    Align::Center => (box_width - line.width) / 2.0,
                    Align::Right => box_width - line.width,
                };
                crate::model::Rect::new(
                    origin.x + dx,
                    origin.y + i as f32 * lh + lh * 0.3,
                    line.width,
                    lh * 0.45,
                )
            })
            .collect()
    }
}

/// Lay `text` out at `font_size`, wrapping at `wrap` board units when given
/// (a word wider than that breaks between characters). Every `\n` starts a
/// line; an empty text is one empty line.
pub fn layout(text: &str, font_size: f32, wrap: Option<f32>) -> TextLayout {
    let f = font();
    let scale = font_size / f.units_per_em;
    let advance = |c: char| -> (u16, f32) {
        let id = f.face.glyph_index(c).unwrap_or(GlyphId(0));
        let adv = f.face.glyph_hor_advance(id).unwrap_or(0) as f32 * scale;
        (id.0, adv)
    };
    let wrap = wrap.filter(|w| *w > 0.0);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = Line { glyphs: Vec::new(), width: 0.0 };
        // The line as it would be without its trailing spaces (what a wrap
        // measures), and where the current word started.
        let mut word_start = 0usize;
        for c in paragraph.chars() {
            if c == '\r' {
                continue;
            }
            let (id, adv) = advance(c);
            let is_space = c.is_whitespace();
            if is_space {
                line.glyphs.push(Glyph { id, x: line.width });
                line.width += adv;
                word_start = line.glyphs.len();
                continue;
            }
            if let Some(w) = wrap {
                if line.width + adv > w && !line.glyphs.is_empty() {
                    if word_start > 0 {
                        // Move the current word to a new line.
                        let word: Vec<Glyph> = line.glyphs.drain(word_start..).collect();
                        let start_x = word.first().map(|g| g.x).unwrap_or(line.width);
                        line.width = trimmed_width(&line.glyphs, start_x);
                        lines.push(std::mem::replace(&mut line, Line { glyphs: Vec::new(), width: 0.0 }));
                        for g in word {
                            line.glyphs.push(Glyph { id: g.id, x: g.x - start_x });
                        }
                        line.width = line.glyphs.last().map(|g| g.x).unwrap_or(0.0)
                            + line.glyphs.last().map(|g| glyph_advance(g.id, scale)).unwrap_or(0.0);
                        word_start = 0;
                    } else {
                        // One word wider than the line: break it here.
                        lines.push(std::mem::replace(&mut line, Line { glyphs: Vec::new(), width: 0.0 }));
                    }
                    if line.width + adv > w && !line.glyphs.is_empty() {
                        lines.push(std::mem::replace(&mut line, Line { glyphs: Vec::new(), width: 0.0 }));
                    }
                }
            }
            line.glyphs.push(Glyph { id, x: line.width });
            line.width += adv;
        }
        // Trailing spaces do not widen a line.
        line.width = trimmed_width(&line.glyphs, line.width);
        lines.push(line);
    }
    let width = lines.iter().map(|l| l.width).fold(0.0, f32::max);
    let height = lines.len() as f32 * font_size * LINE_HEIGHT;
    TextLayout { lines, font_size, width, height }
}

fn glyph_advance(id: u16, scale: f32) -> f32 {
    font().face.glyph_hor_advance(GlyphId(id)).unwrap_or(0) as f32 * scale
}

/// The width of `glyphs` (ending at `end`) without trailing spaces.
fn trimmed_width(glyphs: &[Glyph], end: f32) -> f32 {
    let f = font();
    let mut width = end;
    for g in glyphs.iter().rev() {
        let is_space = f.face.glyph_index(' ').map(|s| s.0) == Some(g.id);
        if is_space {
            width = g.x;
        } else {
            break;
        }
    }
    width
}

fn glyph_outline(face: &Face<'static>, id: u16) -> Option<Arc<Path>> {
    let mut b = Outline(PathBuilder::new());
    face.outline_glyph(GlyphId(id), &mut b)?;
    b.0.finish().map(Arc::new)
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
}

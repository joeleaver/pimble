//! Colored text in notes, read well on the light theme and on the dark one
//! (docs/TEXT_COLOR_CONTRACT.md).
//!
//! What is stored is rinch's `text_color` mark, its `color` a palette entry's
//! light-theme shade ([`PALETTE`], lowercase `#rrggbb`), so Markdown, the MCP,
//! copied HTML and other apps get a real color that reads on white paper.
//! Nothing Pimble-specific enters the document, and "Default" removes the mark.
//!
//! What is shown is the business of [`TextColorPlugin`]: an `Inline`
//! decoration over every colored run, whose classes the editor stylesheet
//! (`styles::EDITOR_CSS`, `pimble-tc`) reads. A palette color is shown in the
//! current theme's shade of it. Any other color (pasted from a web page, Google
//! Docs or Word) is shown as authored wherever it reads (at least
//! [`MIN_CONTRAST`] against the editor's background), and where it does not,
//! as the palette shade nearest its hue, or as the theme's ordinary text when
//! it has hardly any hue (pasted black on the dark theme). The decoration's
//! span sits inside the mark's, so its class outweighs the mark's inline
//! color; color only, so the text lays out exactly as before and the caret,
//! selection and typing are untouched. The stored value is never altered by
//! showing it.
//!
//! The plugin keeps what it found per top-level block and, after an edit,
//! reads again only the blocks that are not the same as before, as
//! `hashtags::HashtagsPlugin` does.

use std::rc::Rc;

use rinch::prelude::*;
use rinch_editor_core::state::{EditorState, Transaction};
use rinch_editor_core::{AttrValue, Attrs, Decoration, DecorationSet, Mark, MarkType, Node as EditorNode, Plugin, PluginKey, Pos};

use crate::state::AppStore;

/// The mark that carries a color, and its attribute.
pub const MARK: &str = "text_color";
const ATTR: &str = "color";
/// The textblock that takes no marks at all: no color there.
pub const CODE_BLOCK: &str = "code_block";

const PLUGIN_KEY: PluginKey = PluginKey("pimble.text_color");

/// The class every colored run carries in the editor.
pub const CLASS: &str = "pimble-tc";
/// Show the run in its palette class's light-theme shade on the light theme.
pub const FIX_LIGHT: &str = "pimble-tc-fix-light";
/// Show the run in its palette class's dark-theme shade on the dark theme.
pub const FIX_DARK: &str = "pimble-tc-fix-dark";
/// The palette class of a color with hardly any hue: fixed, it is the theme's
/// ordinary text color.
pub const NEUTRAL: &str = "neutral";

/// The editor's background on the light theme (rinch's editor sheet: the page
/// written on is white) and on the dark one (`--rinch-color-body`, rinch's
/// `dark-7`, which `EDITOR_CSS` gives the dark editor).
pub const LIGHT_BACKGROUND: &str = "#ffffff";
pub const DARK_BACKGROUND: &str = "#242424";

/// Below this contrast against the editor's background a pasted color is shown
/// as its nearest palette shade instead (WCAG's floor for large text and for
/// interface parts; the palette itself is chosen for 4.5:1 and better).
pub const MIN_CONTRAST: f32 = 3.0;

/// A color whose channels spread less than this (of 1.0) has no hue worth
/// keeping: black, white, the grays, a near-black navy.
const NEUTRAL_CHROMA: f32 = 0.12;

/// One color the picker offers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Swatch {
    /// The class suffix (`pimble-tc-<name>`).
    pub name: &'static str,
    /// What the person reads: the tooltip and the menu item.
    pub label: &'static str,
    /// The light-theme shade: what is stored, and shown on the light theme.
    pub light: &'static str,
    /// The dark-theme shade, shown on the dark theme.
    pub dark: &'static str,
    /// The hue (degrees) a pasted color is matched against, `None` for gray,
    /// which nothing is matched to (a pasted color with no hue is `neutral`).
    hue: Option<f32>,
}

const fn swatch(name: &'static str, label: &'static str, light: &'static str, dark: &'static str, hue: Option<f32>) -> Swatch {
    Swatch { name, label, light, dark, hue }
}

/// The palette, in the picker's order after "Default" (Joe, 2026-11-21).
/// Every light shade reads at 4.5:1 or better on white and every dark shade at
/// 6:1 or better on the dark editor (`the_palette_reads_in_both_themes`).
pub const PALETTE: [Swatch; 9] = [
    swatch("gray", "Gray", "#6b7280", "#a8adb7", None),
    swatch("red", "Red", "#c92a2a", "#ff8787", Some(0.0)),
    swatch("orange", "Orange", "#c2410c", "#ffa94d", Some(28.0)),
    swatch("yellow", "Yellow", "#a16207", "#ffd43b", Some(50.0)),
    swatch("green", "Green", "#2b7a3b", "#69db7c", Some(125.0)),
    swatch("teal", "Teal", "#0f766e", "#38d9a9", Some(172.0)),
    swatch("blue", "Blue", "#1971c2", "#74c0fc", Some(215.0)),
    swatch("purple", "Purple", "#7c3aed", "#b197fc", Some(272.0)),
    swatch("pink", "Pink", "#c2255c", "#f783ac", Some(332.0)),
];

/// The palette entry whose stored shade `color` is, in any spelling
/// (`#C92A2A`, `#c92a2a`, `rgb(201, 42, 42)`).
pub fn palette_entry(color: &str) -> Option<&'static Swatch> {
    let rgb = parse_color(color)?;
    PALETTE.iter().find(|s| parse_color(s.light) == Some(rgb))
}

// ── Color arithmetic ─────────────────────────────────────────────────────

/// An sRGB color, 0..=255 per channel.
pub type Rgb = (u8, u8, u8);

/// `color` as sRGB: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()`/`rgba()`
/// (commas or spaces, numbers or percentages; alpha ignored) and the common
/// named colors. Anything else is `None` and is shown as authored.
pub fn parse_color(color: &str) -> Option<Rgb> {
    let c = color.trim().to_ascii_lowercase();
    if let Some(hex) = c.strip_prefix('#') {
        let digit = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok();
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        if !hex.is_ascii() {
            return None;
        }
        return match hex.len() {
            3 | 4 => Some((digit(0)? * 17, digit(1)? * 17, digit(2)? * 17)),
            6 | 8 => Some((byte(0)?, byte(2)?, byte(4)?)),
            _ => None,
        };
    }
    if let Some(args) = c.strip_prefix("rgba(").or_else(|| c.strip_prefix("rgb(")) {
        let args = args.strip_suffix(')')?;
        let args = args.split('/').next()?;
        let parts: Vec<&str> = args.split(|ch: char| ch == ',' || ch.is_whitespace()).filter(|p| !p.is_empty()).collect();
        if parts.len() < 3 {
            return None;
        }
        let channel = |p: &str| -> Option<u8> {
            let v = match p.strip_suffix('%') {
                Some(pct) => pct.parse::<f32>().ok()? * 2.55,
                None => p.parse::<f32>().ok()?,
            };
            Some(v.round().clamp(0.0, 255.0) as u8)
        };
        return Some((channel(parts[0])?, channel(parts[1])?, channel(parts[2])?));
    }
    NAMED.iter().find(|(name, _)| *name == c).map(|&(_, rgb)| rgb)
}

/// The named colors pasted text is likely to carry.
const NAMED: &[(&str, Rgb)] = &[
    ("black", (0, 0, 0)),
    ("white", (255, 255, 255)),
    ("gray", (128, 128, 128)),
    ("grey", (128, 128, 128)),
    ("silver", (192, 192, 192)),
    ("red", (255, 0, 0)),
    ("maroon", (128, 0, 0)),
    ("orange", (255, 165, 0)),
    ("yellow", (255, 255, 0)),
    ("olive", (128, 128, 0)),
    ("lime", (0, 255, 0)),
    ("green", (0, 128, 0)),
    ("teal", (0, 128, 128)),
    ("aqua", (0, 255, 255)),
    ("cyan", (0, 255, 255)),
    ("blue", (0, 0, 255)),
    ("navy", (0, 0, 128)),
    ("purple", (128, 0, 128)),
    ("fuchsia", (255, 0, 255)),
    ("magenta", (255, 0, 255)),
    ("pink", (255, 192, 203)),
];

/// WCAG relative luminance.
fn luminance((r, g, b): Rgb) -> f32 {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG contrast ratio of two colors, 1.0 to 21.0.
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// The palette entry nearest `rgb` in hue, or [`NEUTRAL`] for a color with
/// hardly any hue.
fn nearest(rgb: Rgb) -> &'static str {
    let (r, g, b) = (rgb.0 as f32 / 255.0, rgb.1 as f32 / 255.0, rgb.2 as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if d < NEUTRAL_CHROMA {
        return NEUTRAL;
    }
    let hue = 60.0
        * if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
    let distance = |h: f32| {
        let x = (hue - h).abs() % 360.0;
        x.min(360.0 - x)
    };
    PALETTE
        .iter()
        .filter_map(|s| s.hue.map(|h| (s.name, distance(h))))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(NEUTRAL, |(name, _)| name)
}

/// The classes a run colored `color` carries: [`CLASS`] always (a hashtag in
/// colored text takes the text's color), and with a palette class the themes
/// to show that class's shade in. A palette color is fixed in both themes; a
/// pasted one only where it reads below [`MIN_CONTRAST`].
pub fn classes_for(color: &str) -> String {
    if let Some(entry) = palette_entry(color) {
        return format!("{CLASS} {CLASS}-{} {FIX_LIGHT} {FIX_DARK}", entry.name);
    }
    let Some(rgb) = parse_color(color) else { return CLASS.to_string() };
    let light = contrast(rgb, parse_color(LIGHT_BACKGROUND).unwrap()) < MIN_CONTRAST;
    let dark = contrast(rgb, parse_color(DARK_BACKGROUND).unwrap()) < MIN_CONTRAST;
    if !light && !dark {
        return CLASS.to_string();
    }
    let mut out = format!("{CLASS} {CLASS}-{}", nearest(rgb));
    if light {
        out.push(' ');
        out.push_str(FIX_LIGHT);
    }
    if dark {
        out.push(' ');
        out.push_str(FIX_DARK);
    }
    out
}

/// The CSS color text colored `color` is shown in on the given theme, as the
/// stylesheet shows it: what the toolbar's color bar draws.
pub fn shown_color(color: &str, dark: bool) -> String {
    let classes = classes_for(color);
    let fixed = classes.split(' ').any(|c| c == if dark { FIX_DARK } else { FIX_LIGHT });
    if !fixed {
        return color.to_string();
    }
    let name = classes.split(' ').find_map(|c| c.strip_prefix("pimble-tc-").filter(|n| !n.starts_with("fix-"))).unwrap_or(NEUTRAL);
    match PALETTE.iter().find(|s| s.name == name) {
        Some(s) => (if dark { s.dark } else { s.light }).to_string(),
        None => "var(--rinch-color-text)".to_string(),
    }
}

// ── The plugin ───────────────────────────────────────────────────────────

/// One colored stretch of a top-level block: positions measured from the
/// position before the block, and its classes.
type Run = (usize, usize, Rc<str>);

/// What the plugin found: per top-level block of the document, its colored
/// runs. Cloning shares it.
#[derive(Debug, Clone, Default)]
struct Found {
    blocks: Rc<Vec<Rc<[Run]>>>,
}

impl Found {
    /// What `doc` holds, reusing `prev`'s answer for every top-level block
    /// `doc` shares with `prev_doc` at its start or its end.
    fn of(doc: &EditorNode, prev: Option<(&EditorNode, &Found)>) -> Found {
        let n = doc.child_count();
        let mut blocks: Vec<Rc<[Run]>> = Vec::with_capacity(n);
        match prev {
            Some((old, found)) if found.blocks.len() == old.child_count() => {
                let m = old.child_count();
                let mut head = 0;
                while head < n && head < m && doc.child(head).same_ref(old.child(head)) {
                    head += 1;
                }
                let mut tail = 0;
                while tail < n - head && tail < m - head && doc.child(n - 1 - tail).same_ref(old.child(m - 1 - tail)) {
                    tail += 1;
                }
                blocks.extend(found.blocks[..head].iter().cloned());
                blocks.extend((head..n - tail).map(|i| block_runs(doc.child(i)).into()));
                blocks.extend(found.blocks[m - tail..].iter().cloned());
            }
            _ => blocks.extend((0..n).map(|i| block_runs(doc.child(i)).into())),
        }
        Found { blocks: Rc::new(blocks) }
    }

    /// Every run in document positions.
    fn ranges(&self, doc: &EditorNode) -> Vec<(usize, usize, Rc<str>)> {
        let mut out = Vec::new();
        let mut at = 0;
        for (i, runs) in self.blocks.iter().enumerate() {
            out.extend(runs.iter().map(|(from, to, class)| (at + from, at + to, class.clone())));
            at += doc.child(i).node_size();
        }
        out
    }
}

/// The colored runs of top-level block `block`, positions measured from the
/// position before it; neighbouring text of one color is one run.
fn block_runs(block: &EditorNode) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let mut last: Option<(String, Rc<str>)> = None;
    // A block's content starts at 1 (its opening).
    block.nodes_between(0, block.content_size(), &mut |node, pos, _| {
        if !node.is_text() {
            return true;
        }
        let Some(color) = node.marks().iter().find(|m| m.type_name() == MARK).and_then(|m| m.attrs.get_str(ATTR)).filter(|c| !c.is_empty()) else {
            return false;
        };
        let (from, to) = (pos + 1, pos + 1 + node.node_size());
        let class = match &last {
            Some((c, class)) if c == color => class.clone(),
            _ => {
                let class: Rc<str> = classes_for(color).into();
                last = Some((color.to_string(), class.clone()));
                class
            }
        };
        match out.last_mut() {
            Some(run) if run.1 == from && Rc::ptr_eq(&run.2, &class) => run.1 = to,
            _ => out.push((from, to, class)),
        }
        false
    });
    out
}

/// The editor plugin that shows colored text legibly in either theme. Added
/// to every pane's editor before any content (adding a plugin resets history).
pub struct TextColorPlugin;

impl Plugin for TextColorPlugin {
    fn key(&self) -> PluginKey {
        PLUGIN_KEY
    }

    fn init_state(&self, doc: &EditorNode) -> Option<Rc<dyn std::any::Any>> {
        Some(Rc::new(Found::of(doc, None)))
    }

    fn apply(&self, tr: &Transaction, old: &EditorState, new_doc: &EditorNode, prev: Option<&dyn std::any::Any>) -> Option<Rc<dyn std::any::Any>> {
        let prev = prev.and_then(|p| p.downcast_ref::<Found>());
        if let (Some(prev), false) = (prev, tr.doc_changed()) {
            return Some(Rc::new(prev.clone()));
        }
        Some(Rc::new(Found::of(new_doc, prev.map(|p| (&old.doc, p)))))
    }

    fn decorations(&self, state: &EditorState) -> DecorationSet {
        let Some(found) = state.plugin_state::<Found>(PLUGIN_KEY) else { return DecorationSet::empty() };
        if found.blocks.iter().all(|b| b.is_empty()) {
            return DecorationSet::empty();
        }
        let size = state.doc.content_size();
        DecorationSet::new(
            found
                .ranges(&state.doc)
                .into_iter()
                .filter(|&(from, to, _)| from < to && to <= size)
                .map(|(from, to, class)| Decoration::inline(Pos(from), Pos(to), Attrs::new().with("class", class.to_string())))
                .collect(),
        )
    }
}

// ── Reading and writing a color ──────────────────────────────────────────

fn color_of(marks: &[Mark]) -> Option<String> {
    marks.iter().find(|m| m.type_name() == MARK).and_then(|m| m.attrs.get_str(ATTR)).map(str::to_string)
}

/// The color stored at the selection: for a caret, what a character typed
/// there would carry (the stored marks, else the marks there); for a range,
/// the first colored text in it. `None` is the default color.
pub fn current_color(state: &EditorState) -> Option<String> {
    let sel = &state.selection;
    if sel.is_empty() {
        return match &state.stored_marks {
            Some(marks) => color_of(marks),
            None => state.doc.resolve(sel.head()).ok().and_then(|r| color_of(&r.marks())),
        };
    }
    let mut found = None;
    for (from, to) in sel.ranges(&state.doc) {
        state.doc.nodes_between(from.0, to.0, &mut |node, _, _| {
            if found.is_none() && node.is_inline() {
                found = color_of(node.marks());
            }
            found.is_none()
        });
        if found.is_some() {
            break;
        }
    }
    found
}

/// Whether the selection's head is in a code block, which takes no color.
pub fn in_code_block(state: &EditorState) -> bool {
    state.doc.resolve(state.selection.head()).is_ok_and(|r| r.parent().type_name() == CODE_BLOCK)
}

/// The transaction that colors the selection `color` (a palette entry's light
/// shade), or takes its color off (`None`, "Default"). Over a range every
/// color already there is removed first (rinch's `text_color` does not exclude
/// itself, so adding alone would stack two). At a caret it sets the stored
/// marks, so what is typed next is colored. `None` when nothing applies.
pub fn color_transaction(state: &EditorState, color: Option<&str>) -> Option<Transaction> {
    let mark_type = state.schema().mark_type(MARK)?.clone();
    let mark = color.map(|c| Mark::new(mark_type.clone(), Attrs::from_iter([(ATTR, AttrValue::from(c.to_string()))])));
    let sel = &state.selection;
    let mut tr = state.tr();
    if sel.is_empty() {
        if in_code_block(state) {
            return None;
        }
        let base = match &state.stored_marks {
            Some(marks) => marks.clone(),
            None => state.doc.resolve(sel.head()).ok()?.marks(),
        };
        let kept: Vec<Mark> = base.into_iter().filter(|m| m.typ != mark_type).collect();
        let next = match &mark {
            Some(mark) => mark.add_to_set(&kept),
            None => kept,
        };
        tr.set_stored_marks(Some(next));
        return Some(tr);
    }
    for (from, to) in sel.ranges(&state.doc) {
        remove_colors(&mut tr, &state.doc, from.0, to.0, &mark_type).ok()?;
        if let Some(mark) = &mark {
            tr.add_mark(from.0, to.0, mark.clone()).ok()?;
        }
    }
    tr.doc_changed().then_some(tr)
}

/// Take every color off `from..to`.
fn remove_colors(tr: &mut Transaction, doc: &EditorNode, from: usize, to: usize, mark_type: &MarkType) -> Result<(), rinch_editor_core::transform::StepError> {
    let mut found: Vec<Mark> = Vec::new();
    doc.nodes_between(from, to, &mut |node, _, _| {
        for m in node.marks() {
            if &m.typ == mark_type && !found.contains(m) {
                found.push(m.clone());
            }
        }
        true
    });
    for m in found {
        tr.remove_mark(from, to, m)?;
    }
    Ok(())
}

/// Color the focused pane's selection `color` (a palette entry's light shade;
/// `None` is "Default"): the toolbar's swatches and Edit > Text Color both
/// run this. With no note there, one this device may only read, or the caret
/// in a code block, it says so. The keyboard goes back to the editor.
pub fn apply(store: AppStore, color: Option<&'static str>) {
    let pane = store.focused();
    let Some(active) = untracked(|| store.pane(pane).active_edit.get()) else {
        crate::events::show_notice(store, "Open a note to color its text.".to_string());
        return;
    };
    if !store.node_access(active.store_id, active.node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return;
    }
    let h = crate::editor::editor(pane);
    if in_code_block(&h.state()) {
        crate::events::show_notice(store, "Text in a code block has no color.".to_string());
        return;
    }
    h.update(|state| color_transaction(state, color));
    h.focus();
    crate::toolbar::bump_toolbar(pane);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rinch_editor_core::selection::Selection;
    use rinch_editor_core::{default_plugins, Fragment, Schema};

    fn schema() -> Rc<Schema> {
        Rc::new(Schema::starter_kit())
    }

    fn colored(s: &Schema, text: &str, color: &str) -> EditorNode {
        let mark = Mark::new(s.mark_type(MARK).unwrap().clone(), Attrs::new().with(ATTR, color.to_string()));
        s.text_with_marks(text, vec![mark]).unwrap()
    }

    fn para(s: &Schema, runs: Vec<EditorNode>) -> EditorNode {
        s.branch("paragraph", Fragment::from_children(runs)).unwrap()
    }

    fn state_of(s: Rc<Schema>, blocks: Vec<EditorNode>) -> EditorState {
        let doc = s.branch(&s.top_node, Fragment::from_children(blocks)).unwrap();
        let mut plugins = default_plugins();
        plugins.push(Rc::new(TextColorPlugin) as Rc<dyn Plugin>);
        plugins.push(Rc::new(crate::hashtags::HashtagsPlugin) as Rc<dyn Plugin>);
        EditorState::create(s, doc, plugins)
    }

    /// Each decorated range: its text and its classes.
    fn decorated(state: &EditorState) -> Vec<(String, String)> {
        let doc = &state.doc;
        TextColorPlugin
            .decorations(state)
            .iter()
            .map(|d| {
                let (from, to) = d.range();
                let mut text = String::new();
                doc.nodes_between(from.0, to.0, &mut |node, pos, _| {
                    if let Some(t) = node.text() {
                        let skip = from.0.saturating_sub(pos);
                        let take = to.0.min(pos + node.node_size()) - pos.max(from.0);
                        text.extend(t.chars().skip(skip).take(take));
                    }
                    true
                });
                (text, d.inline_class().unwrap().to_string())
            })
            .collect()
    }

    fn rgb(c: &str) -> Rgb {
        parse_color(c).unwrap()
    }

    /// The numbers behind the palette: every light shade at 4.5:1 or better on
    /// the light editor, every dark shade at 6:1 or better on the dark one.
    #[test]
    fn the_palette_reads_in_both_themes() {
        for s in PALETTE {
            let light = contrast(rgb(s.light), rgb(LIGHT_BACKGROUND));
            let dark = contrast(rgb(s.dark), rgb(DARK_BACKGROUND));
            println!("{:<7} {} {:.2}:1 on white   {} {:.2}:1 on {}", s.label, s.light, light, s.dark, dark, DARK_BACKGROUND);
            assert!(light >= 4.5, "{} light {} reads {light:.2}:1 on white", s.label, s.light);
            assert!(dark >= 6.0, "{} dark {} reads {dark:.2}:1 on the dark editor", s.label, s.dark);
            assert_eq!(s.light, s.light.to_ascii_lowercase(), "stored lowercase");
            assert_eq!(palette_entry(s.light), Some(&s));
        }
        // Yellow is an amber that reads on white, not a yellow that does not.
        assert!(contrast(rgb(PALETTE[3].light), rgb(LIGHT_BACKGROUND)) >= 4.5);
    }

    /// The stylesheet shows every palette class in its shades, so the two
    /// cannot drift apart.
    #[test]
    fn the_stylesheet_has_every_shade() {
        let css = crate::styles::EDITOR_CSS;
        for s in PALETTE {
            assert!(css.contains(&format!(".pimble-tc-fix-light.pimble-tc-{} {{\n    color: {};", s.name, s.light)), "{} light", s.name);
            assert!(css.contains(&format!(".pimble-tc-fix-dark.pimble-tc-{} {{\n    color: {};", s.name, s.dark)), "{} dark", s.name);
        }
    }

    #[test]
    fn colors_parse_in_every_spelling_pasted_text_uses() {
        assert_eq!(parse_color("#C92A2A"), Some((201, 42, 42)));
        assert_eq!(parse_color("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_color("#00000080"), Some((0, 0, 0)));
        assert_eq!(parse_color("rgb(0, 0, 0)"), Some((0, 0, 0)));
        assert_eq!(parse_color("rgba(201,42,42,0.5)"), Some((201, 42, 42)));
        assert_eq!(parse_color("rgb(100% 0% 0% / 50%)"), Some((255, 0, 0)));
        assert_eq!(parse_color("Black"), Some((0, 0, 0)));
        assert_eq!(parse_color("var(--x)"), None);
        assert_eq!(palette_entry("RGB(201, 42, 42)").map(|s| s.name), Some("red"));
    }

    #[test]
    fn palette_colors_and_pasted_ones_get_the_classes_that_keep_them_legible() {
        // A palette color: its shade in either theme.
        assert_eq!(classes_for("#c92a2a"), "pimble-tc pimble-tc-red pimble-tc-fix-light pimble-tc-fix-dark");
        assert_eq!(classes_for("#C92A2A"), classes_for("#c92a2a"));
        // Pasted black: as authored on light, ordinary text on dark.
        assert_eq!(classes_for("rgb(0, 0, 0)"), "pimble-tc pimble-tc-neutral pimble-tc-fix-dark");
        assert_eq!(shown_color("#000000", true), "var(--rinch-color-text)");
        assert_eq!(shown_color("#000000", false), "#000000");
        // Pasted near-white: ordinary text on light, as authored on dark.
        assert_eq!(classes_for("#f4f4f4"), "pimble-tc pimble-tc-neutral pimble-tc-fix-light");
        // A pasted pale yellow is the palette's amber on light.
        assert_eq!(classes_for("#ffff66"), "pimble-tc pimble-tc-yellow pimble-tc-fix-light");
        assert_eq!(shown_color("#ffff66", false), "#a16207");
        // A pasted navy is the palette's blue on dark.
        assert_eq!(classes_for("navy"), "pimble-tc pimble-tc-blue pimble-tc-fix-dark");
        // One that reads in both: no fix at all, shown as authored.
        assert_eq!(classes_for("#e8590c"), "pimble-tc");
        assert_eq!(shown_color("#e8590c", true), "#e8590c");
        // Something unreadable as a color: as authored.
        assert_eq!(classes_for("currentcolor"), "pimble-tc");
    }

    #[test]
    fn colored_runs_are_decorated_with_their_classes() {
        let s = schema();
        let bold = Mark::new(s.mark_type("bold").unwrap().clone(), Attrs::new());
        let red_bold = {
            let red = Mark::new(s.mark_type(MARK).unwrap().clone(), Attrs::new().with(ATTR, "#c92a2a".to_string()));
            s.text_with_marks("redder", vec![bold, red]).unwrap()
        };
        let first = para(&s, vec![s.text("plain ").unwrap(), colored(&s, "red", "#c92a2a"), red_bold, s.text(" and ").unwrap(), colored(&s, "black", "#000000")]);
        let list = s
            .branch("bullet_list", Fragment::from_node(s.branch("list_item", Fragment::from_node(para(&s, vec![colored(&s, "white", "#ffffff")]))).unwrap()))
            .unwrap();
        let fine = para(&s, vec![colored(&s, "fine", "#e8590c")]);
        let state = state_of(s, vec![first, list, fine]);
        assert_eq!(
            decorated(&state),
            [
                ("redredder".to_string(), "pimble-tc pimble-tc-red pimble-tc-fix-light pimble-tc-fix-dark".to_string()),
                ("black".to_string(), "pimble-tc pimble-tc-neutral pimble-tc-fix-dark".to_string()),
                ("white".to_string(), "pimble-tc pimble-tc-neutral pimble-tc-fix-light".to_string()),
                ("fine".to_string(), "pimble-tc".to_string()),
            ]
        );
    }

    #[test]
    fn an_edit_reads_again_only_the_blocks_it_changed() {
        let s = schema();
        let state = state_of(s.clone(), vec![para(&s, vec![s.text("first").unwrap()]), para(&s, vec![colored(&s, "blue words", "#1971c2")])]);
        let second_before = state.plugin_state::<Found>(PLUGIN_KEY).unwrap().blocks[1].clone();
        let mut tr = state.tr();
        tr.set_selection(Selection::cursor(Pos(1)));
        let state = state.apply(tr);
        let mut tr = state.tr();
        tr.insert_text("more ").unwrap();
        let state = state.apply(tr);
        let found = state.plugin_state::<Found>(PLUGIN_KEY).unwrap();
        assert!(Rc::ptr_eq(&found.blocks[1], &second_before), "the untouched block was not read again");
        // And its run moved with the text before it.
        assert_eq!(decorated(&state), [("blue words".to_string(), "pimble-tc pimble-tc-blue pimble-tc-fix-light pimble-tc-fix-dark".to_string())]);
    }

    #[test]
    fn coloring_a_range_replaces_its_color_and_default_takes_it_off() {
        let s = schema();
        let state = state_of(s.clone(), vec![para(&s, vec![s.text("one ").unwrap(), colored(&s, "two", "#ff0000"), s.text(" three").unwrap()])]);
        // "one two" (1..8): the pasted red and the plain text both become blue.
        let mut tr = state.tr();
        tr.set_selection(Selection::text(Pos(1), Pos(8)));
        let state = state.apply(tr);
        assert_eq!(current_color(&state).as_deref(), Some("#ff0000"));
        let state = state.apply(color_transaction(&state, Some("#1971c2")).unwrap());
        let block = state.doc.child(0);
        let colors: Vec<(String, Vec<String>)> = (0..block.child_count())
            .map(|i| {
                let n = block.child(i);
                (n.text().unwrap().to_string(), n.marks().iter().filter(|m| m.type_name() == MARK).map(|m| m.attrs.get_str(ATTR).unwrap().to_string()).collect())
            })
            .collect();
        assert_eq!(colors, [("one two".to_string(), vec!["#1971c2".to_string()]), (" three".to_string(), vec![])]);
        assert_eq!(current_color(&state).as_deref(), Some("#1971c2"));
        // Default: no color left anywhere.
        let state = state.apply(color_transaction(&state, None).unwrap());
        assert_eq!(state.doc.child(0).child_count(), 1, "one plain run");
        assert_eq!(current_color(&state), None);
        // Nothing to take off: nothing to do.
        assert!(color_transaction(&state, None).is_none());
    }

    #[test]
    fn a_color_chosen_at_the_caret_colors_what_is_typed_next() {
        let s = schema();
        let state = state_of(s.clone(), vec![para(&s, vec![s.text("ab").unwrap()])]);
        let mut tr = state.tr();
        tr.set_selection(Selection::cursor(Pos(2)));
        let state = state.apply(tr);
        let state = state.apply(color_transaction(&state, Some("#2b7a3b")).unwrap());
        assert_eq!(current_color(&state).as_deref(), Some("#2b7a3b"));
        let mut tr = state.tr();
        tr.insert_text("X").unwrap();
        let state = state.apply(tr);
        assert_eq!(decorated(&state), [("X".to_string(), "pimble-tc pimble-tc-green pimble-tc-fix-light pimble-tc-fix-dark".to_string())]);
        // And inside colored text, Default makes the next character plain.
        let state = state.apply(color_transaction(&state, None).unwrap());
        let mut tr = state.tr();
        tr.insert_text("y").unwrap();
        let state = state.apply(tr);
        // "a", the green "X", then "y" joined to the plain "b" after it.
        assert_eq!(state.doc.child(0).child(2).text(), Some("yb"));
        assert!(state.doc.child(0).child(2).marks().is_empty());
    }

    #[test]
    fn a_code_block_takes_no_color() {
        let s = schema();
        let block = s.branch(CODE_BLOCK, Fragment::from_node(s.text("let x = 1;").unwrap())).unwrap();
        let state = state_of(s, vec![block]);
        let mut tr = state.tr();
        tr.set_selection(Selection::cursor(Pos(3)));
        let state = state.apply(tr);
        assert!(in_code_block(&state));
        assert!(color_transaction(&state, Some("#c92a2a")).is_none());
    }

    /// Markdown (the MCP, copy-out) carries a palette color as written.
    #[test]
    fn a_palette_color_round_trips_through_markdown_unchanged() {
        let s = Schema::starter_kit();
        let md = "Plain and <span style=\"color:#c92a2a\">red words</span> here.\n";
        let doc = pimble_crdt::markdown::parse(&s, md).unwrap();
        let back = pimble_crdt::markdown::render(&doc);
        assert!(back.contains("<span style=\"color:#c92a2a\">red words</span>"), "{back}");
        let state = state_of(Rc::new(s), (0..doc.child_count()).map(|i| doc.child(i).clone()).collect());
        assert_eq!(decorated(&state)[0].0, "red words");
    }
}

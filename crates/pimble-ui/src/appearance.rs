//! A node's icon and colors as every view draws them: the tree, a pane's
//! title, a metanode's outline, a plugin's cards.
//!
//! A node's icon is a Tabler icon name (`pimble_core::custom_keys::ICON`) and its
//! colors are `#rrggbb` (`COLOR`, `BACKGROUND`); the stored values are never
//! altered, only the way they are drawn on the current theme.

use pimble_core::{node_types, Node};
use rinch::prelude::*;
use rinch_tabler_icons::{render_tabler_icon, TablerIcon, TablerIconStyle, ALL_ICONS};

/// Any Tabler icon by its name (`TablerIcon::name()`, kebab-case).
pub fn icon_by_name(name: &str) -> Option<TablerIcon> {
    ALL_ICONS.iter().copied().find(|icon| icon.name() == name)
}

/// The icon a node is drawn with: its own, or its type's as the explorer
/// draws it.
pub fn node_icon(node: &Node) -> String {
    if let Some(icon) = node.metadata.icon() {
        return icon.to_string();
    }
    // A query metanode, a query metanode (docs/QUERY_METANODES_CONTRACT.md
    // section 6), whatever its type.
    if node.metadata.metanode.as_ref().is_some_and(|m| m.is_query()) {
        return TablerIcon::FilterSearch.name().to_string();
    }
    let icon = match node.node_type.as_str() {
        node_types::FOLDER => TablerIcon::Folder,
        node_types::MOUNT => TablerIcon::Link,
        node_types::BOARD => TablerIcon::Chalkboard,
        node_types::SHEET => TablerIcon::Table,
        node_types::DISCUSSION => TablerIcon::Messages,
        _ => TablerIcon::File,
    };
    icon.name().to_string()
}

/// The violet a query metanode and the rows it finds are drawn with
/// (docs/QUERY_METANODES_CONTRACT.md section 6: purple means "gathered
/// here", as blue means "mounted here"): the theme's violet on the light
/// theme, a lifted one on the dark. Render-time only; nothing purple is
/// stored on a node.
pub fn gathered_violet(dark_mode: bool) -> &'static str {
    if dark_mode {
        "var(--rinch-color-violet-4)"
    } else {
        "var(--rinch-color-violet-6)"
    }
}

/// One Tabler icon by name, as a component so a reactive `for` can render it
/// (an icon needs a render scope, which a component has and a reactive block
/// does not). An unknown name renders nothing.
#[component]
pub fn IconGlyph(name: String) -> NodeHandle {
    match icon_by_name(&name) {
        Some(icon) => render_tabler_icon(__scope, icon, TablerIconStyle::Outline),
        None => rsx! { span {} },
    }
}

/// The color to draw `hex` with on the current theme. Stored colors are kept as
/// given (an import brings Scrivener's, which are made for light backgrounds), but
/// text and icons drawn in a dark navy on the dark theme are unreadable, so in dark
/// mode the lightness is raised to a floor; in light mode it is capped. Anything
/// that is not `#rrggbb` is passed through untouched.
pub fn display_color(hex: &str, dark_mode: bool) -> String {
    let Some((r, g, b)) = parse_hex(hex) else { return hex.to_string() };
    let (h, s, l) = rgb_to_hsl(r, g, b);
    let adjusted = if dark_mode { l.max(0.62) } else { l.min(0.42) };
    if (adjusted - l).abs() < f32::EPSILON {
        // Already readable: keep the exact stored value, no float round trip.
        return hex.to_string();
    }
    let (r, g, b) = hsl_to_rgb(h, s, adjusted);
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The CSS background for a row tinted `hex` on the current theme: the stored
/// color at an opacity that keeps the row's text readable. A light theme
/// shows it nearly as given (Scrivener's pastels are made for one); a dark
/// theme lets a little of it through, so a pastel reads as a tint of the dark
/// row rather than a light bar under light text. A dark stored color is
/// lifted first, as [`display_color`] lifts text. Anything that is not
/// `#rrggbb` is passed through untouched.
pub fn row_background(hex: &str, dark_mode: bool) -> String {
    let Some((r, g, b)) = parse_hex(hex) else { return hex.to_string() };
    let (h, s, l) = rgb_to_hsl(r, g, b);
    let (l, alpha) = if dark_mode { (l.max(0.55), 0.28) } else { (l.max(0.72), 0.85) };
    let (r, g, b) = hsl_to_rgb(h, s, l);
    format!("rgba({r}, {g}, {b}, {alpha})")
}

fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let channel = |t: f32| {
        let t = if t < 0.0 { t + 1.0 } else if t > 1.0 { t - 1.0 } else { t };
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
        let p = 2.0 * l - q;
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    if s.abs() < f32::EPSILON {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    (channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_is_a_translucent_tint_and_a_dark_one_is_lifted() {
        assert_eq!(row_background("#b0d7ff", false), "rgba(176, 215, 255, 0.85)");
        assert!(row_background("#b0d7ff", true).ends_with(", 0.28)"));
        // The navy "TOP LEVEL TOPIC" label is lifted to a light tint on a light theme.
        let shown = row_background("#2317be", false);
        let rgb: Vec<u8> = shown.trim_start_matches("rgba(").split(", ").take(3).map(|v| v.parse().unwrap()).collect();
        let (_, _, l) = rgb_to_hsl(rgb[0], rgb[1], rgb[2]);
        assert!(l >= 0.7, "{shown}");
        assert_eq!(row_background("red", true), "red");
    }

    #[test]
    fn dark_mode_lifts_a_navy_label_to_something_readable() {
        // Scrivener's "TOP LEVEL TOPIC" label: 0.137 0.090 0.745.
        let shown = display_color("#2317be", true);
        let (r, g, b) = parse_hex(&shown).unwrap();
        let (_, _, l) = rgb_to_hsl(r, g, b);
        assert!(l >= 0.6, "lightness {l} for {shown}");
        // Already-light colors are left alone.
        assert_eq!(display_color("#f5e6b3", true), "#f5e6b3");
        // Non-hex passes through.
        assert_eq!(display_color("rebeccapurple", true), "rebeccapurple");
    }

    #[test]
    fn a_node_is_drawn_with_its_own_icon_or_its_types() {
        let mut folder = Node::new(node_types::FOLDER);
        assert_eq!(node_icon(&folder), "folder");
        folder.metadata.set_icon(Some("star".into()));
        assert_eq!(node_icon(&folder), "star");
        assert!(icon_by_name("brand-github").is_some());
        assert!(icon_by_name("no-such-icon").is_none());
    }
}

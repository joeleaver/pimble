//! The tree's per-node appearance: a custom icon and a colour, stored in the node's
//! metadata (`pimble_core::custom_keys::ICON` / `COLOR`) so they replicate with the
//! store and any client can render them.
//!
//! The icon is a Tabler icon name (`TablerIcon::name()`, kebab-case). Only the names in
//! [`ICON_CHOICES`] are offered in the picker, but any name in the table is looked up by
//! [`icon_by_name`], so an importer may set one the picker does not show. A name the
//! table does not know falls back to the node type's icon.

use rinch_tabler_icons::TablerIcon;

/// The icons the appearance picker offers, in display order. Each entry's name is its
/// `TablerIcon::name()`, which is what the metadata stores.
pub const ICON_CHOICES: &[TablerIcon] = &[
    TablerIcon::Star,
    TablerIcon::Heart,
    TablerIcon::Flag,
    TablerIcon::Bookmark,
    TablerIcon::Pin,
    TablerIcon::Tag,
    TablerIcon::Check,
    TablerIcon::Checkbox,
    TablerIcon::Square,
    TablerIcon::ListCheck,
    TablerIcon::AlertTriangle,
    TablerIcon::AlertCircle,
    TablerIcon::InfoCircle,
    TablerIcon::Help,
    TablerIcon::Bulb,
    TablerIcon::Target,
    TablerIcon::Rocket,
    TablerIcon::Trophy,
    TablerIcon::Notes,
    TablerIcon::FileText,
    TablerIcon::Book,
    TablerIcon::Pencil,
    TablerIcon::Archive,
    TablerIcon::Folder,
    TablerIcon::Home,
    TablerIcon::Building,
    TablerIcon::Briefcase,
    TablerIcon::School,
    TablerIcon::Calendar,
    TablerIcon::Clock,
    TablerIcon::MapPin,
    TablerIcon::World,
    TablerIcon::Phone,
    TablerIcon::Mail,
    TablerIcon::User,
    TablerIcon::Users,
    TablerIcon::Cash,
    TablerIcon::CreditCard,
    TablerIcon::Wallet,
    TablerIcon::Receipt,
    TablerIcon::ShoppingCart,
    TablerIcon::Gift,
    TablerIcon::Cake,
    TablerIcon::ChefHat,
    TablerIcon::Coffee,
    TablerIcon::MedicalCross,
    TablerIcon::Pill,
    TablerIcon::Bed,
    TablerIcon::Car,
    TablerIcon::Plane,
    TablerIcon::Anchor,
    TablerIcon::Tool,
    TablerIcon::Key,
    TablerIcon::Lock,
    TablerIcon::Bug,
    TablerIcon::Code,
    TablerIcon::Database,
    TablerIcon::ChartBar,
    TablerIcon::Music,
    TablerIcon::Camera,
    TablerIcon::Palette,
    TablerIcon::Plant,
    TablerIcon::Tree,
    TablerIcon::Paw,
    TablerIcon::Dog,
    TablerIcon::Cat,
    TablerIcon::Sun,
    TablerIcon::Moon,
    TablerIcon::Umbrella,
    TablerIcon::Link,
];

/// The colours the picker offers: a display name and its CSS value. Any `#rrggbb` is
/// accepted in metadata (an import may bring its own); these are the ones offered.
pub const COLOR_CHOICES: &[(&str, &str)] = &[
    ("Red", "#e5484d"),
    ("Orange", "#f76b15"),
    ("Yellow", "#f5d90a"),
    ("Green", "#46a758"),
    ("Teal", "#12a594"),
    ("Blue", "#0090ff"),
    ("Indigo", "#6e56cf"),
    ("Pink", "#e93d82"),
    ("Brown", "#ad7f58"),
    ("Grey", "#8b8d98"),
];

/// The icon stored under `name`, if the picker's table knows it.
pub fn icon_by_name(name: &str) -> Option<TablerIcon> {
    ICON_CHOICES.iter().copied().find(|icon| icon.name() == name)
}

/// The colour to draw `hex` with on the current theme. Stored colours are kept as
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
    fn dark_mode_lifts_a_navy_label_to_something_readable() {
        // Scrivener's "TOP LEVEL TOPIC" label: 0.137 0.090 0.745.
        let shown = display_color("#2317be", true);
        let (r, g, b) = parse_hex(&shown).unwrap();
        let (_, _, l) = rgb_to_hsl(r, g, b);
        assert!(l >= 0.6, "lightness {l} for {shown}");
        // Already-light colours are left alone.
        assert_eq!(display_color("#f5e6b3", true), "#f5e6b3");
        // Non-hex passes through.
        assert_eq!(display_color("rebeccapurple", true), "rebeccapurple");
    }

    #[test]
    fn every_picker_icon_round_trips_through_its_name() {
        for icon in ICON_CHOICES {
            assert_eq!(icon_by_name(icon.name()), Some(*icon));
        }
    }
}

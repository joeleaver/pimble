//! The tree's per-node appearance: a custom icon and a color, stored in the node's
//! metadata (`pimble_core::custom_keys::ICON` / `COLOR`) so they replicate with the
//! store and any client can render them.
//!
//! The icon is a Tabler icon name (`TablerIcon::name()`, kebab-case). Only the names in
//! [`ICON_CHOICES`] are offered in the picker, but any name in the table is looked up by
//! [`icon_by_name`], so an importer may set one the picker does not show. A name the
//! table does not know falls back to the node type's icon.

use rinch_tabler_icons::{TablerIcon, ALL_ICONS};

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

/// The colors the picker offers: a display name and its CSS value. Any `#rrggbb` is
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

/// The row colors the picker offers. Soft ones, as Scrivener's binder labels
/// usually are: a row is tinted behind its text, which stays the theme's
/// color. Any `#rrggbb` is accepted, as for [`COLOR_CHOICES`].
pub const ROW_COLOR_CHOICES: &[(&str, &str)] = &[
    ("Red", "#ffd6d2"),
    ("Orange", "#ffdcb8"),
    ("Yellow", "#fff3a8"),
    ("Green", "#a2ffb5"),
    ("Teal", "#b3f0e6"),
    ("Blue", "#b0d7ff"),
    ("Indigo", "#cfc8ff"),
    ("Purple", "#dfb7ff"),
    ("Pink", "#ffc8e0"),
    ("Grey", "#d9dae0"),
];

/// The icon stored under `name`: one of the picker's, or any icon in the Tabler set
/// (the picker's search offers all of them, and an import may name one).
pub fn icon_by_name(name: &str) -> Option<TablerIcon> {
    ICON_CHOICES
        .iter()
        .copied()
        .find(|icon| icon.name() == name)
        .or_else(|| ALL_ICONS.iter().copied().find(|icon| icon.name() == name))
}

/// How many icons the picker's search shows at most.
pub const SEARCH_LIMIT: usize = 60;

/// The icons the picker shows for `query`: the curated set when it is empty,
/// otherwise every Tabler icon whose name contains it, exact and prefix matches
/// first, capped at [`SEARCH_LIMIT`].
pub fn icons_matching(query: &str) -> Vec<TablerIcon> {
    let q = query.trim().to_ascii_lowercase().replace(' ', "-");
    if q.is_empty() {
        return ICON_CHOICES.to_vec();
    }
    let mut hits: Vec<(u8, TablerIcon)> = ALL_ICONS
        .iter()
        .copied()
        .filter_map(|icon| {
            let name = icon.name();
            let rank = if name == q {
                0
            } else if name.starts_with(&q) {
                1
            } else if name.contains(&q) {
                2
            } else {
                return None;
            };
            Some((rank, icon))
        })
        .collect();
    hits.sort_by(|a, b| (a.0, a.1.name()).cmp(&(b.0, b.1.name())));
    hits.into_iter().map(|(_, icon)| icon).take(SEARCH_LIMIT).collect()
}

// The glyph and the colors as drawn on the current theme are pimble-ui's,
// shared with plugins so a card in a plugin's view looks as the tree draws it.
pub use pimble_ui::appearance::{display_color, gathered_violet, row_background, IconGlyph};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_tabler_icon_resolves_by_name_and_search_ranks_prefixes_first() {
        assert!(icon_by_name("brand-github").is_some(), "outside the curated table");
        assert!(icon_by_name("no-such-icon").is_none());
        let hits = icons_matching("star");
        assert_eq!(hits.first().map(|i| i.name()), Some("star"));
        assert!(hits.iter().all(|i| i.name().contains("star")));
        assert!(hits.len() <= SEARCH_LIMIT);
        assert_eq!(icons_matching("").len(), ICON_CHOICES.len());
    }

    #[test]
    fn every_picker_icon_round_trips_through_its_name() {
        for icon in ICON_CHOICES {
            assert_eq!(icon_by_name(icon.name()), Some(*icon));
        }
    }
}

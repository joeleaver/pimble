//! Colors in a document theme (docs/THEMES_CONTRACT.md section 2.1): a color
//! is `#rrggbb`, always given as a pair (one for the light scheme, one for
//! the dark), and a block's color either names a palette slot or gives a pair
//! of its own.

use std::fmt;

/// A color, `#rrggbb`. Only ever written back out by [`Rgb::css`], so nothing
/// a theme holds reaches a stylesheet as typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `#rrggbb` (either case), or `None`.
    pub fn parse(text: &str) -> Option<Rgb> {
        let hex = text.strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
        Some(Rgb(byte(0)?, byte(2)?, byte(4)?))
    }

    /// The color as CSS: `#rrggbb`, lower case.
    pub fn css(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.css())
    }
}

/// A color for each scheme (decision 7: every color is a pair).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pair {
    pub light: Rgb,
    pub dark: Rgb,
}

impl Pair {
    /// The half drawn in this scheme.
    pub fn for_scheme(self, dark: bool) -> Rgb {
        if dark {
            self.dark
        } else {
            self.light
        }
    }
}

/// The palette's named slots (section 2.1, "The palette").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    /// The page behind the text.
    Page,
    /// Body text, and headings without their own color.
    Text,
    /// h5/h6, a quote's text when its style dims it.
    Dimmed,
    /// Links, hashtags, mentions, list markers without their own color.
    Accent,
    /// Horizontal rules, table lines, a quote's bar.
    Rule,
    /// Code blocks and inline code.
    CodeBg,
    /// The highlight mark's default.
    Highlight,
}

impl Slot {
    /// Every slot, in the order the palette lists them.
    pub const ALL: [Slot; 7] = [
        Slot::Page,
        Slot::Text,
        Slot::Dimmed,
        Slot::Accent,
        Slot::Rule,
        Slot::CodeBg,
        Slot::Highlight,
    ];

    /// The slot's name, as a theme and a block's color name it.
    pub fn name(self) -> &'static str {
        match self {
            Slot::Page => "page",
            Slot::Text => "text",
            Slot::Dimmed => "dimmed",
            Slot::Accent => "accent",
            Slot::Rule => "rule",
            Slot::CodeBg => "code_bg",
            Slot::Highlight => "highlight",
        }
    }

    /// The slot named `name`.
    pub fn named(name: &str) -> Option<Slot> {
        Slot::ALL.into_iter().find(|slot| slot.name() == name)
    }

    /// The palette token that sets this slot (`palette.<name>`).
    pub fn token(self) -> &'static str {
        match self {
            Slot::Page => "palette.page",
            Slot::Text => "palette.text",
            Slot::Dimmed => "palette.dimmed",
            Slot::Accent => "palette.accent",
            Slot::Rule => "palette.rule",
            Slot::CodeBg => "palette.code_bg",
            Slot::Highlight => "palette.highlight",
        }
    }

    /// What a slot no theme in the chain sets is drawn with: the app theme's
    /// own value for the scheme, exactly what the editor drew before themes
    /// (section 2.1: "where no theme in the chain sets it, from the app
    /// theme"). Pimble's text, never a theme's.
    pub fn app_value(self, dark: bool) -> &'static str {
        match (self, dark) {
            (Slot::Page, true) => "var(--rinch-color-body)",
            (Slot::Page, false) => "var(--rinch-color-surface)",
            (Slot::Text, _) => "var(--rinch-color-text)",
            (Slot::Dimmed, _) => "var(--rinch-color-dimmed)",
            (Slot::Accent, true) => "var(--rinch-primary-color-4)",
            (Slot::Accent, false) => "var(--rinch-primary-color-6)",
            (Slot::Rule, _) => "var(--rinch-color-border)",
            (Slot::CodeBg, true) => "var(--rinch-color-option-hover)",
            (Slot::CodeBg, false) => "#eef1f4",
            (Slot::Highlight, true) => "#bb8009",
            (Slot::Highlight, false) => "#fff3a3",
        }
    }
}

/// A block's color: a palette slot by name, or a pair of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorRef {
    Slot(Slot),
    Pair(Pair),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_and_print_as_css() {
        assert_eq!(Rgb::parse("#3F6491"), Some(Rgb(0x3f, 0x64, 0x91)));
        assert_eq!(Rgb::parse("#3f6491").unwrap().css(), "#3f6491");
        for bad in [
            "3f6491", "#3f649", "#3f64911", "#3g6491", "", "#", "red", "#3f6491;", "#+f6491",
        ] {
            assert_eq!(Rgb::parse(bad), None, "{bad:?} is no color");
        }
    }

    #[test]
    fn every_slot_is_named_once() {
        for slot in Slot::ALL {
            assert_eq!(Slot::named(slot.name()), Some(slot));
            assert_eq!(slot.token(), format!("palette.{}", slot.name()));
        }
        assert_eq!(Slot::named("background"), None);
    }
}

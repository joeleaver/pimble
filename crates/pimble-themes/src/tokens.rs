//! The document theme's tokens (docs/THEMES_CONTRACT.md section 2.1): a
//! fixed set, each with a type the app checks every value against, and the
//! value the editor drew before themes ("today"), which is what a token no
//! theme in the chain sets reads as.
//!
//! A token's key is what a theme stores (`body.size`); its CSS custom
//! property is derived from the key ([`var_name`]).

/// A length's unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Px,
    Em,
}

impl Unit {
    pub fn suffix(self) -> &'static str {
        match self {
            Unit::Px => "px",
            Unit::Em => "em",
        }
    }
}

/// What a token holds, and the range a value must be in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TokenType {
    /// A pair of `#rrggbb`, one for each scheme (the palette's slots).
    Color,
    /// A palette slot by name, or a pair of its own (a block's color).
    ColorRef,
    /// A family of the font catalog, by id.
    Font,
    /// A number and a unit, the unit fixed per token.
    Length {
        unit: Unit,
        min: f32,
        max: f32,
    },
    /// A plain number (a line height).
    Number {
        min: f32,
        max: f32,
    },
    /// A size relative to the text around it (a heading's size is a ratio of
    /// the body's).
    Ratio {
        min: f32,
        max: f32,
    },
    /// A maximum line length: `none`, or a length in `em`.
    Measure {
        min: f32,
        max: f32,
    },
    /// One of a fixed set of words.
    Enum(&'static [&'static str]),
    Bool,
}

/// One token of a document theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Token {
    /// What a theme stores the token under.
    pub key: &'static str,
    pub ty: TokenType,
    /// The value the editor drew before themes, in a theme's stored form
    /// (`"15px"`, `"1.7"`, `"compact"`); empty where the token is optional
    /// and today nothing sets it (a block's color, a quote's own font).
    pub today: &'static str,
}

const WEIGHTS: &[&str] = &["normal", "medium", "semibold", "bold"];
const CASES: &[&str] = &["none", "upper"];
const HEADING_ALIGNS: &[&str] = &["start", "center"];

const fn length(unit: Unit, min: f32, max: f32) -> TokenType {
    TokenType::Length { unit, min, max }
}

const fn token(key: &'static str, ty: TokenType, today: &'static str) -> Token {
    Token { key, ty, today }
}

/// The ten tokens of one heading level, with the level's look before themes
/// (rinch's editor sheet).
macro_rules! heading {
    ($h:literal, $size:literal, $weight:literal, $before:literal, $after:literal) => {
        [
            token(concat!($h, ".font"), TokenType::Font, ""),
            token(
                concat!($h, ".size"),
                TokenType::Ratio { min: 0.5, max: 4.0 },
                $size,
            ),
            token(concat!($h, ".weight"), TokenType::Enum(WEIGHTS), $weight),
            token(concat!($h, ".case"), TokenType::Enum(CASES), "none"),
            token(concat!($h, ".tracking"), length(Unit::Em, -0.05, 0.5), ""),
            token(
                concat!($h, ".align"),
                TokenType::Enum(HEADING_ALIGNS),
                "start",
            ),
            token(
                concat!($h, ".space_before"),
                length(Unit::Em, 0.0, 4.0),
                $before,
            ),
            token(
                concat!($h, ".space_after"),
                length(Unit::Em, 0.0, 4.0),
                $after,
            ),
            token(concat!($h, ".color"), TokenType::ColorRef, ""),
            token(concat!($h, ".rule_below"), TokenType::Bool, "false"),
        ]
    };
}

const PAGE_AND_BODY: [Token; 7] = [
    // Wider than the contract's 40-100 em: its own Book (36 em) and Journal
    // (34 em) are narrower than 40.
    token(
        "page.measure",
        TokenType::Measure {
            min: 30.0,
            max: 100.0,
        },
        "none",
    ),
    token("page.padding", length(Unit::Px, 0.0, 160.0), "20px"),
    token("body.font", TokenType::Font, "system"),
    token("body.size", length(Unit::Px, 12.0, 24.0), "15px"),
    token(
        "body.line_height",
        TokenType::Number { min: 1.0, max: 2.2 },
        "1.7",
    ),
    token("body.letter_spacing", length(Unit::Em, -0.05, 0.2), ""),
    token(
        "body.align",
        TokenType::Enum(&["start", "justify"]),
        "start",
    ),
];

const PARAGRAPH: [Token; 4] = [
    token(
        "paragraph.spacing",
        TokenType::Enum(&["compact", "spaced", "indented"]),
        "compact",
    ),
    token("paragraph.indent", length(Unit::Em, 0.0, 4.0), "1.5em"),
    token("paragraph.gap", length(Unit::Em, 0.0, 3.0), "0.75em"),
    // What every level's font is when the level names none; the body's
    // when this is unset too.
    token("headings.font", TokenType::Font, ""),
];

const HEADINGS: [[Token; 10]; 6] = [
    heading!("h1", "2", "bold", "0.5em", "0.5em"),
    heading!("h2", "1.5", "bold", "0.7em", "0.4em"),
    heading!("h3", "1.25", "semibold", "0.7em", "0.3em"),
    heading!("h4", "1.05", "semibold", "0.7em", "0.3em"),
    heading!("h5", "0.95", "semibold", "0.7em", "0.3em"),
    heading!("h6", "0.9", "semibold", "0.7em", "0.3em"),
];

const BLOCKS: [Token; 13] = [
    token(
        "quote.style",
        TokenType::Enum(&["bar", "indent", "italic", "boxed"]),
        "bar",
    ),
    token("quote.font", TokenType::Font, ""),
    token("quote.color", TokenType::ColorRef, ""),
    token("code.font", TokenType::Font, ""),
    token("code.size", TokenType::Ratio { min: 0.6, max: 1.4 }, "0.9"),
    token("code.background", TokenType::ColorRef, ""),
    token("code.wrap", TokenType::Bool, "true"),
    token("lists.marker_color", TokenType::ColorRef, ""),
    token("lists.item_spacing", length(Unit::Em, 0.0, 1.0), "0.15em"),
    token(
        "tables.style",
        TokenType::Enum(&["grid", "rows", "minimal"]),
        "grid",
    ),
    token(
        "tables.header",
        TokenType::Enum(&["bold", "shaded"]),
        "shaded",
    ),
    token("tables.font", TokenType::Font, ""),
    token("tables.size", TokenType::Ratio { min: 0.6, max: 1.4 }, "1"),
];

const RULE_AND_PALETTE: [Token; 8] = [
    token(
        "rule.style",
        TokenType::Enum(&["line", "ornament", "space"]),
        "line",
    ),
    token("palette.page", TokenType::Color, ""),
    token("palette.text", TokenType::Color, ""),
    token("palette.dimmed", TokenType::Color, ""),
    token("palette.accent", TokenType::Color, ""),
    token("palette.rule", TokenType::Color, ""),
    token("palette.code_bg", TokenType::Color, ""),
    token("palette.highlight", TokenType::Color, ""),
];

/// Every token, in the order the theme editor groups them.
pub fn all() -> impl Iterator<Item = &'static Token> {
    PAGE_AND_BODY
        .iter()
        .chain(PARAGRAPH.iter())
        .chain(HEADINGS.iter().flatten())
        .chain(BLOCKS.iter())
        .chain(RULE_AND_PALETTE.iter())
}

/// The token stored under `key`.
pub fn token_of(key: &str) -> Option<&'static Token> {
    all().find(|token| token.key == key)
}

/// The heading levels' key prefixes, h1 first.
pub const LEVELS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];

/// The CSS custom property a token's value is written as: `--pd-` and the
/// key with its dots and underscores as dashes (`body.line_height` is
/// `--pd-body-line-height`). A palette slot is `--pd-<slot>` (`--pd-accent`).
pub fn var_name(key: &str) -> String {
    let name = key.strip_prefix("palette.").unwrap_or(key);
    format!("--pd-{}", name.replace(['.', '_'], "-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_distinct_and_plain() {
        let keys: Vec<_> = all().map(|t| t.key).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "no key twice");
        assert_eq!(keys.len(), 7 + 4 + 60 + 13 + 8);
        for key in keys {
            assert!(
                key.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b'.'
                    || b == b'_'),
                "{key}"
            );
        }
    }

    #[test]
    fn var_names() {
        assert_eq!(var_name("body.line_height"), "--pd-body-line-height");
        assert_eq!(var_name("h3.space_before"), "--pd-h3-space-before");
        assert_eq!(var_name("palette.code_bg"), "--pd-code-bg");
        assert_eq!(token_of("h6.size").unwrap().today, "0.9");
        assert!(token_of("h7.size").is_none());
    }
}

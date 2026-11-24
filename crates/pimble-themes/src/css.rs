//! A folded theme as CSS (docs/THEMES_CONTRACT.md sections 2 and 7): custom
//! properties for the element the theme applies to, and the structural
//! modifiers for the enum tokens that change which rules apply rather than a
//! value.
//!
//! Only tokens the theme (or a base) sets are written, so Default writes
//! nothing and the app's stylesheet draws today's look from its `var()`
//! fallbacks. Every value is written by this module from a checked
//! [`Value`], never copied from what a theme stores, so nothing a theme holds
//! can end a declaration or open a rule.

use crate::color::{ColorRef, Slot};
use crate::theme::Tokens;
use crate::tokens::{self, TokenType};
use crate::value::{number_text, Value};

/// A theme ready to draw in one scheme.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drawn {
    /// Declarations of `--pd-*` custom properties, each ending in `;`, for an
    /// inline `style`.
    pub style: String,
    /// The structural modifiers in force (`para-indented`, `quote-boxed`,
    /// ...), which the app adds to the element's classes as
    /// `<block>--<modifier>`.
    pub modifiers: Vec<&'static str>,
}

/// Every modifier [`draw`] may answer, for the stylesheet's tests.
pub const MODIFIERS: &[&str] = &[
    "measured",
    "para-spaced",
    "para-indented",
    "quote-indent",
    "quote-italic",
    "quote-boxed",
    "table-rows",
    "table-minimal",
    "table-header-bold",
    "rule-ornament",
    "rule-space",
    "code-unwrapped",
    "marker-color",
];

/// The modifier an enum token's word draws with, if it is not the look
/// before themes.
fn modifier(key: &str, word: &str) -> Option<&'static str> {
    Some(match (key, word) {
        ("paragraph.spacing", "spaced") => "para-spaced",
        ("paragraph.spacing", "indented") => "para-indented",
        ("quote.style", "indent") => "quote-indent",
        ("quote.style", "italic") => "quote-italic",
        ("quote.style", "boxed") => "quote-boxed",
        ("tables.style", "rows") => "table-rows",
        ("tables.style", "minimal") => "table-minimal",
        ("tables.header", "bold") => "table-header-bold",
        ("rule.style", "ornament") => "rule-ornament",
        ("rule.style", "space") => "rule-space",
        _ => return None,
    })
}

/// An enum word as the CSS value its custom property carries, for the enum
/// tokens drawn as values rather than modifiers.
fn word_value(word: &str) -> Option<&'static str> {
    Some(match word {
        "normal" => "400",
        "medium" => "500",
        "semibold" => "600",
        "bold" => "700",
        "none" => "none",
        "upper" => "uppercase",
        "start" => "start",
        "center" => "center",
        "justify" => "justify",
        _ => return None,
    })
}

/// What a slot is drawn with in this scheme: the theme's pair if the chain
/// sets the slot, else the app theme's own value.
fn slot_value(tokens: &Tokens, slot: Slot, dark: bool) -> String {
    match tokens.get(slot.token()) {
        Some(Value::Color(pair)) => pair.for_scheme(dark).css(),
        _ => slot.app_value(dark).to_string(),
    }
}

fn color_value(tokens: &Tokens, color: &ColorRef, dark: bool) -> String {
    match color {
        ColorRef::Slot(slot) => slot_value(tokens, *slot, dark),
        ColorRef::Pair(pair) => pair.for_scheme(dark).css(),
    }
}

/// Draws the folded `tokens` in the dark scheme or the light.
pub fn draw(tokens: &Tokens, dark: bool) -> Drawn {
    let mut drawn = Drawn::default();
    let mut declare = |key: &str, value: String| {
        drawn.style.push_str(&tokens::var_name(key));
        drawn.style.push_str(": ");
        drawn.style.push_str(&value);
        drawn.style.push(';');
    };
    let mut modifiers = Vec::new();
    // In the token table's order, so the same theme always writes the same
    // style (rinch compares it before restyling).
    for token in tokens::all() {
        let Some(value) = tokens.get(token.key) else {
            continue;
        };
        match (token.ty, value) {
            (_, Value::Color(pair)) => declare(token.key, pair.for_scheme(dark).css()),
            (_, Value::ColorRef(color)) => {
                if token.key == "lists.marker_color" {
                    modifiers.push("marker-color");
                }
                declare(token.key, color_value(tokens, color, dark));
            }
            (_, Value::Font(family)) => declare(token.key, family.css_stack()),
            (TokenType::Length { unit, .. }, Value::Length(n)) => {
                declare(token.key, format!("{}{}", number_text(*n), unit.suffix()))
            }
            (TokenType::Ratio { .. }, Value::Number(n)) => {
                declare(token.key, format!("{}em", number_text(*n)))
            }
            (_, Value::Number(n)) => declare(token.key, number_text(*n)),
            (_, Value::Measure(Some(n))) => {
                modifiers.push("measured");
                declare(token.key, format!("{}em", number_text(*n)));
            }
            (_, Value::Measure(None)) => {}
            (_, Value::Word(word)) => match modifier(token.key, word) {
                Some(class) => modifiers.push(class),
                // The look before themes for a structural token ("compact",
                // "bar", "grid", "shaded", "line") has no value to write.
                None => {
                    if let Some(css) = word_value(word) {
                        declare(token.key, css.to_string());
                    }
                }
            },
            (_, Value::Bool(on)) => match (token.key, on) {
                ("code.wrap", false) => modifiers.push("code-unwrapped"),
                (key, true) if key.ends_with(".rule_below") => declare(
                    key,
                    format!("1px solid {}", slot_value(tokens, Slot::Rule, dark)),
                ),
                _ => {}
            },
            (_, Value::Length(_)) => unreachable!("a length is checked against a length token"),
        }
    }
    drawn.modifiers = modifiers;
    drawn
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::{fold, Builtins};
    use crate::theme::{builtin_ids, Theme};
    use serde_json::json;

    fn declarations(style: &str) -> Vec<(&str, &str)> {
        style
            .split_terminator(';')
            .map(|d| d.split_once(": ").expect("name: value"))
            .collect()
    }

    #[test]
    fn default_draws_nothing_and_document_only_spaces_paragraphs() {
        for dark in [false, true] {
            assert_eq!(
                draw(&fold("builtin:default", &Builtins).tokens, dark),
                Drawn::default()
            );
            let document = draw(&fold("builtin:document", &Builtins).tokens, dark);
            assert_eq!(document.style, "");
            assert_eq!(document.modifiers, ["para-spaced"]);
        }
    }

    /// Whatever a theme holds, what is written is only declarations of
    /// `--pd-*` properties with values that cannot end one or open a rule,
    /// and only known modifiers.
    #[test]
    fn only_custom_property_declarations_are_written() {
        let hostile = json!({
            "body.font": "system; } body { display: none",
            "body.size": "18px;}",
            "h1.color": { "light": "#ff0000", "dark": "#000000;}" },
            "palette.accent": { "light": "#123456", "dark": "#654321" },
            "h1.rule_below": true,
            "lists.marker_color": "accent",
        });
        let (theme, refusals) = Theme::from_json("t", "t", hostile.as_object().unwrap());
        assert_eq!(refusals.len(), 3);
        let mut all = theme.tokens.clone();
        for id in builtin_ids() {
            all.extend(fold(&id, &Builtins).tokens);
        }
        for tokens in [theme.tokens, all] {
            for dark in [false, true] {
                let drawn = draw(&tokens, dark);
                for (name, value) in declarations(&drawn.style) {
                    assert!(
                        name.starts_with("--pd-")
                            && name
                                .bytes()
                                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                        "{name}"
                    );
                    assert!(
                        !value.is_empty()
                            && !value.contains([';', '{', '}', '\\', '<', '>'])
                            && !value.contains("url("),
                        "{name}: {value}"
                    );
                    assert_eq!(value.matches('"').count() % 2, 0, "{name}: {value}");
                }
                assert!(drawn.modifiers.iter().all(|m| MODIFIERS.contains(m)));
            }
        }
    }

    #[test]
    fn colors_draw_the_scheme_s_half_and_slots_fall_back_to_the_app() {
        let json = json!({
            "palette.rule": { "light": "#aaaaaa", "dark": "#222222" },
            "h1.rule_below": true,
            "h2.color": "accent",
            "h3.color": "rule",
            "lists.marker_color": { "light": "#ff0000", "dark": "#00ff00" },
        });
        let tokens = Theme::from_json("t", "t", json.as_object().unwrap())
            .0
            .tokens;
        let light = draw(&tokens, false);
        assert!(light.style.contains("--pd-rule: #aaaaaa;"));
        assert!(light
            .style
            .contains("--pd-h1-rule-below: 1px solid #aaaaaa;"));
        assert!(light
            .style
            .contains("--pd-h2-color: var(--rinch-primary-color-6);"));
        assert!(light.style.contains("--pd-h3-color: #aaaaaa;"));
        assert!(light.style.contains("--pd-lists-marker-color: #ff0000;"));
        assert_eq!(light.modifiers, ["marker-color"]);
        let dark = draw(&tokens, true);
        assert!(dark.style.contains("--pd-rule: #222222;"));
        assert!(dark
            .style
            .contains("--pd-h2-color: var(--rinch-primary-color-4);"));
        assert!(dark.style.contains("--pd-lists-marker-color: #00ff00;"));
    }

    #[test]
    fn book_draws_its_values_and_its_structure() {
        let drawn = draw(&fold("builtin:book", &Builtins).tokens, false);
        let style = declarations(&drawn.style);
        let get = |name: &str| style.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
        assert_eq!(get("--pd-body-font"), Some("\"EB Garamond\", serif"));
        assert_eq!(get("--pd-body-size"), Some("18px"));
        assert_eq!(get("--pd-body-line-height"), Some("1.55"));
        assert_eq!(get("--pd-body-align"), Some("justify"));
        assert_eq!(get("--pd-page-measure"), Some("36em"));
        assert_eq!(get("--pd-h1-size"), Some("1.6em"));
        assert_eq!(get("--pd-h1-case"), Some("uppercase"));
        assert_eq!(get("--pd-h1-weight"), Some("500"));
        assert_eq!(get("--pd-h1-align"), Some("center"));
        assert_eq!(
            get("--pd-paragraph-spacing"),
            None,
            "structure is a modifier, never a value"
        );
        let mut modifiers = drawn.modifiers.clone();
        modifiers.sort();
        assert_eq!(
            modifiers,
            ["measured", "para-indented", "quote-italic", "rule-ornament"]
        );
    }
}

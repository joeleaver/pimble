//! `document_theme.css` against the tokens it reads (docs/THEMES_CONTRACT.md
//! section 14): Default must draw today's look, so every token's value
//! before themes is the fallback of the property that reads it.

use std::collections::BTreeSet;

use pimble_themes::tokens;
use pimble_themes::value::today;
use pimble_themes::{draw, Tokens};

use super::DOCUMENT_THEME_CSS;

/// `DOCUMENT_THEME_CSS` with its comments taken out.
fn rules() -> String {
    let mut out = String::new();
    let mut rest = DOCUMENT_THEME_CSS;
    while let Some(at) = rest.find("/*") {
        out.push_str(&rest[..at]);
        let end = rest[at + 2..].find("*/").expect("every comment closes");
        rest = &rest[at + 2 + end + 2..];
    }
    out.push_str(rest);
    out
}

/// Every token set to its value before themes, drawn: the declarations a
/// theme that changes nothing would write, as `(name, value)`.
fn todays_declarations() -> Vec<(String, String)> {
    let mut set = Tokens::new();
    for token in tokens::all() {
        if let Some(value) = today(token) {
            set.insert(token.key, value);
        }
    }
    let drawn = draw(&set, false);
    assert!(drawn.modifiers.is_empty(), "today's look needs no modifier: {:?}", drawn.modifiers);
    drawn
        .style
        .split_terminator(';')
        .map(|d| {
            let (name, value) = d.split_once(": ").expect("name: value");
            (name.to_string(), value.to_string())
        })
        .collect()
}

#[test]
fn every_value_before_themes_is_the_fallback_of_what_reads_it() {
    let css = rules();
    let declarations = todays_declarations();
    // The values before themes are drawn at all: the body, page, a heading
    // and code each have one.
    for name in ["--pd-body-size", "--pd-body-line-height", "--pd-page-padding", "--pd-h1-size", "--pd-code-size"] {
        assert!(declarations.iter().any(|(n, _)| n == name), "{name} has a value before themes");
    }
    for (name, value) in declarations {
        let read = format!("var({name}, {value}");
        assert!(css.contains(&read), "document_theme.css reads {name} with today's {value} as its fallback");
        // And nowhere with another fallback.
        let mut rest = css.as_str();
        let opening = format!("var({name}, ");
        while let Some(at) = rest.find(&opening) {
            let after = &rest[at + opening.len()..];
            assert!(after.starts_with(&value), "{name} falls back to {value} everywhere, not to {:?}", &after[..after.len().min(30)]);
            rest = after;
        }
    }
}

#[test]
fn every_property_read_is_a_token() {
    let known: BTreeSet<String> = tokens::all().map(|t| tokens::var_name(t.key)).collect();
    let css = rules();
    let mut rest = css.as_str();
    while let Some(at) = rest.find("var(--pd-") {
        let name: String = rest[at + 4..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        assert!(known.contains(&name), "{name} is a token's property");
        rest = &rest[at + 4..];
    }
}

#[test]
fn every_modifier_has_its_rules() {
    let css = rules();
    for modifier in pimble_themes::css::MODIFIERS {
        assert!(
            css.contains(&format!(".pimble-editor__content-wrap--{modifier} ")),
            "the editor draws the {modifier} modifier"
        );
    }
}

#[test]
fn no_fallback_is_a_css_wide_keyword() {
    let css = rules();
    for keyword in ["inherit", "initial", "unset", "revert"] {
        assert!(!css.contains(&format!(", {keyword})")), "no var() falls back to {keyword}");
    }
}

#[test]
fn the_system_and_code_stacks_are_the_editors_own() {
    let css = rules();
    assert!(css.contains(&format!("var(--pd-body-font, {})", pimble_themes::fonts::SYSTEM_STACK)));
    assert!(css.contains(&format!("var(--pd-code-font, {})", pimble_themes::fonts::CODE_STACK)));
}

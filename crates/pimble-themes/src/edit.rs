//! What making and editing a theme needs beyond reading one
//! (docs/THEMES_CONTRACT.md sections 3.1, 6 and 9): whether a base would
//! make a cycle, a theme flattened for a copy into another store, a typed
//! value read from what a person entered, and a value as the theme editor
//! shows it.

use std::collections::HashSet;

use serde_json::{Map, Value as Json};

use crate::color::ColorRef;
use crate::resolve::{fold, ThemeSource};
use crate::theme::{self, Tokens};
use crate::tokens::{Token, TokenType, Unit};
use crate::value::{self, number_text, Refusal, Value};

/// Whether making `base` the base of theme `id` would make a cycle: `base`
/// is `id`, or `id` is somewhere on `base`'s chain. Refused when it is made
/// (section 3.1); only concurrent edits can make one, and `fold` breaks it.
pub fn would_cycle(id: &str, base: &str, source: &impl ThemeSource) -> bool {
    let mut seen = HashSet::new();
    let mut at = Some(base.to_string());
    while let Some(next) = at.take() {
        if next == id {
            return true;
        }
        if !seen.insert(next.clone()) {
            return false;
        }
        at = source.theme(&next).and_then(|theme| theme.base);
    }
    false
}

/// Theme `id` as a copy into another store carries it (section 6): a base
/// that is a built-in carries over by id, and every theme of the source
/// store between it and that built-in is folded into the copy's own tokens,
/// since a base in another store is never named. A chain that ends at no
/// built-in (a missing base, a cycle broken) has no base: Default.
pub fn flatten(id: &str, source: &impl ThemeSource) -> (Option<String>, Tokens) {
    let folded = fold(id, source);
    let mut tokens = Tokens::new();
    let mut base = None;
    for link in &folded.chain {
        if link.starts_with(theme::BUILTIN_PREFIX) {
            base = Some(link.clone());
            break;
        }
        if let Some(theme) = source.theme(link) {
            for (key, value) in theme.tokens {
                tokens.entry(key).or_insert(value);
            }
        }
    }
    (base, tokens)
}

/// A theme's stored form for its node's `data.theme`: its base, the tokens
/// it sets and, for a copy, where it was copied from.
pub fn stored(base: Option<&str>, tokens: &Tokens, copied_from: Option<Json>) -> Map<String, Json> {
    let mut map = Map::new();
    if let Some(base) = base {
        map.insert(theme::BASE.into(), Json::from(base));
    }
    for (key, value) in tokens {
        if let Some(token) = crate::tokens::token_of(key) {
            map.insert((*key).to_string(), value::to_json(token, value));
        }
    }
    if let Some(from) = copied_from {
        map.insert(theme::COPIED_FROM.into(), from);
    }
    map
}

/// What a person typed for `token` (a length, a number, a measure), as the
/// JSON a theme stores, checked. A length may be typed without its unit
/// (`17` for `17px`), and a measure as `none` or a number of `em`. The other
/// types are chosen from lists, never typed, and come here as their stored
/// form already.
pub fn typed(token: &Token, text: &str) -> Result<Json, Refusal> {
    let text = text.trim();
    let json = match token.ty {
        TokenType::Length { unit, .. } => Json::from(with_unit(text, unit)),
        TokenType::Measure { .. } if text.eq_ignore_ascii_case("none") => Json::from("none"),
        TokenType::Measure { .. } => Json::from(with_unit(text, Unit::Em)),
        TokenType::Number { .. } | TokenType::Ratio { .. } => match text.parse::<f64>() {
            Ok(n) if n.is_finite() => Json::from(n),
            _ => {
                return Err(Refusal { key: token.key.to_string(), reason: format!("{text:?} is not a number.") });
            }
        },
        _ => Json::from(text),
    };
    let checked = value::check(token, &json)?;
    Ok(value::to_json(token, &checked))
}

/// `text` with `unit` added when it is a bare number.
fn with_unit(text: &str, unit: Unit) -> String {
    if text.ends_with(unit.suffix()) {
        text.to_string()
    } else {
        format!("{text}{}", unit.suffix())
    }
}

/// A checked value as the theme editor shows it: `17px`, `1.55`, `36em`,
/// `none`, a word, a family's name, `on`/`off`, a slot's name, or a pair as
/// `#light / #dark`.
pub fn shown(token: &Token, value: &Value) -> String {
    match (token.ty, value) {
        (TokenType::Length { unit, .. }, Value::Length(n)) => format!("{}{}", number_text(*n), unit.suffix()),
        (_, Value::Length(n)) | (_, Value::Number(n)) => number_text(*n),
        (_, Value::Measure(None)) => "none".to_string(),
        (_, Value::Measure(Some(n))) => format!("{}em", number_text(*n)),
        (_, Value::Word(word)) => (*word).to_string(),
        (_, Value::Font(family)) => family.name.to_string(),
        (_, Value::Bool(on)) => if *on { "on" } else { "off" }.to_string(),
        (_, Value::ColorRef(ColorRef::Slot(slot))) => slot.name().to_string(),
        (_, Value::Color(pair)) | (_, Value::ColorRef(ColorRef::Pair(pair))) => format!("{} / {}", pair.light, pair.dark),
    }
}

/// The value a token reads as where no theme of the chain sets it: the look
/// before themes, or nothing for an optional token nothing sets.
pub fn unset(token: &Token) -> Option<Value> {
    value::today(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::WithStore;
    use crate::theme::Theme;
    use crate::tokens::token_of;
    use serde_json::json;

    fn themes(list: &[(&str, Json)]) -> Vec<Theme> {
        list.iter().map(|(id, json)| Theme::from_json(id, id, json.as_object().unwrap()).0).collect()
    }

    #[test]
    fn a_base_that_leads_back_is_a_cycle() {
        let list = themes(&[
            ("a", json!({ "base": "b" })),
            ("b", json!({ "base": "builtin:book" })),
            ("c", json!({})),
        ]);
        let source = WithStore(&list);
        assert!(would_cycle("b", "a", &source), "a is based on b");
        assert!(would_cycle("a", "a", &source), "a theme is not its own base");
        assert!(!would_cycle("c", "a", &source));
        assert!(!would_cycle("a", "builtin:book", &source));
        assert!(!would_cycle("a", "gone", &source));
        let looped = themes(&[("x", json!({ "base": "y" })), ("y", json!({ "base": "x" }))]);
        assert!(!would_cycle("z", "x", &WithStore(&looped)), "an existing loop ends the walk");
    }

    #[test]
    fn a_copy_keeps_a_builtin_base_and_folds_the_rest() {
        let list = themes(&[
            ("mine", json!({ "base": "tweak", "body.size": "17px" })),
            ("tweak", json!({ "base": "builtin:book", "body.size": "20px", "page.measure": "44em" })),
            ("lost", json!({ "base": "gone", "body.size": "13px" })),
        ]);
        let source = WithStore(&list);
        let (base, tokens) = flatten("mine", &source);
        assert_eq!(base.as_deref(), Some("builtin:book"));
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens["body.size"], Value::Length(17.0));
        assert_eq!(tokens["page.measure"], Value::Measure(Some(44.0)));
        let (base, tokens) = flatten("lost", &source);
        assert_eq!(base, None);
        assert_eq!(tokens.len(), 1);
        let map = stored(Some("builtin:book"), &flatten("mine", &source).1, Some(json!({ "store": "s" })));
        let (back, refusals) = Theme::from_json("copy", "Copy", &map);
        assert!(refusals.is_empty());
        assert_eq!(back.base.as_deref(), Some("builtin:book"));
        assert_eq!(back.tokens.len(), 2);
        assert_eq!(map["copied_from"], json!({ "store": "s" }));
    }

    #[test]
    fn typed_values_take_their_unit_and_are_checked() {
        let size = token_of("body.size").unwrap();
        assert_eq!(typed(size, "17"), Ok(json!("17px")));
        assert_eq!(typed(size, " 18px "), Ok(json!("18px")));
        assert!(typed(size, "40").is_err());
        assert!(typed(size, "big").is_err());
        let measure = token_of("page.measure").unwrap();
        assert_eq!(typed(measure, "None"), Ok(json!("none")));
        assert_eq!(typed(measure, "40"), Ok(json!("40em")));
        assert!(typed(measure, "10").is_err());
        let line = token_of("body.line_height").unwrap();
        assert_eq!(typed(line, "1.5"), Ok(json!(1.5)));
        assert!(typed(line, "NaN").is_err());
        assert!(typed(line, "9").is_err());
    }

    #[test]
    fn values_show_as_people_read_them() {
        let check = |key: &str, json: Json| {
            let token = token_of(key).unwrap();
            shown(token, &value::check(token, &json).unwrap())
        };
        assert_eq!(check("body.size", json!("17px")), "17px");
        assert_eq!(check("body.line_height", json!(1.55)), "1.55");
        assert_eq!(check("page.measure", json!("none")), "none");
        assert_eq!(check("body.font", json!("eb-garamond")), "EB Garamond");
        assert_eq!(check("code.wrap", json!(false)), "off");
        assert_eq!(check("h1.color", json!("accent")), "accent");
        assert_eq!(check("palette.page", json!({ "light": "#FFFFFF", "dark": "#000000" })), "#ffffff / #000000");
        assert_eq!(unset(token_of("body.size").unwrap()), Some(Value::Length(15.0)));
        assert_eq!(unset(token_of("h1.color").unwrap()), None);
    }
}

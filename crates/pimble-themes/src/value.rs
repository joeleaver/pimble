//! A token's value, checked against the token's type (docs/THEMES_CONTRACT.md
//! section 2): what a theme stores is JSON, and nothing reaches the screen
//! until it has been read into a [`Value`] here. A value that fails is
//! refused with a sentence naming the token and why, and the base's value
//! stands in.
//!
//! The stored forms:
//!
//! | Type | Stored as |
//! | --- | --- |
//! | color | `{ "light": "#rrggbb", "dark": "#rrggbb" }` |
//! | color reference | a slot's name (`"accent"`) or a color's pair |
//! | font | a family's id (`"eb-garamond"`) |
//! | length | a number and the token's unit (`"18px"`, `"1.5em"`) |
//! | number, ratio | a number (`1.55`) |
//! | measure | `"none"` or a length in `em` (`"36em"`) |
//! | enum | one of the token's words (`"indented"`) |
//! | bool | `true` or `false` |

use std::fmt;

use serde_json::Value as Json;

use crate::color::{ColorRef, Pair, Rgb, Slot};
use crate::fonts::{self, Family};
use crate::tokens::{Token, TokenType, Unit};

/// A checked token value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Color(Pair),
    ColorRef(ColorRef),
    Font(&'static Family),
    /// In the token's own unit.
    Length(f32),
    /// A number or a ratio.
    Number(f32),
    /// A maximum line length in `em`, or none.
    Measure(Option<f32>),
    /// One of the token's words.
    Word(&'static str),
    Bool(bool),
}

/// Why a stored value was not taken: a whole sentence, which the theme
/// editor shows beside the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub key: String,
    pub reason: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.reason)
    }
}

/// A number as a theme and a stylesheet write it: no trailing zeros
/// (`1.5`, `18`).
pub fn number_text(n: f32) -> String {
    format!("{n}")
}

fn range_text(min: f32, max: f32, unit: &str) -> String {
    format!(
        "between {}{unit} and {}{unit}",
        number_text(min),
        number_text(max)
    )
}

fn in_range(n: f32, min: f32, max: f32) -> bool {
    n.is_finite() && n >= min && n <= max
}

/// `"18px"` in `unit`, or `None`.
fn parse_length(text: &str, unit: Unit) -> Option<f32> {
    let number = text.strip_suffix(unit.suffix())?;
    // Only digits, one point and a leading minus: never an exponent, `inf`
    // or `NaN`, which `f32::from_str` would take.
    let digits = number.strip_prefix('-').unwrap_or(number);
    if digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || digits.matches('.').count() > 1
    {
        return None;
    }
    number.parse().ok()
}

fn parse_pair(json: &Json) -> Result<Pair, String> {
    let half = |name: &str| -> Result<Rgb, String> {
        let text = json
            .get(name)
            .and_then(Json::as_str)
            .ok_or_else(|| format!("a color is a pair: it needs its {name} half as #rrggbb."))?;
        Rgb::parse(text).ok_or_else(|| format!("{text:?} is not a color; write it as #rrggbb."))
    };
    if !json.is_object() {
        return Err("a color is a pair, one #rrggbb for light and one for dark.".into());
    }
    Ok(Pair {
        light: half("light")?,
        dark: half("dark")?,
    })
}

/// Reads `json` as a value of `token`, or says why not.
pub fn check(token: &Token, json: &Json) -> Result<Value, Refusal> {
    check_type(token.ty, json).map_err(|reason| Refusal {
        key: token.key.to_string(),
        reason,
    })
}

fn check_type(ty: TokenType, json: &Json) -> Result<Value, String> {
    match ty {
        TokenType::Color => parse_pair(json).map(Value::Color),
        TokenType::ColorRef => match json.as_str() {
            Some(name) => Slot::named(name)
                .map(|slot| Value::ColorRef(ColorRef::Slot(slot)))
                .ok_or_else(|| {
                    format!(
                        "{name:?} is not a palette slot; it is one of {}, or a color's pair.",
                        slot_list()
                    )
                }),
            None => parse_pair(json).map(|pair| Value::ColorRef(ColorRef::Pair(pair))),
        },
        TokenType::Font => {
            let id = json.as_str().ok_or("a font is named by its family.")?;
            fonts::family(id)
                .map(Value::Font)
                .ok_or_else(|| format!("{id:?} is not one of Pimble's fonts."))
        }
        TokenType::Length { unit, min, max } => {
            let text = json.as_str().ok_or_else(|| {
                format!(
                    "a length is a number and {}, such as \"{}{}\".",
                    unit.suffix(),
                    number_text(min.max(1.0)),
                    unit.suffix()
                )
            })?;
            let n = parse_length(text, unit)
                .ok_or_else(|| format!("{text:?} is not a length in {}.", unit.suffix()))?;
            in_range(n, min, max)
                .then_some(Value::Length(n))
                .ok_or_else(|| {
                    format!(
                        "{text} is outside its range: it must be {}.",
                        range_text(min, max, unit.suffix())
                    )
                })
        }
        TokenType::Number { min, max } | TokenType::Ratio { min, max } => {
            let n = json.as_f64().ok_or("this is a number, such as 1.5.")? as f32;
            in_range(n, min, max)
                .then_some(Value::Number(n))
                .ok_or_else(|| {
                    format!(
                        "{} is outside its range: it must be {}.",
                        number_text(n),
                        range_text(min, max, "")
                    )
                })
        }
        TokenType::Measure { min, max } => {
            let text = json
                .as_str()
                .ok_or("a measure is \"none\" or a length in em, such as \"36em\".")?;
            if text == "none" {
                return Ok(Value::Measure(None));
            }
            let n = parse_length(text, Unit::Em)
                .ok_or_else(|| format!("{text:?} is not \"none\" or a length in em."))?;
            in_range(n, min, max)
                .then_some(Value::Measure(Some(n)))
                .ok_or_else(|| {
                    format!(
                        "{text} is outside its range: it must be \"none\" or {}.",
                        range_text(min, max, "em")
                    )
                })
        }
        TokenType::Enum(words) => {
            let text = json
                .as_str()
                .ok_or_else(|| format!("this is one of {}.", words.join(", ")))?;
            words
                .iter()
                .find(|word| **word == text)
                .map(|word| Value::Word(word))
                .ok_or_else(|| format!("{text:?} is not one of {}.", words.join(", ")))
        }
        TokenType::Bool => json
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| "this is true or false.".into()),
    }
}

fn slot_list() -> String {
    Slot::ALL
        .iter()
        .map(|slot| slot.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The value the editor drew before themes, for a token that has one.
pub fn today(token: &Token) -> Option<Value> {
    if token.today.is_empty() {
        return None;
    }
    let json = match token.ty {
        TokenType::Number { .. } | TokenType::Ratio { .. } => {
            Json::from(token.today.parse::<f64>().ok()?)
        }
        TokenType::Bool => Json::Bool(token.today == "true"),
        _ => Json::from(token.today),
    };
    check(token, &json).ok()
}

/// The value as a theme stores it.
pub fn to_json(token: &Token, value: &Value) -> Json {
    let pair =
        |pair: &Pair| serde_json::json!({ "light": pair.light.css(), "dark": pair.dark.css() });
    match (token.ty, value) {
        (_, Value::Color(p)) | (_, Value::ColorRef(ColorRef::Pair(p))) => pair(p),
        (_, Value::ColorRef(ColorRef::Slot(slot))) => Json::from(slot.name()),
        (_, Value::Font(family)) => Json::from(family.id),
        (TokenType::Length { unit, .. }, Value::Length(n)) => {
            Json::from(format!("{}{}", number_text(*n), unit.suffix()))
        }
        (_, Value::Length(n)) | (_, Value::Number(n)) => Json::from(*n as f64),
        (_, Value::Measure(None)) => Json::from("none"),
        (_, Value::Measure(Some(n))) => Json::from(format!("{}em", number_text(*n))),
        (_, Value::Word(word)) => Json::from(*word),
        (_, Value::Bool(b)) => Json::Bool(*b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::{all, token_of};
    use serde_json::json;

    fn checked(key: &str, json: Json) -> Result<Value, String> {
        check(token_of(key).unwrap(), &json).map_err(|r| r.to_string())
    }

    #[test]
    fn every_today_value_checks_and_round_trips() {
        for token in all() {
            if token.today.is_empty() {
                continue;
            }
            let value = today(token)
                .unwrap_or_else(|| panic!("{}'s today {:?} checks", token.key, token.today));
            assert_eq!(
                check(token, &to_json(token, &value)),
                Ok(value),
                "{}",
                token.key
            );
        }
    }

    #[test]
    fn lengths_keep_to_their_unit_and_range() {
        assert_eq!(checked("body.size", json!("18px")), Ok(Value::Length(18.0)));
        assert_eq!(checked("body.size", json!("12px")), Ok(Value::Length(12.0)));
        assert_eq!(
            checked("body.size", json!("30px")).unwrap_err(),
            "body.size: 30px is outside its range: it must be between 12px and 24px."
        );
        for bad in [
            json!("18em"),
            json!("18"),
            json!(18),
            json!("1e1px"),
            json!("NaNpx"),
            json!("infpx"),
            json!("18px;color:red"),
            json!("-px"),
            json!("1.2.3px"),
        ] {
            assert!(checked("body.size", bad.clone()).is_err(), "{bad}");
        }
        assert_eq!(
            checked("body.letter_spacing", json!("-0.02em")),
            Ok(Value::Length(-0.02))
        );
    }

    #[test]
    fn numbers_ratios_and_measures() {
        assert_eq!(
            checked("body.line_height", json!(1.55)),
            Ok(Value::Number(1.55))
        );
        assert!(checked("body.line_height", json!(2.5)).is_err());
        assert!(checked("body.line_height", json!("1.5")).is_err());
        assert_eq!(checked("h1.size", json!(1.6)), Ok(Value::Number(1.6)));
        assert_eq!(
            checked("page.measure", json!("none")),
            Ok(Value::Measure(None))
        );
        assert_eq!(
            checked("page.measure", json!("36em")),
            Ok(Value::Measure(Some(36.0)))
        );
        assert!(checked("page.measure", json!("36px")).is_err());
        assert!(checked("page.measure", json!("200em")).is_err());
    }

    #[test]
    fn words_fonts_bools_and_colors() {
        assert_eq!(
            checked("paragraph.spacing", json!("indented")),
            Ok(Value::Word("indented"))
        );
        assert_eq!(
            checked("paragraph.spacing", json!("double")).unwrap_err(),
            "paragraph.spacing: \"double\" is not one of compact, spaced, indented."
        );
        assert!(
            matches!(checked("body.font", json!("eb-garamond")), Ok(Value::Font(f)) if f.name == "EB Garamond")
        );
        assert!(checked("body.font", json!("Comic Sans")).is_err());
        assert!(checked("body.font", json!("url(x)")).is_err());
        assert_eq!(checked("code.wrap", json!(false)), Ok(Value::Bool(false)));
        assert!(checked("code.wrap", json!("false")).is_err());
        let pair = Pair {
            light: Rgb(0xff, 0, 0),
            dark: Rgb(0, 0, 0xff),
        };
        assert_eq!(
            checked(
                "palette.accent",
                json!({"light": "#ff0000", "dark": "#0000ff"})
            ),
            Ok(Value::Color(pair))
        );
        assert!(checked("palette.accent", json!({"light": "#ff0000"})).is_err());
        assert!(
            checked("palette.accent", json!("accent")).is_err(),
            "a palette slot is a pair, never a name"
        );
        assert_eq!(
            checked("h1.color", json!("accent")),
            Ok(Value::ColorRef(ColorRef::Slot(Slot::Accent)))
        );
        assert_eq!(
            checked("h1.color", json!({"light": "#ff0000", "dark": "#0000ff"})),
            Ok(Value::ColorRef(ColorRef::Pair(pair)))
        );
        assert!(checked("h1.color", json!("red")).is_err());
    }
}

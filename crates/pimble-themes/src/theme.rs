//! A document theme: a name, a base, and the tokens it sets
//! (docs/THEMES_CONTRACT.md sections 3.1 and 3.4). A theme sets only what it
//! changes; everything else comes from its base, its base's base, and so on,
//! then Default ([`crate::resolve`]).

use std::collections::BTreeMap;

use serde_json::{json, Map, Value as Json};

use crate::tokens::{self, Token};
use crate::value::{self, Refusal, Value};

/// The prefix of a built-in theme's id.
pub const BUILTIN_PREFIX: &str = "builtin:";

/// Default's id: today's look, and what a node with no theme in force reads
/// as.
pub const DEFAULT: &str = "builtin:default";

/// Document's id: Default with spaced paragraphs, what a note that was made
/// spaced before themes reads as (section 4).
pub const DOCUMENT: &str = "builtin:document";

/// The key a stored theme names its base under.
pub const BASE: &str = "base";

/// The key a copied theme records its original under
/// (docs/THEMES_CONTRACT.md section 6): `{ "store", "node", "at" }`. It is
/// provenance, not a token, and reading a theme passes over it.
pub const COPIED_FROM: &str = "copied_from";

/// The tokens a theme sets, by key, each checked.
pub type Tokens = BTreeMap<&'static str, Value>;

/// One document theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// `builtin:<name>`, or a theme node's id.
    pub id: String,
    /// What the picker shows.
    pub name: String,
    /// The theme this one changes; `None` is Default.
    pub base: Option<String>,
    pub tokens: Tokens,
}

impl Theme {
    /// Reads a theme stored as JSON (`{ "base": ..., "<token>": <value>, ... }`):
    /// every value is checked, and what fails is left out and said, so the
    /// base's value stands in for it.
    pub fn from_json(id: &str, name: &str, json: &Map<String, Json>) -> (Theme, Vec<Refusal>) {
        let mut refusals = Vec::new();
        let mut tokens = Tokens::new();
        let mut base = None;
        for (key, stored) in json {
            if key == COPIED_FROM {
                continue;
            }
            if key == BASE {
                match stored.as_str() {
                    Some(text) if !text.is_empty() => base = Some(text.to_string()),
                    _ => refusals.push(Refusal {
                        key: key.clone(),
                        reason: "a base is a theme's id.".into(),
                    }),
                }
                continue;
            }
            let Some(token) = tokens::token_of(key) else {
                refusals.push(Refusal {
                    key: key.clone(),
                    reason: "this is not a token of a document theme.".into(),
                });
                continue;
            };
            match value::check(token, stored) {
                Ok(value) => {
                    tokens.insert(token.key, value);
                }
                Err(refusal) => refusals.push(refusal),
            }
        }
        (
            Theme {
                id: id.to_string(),
                name: name.to_string(),
                base,
                tokens,
            },
            refusals,
        )
    }

    /// The theme as it is stored.
    pub fn to_json(&self) -> Map<String, Json> {
        let mut map = Map::new();
        if let Some(base) = &self.base {
            map.insert(BASE.into(), Json::from(base.as_str()));
        }
        for (key, value) in &self.tokens {
            let token: &Token = tokens::token_of(key).expect("a theme holds only checked tokens");
            map.insert((*key).to_string(), value::to_json(token, value));
        }
        map
    }

    pub fn is_builtin(&self) -> bool {
        self.id.starts_with(BUILTIN_PREFIX)
    }
}

/// The tokens every level of headings sets to `value`.
fn every_level(map: &mut Map<String, Json>, token: &str, value: Json) {
    for level in tokens::LEVELS {
        map.insert(format!("{level}.{token}"), value.clone());
    }
}

/// The built-ins as stored (section 11), so they are read through the same
/// checks as anyone's theme. They set no colors, so they read in both
/// schemes.
fn builtin_json(name: &str) -> Option<(&'static str, Map<String, Json>)> {
    let object = |value: Json| match value {
        Json::Object(map) => map,
        _ => unreachable!("built-ins are objects"),
    };
    Some(match name {
        "default" => ("Default", Map::new()),
        "document" => ("Document", object(json!({ "paragraph.spacing": "spaced" }))),
        "book" => {
            let mut map = object(json!({
                "body.font": "eb-garamond",
                "body.size": "18px",
                "body.line_height": 1.55,
                "body.align": "justify",
                "page.measure": "36em",
                "paragraph.spacing": "indented",
                "rule.style": "ornament",
                "quote.style": "italic",
                "h1.size": 1.6, "h2.size": 1.3, "h3.size": 1.1,
                "h1.space_before": "1.5em", "h1.space_after": "1em",
                "h2.space_before": "1.2em", "h2.space_after": "0.8em",
                "subtitle.size": 1, "subtitle.space_after": "1.5em",
                "picture.align": "center", "picture.space": "1em",
            }));
            for level in ["h1", "h2", "h3"] {
                map.insert(format!("{level}.align"), json!("center"));
                map.insert(format!("{level}.case"), json!("upper"));
                map.insert(format!("{level}.tracking"), json!("0.08em"));
                map.insert(format!("{level}.weight"), json!("medium"));
            }
            ("Book", map)
        }
        "manuscript" => {
            let mut map = object(json!({
                "body.font": "source-serif-4",
                "body.size": "16px",
                "body.line_height": 2.0,
                "page.padding": "72px",
                "paragraph.spacing": "indented",
                "paragraph.indent": "2em",
                "h1.align": "center",
                "subtitle.size": 1, "subtitle.italic": false,
                "subtitle.space_after": "2em",
                "picture.align": "center", "picture.space": "1em",
            }));
            every_level(&mut map, "size", json!(1));
            every_level(&mut map, "weight", json!("bold"));
            ("Manuscript", map)
        }
        "code" => {
            let mut map = object(json!({
                "body.font": "jetbrains-mono",
                "body.size": "14px",
                "body.line_height": 1.55,
                "code.wrap": false,
                "tables.style": "grid",
                "subtitle.size": 1, "subtitle.italic": false,
                "picture.space": "0.75em",
            }));
            every_level(&mut map, "weight", json!("bold"));
            ("Code", map)
        }
        "markdown" => {
            let mut map = object(json!({
                "body.font": "jetbrains-mono",
                "body.size": "14px",
                "body.line_height": 1.6,
                "code.size": 1,
                "quote.style": "bar",
                "subtitle.size": 1, "subtitle.italic": false,
                "picture.space": "0.75em",
            }));
            every_level(&mut map, "size", json!(1));
            every_level(&mut map, "weight", json!("bold"));
            every_level(&mut map, "space_before", json!("0em"));
            every_level(&mut map, "space_after", json!("0em"));
            ("Markdown", map)
        }
        "report" => (
            "Report",
            object(json!({
            "body.font": "inter",
            "body.size": "15px",
            "body.line_height": 1.6,
            "paragraph.spacing": "spaced",
            "h1.size": 1.7, "h2.size": 1.35, "h3.size": 1.15,
            "h1.weight": "semibold", "h2.weight": "semibold",
            "h1.rule_below": true, "h2.rule_below": true,
            "h1.space_before": "1.2em", "h2.space_before": "1.2em",
            "tables.style": "rows",
            "tables.header": "shaded",
            "subtitle.size": 1.15, "subtitle.italic": false,
            "picture.align": "center", "picture.space": "1em",
            })),
        ),
        "journal" => (
            "Journal",
            object(json!({
            "body.font": "source-serif-4",
            "body.size": "17px",
            "body.line_height": 1.85,
            "page.measure": "34em",
            "paragraph.spacing": "spaced",
            "paragraph.gap": "0.9em",
            "h1.weight": "semibold", "h2.weight": "semibold",
            "picture.align": "center", "picture.space": "1em",
            })),
        ),
        "compact" => {
            let mut map = object(json!({
                "body.font": "inter",
                "body.size": "13px",
                "body.line_height": 1.4,
                "lists.item_spacing": "0em",
                "tables.size": 0.95,
                "subtitle.size": 1, "subtitle.space_after": "0.6em",
                "picture.space": "0.5em",
            }));
            every_level(&mut map, "space_before", json!("0.5em"));
            every_level(&mut map, "space_after", json!("0.2em"));
            ("Compact", map)
        }
        _ => return None,
    })
}

/// The built-ins' names after `builtin:`, in the order the picker lists
/// them.
pub const BUILTIN_NAMES: [&str; 9] = [
    "default",
    "document",
    "book",
    "manuscript",
    "code",
    "markdown",
    "report",
    "journal",
    "compact",
];

/// The built-in theme `id` (`builtin:book`).
pub fn builtin(id: &str) -> Option<Theme> {
    let name = id.strip_prefix(BUILTIN_PREFIX)?;
    let (display, json) = builtin_json(name)?;
    let (theme, refusals) = Theme::from_json(id, display, &json);
    debug_assert!(refusals.is_empty(), "{id}: {refusals:?}");
    Some(theme)
}

/// Every built-in's id, in the picker's order.
pub fn builtin_ids() -> impl Iterator<Item = String> {
    BUILTIN_NAMES
        .iter()
        .map(|name| format!("{BUILTIN_PREFIX}{name}"))
}

/// Every built-in, in the picker's order.
pub fn builtins() -> impl Iterator<Item = Theme> {
    builtin_ids().filter_map(|id| builtin(&id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_checks_cleanly() {
        for name in BUILTIN_NAMES {
            let (display, json) = builtin_json(name).unwrap();
            let (theme, refusals) = Theme::from_json(&format!("builtin:{name}"), display, &json);
            assert!(refusals.is_empty(), "{name}: {refusals:?}");
            assert_eq!(
                theme.tokens.len(),
                json.len(),
                "{name} sets every token it names"
            );
            assert!(theme.base.is_none());
            assert!(
                !theme.tokens.keys().any(|k| k.starts_with("palette.")),
                "{name} sets no colors"
            );
        }
        assert_eq!(builtins().count(), 9);
        assert!(
            builtin(DEFAULT).unwrap().tokens.is_empty(),
            "Default is today's look: it sets nothing"
        );
        assert_eq!(builtin(DOCUMENT).unwrap().tokens.len(), 1);
        assert!(builtin("builtin:nope").is_none());
        assert!(builtin("book").is_none());
    }

    #[test]
    fn a_bad_value_is_left_out_and_said() {
        let json = json!({ "base": "builtin:book", "body.size": "40px", "body.line_height": 1.5, "font.weird": 1, "copied_from": { "store": "s", "node": "n", "at": "t" } });
        let (theme, refusals) = Theme::from_json("t1", "Mine", json.as_object().unwrap());
        assert_eq!(theme.base.as_deref(), Some("builtin:book"));
        assert_eq!(theme.tokens.len(), 1);
        let keys: Vec<_> = refusals.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["body.size", "font.weird"]);
    }

    #[test]
    fn a_theme_round_trips_through_its_stored_form() {
        for theme in builtins() {
            let (back, refusals) = Theme::from_json(&theme.id, &theme.name, &theme.to_json());
            assert!(refusals.is_empty());
            assert_eq!(back, theme);
        }
    }
}

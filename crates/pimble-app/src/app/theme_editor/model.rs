//! What the theme editor's token column shows (docs/THEMES_CONTRACT.md
//! section 9), as plain data so it is tested apart from drawing: the tokens
//! in their groups, each with the value it reads as, whether this theme sets
//! it or where it comes from, the control fitting its type, and a stored
//! value that was refused, said in a sentence.

use serde_json::{Map, Value as Json};

use pimble_themes::edit::{shown, unset};
use pimble_themes::tokens::{all, Token, TokenType};
use pimble_themes::{fold, fonts, ColorRef, Slot, Theme, Value, WithStore};

/// The name an inherited value comes from when no theme of the chain sets
/// it: the look before themes, which is Default's.
pub(crate) const BEFORE_THEMES: &str = "Default";

/// One group of tokens ("Body", "Heading 1").
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Group {
    pub title: String,
    pub lines: Vec<Line>,
}

/// One token's line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Line {
    pub key: &'static str,
    /// What the line is called ("Line height").
    pub label: String,
    /// Whether this theme stores a value for it (a reset is offered).
    pub own: bool,
    /// The value it reads as, as a person reads it ("17px"); empty for an
    /// optional token nothing sets.
    pub value: String,
    /// The theme an inherited value comes from ("Book"); `None` for one
    /// this theme sets.
    pub from: Option<String>,
    pub control: Control,
    /// Why the stored value was not taken, when it was not.
    pub refusal: Option<String>,
}

/// The control a token's type takes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Control {
    /// Typed: a length, a number, a ratio, a measure.
    Text,
    /// One of a list: `(the JSON stored, what the list shows)`, and the
    /// one in force.
    Choice { options: Vec<(Json, String)>, current: Option<Json> },
    /// On or off, and which it is.
    Toggle(bool),
    /// A pair of colors, `#rrggbb` each, as in force ("" when unset).
    Pair { light: String, dark: String },
    /// A palette slot by name, or a pair of its own.
    ColorRef { slot: Option<String>, light: String, dark: String },
}

/// The lines of theme `id` among `themes` (its store's), `stored` being
/// what its node holds. With `only_changed`, only the tokens it sets.
/// `scheme_vars` are the app's color variables of the light and the dark
/// scheme ([`root_vars`]), what a palette slot no theme sets is drawn with.
pub(crate) fn groups(id: &str, themes: &[Theme], stored: &Map<String, Json>, only_changed: bool, scheme_vars: &[String; 2]) -> Vec<Group> {
    let source = WithStore(themes);
    let folded = fold(id, &source);
    let (own, refusals) = Theme::from_json(id, "", stored);
    let name_of = |theme_id: &str| crate::themes::theme_name(themes, theme_id);
    let mut groups: Vec<Group> = Vec::new();
    for token in all() {
        let is_own = stored.contains_key(token.key);
        if only_changed && !is_own {
            continue;
        }
        let (value, from) = match own.tokens.get(token.key) {
            Some(value) => (Some(*value), None),
            None => match folded.tokens.get(token.key) {
                Some(value) => {
                    let from = folded.from.get(token.key).map(|t| name_of(t)).unwrap_or_else(|| BEFORE_THEMES.to_string());
                    (Some(*value), Some(from))
                }
                None => (unset(token), Some(BEFORE_THEMES.to_string())),
            },
        };
        let line = Line {
            key: token.key,
            label: label(token.key),
            own: is_own,
            value: value.map(|v| shown(token, &v)).unwrap_or_default(),
            from: if is_own { None } else { from },
            control: match control(token, value.as_ref()) {
                // A slot's line shows the colors the slot is drawn with,
                // which "Own colors" keeps as the block's own pair.
                Control::ColorRef { slot: Some(slot), .. } => {
                    let (light, dark) = Slot::named(&slot).and_then(|s| slot_colors(s, &folded.tokens, scheme_vars)).unwrap_or_default();
                    Control::ColorRef { slot: Some(slot), light, dark }
                }
                other => other,
            },
            refusal: refusals.iter().find(|r| r.key == token.key).map(|r| r.reason.clone()),
        };
        let title = group_title(token.key);
        match groups.last_mut() {
            Some(group) if group.title == title => group.lines.push(line),
            _ => groups.push(Group { title, lines: vec![line] }),
        }
    }
    groups
}

/// The group a token is listed in, from its key's first part.
pub(crate) fn group_title(key: &str) -> String {
    let prefix = key.split('.').next().unwrap_or(key);
    match prefix {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => format!("Heading {}", &prefix[1..]),
        other => capitalized(other),
    }
}

/// A token's label: its key's last part in words ("line_height" reads
/// "Line height").
pub(crate) fn label(key: &str) -> String {
    capitalized(&key.rsplit('.').next().unwrap_or(key).replace('_', " "))
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn control(token: &Token, value: Option<&Value>) -> Control {
    match token.ty {
        TokenType::Enum(words) => Control::Choice {
            options: words.iter().map(|w| (Json::from(*w), capitalized(w))).collect(),
            current: match value {
                Some(Value::Word(w)) => Some(Json::from(*w)),
                _ => None,
            },
        },
        TokenType::Font => Control::Choice {
            options: fonts::FAMILIES.iter().map(|f| (Json::from(f.id), f.name.to_string())).collect(),
            current: match value {
                Some(Value::Font(f)) => Some(Json::from(f.id)),
                _ => None,
            },
        },
        TokenType::Bool => Control::Toggle(matches!(value, Some(Value::Bool(true)))),
        TokenType::Color => {
            let (light, dark) = match value {
                Some(Value::Color(pair)) => (pair.light.css(), pair.dark.css()),
                _ => Default::default(),
            };
            Control::Pair { light, dark }
        }
        TokenType::ColorRef => match value {
            Some(Value::ColorRef(ColorRef::Slot(slot))) => Control::ColorRef { slot: Some(slot.name().to_string()), light: String::new(), dark: String::new() },
            Some(Value::ColorRef(ColorRef::Pair(pair))) => Control::ColorRef { slot: None, light: pair.light.css(), dark: pair.dark.css() },
            _ => Control::ColorRef { slot: None, light: String::new(), dark: String::new() },
        },
        TokenType::Length { .. } | TokenType::Number { .. } | TokenType::Ratio { .. } | TokenType::Measure { .. } => Control::Text,
    }
}

/// The colors palette slot `slot` is drawn with, light then dark, as
/// `#rrggbb`: the chain's (`tokens`, a theme folded) where it sets the slot,
/// else the app's for each scheme, read from `scheme_vars`. `None` when one
/// of them is not a plain color.
pub(crate) fn slot_colors(slot: Slot, tokens: &pimble_themes::Tokens, scheme_vars: &[String; 2]) -> Option<(String, String)> {
    if let Some(Value::Color(pair)) = tokens.get(slot.token()) {
        return Some((pair.light.css(), pair.dark.css()));
    }
    let app = |dark: bool| -> Option<String> {
        let value = resolved(&scheme_vars[usize::from(dark)], slot.app_value(dark));
        Some(pimble_themes::Rgb::parse(value?.trim())?.css())
    };
    Some((app(false)?, app(true)?))
}

/// `value` with every `var(--name)` it is followed through `vars` (`--a: x;
/// --b: y;`, the scheme's root variables) as the page would: the last
/// declaration of a name wins (Pimble's own light-scheme values follow
/// rinch's), and a variable may name another. `None` when a name is not
/// declared and has no fallback, or the chain does not end.
fn resolved(vars: &str, value: &str) -> Option<String> {
    let mut value = value.trim().to_string();
    // A chain longer than this is a loop: no sheet nests variables so deep.
    for _ in 0..16 {
        let Some(inner) = value.strip_prefix("var(").and_then(|v| v.strip_suffix(')')) else { return Some(value) };
        let (name, fallback) = match inner.split_once(',') {
            Some((name, fallback)) => (name.trim(), Some(fallback.trim())),
            None => (inner.trim(), None),
        };
        value = match var_value(vars, name) {
            Some(v) => v,
            None => fallback?.to_string(),
        };
    }
    None
}

/// The value custom property `name` is given in `vars` by its last
/// declaration.
fn var_value(vars: &str, name: &str) -> Option<String> {
    vars.split(';').filter_map(|decl| {
        let (key, value) = decl.split_once(':')?;
        (key.trim() == name).then(|| value.trim().to_string())
    }).last()
}

/// The palette's slots as a color line's list shows them.
pub(crate) fn slot_names() -> Vec<&'static str> {
    Slot::ALL.iter().map(|s| s.name()).collect()
}

/// A pair as stored, from what was typed for one half: the other half kept
/// when the token has one, else offered (the light one lifted for the dark
/// scheme, `display_color`, or the dark one kept for light), so setting one
/// color gives both schemes something readable (section 2.1).
pub(crate) fn pair_with(light: &str, dark: &str, typed_dark: bool, text: &str) -> Option<Json> {
    let typed = pimble_themes::Rgb::parse(text.trim())?.css();
    let (light, dark) = if typed_dark {
        let light = if light.is_empty() { typed.clone() } else { light.to_string() };
        (light, typed)
    } else {
        let dark = if dark.is_empty() { pimble_ui::appearance::display_color(&typed, true) } else { dark.to_string() };
        (typed, dark)
    };
    Some(serde_json::json!({ "light": light, "dark": dark }))
}

/// The bases theme `id` may take: every built-in and every theme of its
/// store but itself and those that would make a cycle, by name.
pub(crate) fn bases(id: &str, themes: &[Theme]) -> Vec<(String, String)> {
    let source = WithStore(themes);
    let mut out: Vec<(String, String)> = pimble_themes::builtins().map(|t| (t.id, t.name)).collect();
    let mut own: Vec<(String, String)> = themes
        .iter()
        .filter(|t| !pimble_themes::edit::would_cycle(id, &t.id, &source))
        .map(|t| (t.id.clone(), t.name.clone()))
        .collect();
    own.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    out.extend(own);
    out
}

/// The custom properties `css` declares on `:root`, as one inline style:
/// the app's colors for one scheme, scoped onto the sample so a theme that
/// sets no colors previews in that scheme's app colors (section 2.1: an
/// unset slot comes from the app theme) whichever scheme the app is in.
pub(crate) fn root_vars(css: &str) -> String {
    let mut out = String::new();
    let mut rest = css;
    while let Some(at) = rest.find(":root") {
        let after = &rest[at..];
        let (Some(open), Some(close)) = (after.find('{'), after.find('}')) else { break };
        if close > open {
            for declaration in after[open + 1..close].split(';') {
                let declaration = declaration.trim();
                if declaration.starts_with("--") && declaration.contains(':') {
                    out.push_str(declaration);
                    out.push_str("; ");
                }
            }
        }
        rest = &after[close + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn theme(id: &str, name: &str, stored: Json) -> Theme {
        Theme::from_json(id, name, stored.as_object().unwrap()).0
    }

    fn line<'a>(groups: &'a [Group], key: &str) -> &'a Line {
        groups.iter().flat_map(|g| &g.lines).find(|l| l.key == key).unwrap()
    }

    #[test]
    fn inherited_values_say_where_they_come_from() {
        let stored = json!({ "base": "tweak", "body.size": "17px", "body.line_height": 9 });
        let themes = vec![
            theme("mine", "Mine", stored.clone()),
            theme("tweak", "Tweak", json!({ "base": "builtin:book", "page.measure": "44em" })),
        ];
        let groups = groups("mine", &themes, stored.as_object().unwrap(), false, &Default::default());
        let size = line(&groups, "body.size");
        assert!(size.own && size.from.is_none());
        assert_eq!(size.value, "17px");
        let measure = line(&groups, "page.measure");
        assert_eq!((measure.value.as_str(), measure.from.as_deref()), ("44em", Some("Tweak")));
        let font = line(&groups, "body.font");
        assert_eq!(font.from.as_deref(), Some("Book"), "the built-in at the end of the chain");
        let wrap = line(&groups, "code.wrap");
        assert_eq!(wrap.control, Control::Toggle(true));
        // A refused value is the theme's own (a reset clears it) and says why,
        // while the base's value stands in.
        let refused = line(&groups, "body.line_height");
        assert!(refused.own && refused.refusal.is_some(), "{refused:?}");
        assert_eq!(groups[0].title, "Page");
        assert!(groups.iter().any(|g| g.title == "Heading 3"));
        assert_eq!(label("body.line_height"), "Line height");
    }

    #[test]
    fn only_what_the_theme_changes() {
        let stored = json!({ "base": "builtin:book", "body.size": "17px" });
        let themes = vec![theme("mine", "Mine", stored.clone())];
        let groups = groups("mine", &themes, stored.as_object().unwrap(), true, &Default::default());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].lines.iter().map(|l| l.key).collect::<Vec<_>>(), vec!["body.size"]);
        let none = super::groups("x", &[], &Map::new(), false, &Default::default());
        assert_eq!(line(&none, "body.size").from.as_deref(), Some(BEFORE_THEMES));
        assert_eq!(line(&none, "body.size").value, "15px");
    }

    #[test]
    fn setting_one_color_offers_the_other() {
        assert_eq!(pair_with("", "", false, "#202020"), Some(json!({ "light": "#202020", "dark": pimble_ui::appearance::display_color("#202020", true) })));
        assert_eq!(pair_with("#111111", "#eeeeee", true, "#dddddd"), Some(json!({ "light": "#111111", "dark": "#dddddd" })));
        assert_eq!(pair_with("", "", true, "not a color"), None);
    }

    #[test]
    fn root_variables_are_read_from_every_root_block() {
        let css = ":root {\n  --a: 1px;\n  --b: #fff;\n  color: red;\n}\n.x { --c: 2; }\n:root { --d: var(--a); }";
        assert_eq!(root_vars(css), "--a: 1px; --b: #fff; --d: var(--a); ");
        let light = root_vars(&crate::app::theme_css(false));
        assert!(light.contains("--rinch-color-body:") && light.contains("--rinch-color-text:"), "{light}");
        assert_ne!(light, root_vars(&crate::app::theme_css(true)));
    }

    /// A slot's colors are the chain's where it sets them, else the app's
    /// for each scheme; a slot whose app value is no plain color has none.
    #[test]
    fn a_slots_colors_come_from_the_chain_or_the_app() {
        let vars = ["--rinch-color-text: #111111; --x: 1px".to_string(), "--rinch-color-text:#eeeeee;".to_string()];
        let none = pimble_themes::Tokens::new();
        assert_eq!(slot_colors(Slot::Text, &none, &vars), Some(("#111111".into(), "#eeeeee".into())));
        assert_eq!(slot_colors(Slot::Highlight, &none, &vars), Some(("#fff3a3".into(), "#bb8009".into())));
        assert_eq!(slot_colors(Slot::Accent, &none, &vars), None, "no variable for the accent here");
        let set = theme("t", "T", json!({ "palette.text": { "light": "#202020", "dark": "#d0d0d0" } }));
        assert_eq!(slot_colors(Slot::Text, &set.tokens, &vars), Some(("#202020".into(), "#d0d0d0".into())));
    }

    #[test]
    fn every_slot_has_colors_from_the_apps_own_sheet() {
        // The sheet the editor reads: rinch's variables nest (`--a:
        // var(--b)`) and Pimble's light values come after rinch's.
        let vars = [root_vars(&crate::app::theme_css(false)), root_vars(&crate::app::theme_css(true))];
        let none = pimble_themes::Tokens::new();
        for slot in Slot::ALL {
            assert!(slot_colors(slot, &none, &vars).is_some(), "{}", slot.name());
        }
        let chained = ["--a: var(--b); --b: #101010; --b: #202020".to_string(), "--a: var(--missing, #303030)".to_string()];
        assert_eq!(resolved(&chained[0], "var(--a)").as_deref(), Some("#202020"), "the last declaration wins");
        assert_eq!(resolved(&chained[1], "var(--a)").as_deref(), Some("#303030"), "a fallback is taken");
        assert_eq!(resolved("--a: var(--a)", "var(--a)"), None, "a loop ends");
    }

    #[test]
    fn bases_leave_out_cycles() {
        let themes = vec![theme("a", "A", json!({ "base": "b" })), theme("b", "B", json!({})), theme("c", "C", json!({}))];
        let ids: Vec<String> = bases("b", &themes).into_iter().map(|(id, _)| id).collect();
        assert!(ids.contains(&"c".to_string()) && ids.contains(&"builtin:book".to_string()));
        assert!(!ids.contains(&"a".to_string()) && !ids.contains(&"b".to_string()));
    }
}

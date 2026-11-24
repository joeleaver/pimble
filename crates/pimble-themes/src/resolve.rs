//! Which theme a node reads in, and what that theme's tokens are once its
//! bases are folded in (docs/THEMES_CONTRACT.md sections 3.1, 3.2 and 4).
//!
//! Both are pure functions of data the caller holds: the names on a node's
//! chain (the node, then its ancestors, nearest first) and a [`ThemeSource`]
//! that answers a theme by id.

use crate::theme::{self, Theme, Tokens};

/// Answers a theme by id: the built-ins ([`Builtins`]), and from wave 2 a
/// store's theme nodes in front of them.
pub trait ThemeSource {
    fn theme(&self, id: &str) -> Option<Theme>;
}

/// The built-in themes.
#[derive(Debug, Clone, Copy, Default)]
pub struct Builtins;

impl ThemeSource for Builtins {
    fn theme(&self, id: &str) -> Option<Theme> {
        theme::builtin(id)
    }
}

/// A theme with its bases folded in.
#[derive(Debug, Clone, PartialEq)]
pub struct Folded {
    /// Every token the theme or a base sets, the nearest setting winning.
    pub tokens: Tokens,
    /// The themes folded, the theme first, then its bases in order. Default
    /// is last only where a theme names it.
    pub chain: Vec<String>,
    /// A base no source answers, where the chain stopped (it reads as
    /// Default from there).
    pub missing_base: Option<String>,
}

/// Folds theme `id` and its bases. Bases chain to any depth; a cycle (only
/// concurrent edits can make one) is broken at the theme with the smallest
/// id, which reads its base as Default, so every device and every entry
/// into the cycle folds the same chain.
pub fn fold(id: &str, source: &impl ThemeSource) -> Folded {
    let mut walked: Vec<Theme> = Vec::new();
    let mut missing_base = None;
    let mut next = Some(id.to_string());
    while let Some(id) = next.take() {
        if let Some(at) = walked.iter().position(|theme| theme.id == id) {
            // `walked[at..]` is the cycle: keep up to its smallest id.
            let smallest = walked[at..]
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.id.cmp(&b.1.id))
                .map(|(i, _)| at + i)
                .expect("a cycle has members");
            walked.truncate(smallest + 1);
            break;
        }
        match source.theme(&id) {
            Some(theme) => {
                next = theme.base.clone();
                walked.push(theme);
            }
            None => {
                if !walked.is_empty() {
                    missing_base = Some(id);
                }
            }
        }
    }
    let mut tokens = Tokens::new();
    for theme in &walked {
        for (key, value) in &theme.tokens {
            tokens.entry(key).or_insert(*value);
        }
    }
    Folded {
        tokens,
        chain: walked.into_iter().map(|theme| theme.id).collect(),
        missing_base,
    }
}

/// Where the theme in force comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Named by the node itself (depth 0) or the ancestor `depth` steps up.
    Named { depth: usize },
    /// No node on the chain names a theme; the node's own paragraphs were
    /// made spaced before themes, so it reads as Document (section 4).
    SpacedParagraphs,
    /// Nothing names one: Default.
    Default,
}

/// The theme in force for a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InForce {
    /// The theme's id.
    pub id: String,
    pub origin: Origin,
    /// Names nearer than the one in force that no source answers (a theme
    /// that was deleted, or that this device does not hold), nearest first,
    /// with their depth: the picker says so.
    pub unresolved: Vec<(usize, String)>,
}

/// One node of a chain, as resolution reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link<'a> {
    /// `custom["theme"]`, if the node names one.
    pub theme: Option<&'a str>,
}

/// The theme in force for the first node of `chain` (the node, then its
/// ancestors, nearest first): the nearest name a source answers; with none,
/// Document when `spaced` (the node's own `paragraphs` is `"spaced"`), else
/// Default.
pub fn in_force<'a>(
    chain: impl IntoIterator<Item = Link<'a>>,
    spaced: bool,
    source: &impl ThemeSource,
) -> InForce {
    let mut unresolved = Vec::new();
    for (depth, link) in chain.into_iter().enumerate() {
        let Some(name) = link.theme.filter(|name| !name.is_empty()) else {
            continue;
        };
        if source.theme(name).is_some() {
            return InForce {
                id: name.to_string(),
                origin: Origin::Named { depth },
                unresolved,
            };
        }
        unresolved.push((depth, name.to_string()));
    }
    if spaced {
        InForce {
            id: theme::DOCUMENT.to_string(),
            origin: Origin::SpacedParagraphs,
            unresolved,
        }
    } else {
        InForce {
            id: theme::DEFAULT.to_string(),
            origin: Origin::Default,
            unresolved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use serde_json::json;

    /// Store themes over the built-ins.
    struct Store(HashMap<&'static str, Theme>);

    impl ThemeSource for Store {
        fn theme(&self, id: &str) -> Option<Theme> {
            self.0.get(id).cloned().or_else(|| Builtins.theme(id))
        }
    }

    fn store(themes: &[(&'static str, serde_json::Value)]) -> Store {
        Store(
            themes
                .iter()
                .map(|(id, json)| (*id, Theme::from_json(id, id, json.as_object().unwrap()).0))
                .collect(),
        )
    }

    fn link(theme: Option<&str>) -> Link<'_> {
        Link { theme }
    }

    #[test]
    fn the_nearest_named_theme_wins() {
        let source = Builtins;
        let chain = [
            link(None),
            link(Some("builtin:code")),
            link(Some("builtin:book")),
        ];
        let found = in_force(chain, true, &source);
        assert_eq!(found.id, "builtin:code");
        assert_eq!(found.origin, Origin::Named { depth: 1 });
        assert!(found.unresolved.is_empty());
        let own = in_force(
            [link(Some("builtin:journal")), link(Some("builtin:code"))],
            false,
            &source,
        );
        assert_eq!(
            (own.id.as_str(), own.origin),
            ("builtin:journal", Origin::Named { depth: 0 })
        );
    }

    #[test]
    fn spaced_notes_read_as_document_until_a_theme_is_named_above() {
        let source = Builtins;
        assert_eq!(
            in_force([link(None), link(None)], true, &source).id,
            theme::DOCUMENT
        );
        assert_eq!(
            in_force([link(None), link(None)], true, &source).origin,
            Origin::SpacedParagraphs
        );
        assert_eq!(in_force([link(None)], false, &source).id, theme::DEFAULT);
        assert_eq!(in_force([], false, &source).origin, Origin::Default);
        // The first theme applied anywhere above a spaced note replaces the
        // fallback (section 4).
        assert_eq!(
            in_force([link(None), link(Some("builtin:default"))], true, &source).id,
            theme::DEFAULT
        );
    }

    #[test]
    fn a_name_nothing_answers_is_skipped_and_said() {
        let source = Builtins;
        let found = in_force(
            [link(Some("deleted-theme")), link(Some("builtin:book"))],
            false,
            &source,
        );
        assert_eq!(found.id, "builtin:book");
        assert_eq!(found.unresolved, vec![(0, "deleted-theme".to_string())]);
        let none = in_force([link(Some("gone")), link(Some(""))], false, &source);
        assert_eq!(none.id, theme::DEFAULT);
        assert_eq!(none.unresolved.len(), 1);
    }

    #[test]
    fn bases_fold_nearest_first() {
        let source = store(&[
            ("mine", json!({ "base": "tweak", "body.size": "17px" })),
            (
                "tweak",
                json!({ "base": "builtin:book", "body.size": "20px", "page.measure": "44em" }),
            ),
        ]);
        let folded = fold("mine", &source);
        assert_eq!(folded.chain, ["mine", "tweak", "builtin:book"]);
        let size = folded.tokens["body.size"];
        assert_eq!(size, crate::Value::Length(17.0));
        assert_eq!(
            folded.tokens["page.measure"],
            crate::Value::Measure(Some(44.0))
        );
        assert_eq!(
            folded.tokens["paragraph.spacing"],
            crate::Value::Word("indented"),
            "from Book"
        );
        assert_eq!(folded.missing_base, None);
    }

    #[test]
    fn a_missing_base_reads_as_default_from_there() {
        let source = store(&[("mine", json!({ "base": "deleted", "body.size": "17px" }))]);
        let folded = fold("mine", &source);
        assert_eq!(folded.chain, ["mine"]);
        assert_eq!(folded.missing_base.as_deref(), Some("deleted"));
        assert_eq!(folded.tokens.len(), 1);
        assert_eq!(fold("nothing", &source).chain, Vec::<String>::new());
    }

    #[test]
    fn a_cycle_is_broken_at_the_smallest_id_from_any_entry() {
        let source = store(&[
            ("c", json!({ "base": "a", "body.size": "13px" })),
            (
                "a",
                json!({ "base": "b", "body.size": "14px", "page.padding": "10px" }),
            ),
            (
                "b",
                json!({ "base": "c", "body.size": "15px", "page.padding": "11px", "body.line_height": 1.2 }),
            ),
            ("outside", json!({ "base": "b" })),
        ]);
        // "a" is the smallest: it reads its base as Default, so the cycle is
        // the line b -> c -> a.
        assert_eq!(fold("b", &source).chain, ["b", "c", "a"]);
        assert_eq!(fold("c", &source).chain, ["c", "a"]);
        assert_eq!(fold("a", &source).chain, ["a"]);
        assert_eq!(fold("outside", &source).chain, ["outside", "b", "c", "a"]);
        let folded = fold("c", &source);
        assert_eq!(folded.tokens["body.size"], crate::Value::Length(13.0));
        assert_eq!(folded.tokens["page.padding"], crate::Value::Length(10.0));
        assert!(
            !folded.tokens.contains_key("body.line_height"),
            "b is past the break"
        );
        let self_based = store(&[("x", json!({ "base": "x", "body.size": "13px" }))]);
        assert_eq!(fold("x", &self_based).chain, ["x"]);
    }
}

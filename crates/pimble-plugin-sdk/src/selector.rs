//! The CSS selectors a plugin's document answers by itself: compounds of a tag (or
//! `*`), `#id`, `.class` and `[attr]` / `[attr=value]`, joined by descendant and
//! child (`>`) combinators, in comma lists. A selector with anything else (a pseudo
//! class, a sibling combinator) matches nothing, as an invalid one does on every
//! backend; rinch's components select nothing at all.

/// What a selector needs to know of an element.
pub struct ElementView<'a> {
    pub tag: &'a str,
    pub attributes: &'a [(String, String)],
    pub parent: Option<u64>,
}

#[derive(Debug, Default, PartialEq)]
struct Compound {
    tag: Option<String>,
    ids: Vec<String>,
    classes: Vec<String>,
    attributes: Vec<(String, Option<String>)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Combinator {
    Descendant,
    Child,
}

/// A parsed selector list.
#[derive(Debug, PartialEq)]
pub struct Selector {
    /// Each alternative as its compounds left to right; each compound after the
    /// first carries the combinator that joins it to the one before.
    alternatives: Vec<Vec<(Combinator, Compound)>>,
}

impl Selector {
    /// The selector, or `None` for one this document does not understand.
    pub fn parse(text: &str) -> Option<Selector> {
        let mut alternatives = Vec::new();
        for alternative in text.split(',') {
            let mut parts = Vec::new();
            let mut combinator = Combinator::Descendant;
            let spaced = alternative.replace('>', " > ");
            for token in spaced.split_whitespace() {
                if token == ">" {
                    if parts.is_empty() {
                        return None;
                    }
                    combinator = Combinator::Child;
                    continue;
                }
                parts.push((combinator, parse_compound(token)?));
                combinator = Combinator::Descendant;
            }
            if parts.is_empty() || combinator == Combinator::Child {
                return None;
            }
            alternatives.push(parts);
        }
        Some(Selector { alternatives })
    }

    /// Whether element `node` matches, reading elements through `view`.
    pub fn matches<'a>(&self, node: u64, view: &dyn Fn(u64) -> Option<ElementView<'a>>) -> bool {
        self.alternatives.iter().any(|parts| matches_from(parts, parts.len() - 1, node, view))
    }
}

fn matches_from<'a>(
    parts: &[(Combinator, Compound)],
    index: usize,
    node: u64,
    view: &dyn Fn(u64) -> Option<ElementView<'a>>,
) -> bool {
    let Some(element) = view(node) else { return false };
    if !compound_matches(&parts[index].1, &element) {
        return false;
    }
    if index == 0 {
        return true;
    }
    match parts[index].0 {
        Combinator::Child => element.parent.is_some_and(|p| matches_from(parts, index - 1, p, view)),
        Combinator::Descendant => {
            let mut at = element.parent;
            while let Some(ancestor) = at {
                if matches_from(parts, index - 1, ancestor, view) {
                    return true;
                }
                at = view(ancestor).and_then(|e| e.parent);
            }
            false
        }
    }
}

fn compound_matches(compound: &Compound, element: &ElementView<'_>) -> bool {
    let attribute = |name: &str| element.attributes.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    if compound.tag.as_deref().is_some_and(|tag| !tag.eq_ignore_ascii_case(element.tag)) {
        return false;
    }
    if compound.ids.iter().any(|id| attribute("id") != Some(id.as_str())) {
        return false;
    }
    let classes: Vec<&str> = attribute("class").unwrap_or("").split_whitespace().collect();
    if compound.classes.iter().any(|c| !classes.contains(&c.as_str())) {
        return false;
    }
    compound.attributes.iter().all(|(name, value)| match (attribute(name), value) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(have), Some(want)) => have == want,
    })
}

fn parse_compound(token: &str) -> Option<Compound> {
    let mut compound = Compound::default();
    let chars: Vec<char> = token.chars().collect();
    let mut i = 0;
    let ident = |i: &mut usize| {
        let start = *i;
        while *i < chars.len() && (chars[*i].is_alphanumeric() || chars[*i] == '-' || chars[*i] == '_') {
            *i += 1;
        }
        (start < *i).then(|| chars[start..*i].iter().collect::<String>())
    };
    if i < chars.len() && chars[i] == '*' {
        i += 1;
    } else if let Some(tag) = ident(&mut i) {
        compound.tag = Some(tag);
    }
    while i < chars.len() {
        match chars[i] {
            '#' => {
                i += 1;
                compound.ids.push(ident(&mut i)?);
            }
            '.' => {
                i += 1;
                compound.classes.push(ident(&mut i)?);
            }
            '[' => {
                let close = chars[i..].iter().position(|&c| c == ']')? + i;
                let inner: String = chars[i + 1..close].iter().collect();
                i = close + 1;
                match inner.split_once('=') {
                    Some((name, value)) => {
                        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
                        compound.attributes.push((name.trim().to_string(), Some(value.to_string())));
                    }
                    None => compound.attributes.push((inner.trim().to_string(), None)),
                }
            }
            _ => return None,
        }
    }
    Some(compound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compounds_and_combinators_match_as_css_does() {
        // 1 <section class="board"> > 2 <div id="a" class="card x"> > 3 <span data-k="v">
        let attrs: Vec<Vec<(String, String)>> = vec![
            vec![],
            vec![("class".into(), "board".into())],
            vec![("id".into(), "a".into()), ("class".into(), "card x".into())],
            vec![("data-k".into(), "v".into())],
        ];
        let tags = ["body", "section", "div", "span"];
        let parents = [None, Some(0), Some(1), Some(2)];
        let view = |id: u64| {
            let i = id as usize;
            (i < 4).then(|| ElementView { tag: tags[i], attributes: &attrs[i], parent: parents[i] })
        };
        let hits = |sel: &str, node: u64| Selector::parse(sel).unwrap().matches(node, &view);
        assert!(hits("span", 3));
        assert!(hits(".board span", 3));
        assert!(hits("div.card.x > [data-k=v]", 3));
        assert!(hits("#a", 2));
        assert!(!hits("section > span", 3));
        assert!(hits("p, span[data-k]", 3));
        assert!(Selector::parse("a:hover").is_none());
        assert!(Selector::parse("> a").is_none());
    }
}

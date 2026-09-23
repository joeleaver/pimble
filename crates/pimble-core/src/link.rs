//! Pimble links: the `href` of a link mark in a node's text
//! (docs/LINKS_CONTRACT.md "The URL").
//!
//! ```text
//! pimble:<store uuid>/<node uuid>
//! pimble:<store uuid>/<node uuid>#p=<sticky>&q=<quote>
//! ```
//!
//! [`PimbleUrl`] is the one place the string is built or taken apart. An `href`
//! that does not parse as one is an external URL.

use std::fmt;
use std::str::FromStr;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use url::form_urlencoded;

use crate::{NodeId, StoreId};

/// The scheme, with its colon.
pub const SCHEME: &str = "pimble:";

/// How many characters of text a deep link's quote keeps.
pub const QUOTE_CHARS: usize = 48;

/// A link to a node, or to a spot inside one. Always the canonical
/// `(StoreId, NodeId)`, never a tree path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PimbleUrl {
    pub store: StoreId,
    pub node: NodeId,
    pub anchor: Option<Anchor>,
}

/// A spot inside a node's content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Anchor {
    /// A yrs `StickyIndex` (v1 encoding) into a block's text: a character's
    /// identity, which survives edits around it. `None` when only the quote is
    /// known.
    pub sticky: Option<Vec<u8>>,
    /// Up to [`QUOTE_CHARS`] characters of text starting at the spot: the
    /// fallback when `sticky` does not resolve. May be empty.
    pub quote: String,
}

impl Anchor {
    /// An anchor from a sticky index and the text that follows the spot, which
    /// is cut to [`QUOTE_CHARS`] characters.
    pub fn new(sticky: Option<Vec<u8>>, text_after: &str) -> Self {
        Self {
            sticky,
            quote: text_after.chars().take(QUOTE_CHARS).collect(),
        }
    }
}

impl PimbleUrl {
    /// A link to a whole node.
    pub fn node(store: StoreId, node: NodeId) -> Self {
        Self { store, node, anchor: None }
    }

    /// A link to a spot inside a node.
    pub fn deep(store: StoreId, node: NodeId, anchor: Anchor) -> Self {
        Self { store, node, anchor: Some(anchor) }
    }

    /// `Some` when `href` is a Pimble link, `None` for anything else (an
    /// external URL, or a malformed one).
    pub fn parse(href: &str) -> Option<Self> {
        href.parse().ok()
    }

    /// The same link without its anchor.
    pub fn without_anchor(&self) -> Self {
        Self::node(self.store, self.node)
    }
}

impl fmt::Display for PimbleUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCHEME}{}/{}", self.store, self.node)?;
        if let Some(anchor) = &self.anchor {
            let mut fragment = form_urlencoded::Serializer::new(String::new());
            if let Some(sticky) = &anchor.sticky {
                fragment.append_pair("p", &URL_SAFE_NO_PAD.encode(sticky));
            }
            fragment.append_pair("q", &anchor.quote);
            write!(f, "#{}", fragment.finish())?;
        }
        Ok(())
    }
}

/// Why a string is not a Pimble link.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PimbleUrlError {
    #[error("not a pimble: link")]
    Scheme,
    #[error("a pimble: link names a store and a node")]
    Shape,
    #[error("a pimble: link's store or node is not an id")]
    Id,
    #[error("a pimble: link's anchor is malformed")]
    Anchor,
}

impl FromStr for PimbleUrl {
    type Err = PimbleUrlError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .get(..SCHEME.len())
            .filter(|head| head.eq_ignore_ascii_case(SCHEME))
            .map(|_| &s[SCHEME.len()..])
            .ok_or(PimbleUrlError::Scheme)?;
        let (path, fragment) = match rest.split_once('#') {
            Some((path, fragment)) => (path, Some(fragment)),
            None => (rest, None),
        };
        let (store, node) = path.split_once('/').ok_or(PimbleUrlError::Shape)?;
        if node.contains('/') {
            return Err(PimbleUrlError::Shape);
        }
        let store = StoreId::parse(store).map_err(|_| PimbleUrlError::Id)?;
        let node = NodeId::parse(node).map_err(|_| PimbleUrlError::Id)?;
        let anchor = fragment.map(parse_anchor).transpose()?;
        Ok(Self { store, node, anchor })
    }
}

fn parse_anchor(fragment: &str) -> Result<Anchor, PimbleUrlError> {
    let mut sticky = None;
    let mut quote = String::new();
    for (key, value) in form_urlencoded::parse(fragment.as_bytes()) {
        match key.as_ref() {
            "p" => {
                sticky = Some(URL_SAFE_NO_PAD.decode(value.as_bytes()).map_err(|_| PimbleUrlError::Anchor)?);
            }
            "q" => quote = value.into_owned(),
            // Room for later keys: an older build ignores what it does not know.
            _ => {}
        }
    }
    Ok(Anchor { sticky, quote })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (StoreId, NodeId) {
        (
            StoreId::parse("6f1c2b1e-8d3a-4f63-9a54-2c0f5a1e7b90").unwrap(),
            NodeId::parse("0b7e7c4d-1f2a-4c1b-8e3d-5a6b7c8d9e0f").unwrap(),
        )
    }

    #[test]
    fn a_node_link_prints_and_parses() {
        let (store, node) = ids();
        let url = PimbleUrl::node(store, node);
        let text = url.to_string();
        assert_eq!(text, format!("pimble:{store}/{node}"));
        assert_eq!(PimbleUrl::parse(&text), Some(url));
    }

    #[test]
    fn a_deep_link_round_trips_bytes_and_awkward_quotes() {
        let (store, node) = ids();
        let quote = "Tom & Jerry = 100% #1? /yes/ «ünïcode» ✓";
        let url = PimbleUrl::deep(store, node, Anchor::new(Some(vec![0, 1, 254, 255, 62, 63]), quote));
        let text = url.to_string();
        assert_eq!(text.matches('#').count(), 1, "{text}");
        assert_eq!(PimbleUrl::parse(&text), Some(url));
    }

    #[test]
    fn a_quote_is_cut_to_its_length_in_characters() {
        let long: String = "é".repeat(100);
        assert_eq!(Anchor::new(None, &long).quote.chars().count(), QUOTE_CHARS);
    }

    #[test]
    fn an_anchor_may_have_only_a_quote_or_nothing() {
        let (store, node) = ids();
        for anchor in [Anchor::new(None, "only words"), Anchor::new(None, "")] {
            let url = PimbleUrl::deep(store, node, anchor);
            assert_eq!(PimbleUrl::parse(&url.to_string()), Some(url));
        }
    }

    #[test]
    fn unknown_anchor_keys_are_ignored() {
        let (store, node) = ids();
        let url = PimbleUrl::parse(&format!("pimble:{store}/{node}#q=hi&z=later")).unwrap();
        assert_eq!(url.anchor, Some(Anchor { sticky: None, quote: "hi".into() }));
    }

    #[test]
    fn anything_else_is_not_a_pimble_link() {
        let (store, node) = ids();
        for href in [
            "https://example.com".to_string(),
            "pimble:".to_string(),
            format!("pimble:{store}"),
            format!("pimble:{store}/{node}/extra"),
            format!("pimble:not-a-uuid/{node}"),
            format!("pimble:{store}/{node}#p=!!!"),
            format!("pimbles:{store}/{node}"),
        ] {
            assert_eq!(PimbleUrl::parse(&href), None, "{href}");
        }
    }

    #[test]
    fn the_scheme_is_case_insensitive() {
        let (store, node) = ids();
        assert!(PimbleUrl::parse(&format!("PIMBLE:{store}/{node}")).is_some());
    }
}

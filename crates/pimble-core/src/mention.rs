//! Mentions of a person (docs/NOTIFICATIONS_CONTRACT.md section 1, "Built in
//! wave 0").
//!
//! A mention is a link whose href names an account: `pimble-person:<sub>`,
//! with the mention's own id after it, `pimble-person:<sub>?m=<id>`. The
//! linked words are the name as it was written, `@Will`. The same href is
//! a link mark in a note's text and a link span in a discussion message, so
//! Markdown, search, an offline device and an older build all read a
//! mention as what it says.
//!
//! The id is what keeps one mention one mention: undo and redo put the same
//! href back, and so does a Markdown round trip, so whoever notifies (wave 1)
//! remembers ids it has sent and an id it has seen is never news again.
//! A mention written without one (an agent's Markdown, a message typed by
//! hand) is given one where it enters a document ([`Mention::with_id`]).

/// What a mention's href starts with.
pub const SCHEME: &str = "pimble-person:";

/// The query key that carries a mention's id.
const ID_KEY: &str = "m=";

/// One mention as it is written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Mention {
    /// The account's `sub`.
    pub account: String,
    /// The mention's id, `None` for one written without it.
    pub id: Option<String>,
    /// The name as written, without the `@` (what Markdown export, search and
    /// a device that does not know the share's people show).
    pub name: String,
}

impl Mention {
    /// A new mention of `account` by the name `name`, with a fresh id.
    pub fn new(account: &str, name: &str) -> Self {
        Self { account: account.to_string(), id: Some(new_id()), name: name.trim_start_matches('@').to_string() }
    }

    /// The mention an href and its linked words make, `None` when the href
    /// does not name an account.
    pub fn from_link(href: &str, words: &str) -> Option<Self> {
        let (account, id) = parse_href(href)?;
        Some(Self { account, id, name: words.trim().trim_start_matches('@').to_string() })
    }

    /// The href that says this mention.
    pub fn href(&self) -> String {
        href(&self.account, self.id.as_deref())
    }

    /// The words the link shows: `@` and the name.
    pub fn words(&self) -> String {
        format!("@{}", self.name)
    }

    /// The same mention with an id: its own, or a fresh one.
    pub fn with_id(mut self) -> Self {
        if self.id.is_none() {
            self.id = Some(new_id());
        }
        self
    }
}

/// The href of a mention of `account` with id `id`.
pub fn href(account: &str, id: Option<&str>) -> String {
    match id {
        Some(id) => format!("{SCHEME}{account}?{ID_KEY}{id}"),
        None => format!("{SCHEME}{account}"),
    }
}

/// Whether `href` is a mention's (it may still name nobody: see [`parse_href`]).
pub fn is_mention_href(href: &str) -> bool {
    href.trim().starts_with(SCHEME)
}

/// The account and the id a mention's href names. `None` when it is not a
/// mention's href or names no account.
pub fn parse_href(href: &str) -> Option<(String, Option<String>)> {
    let rest = href.trim().strip_prefix(SCHEME)?;
    let (account, query) = rest.split_once('?').unwrap_or((rest, ""));
    let account = account.trim_matches('/');
    if account.is_empty() || account.chars().any(|c| c.is_whitespace() || matches!(c, '/' | '#' | '?' | '&')) {
        return None;
    }
    let id = query
        .split('&')
        .find_map(|pair| pair.strip_prefix(ID_KEY))
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .map(str::to_string);
    Some((account.to_string(), id))
}

/// A fresh mention id: 12 hex digits (48 random bits; ids only need to be
/// apart among one document's mentions and one sender's memory).
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// The mentions in `after` that are not in `before`: what an edit added.
/// Mentions of `writer` (the account that made the edit) are left out, since
/// nobody is told they mentioned themselves, and a mention is answered once
/// however many copies of it `after` holds (a pasted copy keeps its id).
///
/// A mention is known by its id. One written without an id (nothing Pimble
/// makes; a document from elsewhere) is known by its account and how many
/// id-less mentions of that account come before it.
///
/// Undo and redo: undoing takes a mention out and redoing puts the same href
/// back, so the redo is "added" again here. The caller that notifies keeps
/// the ids it has notified and drops those (wave 1); this function says what
/// the edit changed, nothing more.
pub fn added(before: &[Mention], after: &[Mention], writer: Option<&str>) -> Vec<Mention> {
    let known: std::collections::HashSet<String> = keys(before).into_iter().collect();
    let mut seen = std::collections::HashSet::new();
    keys(after)
        .into_iter()
        .zip(after)
        .filter(|(key, mention)| !known.contains(key) && Some(mention.account.as_str()) != writer && seen.insert(key.clone()))
        .map(|(_, mention)| mention.clone())
        .collect()
}

/// Each mention's key for [`added`], in order.
fn keys(mentions: &[Mention]) -> Vec<String> {
    let mut idless: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    mentions
        .iter()
        .map(|m| match &m.id {
            Some(id) => format!("id:{id}"),
            None => {
                let n = idless.entry(m.account.as_str()).or_default();
                *n += 1;
                format!("acct:{}#{}", m.account, n)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(account: &str, id: Option<&str>) -> Mention {
        Mention { account: account.into(), id: id.map(str::to_string), name: account.into() }
    }

    #[test]
    fn an_href_names_an_account_and_its_id() {
        assert_eq!(parse_href("pimble-person:abc-1?m=f00d"), Some(("abc-1".into(), Some("f00d".into()))));
        assert_eq!(parse_href("pimble-person:abc"), Some(("abc".into(), None)));
        assert_eq!(parse_href("pimble-person:abc?x=1&m=9"), Some(("abc".into(), Some("9".into()))));
        assert_eq!(parse_href("pimble-person:"), None);
        assert_eq!(parse_href("pimble-person:a b"), None);
        assert_eq!(parse_href("pimble:s/n"), None);
        assert_eq!(parse_href("https://x"), None);
        let made = Mention::new("abc", "@Will");
        assert_eq!(made.name, "Will");
        assert_eq!(made.words(), "@Will");
        assert_eq!(Mention::from_link(&made.href(), " @Will "), Some(made.clone()));
        assert_eq!(made.id.as_ref().unwrap().len(), 12);
        assert_ne!(Mention::new("abc", "x").id, made.id, "every mention has its own id");
        assert_eq!(m("abc", None).with_id().id.unwrap().len(), 12);
    }

    #[test]
    fn added_is_what_is_new_and_not_the_writers_own() {
        let will = m("will", Some("1"));
        let sam = m("sam", Some("2"));
        assert_eq!(added(&[], &[will.clone(), sam.clone()], None), vec![will.clone(), sam.clone()]);
        assert_eq!(added(&[will.clone()], &[will.clone(), sam.clone()], None), vec![sam.clone()]);
        assert_eq!(added(&[], &[will.clone(), sam.clone()], Some("sam")), vec![will.clone()], "nobody is told they mentioned themselves");
        assert_eq!(added(&[], &[will.clone(), will.clone()], None), vec![will.clone()], "a pasted copy keeps its id: one mention");
        assert!(added(&[will.clone(), sam.clone()], &[sam.clone()], None).is_empty(), "a deletion adds nothing");
        // A mention whose words changed (a rename typed over it) is the same mention.
        let renamed = Mention { name: "William".into(), ..will.clone() };
        assert!(added(&[will.clone()], &[renamed], None).is_empty());
    }

    #[test]
    fn mentions_without_ids_count_per_account() {
        let a = m("will", None);
        assert!(added(&[a.clone()], &[a.clone()], None).is_empty());
        assert_eq!(added(&[a.clone()], &[a.clone(), a.clone()], None).len(), 1, "a second one of the same account is new");
    }
}

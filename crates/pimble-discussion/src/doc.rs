//! The discussion document in yrs (docs/DISCUSSIONS_CONTRACT.md section 2).
//!
//! ```text
//! data root (the node's own), one key:
//! "discussion": Map {                      created with the node
//!   v: 1,
//!   resolved: Map { at, by_id?, by_name? } | absent,
//!   anchor: Map { kind: "text", text: Buffer } | Map { kind: "board", board, x, y } | absent,
//!   messages: Map<message id, Map {        created with the node
//!     reply_to: String?, created: String, author_id: String?, author_name: String?,
//!     body: Text (attributes bold, italic, link), edited: String?, deleted_at: String?,
//!   }>,
//! }
//! ```
//!
//! The node is created with every container it needs ([`new_content`]): no
//! container is ever made on first use, since two devices that each made one
//! while apart would merge into one of them and lose the other's messages.
//! A discussion is its own node, made by one device, so its first transaction
//! is the only one that makes them.
//!
//! Messages are keyed by id and never kept in an array: two people replying
//! at once each add a key, and nothing moves. Order is computed when the
//! thread is read ([`crate::thread`]), never stored. Editing a message
//! rewrites its body in place with the smallest change, so two of a person's
//! devices editing one message merge character by character.
//!
//! A [`Discussion`] is a handle on where a node's discussion lives in its yrs
//! document; it holds no state of its own. Every write runs in one
//! transaction and answers the update it produced, an ordinary yrs update of
//! the node's document, which the caller sends through `applyEdit` like every
//! other edit (CLAUDE.md: "one way to write node content").

use std::collections::HashMap;
use std::sync::Arc;

use pimble_crdt::NodeDoc;
use yrs::types::text::YChange;
use yrs::{Any, Doc, GetString, Map, MapPrelim, MapRef, OffsetKind, Origin, Out, ReadTxn, Text, TextPrelim, TextRef, Transact, TransactionMut};

use crate::body::{Body, Span};
use crate::thread::{DiscussionSnapshot, MessageSnap, Resolved};

/// The key of the discussion in a node's `data` root.
pub const ROOT_KEY: &str = "discussion";
/// The schema version written to `v`.
pub const VERSION: i64 = 1;

mod keys {
    pub const V: &str = "v";
    pub const RESOLVED: &str = "resolved";
    pub const ANCHOR: &str = "anchor";
    pub const MESSAGES: &str = "messages";
    pub const REPLY_TO: &str = "reply_to";
    pub const CREATED: &str = "created";
    pub const AUTHOR_ID: &str = "author_id";
    pub const AUTHOR_NAME: &str = "author_name";
    pub const BODY: &str = "body";
    pub const EDITED: &str = "edited";
    pub const DELETED_AT: &str = "deleted_at";
    pub const AT: &str = "at";
    pub const BY_ID: &str = "by_id";
    pub const BY_NAME: &str = "by_name";
    pub const KIND: &str = "kind";
    pub const TEXT: &str = "text";
    pub const BOARD: &str = "board";
    pub const X: &str = "x";
    pub const Y: &str = "y";
    // Formatting attributes of a body.
    pub const A_BOLD: &str = "bold";
    pub const A_ITALIC: &str = "italic";
    pub const A_LINK: &str = "link";
}
use keys::*;

/// Who wrote something (contract section 3): the signed-in account's `sub`
/// and its name at the time, or an agent (`agent:<id>` and its name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Author {
    pub id: String,
    pub name: String,
}

impl Author {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { id: id.into(), name: name.into() }
    }

    /// Whether this author is an AI agent (docs/AGENTS_CONTRACT.md).
    pub fn is_agent(&self) -> bool {
        self.id.starts_with("agent:")
    }
}

/// What every write is stamped with: the author when someone is signed in,
/// and the caller's clock.
#[derive(Debug, Clone, Copy)]
pub struct Stamp<'a> {
    pub author: Option<&'a Author>,
    /// RFC 3339, UTC (`2026-10-21T12:00:00.000Z`): messages sort by it as a
    /// string, so every writer must use the same shape.
    pub now: &'a str,
}

impl<'a> Stamp<'a> {
    pub fn new(author: Option<&'a Author>, now: &'a str) -> Self {
        Self { author, now }
    }
}

/// Where a discussion is on its host (contract section 6). Absent: on the
/// node as a whole.
#[derive(Debug, Clone, PartialEq)]
pub enum Anchor {
    /// A place in the host's text: rinch's sticky bytes, opaque here.
    Text { sticky: Vec<u8> },
    /// A point on one of the host's boards (`"board"`, or `"board:<id>"` for
    /// a picture in its text), in board coordinates.
    Board { board: String, x: f64, y: f64 },
}

/// Why a write was refused. A refused write changed nothing. The messages
/// are whole sentences, for the person.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DiscussionError {
    #[error("This discussion is not ready yet; try again in a moment.")]
    NotReady,
    #[error("There is no such message here.")]
    NoMessage(String),
    #[error("A message needs some text.")]
    Empty,
    #[error("That message was deleted.")]
    Deleted,
}

pub type Result<T> = std::result::Result<T, DiscussionError>;

/// What [`Discussion::edit`] answers: the closure's result, and the update
/// the transaction produced (`None` when nothing changed). Every write
/// checks before it writes, so a failure wrote nothing.
#[derive(Debug)]
pub struct Edited<R> {
    pub result: Result<R>,
    pub update: Option<Vec<u8>>,
}

impl<R> Edited<R> {
    pub fn into_result(self) -> Result<(R, Option<Vec<u8>>)> {
        let Edited { result, update } = self;
        result.map(|r| (r, update))
    }
}

/// A random id for a message: a v4 uuid without hyphens.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// A new discussion's document content: the containers, made once, and its
/// anchor. The update to create the node with (`createNode` / `createAttachment`
/// content), so nothing ever makes them again.
pub fn new_content(anchor: Option<&Anchor>) -> Vec<u8> {
    let d = Discussion::standalone();
    let mut txn = d.doc.transact_mut();
    let map: MapRef = d.root.insert(&mut txn, ROOT_KEY, MapPrelim::default());
    map.insert(&mut txn, V, Any::BigInt(VERSION));
    map.insert(&mut txn, MESSAGES, MapPrelim::default());
    if let Some(anchor) = anchor {
        write_anchor(&mut txn, &map, anchor);
    }
    txn.commit();
    txn.encode_update_v1()
}

fn write_anchor(txn: &mut TransactionMut, map: &MapRef, anchor: &Anchor) {
    let a: MapRef = map.insert(txn, ANCHOR, MapPrelim::default());
    match anchor {
        Anchor::Text { sticky } => {
            a.insert(txn, KIND, TEXT);
            a.insert(txn, TEXT, Any::Buffer(sticky.clone().into()));
        }
        Anchor::Board { board, x, y } => {
            a.insert(txn, KIND, BOARD);
            a.insert(txn, BOARD, board.clone());
            a.insert(txn, X, *x);
            a.insert(txn, Y, *y);
        }
    }
}

fn string_of(out: Option<Out>) -> Option<String> {
    match out {
        Some(Out::Any(Any::String(s))) => Some(s.to_string()),
        _ => None,
    }
}

fn number_of(out: Option<Out>) -> Option<f64> {
    match out {
        Some(Out::Any(Any::Number(n))) => Some(n),
        Some(Out::Any(Any::BigInt(n))) => Some(n as f64),
        _ => None,
    }
}

/// Where a node's discussion lives in a yrs document, and the origin its
/// writes carry.
#[derive(Clone)]
pub struct Discussion {
    doc: Doc,
    root: MapRef,
    origin: Option<Origin>,
}

impl Discussion {
    /// The discussion of a node: `data.discussion`.
    pub fn of_node(node: &NodeDoc) -> Self {
        Self { doc: node.ydoc().clone(), root: node.data_root(), origin: None }
    }

    /// A discussion in a document of its own (root map `data`), for tests
    /// and tools that read a document's bytes.
    pub fn standalone() -> Self {
        let doc = Doc::with_options(yrs::Options { offset_kind: OffsetKind::Utf16, ..Default::default() });
        let root = doc.get_or_insert_map(pimble_crdt::ROOT_DATA);
        Self { doc, root, origin: None }
    }

    /// Every write through this handle carries `origin`.
    pub fn with_origin(mut self, origin: impl Into<Origin>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Whether the discussion's containers have arrived.
    pub fn exists(&self) -> bool {
        let txn = self.doc.transact();
        self.map(&txn).is_some()
    }

    fn map<T: ReadTxn>(&self, txn: &T) -> Option<MapRef> {
        match self.root.get(txn, ROOT_KEY) {
            Some(Out::YMap(m)) => Some(m),
            _ => None,
        }
    }

    /// Run `f` in one write transaction and answer its result with the
    /// update the transaction produced.
    pub fn edit<R>(&self, f: impl FnOnce(&mut DiscussionTxn<'_, '_>) -> Result<R>) -> Edited<R> {
        let mut txn = match &self.origin {
            Some(origin) => self.doc.transact_mut_with(origin.clone()),
            None => self.doc.transact_mut(),
        };
        let result = match self.map(&txn) {
            Some(map) => {
                let mut t = DiscussionTxn { txn: &mut txn, map };
                f(&mut t)
            }
            None => Err(DiscussionError::NotReady),
        };
        txn.commit();
        let changed = *txn.before_state() != *txn.after_state() || !txn.delete_set().is_empty();
        let update = changed.then(|| txn.encode_update_v1());
        Edited { result, update }
    }

    /// The discussion read in one transaction (empty when its containers
    /// have not arrived).
    pub fn read(&self) -> DiscussionSnapshot {
        let txn = self.doc.transact();
        let Some(map) = self.map(&txn) else { return DiscussionSnapshot::default() };
        let resolved = match map.get(&txn, RESOLVED) {
            Some(Out::YMap(r)) => Some(Resolved {
                at: string_of(r.get(&txn, AT)).unwrap_or_default(),
                by: string_of(r.get(&txn, BY_ID)).map(|id| Author::new(id, string_of(r.get(&txn, BY_NAME)).unwrap_or_default())),
            }),
            _ => None,
        };
        let anchor = match map.get(&txn, ANCHOR) {
            Some(Out::YMap(a)) => match string_of(a.get(&txn, KIND)).as_deref() {
                Some(TEXT) => match a.get(&txn, TEXT) {
                    Some(Out::Any(Any::Buffer(bytes))) => Some(Anchor::Text { sticky: bytes.to_vec() }),
                    _ => None,
                },
                Some(BOARD) => Some(Anchor::Board {
                    board: string_of(a.get(&txn, BOARD)).unwrap_or_default(),
                    x: number_of(a.get(&txn, X)).unwrap_or(0.0),
                    y: number_of(a.get(&txn, Y)).unwrap_or(0.0),
                }),
                _ => None,
            },
            _ => None,
        };
        let mut messages = Vec::new();
        if let Some(Out::YMap(all)) = map.get(&txn, MESSAGES) {
            for (id, value) in all.iter(&txn) {
                let Out::YMap(m) = value else { continue };
                let Some(created) = string_of(m.get(&txn, CREATED)) else { continue };
                let body = match m.get(&txn, BODY) {
                    Some(Out::YText(text)) => read_body(&txn, &text),
                    _ => Body::default(),
                };
                messages.push(MessageSnap {
                    id: id.to_string(),
                    reply_to: string_of(m.get(&txn, REPLY_TO)),
                    created,
                    author: string_of(m.get(&txn, AUTHOR_ID)).map(|aid| Author::new(aid, string_of(m.get(&txn, AUTHOR_NAME)).unwrap_or_default())),
                    body,
                    edited: string_of(m.get(&txn, EDITED)),
                    deleted_at: string_of(m.get(&txn, DELETED_AT)),
                });
            }
        }
        DiscussionSnapshot::new(messages, resolved, anchor)
    }
}

fn read_body<T: ReadTxn>(txn: &T, text: &TextRef) -> Body {
    let mut spans = Vec::new();
    for diff in text.diff(txn, YChange::identity) {
        let chunk = match diff.insert {
            Out::Any(Any::String(s)) => s.to_string(),
            // Nothing but text is ever inserted; anything else is skipped.
            _ => continue,
        };
        let attrs = diff.attributes.as_deref();
        let flag = |key: &str| attrs.and_then(|a| a.get(key)).is_some_and(|v| matches!(v, Any::Bool(true)));
        let link = attrs.and_then(|a| a.get(A_LINK)).and_then(|v| match v {
            Any::String(s) => Some(s.to_string()),
            _ => None,
        });
        spans.push(Span { text: chunk, bold: flag(A_BOLD), italic: flag(A_ITALIC), link });
    }
    Body { spans }.normalized()
}

/// The attributes a span is written with: every one, so formatting a range
/// also takes away what it no longer has.
fn attrs_of(span: &Span) -> yrs::types::Attrs {
    let mut attrs: HashMap<Arc<str>, Any> = HashMap::new();
    attrs.insert(A_BOLD.into(), if span.bold { Any::Bool(true) } else { Any::Null });
    attrs.insert(A_ITALIC.into(), if span.italic { Any::Bool(true) } else { Any::Null });
    attrs.insert(A_LINK.into(), match &span.link {
        Some(href) => Any::String(href.as_str().into()),
        None => Any::Null,
    });
    attrs
}

/// A string's length in UTF-16 units (the documents' offsets).
fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// A write transaction on a discussion. Every write checks what it needs
/// before it writes, so a refused write changes nothing.
pub struct DiscussionTxn<'a, 'doc> {
    txn: &'a mut TransactionMut<'doc>,
    map: MapRef,
}

impl<'a, 'doc> DiscussionTxn<'a, 'doc> {
    fn messages(&self) -> Result<MapRef> {
        match self.map.get(self.txn, MESSAGES) {
            Some(Out::YMap(m)) => Ok(m),
            _ => Err(DiscussionError::NotReady),
        }
    }

    fn message(&self, id: &str) -> Result<MapRef> {
        match self.messages()?.get(self.txn, id) {
            Some(Out::YMap(m)) => Ok(m),
            _ => Err(DiscussionError::NoMessage(id.to_string())),
        }
    }

    fn stamp_author(&mut self, m: &MapRef, stamp: Stamp<'_>) {
        if let Some(author) = stamp.author {
            m.insert(self.txn, AUTHOR_ID, author.id.clone());
            m.insert(self.txn, AUTHOR_NAME, author.name.clone());
        }
    }

    /// A new message, at the top level or replying to `reply_to` (which must
    /// be here; a deleted one may still be replied to). Answers its id.
    pub fn post(&mut self, stamp: Stamp<'_>, reply_to: Option<&str>, body: &Body) -> Result<String> {
        let body = body.clone().normalized();
        if body.is_blank() {
            return Err(DiscussionError::Empty);
        }
        if let Some(parent) = reply_to {
            self.message(parent)?;
        }
        let messages = self.messages()?;
        let id = new_id();
        let m: MapRef = messages.insert(self.txn, id.as_str(), MapPrelim::default());
        if let Some(parent) = reply_to {
            m.insert(self.txn, REPLY_TO, parent.to_string());
        }
        m.insert(self.txn, CREATED, stamp.now.to_string());
        self.stamp_author(&m, stamp);
        let text: TextRef = m.insert(self.txn, BODY, TextPrelim::new(""));
        let mut at = 0;
        for span in &body.spans {
            text.insert_with_attributes(self.txn, at, &span.text, attrs_of(span));
            at += utf16_len(&span.text);
        }
        Ok(id)
    }

    /// Rewrite message `id`'s text as `body`, changing the least: the text
    /// both share at the start and the end stays, the middle is replaced,
    /// and the formatting of every span is set again.
    pub fn edit_message(&mut self, stamp: Stamp<'_>, id: &str, body: &Body) -> Result<()> {
        let body = body.clone().normalized();
        if body.is_blank() {
            return Err(DiscussionError::Empty);
        }
        let m = self.message(id)?;
        if m.get(self.txn, DELETED_AT).is_some() {
            return Err(DiscussionError::Deleted);
        }
        let text = match m.get(self.txn, BODY) {
            Some(Out::YText(t)) => t,
            _ => m.insert(self.txn, BODY, TextPrelim::new("")),
        };
        let old: Vec<u16> = text.get_string(self.txn).encode_utf16().collect();
        let new_text = body.text();
        let new: Vec<u16> = new_text.encode_utf16().collect();
        let mut prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let mut suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
        // Never split a surrogate pair.
        while prefix > 0 && (0xDC00..0xE000).contains(&new.get(prefix).copied().unwrap_or(0)) {
            prefix -= 1;
        }
        while suffix > 0 && (0xDC00..0xE000).contains(&new.get(new.len() - suffix).copied().unwrap_or(0)) {
            suffix -= 1;
        }
        let removed = old.len() - prefix - suffix;
        if removed > 0 {
            text.remove_range(self.txn, prefix as u32, removed as u32);
        }
        let inserted = String::from_utf16_lossy(&new[prefix..new.len() - suffix]);
        if !inserted.is_empty() {
            text.insert(self.txn, prefix as u32, &inserted);
        }
        let current = read_body(self.txn, &text);
        if current != body {
            let mut at = 0;
            for span in &body.spans {
                let len = utf16_len(&span.text);
                text.format(self.txn, at, len, attrs_of(span));
                at += len;
            }
        }
        m.insert(self.txn, EDITED, stamp.now.to_string());
        Ok(())
    }

    /// A tombstone on message `id`: its text stays in the document, its
    /// replies stay where they are.
    pub fn delete_message(&mut self, stamp: Stamp<'_>, id: &str) -> Result<()> {
        let m = self.message(id)?;
        if m.get(self.txn, DELETED_AT).is_none() {
            m.insert(self.txn, DELETED_AT, stamp.now.to_string());
        }
        Ok(())
    }

    /// Take message `id`'s tombstone back.
    pub fn restore_message(&mut self, id: &str) -> Result<()> {
        let m = self.message(id)?;
        if m.get(self.txn, DELETED_AT).is_some() {
            m.remove(self.txn, DELETED_AT);
        }
        Ok(())
    }

    /// Close the whole discussion (contract section 4).
    pub fn resolve(&mut self, stamp: Stamp<'_>) -> Result<()> {
        let r: MapRef = self.map.insert(self.txn, RESOLVED, MapPrelim::default());
        r.insert(self.txn, AT, stamp.now.to_string());
        if let Some(author) = stamp.author {
            r.insert(self.txn, BY_ID, author.id.clone());
            r.insert(self.txn, BY_NAME, author.name.clone());
        }
        Ok(())
    }

    /// Open it again.
    pub fn reopen(&mut self) -> Result<()> {
        if self.map.get(self.txn, RESOLVED).is_some() {
            self.map.remove(self.txn, RESOLVED);
        }
        Ok(())
    }

    /// Move the discussion to another place on its host (a pin dragged), or
    /// to the node as a whole (`None`).
    pub fn set_anchor(&mut self, anchor: Option<&Anchor>) -> Result<()> {
        match anchor {
            Some(anchor) => write_anchor(self.txn, &self.map, anchor),
            None => {
                if self.map.get(self.txn, ANCHOR).is_some() {
                    self.map.remove(self.txn, ANCHOR);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::updates::decoder::Decode;
    use yrs::{StateVector, Update};

    const T0: &str = "2026-10-21T10:00:00.000Z";
    const T1: &str = "2026-10-21T10:00:01.000Z";
    const T2: &str = "2026-10-21T10:00:02.000Z";

    fn alice() -> Author {
        Author::new("sub-alice", "Alice")
    }

    fn bob() -> Author {
        Author::new("sub-bob", "Bob")
    }

    fn fresh() -> Discussion {
        let d = Discussion::standalone();
        apply(&d, &new_content(None));
        d
    }

    fn apply(d: &Discussion, update: &[u8]) {
        d.doc.transact_mut().apply_update(Update::decode_v1(update).unwrap()).unwrap();
    }

    fn replica(d: &Discussion) -> Discussion {
        let r = Discussion::standalone();
        apply(&r, &d.doc.transact().encode_diff_v1(&StateVector::default()));
        r
    }

    fn sync(a: &Discussion, b: &Discussion) {
        let (sa, sb) = (a.doc.transact().state_vector(), b.doc.transact().state_vector());
        let to_b = a.doc.transact().encode_diff_v1(&sb);
        let to_a = b.doc.transact().encode_diff_v1(&sa);
        apply(b, &to_b);
        apply(a, &to_a);
    }

    fn post(d: &Discussion, who: &Author, at: &str, reply_to: Option<&str>, md: &str) -> String {
        d.edit(|t| t.post(Stamp::new(Some(who), at), reply_to, &Body::from_markdown(md))).into_result().unwrap().0
    }

    #[test]
    fn a_discussion_without_its_containers_refuses_and_writes_nothing() {
        let d = Discussion::standalone();
        let edited = d.edit(|t| t.post(Stamp::new(None, T0), None, &Body::plain("hi")));
        assert_eq!(edited.result, Err(DiscussionError::NotReady));
        assert!(edited.update.is_none());
        assert!(d.read().messages.is_empty());
    }

    #[test]
    fn posts_and_replies_read_back_with_authors_and_formatting() {
        let d = fresh();
        let a = post(&d, &alice(), T0, None, "Hello **there**");
        let b = post(&d, &bob(), T1, Some(&a), "_hi_ [docs](https://x.org)");
        let snap = d.read();
        assert_eq!(snap.messages.len(), 2);
        let first = snap.message(&a).unwrap();
        assert_eq!(first.author, Some(alice()));
        assert_eq!(first.body, Body::from_markdown("Hello **there**"));
        let second = snap.message(&b).unwrap();
        assert_eq!(second.reply_to.as_deref(), Some(a.as_str()));
        assert_eq!(second.body.links(), vec!["https://x.org"]);
    }

    #[test]
    fn a_blank_message_or_a_reply_to_nothing_is_refused() {
        let d = fresh();
        assert_eq!(d.edit(|t| t.post(Stamp::new(None, T0), None, &Body::plain("  "))).result, Err(DiscussionError::Empty));
        assert!(matches!(d.edit(|t| t.post(Stamp::new(None, T0), Some("nope"), &Body::plain("x"))).result, Err(DiscussionError::NoMessage(_))));
        assert!(d.read().messages.is_empty());
    }

    #[test]
    fn concurrent_replies_and_edits_merge_on_three_replicas() {
        let a = fresh();
        let root = post(&a, &alice(), T0, None, "Question?");
        let b = replica(&a);
        let c = replica(&a);
        // Everyone replies at once; Alice edits her question on two devices.
        let ra = post(&a, &alice(), T1, Some(&root), "From a");
        let rb = post(&b, &bob(), T1, Some(&root), "From b");
        let rc = post(&c, &bob(), T1, Some(&root), "From c");
        a.edit(|t| t.edit_message(Stamp::new(Some(&alice()), T2), &root, &Body::from_markdown("Question, **really**?"))).into_result().unwrap();
        c.edit(|t| t.edit_message(Stamp::new(Some(&alice()), T2), &root, &Body::from_markdown("The question?"))).into_result().unwrap();
        b.edit(|t| t.delete_message(Stamp::new(Some(&bob()), T2), &rb)).into_result().unwrap();
        for _ in 0..2 {
            sync(&a, &b);
            sync(&b, &c);
            sync(&a, &c);
        }
        let (sa, sb, sc) = (a.read(), b.read(), c.read());
        assert_eq!(sa, sb);
        assert_eq!(sb, sc);
        let ids: Vec<&str> = sa.thread().iter().map(|item| sa.messages[item.index].id.as_str()).collect();
        assert_eq!(ids.len(), 3, "the question and the two replies not deleted");
        assert_eq!(ids[0], root);
        for reply in [&ra, &rc] {
            assert!(ids.contains(&reply.as_str()));
        }
        assert!(sa.message(&rb).unwrap().deleted_at.is_some(), "the deleted reply is still held");
        // Both edits' text survived the merge, character by character.
        let text = sa.message(&root).unwrap().body.text();
        assert!(text.contains("really") && text.contains("The"), "{text:?}");
    }

    #[test]
    fn an_edit_changes_only_what_differs_and_reformats() {
        let d = fresh();
        let m = post(&d, &alice(), T0, None, "keep this part and that 👍");
        let before = d.doc.transact().state_vector();
        d.edit(|t| t.edit_message(Stamp::new(Some(&alice()), T1), &m, &Body::from_markdown("keep **this** part and that 👍!"))).into_result().unwrap();
        let snap = d.read();
        let msg = snap.message(&m).unwrap();
        assert_eq!(msg.body, Body::from_markdown("keep **this** part and that 👍!"));
        assert_eq!(msg.edited.as_deref(), Some(T1));
        // A small edit is a small update.
        let diff = d.doc.transact().encode_diff_v1(&before);
        assert!(diff.len() < 200, "{} bytes", diff.len());
    }

    #[test]
    fn delete_restore_resolve_reopen() {
        let d = fresh();
        let m = post(&d, &alice(), T0, None, "x");
        d.edit(|t| t.delete_message(Stamp::new(None, T1), &m)).into_result().unwrap();
        assert!(d.read().message(&m).unwrap().deleted_at.is_some());
        assert_eq!(d.edit(|t| t.edit_message(Stamp::new(None, T1), &m, &Body::plain("y"))).result, Err(DiscussionError::Deleted));
        d.edit(|t| t.restore_message(&m)).into_result().unwrap();
        assert!(d.read().message(&m).unwrap().deleted_at.is_none());
        d.edit(|t| t.resolve(Stamp::new(Some(&bob()), T2))).into_result().unwrap();
        assert_eq!(d.read().resolved, Some(Resolved { at: T2.into(), by: Some(bob()) }));
        d.edit(|t| t.reopen()).into_result().unwrap();
        assert_eq!(d.read().resolved, None);
    }

    #[test]
    fn anchors_round_trip() {
        let d = Discussion::standalone();
        apply(&d, &new_content(Some(&Anchor::Text { sticky: vec![1, 2, 3] })));
        assert_eq!(d.read().anchor, Some(Anchor::Text { sticky: vec![1, 2, 3] }));
        let board = Anchor::Board { board: "board:abc".into(), x: 10.5, y: -3.0 };
        d.edit(|t| t.set_anchor(Some(&board))).into_result().unwrap();
        assert_eq!(d.read().anchor, Some(board));
        d.edit(|t| t.set_anchor(None)).into_result().unwrap();
        assert_eq!(d.read().anchor, None);
    }

    #[test]
    fn a_node_document_holds_its_discussion() {
        let mut node = NodeDoc::new();
        node.apply_update(&new_content(None)).unwrap();
        // A peer holding the same document before the post.
        let mut third = NodeDoc::load(&node.save()).unwrap();
        let d = Discussion::of_node(&node);
        let (_, update) = d.edit(|t| t.post(Stamp::new(None, T0), None, &Body::plain("in a node"))).into_result().unwrap();
        let mut other = NodeDoc::load(&node.save()).unwrap();
        assert_eq!(Discussion::of_node(&other).read().messages.len(), 1);
        // The update is an ordinary update of the node's document.
        let effect = third.apply_update(&update.unwrap()).unwrap();
        assert!(effect.changed);
        other.apply_update(&node.save()).unwrap();
    }
}

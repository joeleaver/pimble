//! A discussion read as a thread (docs/DISCUSSIONS_CONTRACT.md section 2):
//! every message once, in the order every device shows it.
//!
//! - Siblings (messages replying to the same one) sort by `(created, id)`.
//! - A reply whose parent is not here yet (it arrived first in a merge) is
//!   shown at the top level and marked `waiting`, until its parent arrives.
//! - A `reply_to` that names its own descendant can only come from
//!   tampering: the cycle is broken at the member with the smallest id, which
//!   is shown at the top level. Nothing is written to fix it.
//! - A deleted message with no reply that is shown is left out; one with
//!   replies stays, as "Message deleted", so its replies keep their place.
//!
//! Depth is the true depth; the view indents five levels and shows deeper
//! replies flat (Joe, 2026-10-21).

use std::collections::{HashMap, HashSet};

use pimble_core::{IndexUnit, UnitKind};

use crate::body::Body;
use crate::doc::{Anchor, Author};

/// A message as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageSnap {
    pub id: String,
    pub reply_to: Option<String>,
    pub created: String,
    pub author: Option<Author>,
    pub body: Body,
    pub edited: Option<String>,
    pub deleted_at: Option<String>,
}

/// Who closed the discussion, and when.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub at: String,
    pub by: Option<Author>,
}

/// One line of the thread.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadItem {
    /// Into [`DiscussionSnapshot::messages`].
    pub index: usize,
    /// 0 at the top level.
    pub depth: usize,
    /// Replies to a message that is not here (yet), shown at the top level.
    pub waiting: bool,
}

/// What a discussion holds, read in one transaction.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiscussionSnapshot {
    /// Every message, sorted by `(created, id)`.
    pub messages: Vec<MessageSnap>,
    pub resolved: Option<Resolved>,
    pub anchor: Option<Anchor>,
}

fn order_key(m: &MessageSnap) -> (&str, &str) {
    (m.created.as_str(), m.id.as_str())
}

impl DiscussionSnapshot {
    pub fn new(mut messages: Vec<MessageSnap>, resolved: Option<Resolved>, anchor: Option<Anchor>) -> Self {
        messages.sort_by(|a, b| order_key(a).cmp(&order_key(b)));
        Self { messages, resolved, anchor }
    }

    pub fn message(&self, id: &str) -> Option<&MessageSnap> {
        self.messages.iter().find(|m| m.id == id)
    }

    /// The messages that are not deleted.
    pub fn live_count(&self) -> usize {
        self.messages.iter().filter(|m| m.deleted_at.is_none()).count()
    }

    /// The most recent message that is not deleted.
    pub fn latest(&self) -> Option<&MessageSnap> {
        self.messages.iter().rev().find(|m| m.deleted_at.is_none())
    }

    /// The deleted messages, newest deletion first ("Recently deleted messages").
    pub fn deleted(&self) -> Vec<&MessageSnap> {
        let mut out: Vec<&MessageSnap> = self.messages.iter().filter(|m| m.deleted_at.is_some()).collect();
        out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
        out
    }

    /// The people who wrote here, each once, in order of their first message.
    pub fn authors(&self) -> Vec<&Author> {
        let mut seen = HashSet::new();
        self.messages.iter().filter_map(|m| m.author.as_ref()).filter(|a| seen.insert(a.id.as_str())).collect()
    }

    /// The thread in display order (the module's rules).
    pub fn thread(&self) -> Vec<ThreadItem> {
        let index_of: HashMap<&str, usize> = self.messages.iter().enumerate().map(|(i, m)| (m.id.as_str(), i)).collect();
        let parent_of = |i: usize| -> Option<usize> {
            let m = &self.messages[i];
            m.reply_to.as_deref().and_then(|p| index_of.get(p).copied()).filter(|&p| p != i)
        };

        // Cycles: walk up from each message; a walk that comes back to a
        // message already on its path found a cycle, broken at its smallest id.
        let mut cut: HashSet<usize> = HashSet::new();
        let mut settled: HashSet<usize> = HashSet::new();
        for start in 0..self.messages.len() {
            let mut path: Vec<usize> = Vec::new();
            let mut on_path: HashSet<usize> = HashSet::new();
            let mut cur = Some(start);
            while let Some(i) = cur {
                if settled.contains(&i) || cut.contains(&i) {
                    break;
                }
                if !on_path.insert(i) {
                    let from = path.iter().position(|&p| p == i).unwrap_or(0);
                    let smallest = path[from..].iter().copied().min_by(|&a, &b| self.messages[a].id.cmp(&self.messages[b].id)).unwrap_or(i);
                    cut.insert(smallest);
                    break;
                }
                path.push(i);
                cur = parent_of(i);
            }
            settled.extend(path);
        }

        let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut tops: Vec<(usize, bool)> = Vec::new();
        for i in 0..self.messages.len() {
            match parent_of(i).filter(|_| !cut.contains(&i)) {
                Some(p) => children.entry(p).or_default().push(i),
                None => {
                    let waiting = self.messages[i].reply_to.as_deref().is_some_and(|p| !index_of.contains_key(p));
                    tops.push((i, waiting));
                }
            }
        }
        // `messages` is in order already, so every list is too.

        // A deleted message is shown only when something under it is.
        let mut shown: HashMap<usize, bool> = HashMap::new();
        fn shows(i: usize, msgs: &[MessageSnap], children: &HashMap<usize, Vec<usize>>, shown: &mut HashMap<usize, bool>) -> bool {
            if let Some(&s) = shown.get(&i) {
                return s;
            }
            shown.insert(i, false);
            let below = children.get(&i).map(|c| c.iter().fold(false, |any, &k| shows(k, msgs, children, shown) || any)).unwrap_or(false);
            let s = msgs[i].deleted_at.is_none() || below;
            shown.insert(i, s);
            s
        }
        let mut out = Vec::new();
        let mut stack: Vec<(usize, usize, bool)> = tops.iter().rev().map(|&(i, w)| (i, 0, w)).collect();
        while let Some((i, depth, waiting)) = stack.pop() {
            if !shows(i, &self.messages, &children, &mut shown) {
                continue;
            }
            out.push(ThreadItem { index: i, depth, waiting });
            if let Some(kids) = children.get(&i) {
                for &k in kids.iter().rev() {
                    stack.push((k, depth + 1, false));
                }
            }
        }
        out
    }

    /// What search indexes of a discussion (contract section 9): one unit per
    /// message that is not deleted, its author's name with its text.
    pub fn index_units(&self) -> Vec<IndexUnit> {
        self.messages
            .iter()
            .filter(|m| m.deleted_at.is_none())
            .map(|m| IndexUnit {
                kind: UnitKind::Prose,
                path: format!("m:{}", m.id),
                text: match &m.author {
                    Some(a) if !a.name.is_empty() => format!("{}: {}", a.name, m.body.text()),
                    _ => m.body.text(),
                },
            })
            .collect()
    }

    /// The thread as Markdown for an LLM (MCP `read_discussion`): one bullet
    /// per message, nested by reply, each with its id, author and time.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        for item in self.thread() {
            let m = &self.messages[item.index];
            let indent = "  ".repeat(item.depth);
            let who = m.author.as_ref().map(|a| a.name.as_str()).filter(|n| !n.is_empty()).unwrap_or("someone");
            let body = if m.deleted_at.is_some() { "(message deleted)".to_string() } else { m.body.to_markdown().replace('\n', &format!("\n{indent}  ")) };
            let edited = if m.edited.is_some() { " (edited)" } else { "" };
            let waiting = if item.waiting { " (replying to a message not here yet)" } else { "" };
            out.push_str(&format!("{indent}- [{}] {who}, {}{edited}{waiting}: {body}\n", m.id, m.created));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, reply_to: Option<&str>, created: &str) -> MessageSnap {
        MessageSnap { id: id.into(), reply_to: reply_to.map(str::to_string), created: created.into(), author: None, body: Body::plain(id), edited: None, deleted_at: None }
    }

    fn order(snap: &DiscussionSnapshot) -> Vec<(String, usize, bool)> {
        snap.thread().into_iter().map(|t| (snap.messages[t.index].id.clone(), t.depth, t.waiting)).collect()
    }

    fn s(id: &str, depth: usize) -> (String, usize, bool) {
        (id.to_string(), depth, false)
    }

    #[test]
    fn siblings_sort_by_time_then_id_and_nest_by_reply() {
        let snap = DiscussionSnapshot::new(
            vec![msg("b", None, "2"), msg("a", None, "1"), msg("c", Some("a"), "3"), msg("e", Some("a"), "3"), msg("d", Some("c"), "4")],
            None,
            None,
        );
        assert_eq!(order(&snap), vec![s("a", 0), s("c", 1), s("d", 2), s("e", 1), s("b", 0)]);
    }

    #[test]
    fn a_reply_before_its_parent_waits_at_the_top() {
        let snap = DiscussionSnapshot::new(vec![msg("a", None, "1"), msg("r", Some("gone"), "2")], None, None);
        assert_eq!(order(&snap), vec![s("a", 0), ("r".into(), 0, true)]);
    }

    #[test]
    fn a_cycle_is_broken_at_its_smallest_id() {
        let snap = DiscussionSnapshot::new(vec![msg("x", Some("z"), "1"), msg("z", Some("y"), "2"), msg("y", Some("x"), "3")], None, None);
        assert_eq!(order(&snap), vec![s("x", 0), s("y", 1), s("z", 2)]);
        let me = DiscussionSnapshot::new(vec![msg("m", Some("m"), "1")], None, None);
        assert_eq!(order(&me), vec![s("m", 0)]);
    }

    #[test]
    fn a_deleted_message_shows_only_while_a_reply_does() {
        let mut gone = msg("a", None, "1");
        gone.deleted_at = Some("9".into());
        let mut gone_reply = msg("c", Some("a"), "3");
        gone_reply.deleted_at = Some("9".into());
        let snap = DiscussionSnapshot::new(vec![gone.clone(), msg("b", Some("a"), "2"), gone_reply], None, None);
        assert_eq!(order(&snap), vec![s("a", 0), s("b", 1)]);
        let alone = DiscussionSnapshot::new(vec![gone], None, None);
        assert!(order(&alone).is_empty());
    }

    #[test]
    fn units_and_markdown_name_every_live_message() {
        let mut m = msg("a", None, "1");
        m.author = Some(Author::new("sub", "Alice"));
        let snap = DiscussionSnapshot::new(vec![m, msg("b", Some("a"), "2")], None, None);
        let units = snap.index_units();
        assert_eq!(units[0].text, "Alice: a");
        assert_eq!(units[1].path, "m:b");
        let md = snap.to_markdown();
        assert!(md.contains("- [a] Alice, 1: a") && md.contains("  - [b] someone, 2: b"), "{md}");
    }
}

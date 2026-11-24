//! What a write tells (section 2): the mentions it added and the messages
//! it wrote in a discussion others wrote in.

use std::collections::HashSet;

use pimble_core::mention::Mention;
use pimble_core::NodeId;
use pimble_crdt::NodeDoc;
use pimble_rpc::NoticeKind;

use super::SNIPPET_CHARS;

/// The text a mention's href starts with, as its bytes appear in a yrs
/// update that writes one: a write without them adds no mention.
const MENTION_BYTES: &[u8] = pimble_core::mention::SCHEME.as_bytes();

/// Whether an update could add a mention (the cheap test before any
/// document is read): it carries a mention's href.
pub(crate) fn may_mention(update: &[u8]) -> bool {
    update.windows(MENTION_BYTES.len()).any(|w| w == MENTION_BYTES)
}

/// One notice a write calls for, before it is addressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub kind: NoticeKind,
    pub recipient: String,
    /// What makes this notice this one within the store: the node and the
    /// mention (or the discussion and the message). The recipient is added.
    pub key: String,
    /// The node it opens: where the mention is, a discussion's host.
    pub node: NodeId,
    pub discussion: Option<NodeId>,
    pub anchor: Option<String>,
    pub snippet: String,
}

/// The notices a write calls for: `before` and `after` are the whole
/// document of `node` around it (`before` `None` for a document the write
/// made). Mentions of the writer and replies to themselves are left in; the
/// caller knows the account and leaves them out.
pub(crate) fn candidates(node: NodeId, before: Option<&[u8]>, after: &[u8]) -> Vec<Candidate> {
    let Ok(after_doc) = NodeDoc::load(after) else { return Vec::new() };
    let before_doc = before.and_then(|b| NodeDoc::load(b).ok());
    let discussion = pimble_discussion::Discussion::of_node(&after_doc);
    if discussion.exists() {
        let host = after_doc.fields().ok().and_then(|f| f.attached_to).unwrap_or(node);
        let before_snap = before_doc.as_ref().map(|d| pimble_discussion::Discussion::of_node(d).read());
        return message_candidates(node, host, before_snap.as_ref(), &discussion.read());
    }
    let before_mentions = before_doc.as_ref().map(|d| d.mentions()).unwrap_or_default();
    let added = pimble_core::mention::added(&before_mentions, &after_doc.mentions(), None);
    if added.is_empty() {
        return Vec::new();
    }
    let text = after_doc.text();
    let placed = pimble_crdt::mentions::placed_in(after);
    added
        .into_iter()
        .map(|m| Candidate {
            kind: NoticeKind::Mention,
            key: format!("{node}/{}", mention_key(&m)),
            recipient: m.account.clone(),
            node,
            discussion: None,
            // Around this mention, not the first words like it: a note can
            // name the same person many times.
            snippet: snippet_at(&text, placed.iter().find(|(p, _)| *p == m).map(|(_, at)| *at).unwrap_or(0)),
            anchor: m.id,
        })
        .collect()
}

fn mention_key(m: &Mention) -> String {
    match &m.id {
        Some(id) => format!("m:{id}"),
        None => format!("a:{}", m.account),
    }
}

/// A discussion's: each new message's mentions, and a reply to everyone
/// else who wrote in it and is not mentioned; an edited message's added
/// mentions.
fn message_candidates(
    discussion: NodeId,
    host: NodeId,
    before: Option<&pimble_discussion::DiscussionSnapshot>,
    after: &pimble_discussion::DiscussionSnapshot,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for message in after.messages.iter().filter(|m| m.deleted_at.is_none()) {
        let old = before.and_then(|b| b.message(&message.id));
        let author = message.author.as_ref().map(|a| a.id.as_str());
        let snippet = clip(&message.body.text(), SNIPPET_CHARS);
        let old_mentions = old.map(|o| o.body.mentions()).unwrap_or_default();
        let added = pimble_core::mention::added(&old_mentions, &message.body.mentions(), author);
        let mentioned: HashSet<String> = message.body.mentions().into_iter().map(|m| m.account).collect();
        for m in added {
            out.push(Candidate {
                kind: NoticeKind::Mention,
                key: format!("{discussion}/{}", mention_key(&m)),
                recipient: m.account.clone(),
                node: host,
                discussion: Some(discussion),
                anchor: Some(message.id.clone()),
                snippet: snippet.clone(),
            });
        }
        if old.is_some() {
            continue;
        }
        // A new message: everyone else who wrote in the thread hears of it,
        // unless it mentions them (then the mention says it).
        let mut told = HashSet::new();
        for other in after.messages.iter().filter(|o| o.id != message.id) {
            let Some(who) = other.author.as_ref() else { continue };
            if who.is_agent() || Some(who.id.as_str()) == author || mentioned.contains(&who.id) || !told.insert(who.id.clone()) {
                continue;
            }
            out.push(Candidate {
                kind: NoticeKind::Reply,
                key: format!("{discussion}/r:{}", message.id),
                recipient: who.id.clone(),
                node: host,
                discussion: Some(discussion),
                anchor: Some(message.id.clone()),
                snippet: snippet.clone(),
            });
        }
    }
    out
}

/// About [`SNIPPET_CHARS`] of `text` around the char at `at` (where a
/// mention starts), on one line, with an ellipsis where it was cut.
pub(crate) fn snippet_at(text: &str, at: usize) -> String {
    let chars: Vec<char> = one_line(text).chars().collect();
    // Where `at` falls once the text is on one line: the words before it,
    // and the space after them when it starts a word of its own.
    let before: String = text.chars().take(at).collect();
    let words_before = one_line(&before);
    let gap = usize::from(!words_before.is_empty() && before.ends_with(char::is_whitespace));
    let at = words_before.chars().count() + gap;
    // A third before the mention at most, never more to fill the window in
    // a short note or near the end: the app opens a notice at the words from
    // the snippet's first `@` (`inbox::target_of`), so words reaching back to
    // an earlier mention would open that one.
    let start = at.saturating_sub(SNIPPET_CHARS / 3).min(chars.len());
    let end = (start + SNIPPET_CHARS).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// `text`'s words on one line, one space between each.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, max: usize) -> String {
    let flat = one_line(text);
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_discussion::{Author, Body, Stamp};

    fn note_with(text_md: &str) -> Vec<u8> {
        let mut doc = NodeDoc::from_plain_text("").unwrap();
        doc.write_markdown(text_md).expect("markdown applies");
        doc.save()
    }

    #[test]
    fn a_mention_added_to_a_note_is_a_notice_once_and_a_removal_or_an_unchanged_one_is_not() {
        let node = NodeId::new();
        let before = note_with("Hello there.");
        let after = note_with("Hello there. [@Will](pimble-person:will?m=abc123) can you look?");
        let got = candidates(node, Some(&before), &after);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!((got[0].kind, got[0].recipient.as_str(), got[0].anchor.as_deref()), (NoticeKind::Mention, "will", Some("abc123")));
        assert_eq!(got[0].key, format!("{node}/m:abc123"));
        assert!(got[0].snippet.contains("@Will can you look?"), "{}", got[0].snippet);
        assert!(candidates(node, Some(&after), &after).is_empty(), "nothing new");
        assert!(candidates(node, Some(&after), &before).is_empty(), "taking it out tells nobody");
        assert_eq!(candidates(node, None, &after).len(), 1, "a new note's mentions count");
        assert!(may_mention(&after) && !may_mention(&before));
    }

    #[test]
    fn a_new_message_tells_the_mentioned_and_everyone_else_who_wrote_in_the_thread() {
        use yrs::updates::decoder::Decode;
        use yrs::Transact;
        let (alice, bob, carol) = (Author::new("alice", "Alice"), Author::new("bob", "Bob"), Author::new("carol", "Carol"));
        let robot = Author::new("agent:1", "Claude");
        let d = pimble_discussion::Discussion::standalone();
        d.doc().transact_mut().apply_update(yrs::Update::decode_v1(&pimble_discussion::new_content(None)).unwrap()).unwrap();
        let first = d.edit(|t| t.post(Stamp::new(Some(&alice), "2026-11-23T10:00:00.000Z"), None, &Body::plain("first"))).into_result().unwrap().0;
        d.edit(|t| t.post(Stamp::new(Some(&bob), "2026-11-23T10:00:01.000Z"), Some(&first), &Body::plain("second"))).into_result().unwrap();
        d.edit(|t| t.post(Stamp::new(Some(&robot), "2026-11-23T10:00:02.000Z"), None, &Body::plain("an agent's"))).into_result().unwrap();
        let before = snapshot_bytes(&d);
        let body = Body::from_markdown("[@Bob](pimble-person:bob?m=m1) look at this");
        let new = d.edit(|t| t.post(Stamp::new(Some(&carol), "2026-11-23T10:00:03.000Z"), None, &body)).into_result().unwrap().0;
        let after = snapshot_bytes(&d);
        let node = NodeId::new();
        let mut got = candidates(node, Some(&before), &after);
        got.sort_by(|a, b| a.recipient.cmp(&b.recipient));
        let who: Vec<(&str, NoticeKind)> = got.iter().map(|c| (c.recipient.as_str(), c.kind)).collect();
        assert_eq!(who, vec![("alice", NoticeKind::Reply), ("bob", NoticeKind::Mention)], "Bob is mentioned, Alice wrote in it, the agent is nobody's inbox");
        assert!(got.iter().all(|c| c.anchor.as_deref() == Some(new.as_str()) && c.discussion == Some(node) && c.node == node));
        assert_eq!(got[0].snippet, "@Bob look at this");
        assert!(candidates(node, Some(&after), &after).is_empty());
    }

    fn snapshot_bytes(d: &pimble_discussion::Discussion) -> Vec<u8> {
        use yrs::{ReadTxn, StateVector, Transact};
        d.doc().transact().encode_state_as_update_v1(&StateVector::default())
    }

    #[test]
    fn a_snippet_is_about_120_characters_around_the_mention() {
        let long = format!("{} @Will here {}", "a".repeat(300), "b".repeat(300));
        let s = snippet_at(&long, 301);
        assert!(s.contains("@Will here"), "{s}");
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert!(s.chars().count() <= SNIPPET_CHARS + 2);
        assert_eq!(snippet_at("short\n\ntext", 0), "short text");
        let short = format!("{} @Will hi", "x".repeat(60));
        assert_eq!(snippet_at(&short, 61), format!("…{} @Will hi", "x".repeat(39)), "a short note is cut too, the mention near the start");
        // Runs of whitespace before the place fold as the text does.
        let spaced = format!("{}\n\n   @Will there {}", "a ".repeat(150), "b".repeat(300));
        let at = spaced.find('@').unwrap();
        assert!(snippet_at(&spaced, at).contains("@Will there"));
    }

    #[test]
    fn a_second_mention_of_the_same_person_is_told_from_its_own_words() {
        // Found walking the notifications through on the local stack: the
        // second mention of Ben in a note said the words around the first,
        // and opening it selected the first.
        let node = NodeId::new();
        let first = "We fly out on Friday. The hotel is booked near the old harbour.\n\n\
                     [@Ben](pimble-person:ben?m=one) can you check the hotel dates before Friday?";
        let before = note_with(first);
        let after = note_with(&format!("{first}\n\n[@Ben](pimble-person:ben?m=two) the window is closed, does this still reach you?"));
        let got = candidates(node, Some(&before), &after);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].anchor.as_deref(), Some("two"));
        assert!(got[0].snippet.contains("@Ben the window is closed, does this still reach you?"), "{}", got[0].snippet);
        let from_the_at = &got[0].snippet[got[0].snippet.find('@').unwrap()..];
        assert!(from_the_at.starts_with("@Ben the window"), "the words from the first @ are this mention's: {from_the_at}");
    }
}

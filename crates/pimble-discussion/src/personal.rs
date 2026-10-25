//! The personal document (docs/DISCUSSIONS_CONTRACT.md sections 8 and 8b):
//! one yrs document per person, edited only by their own devices, never
//! shared. It holds what they have read in discussions.
//!
//! ```text
//! root map "read":   Map<"<store id>/<discussion id>", Map {
//!                       watermark: String?   every message created at or before it is read
//!                       seen: Map<message id, true>   read messages newer than the watermark
//!                    }>
//! root map "places": no longer written: where someone was in each node is
//!                    kept per device (Joe, 2026-10-23), and what earlier
//!                    versions wrote is emptied (`drop_places`)
//! ```
//!
//! They are root maps, so no container is made on first use at the top; each
//! entry is one device's own map, and two devices making the same entry while
//! apart keep one of them whole, which for read state costs at most a few
//! messages shown unread again, never one shown read that was not.

use std::collections::HashSet;

use yrs::updates::decoder::Decode;
use yrs::{Any, Doc, Map, MapPrelim, MapRef, OffsetKind, Out, ReadTxn, StateVector, Transact, Update};

use crate::thread::DiscussionSnapshot;

const READ: &str = "read";
const PLACES: &str = "places";
const WATERMARK: &str = "watermark";
const SEEN: &str = "seen";

fn key(store: &str, node: &str) -> String {
    format!("{store}/{node}")
}

fn string_of(out: Option<Out>) -> Option<String> {
    match out {
        Some(Out::Any(Any::String(s))) => Some(s.to_string()),
        _ => None,
    }
}

/// The personal document.
pub struct Personal {
    doc: Doc,
    read: MapRef,
    places: MapRef,
}

impl Default for Personal {
    fn default() -> Self {
        Self::new()
    }
}

impl Personal {
    pub fn new() -> Self {
        let doc = Doc::with_options(yrs::Options { offset_kind: OffsetKind::Utf16, ..Default::default() });
        let read = doc.get_or_insert_map(READ);
        let places = doc.get_or_insert_map(PLACES);
        Self { doc, read, places }
    }

    /// From saved bytes; an empty or unreadable save is an empty document.
    pub fn load(bytes: &[u8]) -> Self {
        let p = Self::new();
        if !bytes.is_empty() {
            if let Ok(update) = Update::decode_v1(bytes) {
                if p.doc.transact_mut().apply_update(update).is_err() {
                    tracing_free_warn();
                }
            }
        }
        p
    }

    pub fn save(&self) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(&StateVector::default())
    }

    /// Merge another copy (another device's, or one saved earlier).
    pub fn merge(&self, bytes: &[u8]) -> bool {
        let Ok(update) = Update::decode_v1(bytes) else { return false };
        self.doc.transact_mut().apply_update(update).is_ok()
    }

    pub fn state_vector(&self) -> Vec<u8> {
        use yrs::updates::encoder::Encode;
        self.doc.transact().state_vector().encode_v1()
    }

    /// What this copy has that a copy at `sv` lacks.
    pub fn diff_since(&self, sv: &[u8]) -> Vec<u8> {
        let sv = StateVector::decode_v1(sv).unwrap_or_default();
        self.doc.transact().encode_diff_v1(&sv)
    }

    // ── Read state ──────────────────────────────────────────────────────

    fn entry<T: ReadTxn>(&self, txn: &T, store: &str, discussion: &str) -> Option<MapRef> {
        match self.read.get(txn, &key(store, discussion)) {
            Some(Out::YMap(m)) => Some(m),
            _ => None,
        }
    }

    /// Whether message `id`, created at `created`, has been read.
    fn is_read<T: ReadTxn>(&self, txn: &T, entry: Option<&MapRef>, id: &str, created: &str) -> bool {
        let Some(entry) = entry else { return false };
        if string_of(entry.get(txn, WATERMARK)).is_some_and(|w| created <= w.as_str()) {
            return true;
        }
        matches!(entry.get(txn, SEEN), Some(Out::YMap(seen)) if seen.get(txn, id).is_some())
    }

    /// The live messages of `snap` not read yet, and not written by `me`
    /// (one's own messages are read as they are sent), in thread order. A
    /// message with no author is one's own too: nobody is stamped only when
    /// one person uses the store (contract section 3).
    pub fn unread(&self, store: &str, discussion: &str, snap: &DiscussionSnapshot, me: Option<&str>) -> Vec<String> {
        let txn = self.doc.transact();
        let entry = self.entry(&txn, store, discussion);
        snap.messages
            .iter()
            .filter(|m| m.deleted_at.is_none())
            .filter(|m| m.author.as_ref().is_some_and(|a| Some(a.id.as_str()) != me))
            .filter(|m| !self.is_read(&txn, entry.as_ref(), &m.id, &m.created))
            .map(|m| m.id.clone())
            .collect()
    }

    /// Mark messages `ids` of `snap` read; the watermark moves up to the
    /// newest message up to which everything is read, and the ids at or
    /// below it are dropped. `true` when something changed.
    pub fn mark_read(&self, store: &str, discussion: &str, snap: &DiscussionSnapshot, ids: &[String]) -> bool {
        if ids.is_empty() {
            return false;
        }
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let mut txn = self.doc.transact_mut();
        let k = key(store, discussion);
        let entry = match self.read.get(&txn, &k) {
            Some(Out::YMap(m)) => m,
            _ => {
                let m: MapRef = self.read.insert(&mut txn, k.as_str(), MapPrelim::default());
                m.insert(&mut txn, SEEN, MapPrelim::default());
                m
            }
        };
        let seen = match entry.get(&txn, SEEN) {
            Some(Out::YMap(s)) => s,
            _ => entry.insert(&mut txn, SEEN, MapPrelim::default()),
        };
        let mut changed = false;
        let watermark = string_of(entry.get(&txn, WATERMARK));
        for m in &snap.messages {
            if wanted.contains(m.id.as_str())
                && watermark.as_deref().is_none_or(|w| m.created.as_str() > w)
                && seen.get(&txn, &m.id).is_none()
            {
                seen.insert(&mut txn, m.id.as_str(), true);
                changed = true;
            }
        }
        // Compact: the longest run of read messages from the oldest.
        let mut new_mark: Option<&str> = None;
        for m in &snap.messages {
            let read = watermark.as_deref().is_some_and(|w| m.created.as_str() <= w) || seen.get(&txn, &m.id).is_some() || m.deleted_at.is_some();
            if !read {
                break;
            }
            new_mark = Some(m.created.as_str());
        }
        if let Some(mark) = new_mark {
            if watermark.as_deref().is_none_or(|w| mark > w) {
                entry.insert(&mut txn, WATERMARK, mark.to_string());
                let drop: Vec<String> = snap.messages.iter().filter(|m| m.created.as_str() <= mark).map(|m| m.id.clone()).collect();
                for id in drop {
                    if seen.get(&txn, &id).is_some() {
                        seen.remove(&mut txn, &id);
                    }
                }
                changed = true;
            }
        }
        changed
    }

    // ── Places ──────────────────────────────────────────────────────────

    /// Empty the places earlier versions kept here; they are per device now
    /// (docs/DISCUSSIONS_CONTRACT.md section 8b). `true` when there were any.
    pub fn drop_places(&self) -> bool {
        let mut txn = self.doc.transact_mut();
        if self.places.len(&txn) == 0 {
            return false;
        }
        self.places.clear(&mut txn);
        true
    }
}

fn tracing_free_warn() {
    // The crate has no logger; an unreadable save is read as empty.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::Body;
    use crate::doc::{new_content, Author, Discussion, Stamp};

    fn discussion_with(n: usize, who: &Author) -> (Discussion, Vec<String>) {
        let d = Discussion::standalone();
        {
            let update = Update::decode_v1(&new_content(None)).unwrap();
            d.doc().transact_mut().apply_update(update).unwrap();
        }
        let mut ids = Vec::new();
        for i in 0..n {
            let at = format!("2026-10-21T10:00:0{i}.000Z");
            ids.push(d.edit(|t| t.post(Stamp::new(Some(who), &at), None, &Body::plain(format!("m{i}")))).into_result().unwrap().0);
        }
        (d, ids)
    }

    #[test]
    fn reading_moves_the_watermark_and_forgets_the_ids_below_it() {
        let bob = Author::new("bob", "Bob");
        let (d, ids) = discussion_with(4, &bob);
        let snap = d.read();
        let p = Personal::new();
        assert_eq!(p.unread("s", "d", &snap, Some("alice")).len(), 4);
        assert_eq!(p.unread("s", "d", &snap, Some("bob")).len(), 0, "your own messages are read");
        assert!(p.mark_read("s", "d", &snap, &[ids[0].clone(), ids[1].clone(), ids[3].clone()]));
        assert_eq!(p.unread("s", "d", &snap, Some("alice")), vec![ids[2].clone()]);
        assert!(!p.mark_read("s", "d", &snap, &[ids[0].clone()]), "nothing new");
        assert!(p.mark_read("s", "d", &snap, &[ids[2].clone()]));
        assert!(p.unread("s", "d", &snap, Some("alice")).is_empty());
        // Everything is under the watermark now: no ids kept.
        let txn = p.doc.transact();
        let entry = p.entry(&txn, "s", "d").unwrap();
        let Some(Out::YMap(seen)) = entry.get(&txn, SEEN) else { panic!() };
        assert_eq!(seen.len(&txn), 0);
    }

    #[test]
    fn two_devices_merge_what_each_read() {
        let bob = Author::new("bob", "Bob");
        let (d, ids) = discussion_with(3, &bob);
        let snap = d.read();
        let a = Personal::new();
        let b = Personal::load(&a.save());
        a.mark_read("s", "d", &snap, &[ids[0].clone()]);
        b.mark_read("s", "d", &snap, &[ids[2].clone()]);
        a.merge(&b.save());
        b.merge(&a.save());
        assert_eq!(a.unread("s", "d", &snap, None), b.unread("s", "d", &snap, None));
        // At worst something read shows unread again; never the reverse.
        assert!(a.unread("s", "d", &snap, None).len() <= 2);
        assert!(a.unread("s", "d", &snap, None).contains(&ids[1]));
    }

    #[test]
    fn places_from_earlier_versions_are_emptied() {
        let p = Personal::new();
        assert!(!p.drop_places());
        {
            let mut txn = p.doc.transact_mut();
            let m: MapRef = p.places.insert(&mut txn, "s/n", MapPrelim::default());
            m.insert(&mut txn, "anchor", Any::Buffer(vec![1].into()));
        }
        let other = Personal::load(&p.save());
        assert!(other.drop_places());
        p.merge(&other.save());
        assert_eq!(p.places.len(&p.doc.transact()), 0, "the emptying reaches the other devices");
    }
}

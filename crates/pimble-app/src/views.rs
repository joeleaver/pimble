//! What this device remembers of how things looked, so they look the same
//! next time (Joe, 2026-10-23: every client keeps its own): the explorer's
//! expanded rows, the place in every text node (the selection and the first
//! line in view) and the camera of every board. None of it is in a store or
//! in the personal document, and none of it leaves the device:
//! `<data dir>/pimble/views.json` on the desktop, `localStorage`
//! `pimble.views` in the browser, saved a moment after it changes.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;
use serde::{Deserialize, Serialize};

/// How many text places and board cameras are kept each; the ones written
/// longest ago go first.
const MAX_ENTRIES: usize = 2000;
/// How many expanded rows are kept; the ones expanded longest ago go first.
const MAX_EXPANDED: usize = 5000;
/// How long after a change it is written.
const SAVE_AFTER_MS: u32 = 1000;

/// A text node's place: sticky positions (rinch's opaque bytes) for the
/// selection and the first line in view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TextPlace {
    #[serde(with = "b64")]
    pub anchor: Vec<u8>,
    #[serde(with = "b64")]
    pub head: Vec<u8>,
    #[serde(default, with = "b64_opt", skip_serializing_if = "Option::is_none")]
    pub top: Option<Vec<u8>>,
    /// When it was written, in this device's own count.
    #[serde(default)]
    at: u64,
}

impl TextPlace {
    pub(crate) fn new(anchor: Vec<u8>, head: Vec<u8>, top: Option<Vec<u8>>) -> Self {
        Self { anchor, head, top, at: 0 }
    }
}

/// A board's camera: the board point at the middle of the view, and board
/// units to a CSS pixel (not a physical one, so a screen of another density
/// shows the same).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Camera {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
    #[serde(default)]
    at: u64,
}

impl Camera {
    pub(crate) fn new(x: f32, y: f32, zoom: f32) -> Self {
        Self { x, y, zoom, at: 0 }
    }

    fn same_view(&self, other: &Camera) -> bool {
        (self.x, self.y, self.zoom) == (other.x, other.y, other.zoom)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Views {
    /// The explorer's expanded rows as tree values (mount paths included),
    /// in the order they were expanded.
    #[serde(default)]
    expanded: Vec<String>,
    /// The stores this device has shown a row for: one seen before opens as
    /// it was left, a new one opens expanded.
    #[serde(default)]
    stores_seen: Vec<String>,
    /// Keyed `"<store id>/<node id>"`.
    #[serde(default)]
    places: HashMap<String, TextPlace>,
    #[serde(default)]
    boards: HashMap<String, Camera>,
    /// The last `at` handed out.
    #[serde(default)]
    clock: u64,
    /// `stores_seen` as this run found it: the stores whose rows open as
    /// they were left, rather than revealing what a pane shows.
    #[serde(skip)]
    seen_at_start: Vec<String>,
}

fn key(store_id: StoreId, node_id: NodeId) -> String {
    format!("{store_id}/{node_id}")
}

impl Views {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Make the remembered rows `now`: rows no longer expanded go, newly
    /// expanded ones join at the end. Answers whether anything changed.
    fn set_expanded(&mut self, now: &HashSet<String>) -> bool {
        let before = self.expanded.len();
        self.expanded.retain(|v| now.contains(v));
        let mut changed = self.expanded.len() != before;
        let held: HashSet<String> = self.expanded.iter().cloned().collect();
        let mut new: Vec<&String> = now.iter().filter(|v| !held.contains(*v)).collect();
        new.sort();
        for value in new {
            self.expanded.push(value.clone());
            changed = true;
        }
        if self.expanded.len() > MAX_EXPANDED {
            let extra = self.expanded.len() - MAX_EXPANDED;
            self.expanded.drain(..extra);
        }
        changed
    }

    fn set_place(&mut self, key: String, mut place: TextPlace) -> bool {
        if let Some(old) = self.places.get(&key) {
            if place.top.is_none() {
                place.top = old.top.clone();
            }
            if (&old.anchor, &old.head, &old.top) == (&place.anchor, &place.head, &place.top) {
                return false;
            }
        }
        place.at = self.tick();
        self.places.insert(key, place);
        bound(&mut self.places, |p| p.at);
        true
    }

    fn set_camera(&mut self, key: String, mut camera: Camera) -> bool {
        if self.boards.get(&key).is_some_and(|old| old.same_view(&camera)) {
            return false;
        }
        camera.at = self.tick();
        self.boards.insert(key, camera);
        bound(&mut self.boards, |c| c.at);
        true
    }
}

/// Keep the `MAX_ENTRIES` most recently written entries.
fn bound<T>(map: &mut HashMap<String, T>, at: impl Fn(&T) -> u64) {
    if map.len() <= MAX_ENTRIES {
        return;
    }
    let mut ats: Vec<u64> = map.values().map(&at).collect();
    ats.sort_unstable();
    let cut = ats[map.len() - MAX_ENTRIES];
    map.retain(|_, v| at(v) >= cut);
}

thread_local! {
    static VIEWS: RefCell<Option<Views>> = const { RefCell::new(None) };
    static SAVE_DUE: Cell<bool> = const { Cell::new(false) };
}

fn with<R>(f: impl FnOnce(&mut Views) -> R) -> R {
    VIEWS.with(|v| {
        let mut v = v.borrow_mut();
        let views = v.get_or_insert_with(|| {
            let mut views: Views =
                crate::persistence::load_views_json().and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default();
            views.seen_at_start = views.stores_seen.clone();
            views
        });
        f(views)
    })
}

/// Something changed: written about a second later, once for a burst.
fn changed() {
    if SAVE_DUE.with(|s| s.replace(true)) {
        return;
    }
    rinch::core::reactive::unowned(|| {
        set_timeout(SAVE_AFTER_MS, save_now);
    });
}

/// Write what is remembered now (the window is closing).
pub(crate) fn save_now() {
    SAVE_DUE.with(|s| s.set(false));
    if let Ok(json) = with(|v| serde_json::to_string(v)) {
        crate::persistence::save_views_json(&json);
    }
}

// ── The explorer ────────────────────────────────────────────────────────────

/// The rows that were expanded when this device last looked.
pub(crate) fn expanded() -> Vec<String> {
    with(|v| v.expanded.clone())
}

/// The explorer's expanded rows are `now`.
pub(crate) fn expanded_now(now: &HashSet<String>) {
    if with(|v| v.set_expanded(now)) {
        changed();
    }
}

/// Whether store `store_id` has shown a row on this device before; from now
/// on it has.
pub(crate) fn seen_store(store_id: StoreId) -> bool {
    let id = store_id.to_string();
    let seen = with(|v| {
        if v.stores_seen.contains(&id) {
            return true;
        }
        v.stores_seen.push(id);
        false
    });
    if !seen {
        changed();
    }
    seen
}

/// Whether store `store_id`'s rows were left some way on this device
/// before this run: a document a pane opens again at start is then not
/// revealed in the explorer, which shows what was left open.
pub(crate) fn rows_remembered(store_id: StoreId) -> bool {
    let id = store_id.to_string();
    with(|v| v.seen_at_start.contains(&id))
}

// ── Text ────────────────────────────────────────────────────────────────────

pub(crate) fn place(store_id: StoreId, node_id: NodeId) -> Option<TextPlace> {
    with(|v| v.places.get(&key(store_id, node_id)).cloned())
}

/// Remember `place` for the node. A place without a first line keeps the one
/// remembered before (the text was not on screen to say).
pub(crate) fn set_place(store_id: StoreId, node_id: NodeId, place: TextPlace) {
    if with(|v| v.set_place(key(store_id, node_id), place)) {
        changed();
    }
}

// ── Boards ──────────────────────────────────────────────────────────────────

pub(crate) fn camera(store_id: StoreId, node_id: NodeId) -> Option<Camera> {
    with(|v| v.boards.get(&key(store_id, node_id)).copied())
}

pub(crate) fn set_camera(store_id: StoreId, node_id: NodeId, camera: Camera) {
    if with(|v| v.set_camera(key(store_id, node_id), camera)) {
        changed();
    }
}

mod b64 {
    use super::*;

    pub fn serialize<S: serde::Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

mod b64_opt {
    use super::*;

    pub fn serialize<S: serde::Serializer>(bytes: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => super::b64::serialize(bytes, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let text = Option::<String>::deserialize(d)?;
        text.map(|t| base64::engine::general_purpose::STANDARD.decode(t).map_err(serde::de::Error::custom)).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(values: &[&str]) -> HashSet<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn expanded_rows_follow_the_tree_in_the_order_they_opened() {
        let mut v = Views::default();
        assert!(v.set_expanded(&set(&["a"])));
        assert!(v.set_expanded(&set(&["a", "b"])));
        assert!(!v.set_expanded(&set(&["a", "b"])), "the same rows are no change");
        assert!(v.set_expanded(&set(&["b", "c"])));
        assert_eq!(v.expanded, vec!["b", "c"]);
    }

    #[test]
    fn a_place_without_a_first_line_keeps_the_one_before() {
        let mut v = Views::default();
        assert!(v.set_place("s/n".into(), TextPlace::new(vec![1], vec![1], Some(vec![9]))));
        assert!(!v.set_place("s/n".into(), TextPlace::new(vec![1], vec![1], None)), "the same place is no change");
        assert!(v.set_place("s/n".into(), TextPlace::new(vec![2], vec![2], None)));
        assert_eq!(v.places["s/n"].top, Some(vec![9]));
    }

    #[test]
    fn places_and_cameras_are_bounded_oldest_first() {
        let mut v = Views::default();
        for i in 0..MAX_ENTRIES + 10 {
            v.set_place(format!("s/{i}"), TextPlace::new(vec![i as u8], vec![], None));
            v.set_camera(format!("s/{i}"), Camera::new(i as f32, 0.0, 1.0));
        }
        assert_eq!(v.places.len(), MAX_ENTRIES);
        assert_eq!(v.boards.len(), MAX_ENTRIES);
        assert!(!v.places.contains_key("s/0") && v.places.contains_key(&format!("s/{}", MAX_ENTRIES + 9)));
        assert!(!v.boards.contains_key("s/9") && v.boards.contains_key("s/10"));
    }

    #[test]
    fn what_is_remembered_reads_back() {
        let mut v = Views::default();
        v.set_expanded(&set(&["store_x", "node_x_y/x_z"]));
        v.stores_seen.push("x".into());
        v.set_place("s/n".into(), TextPlace::new(vec![1, 2], vec![3], Some(vec![4])));
        v.set_place("s/m".into(), TextPlace::new(vec![5], vec![5], None));
        v.set_camera("s/b".into(), Camera::new(1.5, -2.0, 0.5));
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<Views>(&json).unwrap(), v);
        assert_eq!(serde_json::from_str::<Views>("{}").unwrap(), Views::default());
    }
}

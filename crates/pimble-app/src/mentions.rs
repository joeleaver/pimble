//! Mentions in the app (docs/NOTIFICATIONS_CONTRACT.md section 1, "Built in
//! wave 0"): who `@` offers in a note, how a mention looks in the text and in
//! a discussion, and what its tooltip says.
//!
//! A mention is a link (`pimble_core::mention`): the words `@Name` as they
//! were written, linked to `pimble-person:<sub>?m=<id>`. Nothing about who is
//! in a share is written into the document. What a mention shows is decided
//! here from the share's people as this device knows them
//! (`shares::SharesUi::people`, the accounts service's member list):
//!
//! - `@` in a note offers the people who can open it: the members of every
//!   share the note is in (a share rooted at it or above it, and a share of
//!   the whole store), each by the name they go by in the innermost of those
//!   shares, never by email, never this account itself. Nobody else.
//! - In the text, [`MentionsPlugin`] gives each mention an `Inline`
//!   decoration (`pimble-mention`), greyed (`pimble-mention--gone`) when its
//!   person is not among the people who can open the note any more. The
//!   words stay as written; the tooltip says the name the person goes by now
//!   (rinch has no decoration that replaces text, so a rename shows there and
//!   in discussions, not in the note's own words).
//! - In a discussion message the app draws the text itself, so a mention
//!   shows `@` and the current name there ([`shown`]).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use pimble_core::mention::Mention;
use pimble_core::{NodeId, StoreId};
use pimble_rpc::Person;
use rinch::prelude::*;
use rinch_editor_core::state::{EditorState, Transaction};
use rinch_editor_core::{Attrs, Decoration, DecorationSet, Node as EditorNode, Plugin, PluginKey, Pos};

use crate::panes::PaneId;
use crate::state::AppStore;

/// The class every mention carries in the editor.
pub const CLASS: &str = "pimble-mention";
/// Added for a mention of someone who can no longer open the note.
pub const GONE_CLASS: &str = "pimble-mention--gone";

/// How many people the picker lists.
pub const MAX_ROWS: usize = 8;

const PLUGIN_KEY: PluginKey = PluginKey("pimble.mentions");
/// The meta key a pane's people arrive under.
const META: &str = "pimble.mentions.people";

// ── Who can open a note ──────────────────────────────────────────────────

/// The people who can open `(store_id, node_id)` as this device knows them,
/// `None` when it knows nothing of the store's people (a store that has
/// never been shared, or whose people have not arrived).
pub fn members_for(store: AppStore, store_id: StoreId, node_id: NodeId) -> Option<Vec<Person>> {
    let people = untracked(|| store.shares_ui.people.with(|all| all.get(&store_id).cloned()))?;
    let chain = pimble_core::plan::ancestors(&store.view(), store_id, node_id);
    Some(people.members_of(&chain))
}

/// Whom `@` offers for `query` among `members`: everyone but `me`, those
/// whose name or one of its words starts with the query first, then those
/// whose name holds it, each group by name. An empty query offers everyone.
pub fn offered(members: &[Person], me: Option<&str>, query: &str) -> Vec<Person> {
    let q = query.trim().to_lowercase();
    let mut first = Vec::new();
    let mut then = Vec::new();
    for person in members.iter().filter(|p| Some(p.user_id.as_str()) != me && !p.name.trim().is_empty()) {
        let name = person.name.to_lowercase();
        if q.is_empty() || name.starts_with(&q) || name.split_whitespace().any(|w| w.starts_with(&q)) {
            first.push(person.clone());
        } else if name.contains(&q) {
            then.push(person.clone());
        }
    }
    first.sort_by_key(|p| p.name.to_lowercase());
    then.sort_by_key(|p| p.name.to_lowercase());
    first.extend(then);
    first.truncate(MAX_ROWS);
    first
}

/// This account's `sub`, when signed in.
pub fn me() -> Option<String> {
    crate::boards::current_author().map(|a| a.id).filter(|id| !id.is_empty())
}

/// The node the pane's editor holds.
fn pane_node(store: AppStore, pane: PaneId) -> Option<(StoreId, NodeId)> {
    untracked(|| store.pane(pane).active_edit.get()).map(|a| (a.store_id, a.node_id))
}

/// Whether "Share this with someone..." can be offered for a note: on the
/// desktop (where "Share..." is), in a store that is this account's own.
pub fn can_offer_share(store: AppStore, store_id: StoreId) -> bool {
    cfg!(feature = "native") && store.shared_by(store_id).is_none()
}

// ── Showing a mention ───────────────────────────────────────────────────

/// What a mention shows in `(store_id, node_id)`: the name its person goes
/// by in the note's share now, else the name as written; and whether the
/// person can no longer open the note (known only when the store's people
/// are; unknown is never greyed).
pub fn shown(store: AppStore, store_id: StoreId, node_id: NodeId, mention: &Mention) -> (String, bool) {
    let Some(members) = members_for(store, store_id, node_id) else { return (mention.name.clone(), false) };
    match members.iter().find(|p| p.user_id == mention.account) {
        Some(person) if !person.name.trim().is_empty() => (person.name.clone(), false),
        Some(_) => (mention.name.clone(), false),
        None => (mention.name.clone(), true),
    }
}

/// The text of `doc` between `from` and `to`.
pub fn words_at(doc: &EditorNode, from: usize, to: usize) -> String {
    let mut text = String::new();
    doc.nodes_between(from, to, &mut |node, pos, _| {
        if let Some(t) = node.text() {
            let skip = from.saturating_sub(pos);
            let take = to.min(pos + node.node_size()).saturating_sub(pos.max(from));
            text.extend(t.chars().skip(skip).take(take));
        }
        true
    });
    text
}

/// The tooltip of a mention with `href` over `from..to` of the note
/// `(store_id, node_id)`.
pub fn tooltip(store: AppStore, node: Option<(StoreId, NodeId)>, href: &str, from: usize, to: usize) -> String {
    let words = node
        .and_then(|(s, n)| store.panes_editing(s, n).first().copied())
        .map(|pane| words_at(&crate::editor::editor(pane).doc(), from, to))
        .unwrap_or_default();
    let Some(mention) = Mention::from_link(href, &words) else { return href.to_string() };
    let Some((store_id, node_id)) = node else { return format!("A mention of {}", mention.name) };
    let (name, gone) = shown(store, store_id, node_id, &mention);
    if gone {
        format!("{} can no longer open this note", mention.name)
    } else if name != mention.name {
        format!("A mention of {name} (written as {})", mention.name)
    } else {
        format!("A mention of {name}")
    }
}

// ── The plugin ──────────────────────────────────────────────────────────

/// The people a pane's plugin judges by: `None` when unknown.
type Members = Option<Rc<HashSet<String>>>;

thread_local! {
    /// Per pane, the people who can open the note it holds, as last given
    /// to its editor: what a plugin starting over (a document loaded) begins
    /// with.
    static GIVEN: RefCell<[Members; 4]> = const { RefCell::new([None, None, None, None]) };
}

/// What the plugin keeps: each mention's range and account, and whom it
/// judges by.
#[derive(Debug, Clone, Default)]
struct Found {
    ranges: Rc<Vec<(usize, usize, String)>>,
    members: Members,
}

/// Every mention's `(from, to, account)` in `doc`, positions in the document.
fn ranges_of(doc: &EditorNode) -> Vec<(usize, usize, String)> {
    let mut out: Vec<(usize, usize, String)> = Vec::new();
    // The open run: (from, to, href).
    let mut open: Option<(usize, usize, String)> = None;
    let push = |open: &mut Option<(usize, usize, String)>, out: &mut Vec<(usize, usize, String)>| {
        if let Some((from, to, href)) = open.take() {
            if let Some((account, _)) = pimble_core::mention::parse_href(&href) {
                out.push((from, to, account));
            }
        }
    };
    doc.nodes_between(0, doc.content_size(), &mut |node, pos, _| {
        if node.is_textblock() {
            push(&mut open, &mut out);
            return true;
        }
        let Some(text) = node.text() else {
            if node.is_inline() {
                push(&mut open, &mut out);
            }
            return true;
        };
        let href = node
            .marks()
            .iter()
            .find(|m| m.type_name() == "link")
            .and_then(|m| m.attrs.get_str("href"))
            .filter(|h| pimble_core::mention::is_mention_href(h));
        let end = pos + text.chars().count();
        match (href, open.as_mut()) {
            (Some(href), Some((_, to, current))) if current == href && *to == pos => *to = end,
            (Some(href), _) => {
                push(&mut open, &mut out);
                open = Some((pos, end, href.to_string()));
            }
            (None, _) => push(&mut open, &mut out),
        }
        false
    });
    push(&mut open, &mut out);
    out
}

/// The editor plugin that draws mentions. Added to every pane's editor
/// before any content (adding a plugin resets history).
pub struct MentionsPlugin {
    pub pane: PaneId,
}

impl Plugin for MentionsPlugin {
    fn key(&self) -> PluginKey {
        PLUGIN_KEY
    }

    fn init_state(&self, doc: &EditorNode) -> Option<Rc<dyn std::any::Any>> {
        let members = GIVEN.with(|g| g.borrow()[self.pane.index()].clone());
        Some(Rc::new(Found { ranges: Rc::new(ranges_of(doc)), members }))
    }

    fn apply(&self, tr: &Transaction, _old: &EditorState, new_doc: &EditorNode, prev: Option<&dyn std::any::Any>) -> Option<Rc<dyn std::any::Any>> {
        let mut found = prev.and_then(|p| p.downcast_ref::<Found>()).cloned().unwrap_or_default();
        if let Some(given) = tr.get_meta(META).and_then(|m| m.downcast_ref::<Members>()) {
            found.members = given.clone();
        }
        if tr.doc_changed() {
            found.ranges = Rc::new(ranges_of(new_doc));
        }
        Some(Rc::new(found))
    }

    fn decorations(&self, state: &EditorState) -> DecorationSet {
        let Some(found) = state.plugin_state::<Found>(PLUGIN_KEY) else { return DecorationSet::empty() };
        if found.ranges.is_empty() {
            return DecorationSet::empty();
        }
        let size = state.doc.content_size();
        DecorationSet::new(
            found
                .ranges
                .iter()
                .filter(|(from, to, _)| from < to && *to <= size)
                .map(|(from, to, account)| {
                    let gone = found.members.as_ref().is_some_and(|m| !m.contains(account));
                    let class = if gone { format!("{CLASS} {GONE_CLASS}") } else { CLASS.to_string() };
                    Decoration::inline(Pos(*from), Pos(*to), Attrs::new().with("class", class))
                })
                .collect(),
        )
    }
}

/// Give pane `pane`'s editor the people who can open the note it holds, when
/// they changed. Tracked on the pane's node and the people (an effect per
/// pane calls it); the editor is told a turn later, outside the effect.
pub(crate) fn follow_members(store: AppStore, pane: PaneId) {
    let node = store.pane(pane).active_edit.get().map(|a| (a.store_id, a.node_id));
    store.shares_ui.people.with(|_| ());
    let members: Members = node.and_then(|(s, n)| members_for(store, s, n)).map(|m| Rc::new(m.into_iter().map(|p| p.user_id).collect()));
    let changed = GIVEN.with(|g| {
        let slot = &mut g.borrow_mut()[pane.index()];
        if *slot == members {
            return false;
        }
        *slot = members.clone();
        true
    });
    if changed {
        crate::links::later(move |_| {
            crate::editor::editor(pane).update(move |state| {
                let mut tr = state.tr();
                tr.set_meta(META, members);
                Some(tr)
            });
        });
    }
}

// ── The `@` picker's rows ───────────────────────────────────────────────

/// The `@` picker's rows for `query` in the focused pane's note: the people
/// [`offered`], and with an empty query "Share this with someone..." last
/// where sharing is possible.
pub(crate) fn picker_rows(store: AppStore, query: &str) -> Vec<crate::link_picker::PickerRow> {
    let Some((store_id, node_id)) = pane_node(store, store.focused()) else { return Vec::new() };
    let members = members_for(store, store_id, node_id).unwrap_or_default();
    let mut rows: Vec<crate::link_picker::PickerRow> = offered(&members, me().as_deref(), query)
        .into_iter()
        .map(|p| crate::link_picker::PickerRow::Person { account: p.user_id, name: p.name })
        .collect();
    if query.trim().is_empty() && can_offer_share(store, store_id) {
        rows.push(crate::link_picker::PickerRow::ShareThis);
    }
    rows
}

/// "Share this with someone...": the Share dialog for the focused pane's note.
pub(crate) fn share_focused(store: AppStore) {
    if let Some((store_id, node_id)) = pane_node(store, store.focused()) {
        crate::app::open_share_modal(store, store_id, node_id);
    }
}

// ── A discussion's message box ───────────────────────────────────────────

/// The `@` being typed at the end of a message box: where its `@` is (a byte
/// offset) and what follows it. `None` when the box does not end in one: the
/// `@` must start a word, and what follows is at most a name's length (40
/// characters, three spaces) on one line.
pub fn trailing_query(draft: &str) -> Option<(usize, String)> {
    let at = draft.rfind('@')?;
    let query = &draft[at + 1..];
    let starts_word = draft[..at].chars().last().map_or(true, |c| c.is_whitespace() || "([{\"'".contains(c));
    (starts_word && query.chars().count() <= 40 && query.matches(' ').count() <= 3 && !query.contains('\n')).then(|| (at, query.to_string()))
}

/// Whom a discussion's message box offers now: the people who can open the
/// discussion matching the `@` it ends in. Tracked on the draft.
pub(crate) fn box_offers(store: AppStore, slot: crate::discussions::Slot, ui: crate::discussions::DiscussionUi) -> Vec<Person> {
    let draft = ui.draft.get();
    let Some((_, query)) = trailing_query(&draft) else { return Vec::new() };
    let Some((store_id, node_id)) = crate::discussions::held_by(slot) else { return Vec::new() };
    let members = members_for(store, store_id, node_id).unwrap_or_default();
    let offered = offered(&members, me().as_deref(), &query);
    // A name typed out in full is picked already.
    if offered.len() == 1 && offered[0].name == query {
        return Vec::new();
    }
    offered
}

/// Pick `person` for the `@` the message box ends in: the box shows `@Name `
/// and the name is linked to its account when the message is sent
/// (`Body::with_picked_mentions`).
pub(crate) fn box_pick(ui: crate::discussions::DiscussionUi, person: &Person) {
    let draft = untracked(|| ui.draft.get());
    let Some((at, _)) = trailing_query(&draft) else { return };
    ui.draft.set(format!("{}@{} ", &draft[..at], person.name));
    ui.mentioned.update(|m| {
        if !m.iter().any(|(name, account)| *name == person.name && *account == person.user_id) {
            m.push((person.name.clone(), person.user_id.clone()));
        }
    });
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    fn person(id: &str, name: &str) -> Person {
        Person { user_id: id.into(), name: name.into() }
    }

    #[test]
    fn at_offers_everyone_but_me_by_what_is_typed() {
        let members = vec![person("joe", "Joe"), person("will", "Will Smith"), person("sam", "Samwise"), person("ann", "Hannah")];
        let names = |q: &str| offered(&members, Some("joe"), q).into_iter().map(|p| p.name).collect::<Vec<_>>();
        assert_eq!(names(""), vec!["Hannah", "Samwise", "Will Smith"], "never myself");
        assert_eq!(names("sm"), vec!["Will Smith"], "a word of the name");
        assert_eq!(names("an"), vec!["Hannah"], "inside a name comes last");
        assert_eq!(names("s"), vec!["Samwise", "Will Smith"]);
        assert!(names("zed").is_empty());
        assert!(offered(&[], None, "").is_empty(), "a note nobody else can open offers nobody");
    }

    #[test]
    fn a_message_box_offers_people_for_the_at_it_ends_in() {
        assert_eq!(trailing_query("hi @wi"), Some((3, "wi".to_string())));
        assert_eq!(trailing_query("@"), Some((0, String::new())));
        assert_eq!(trailing_query("(@Will Sm"), Some((1, "Will Sm".to_string())));
        assert_eq!(trailing_query("mail me@home"), None, "inside a word");
        assert_eq!(trailing_query("@will\nnext"), None, "another line");
        assert_eq!(trailing_query("no at"), None);
    }

    fn state_with(blocks_md: &str, pane: PaneId) -> EditorState {
        let schema = Rc::new(rinch_editor_core::Schema::starter_kit());
        let doc = pimble_crdt::markdown::parse(&schema, blocks_md).unwrap();
        let mut plugins = rinch_editor_core::default_plugins();
        plugins.push(Rc::new(MentionsPlugin { pane }) as Rc<dyn Plugin>);
        EditorState::create(schema, doc, plugins)
    }

    fn classes(state: &EditorState, pane: PaneId) -> Vec<(usize, usize, String)> {
        MentionsPlugin { pane }
            .decorations(state)
            .iter()
            .map(|d| {
                let (from, to) = d.range();
                (from.0, to.0, d.inline_class().unwrap().to_string())
            })
            .collect()
    }

    #[test]
    fn mentions_are_drawn_and_greyed_when_their_person_left() {
        let pane = PaneId::ALL[3];
        GIVEN.with(|g| g.borrow_mut()[pane.index()] = None);
        let state = state_with("Hi [@Will](pimble-person:will?m=a1) and [@Sam **S**](pimble-person:sam?m=b2), [x](pimble:s/n)", pane);
        // "Hi " is 1..4, "@Will" 4..9, " and " 9..14, "@Sam S" 14..20.
        assert_eq!(classes(&state, pane), vec![(4, 9, CLASS.to_string()), (14, 20, CLASS.to_string())], "unknown people: nobody greyed");
        let mut tr = state.tr();
        let members: Members = Some(Rc::new(HashSet::from(["will".to_string()])));
        tr.set_meta(META, members);
        let state = state.apply(tr);
        assert_eq!(classes(&state, pane), vec![(4, 9, CLASS.to_string()), (14, 20, format!("{CLASS} {GONE_CLASS}"))]);
    }
}

//! Account > "Shares..." (File > "Shares..." in the browser): the shares this
//! account takes part in, owned or joined, and the name it goes by in each
//! (docs/PRESENCE_CONTRACT.md section 3, Joe 2026-10-22). Members of a share
//! see each other by these names and never by email.
//!
//! Also here: who is who in each open store (`People`, from `cloudPeople`),
//! which turns the account id on a discussion message or a board element into
//! the name its author goes by in the share, and the one-line question asked
//! the first time a share is opened whose name was never chosen.

use std::collections::{HashMap, HashSet};

use pimble_core::{NodeId, PimbleUrl, StoreId};
use pimble_rpc::{CloudListSharesResponse, CloudShareRow, StorePeople};
use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// One share as the dialog keys it.
pub type ShareKey = (StoreId, Option<NodeId>);

/// The dialog's state, the names of the open stores' people, and the
/// first-open question. One per window.
#[derive(Clone, Copy)]
pub struct SharesUi {
    pub open: Signal<bool>,
    pub listing: Signal<Option<CloudListSharesResponse>>,
    pub error: Signal<String>,
    pub busy: Signal<bool>,
    /// The default name as typed, before it is applied.
    pub default_draft: Signal<String>,
    /// Each share's "Appear as" as typed, before it is applied.
    pub drafts: Signal<HashMap<ShareKey, String>>,
    /// Who is who in each open store's shares.
    pub people: Signal<HashMap<StoreId, StorePeople>>,
    /// The share being asked about: "How should the people in <share> see
    /// you?", with the name it is called.
    pub asking: Signal<Option<(ShareKey, String)>>,
    pub ask_draft: Signal<String>,
    /// Shares asked about in this run, answered or not: asked once.
    pub asked: Signal<HashSet<ShareKey>>,
}

impl SharesUi {
    pub fn new() -> Self {
        Self {
            open: Signal::new(false),
            listing: Signal::new(None),
            error: Signal::new(String::new()),
            busy: Signal::new(false),
            default_draft: Signal::new(String::new()),
            drafts: Signal::new(HashMap::new()),
            people: Signal::new(HashMap::new()),
            asking: Signal::new(None),
            ask_draft: Signal::new(String::new()),
            asked: Signal::new(HashSet::new()),
        }
    }
}

impl Default for SharesUi {
    fn default() -> Self {
        Self::new()
    }
}

/// Account > "Shares...".
pub fn open(store: AppStore) {
    let ui = store.shares_ui;
    ui.error.set(String::new());
    ui.busy.set(true);
    ui.open.set(true);
    store.send(BackendCommand::CloudListShares);
}

// ── Events ───────────────────────────────────────────────────────────────

pub(crate) fn listed(store: AppStore, shares: &CloudListSharesResponse) {
    crate::boards::set_author_name(&shares.default_name);
    let ui = store.shares_ui;
    ui.busy.set(false);
    ui.error.set(String::new());
    ui.default_draft.set(shares.display_name.clone());
    ui.drafts.set(shares.shares.iter().map(|row| ((row.store_id, row.root), row.my_name.clone())).collect());
    ui.listing.set(Some(shares.clone()));
    // A name chosen here changes what the open stores' people say.
    for row in &shares.shares {
        if row.here {
            store.send(BackendCommand::GetPeople { store_id: row.store_id });
        }
    }
}

pub(crate) fn failed(store: AppStore, message: &str) {
    let ui = store.shares_ui;
    ui.busy.set(false);
    ui.error.set(message.to_string());
}

/// `PeopleLoaded`: keep them, and ask the one-line question for a share of
/// somebody else's that this account has never chosen a name for.
pub(crate) fn people_loaded(store: AppStore, store_id: StoreId, people: &StorePeople) {
    let ui = store.shares_ui;
    ui.people.update(|all| {
        all.insert(store_id, people.clone());
    });
    // Asked of a share somebody else made: one's own store is named by the
    // Shares dialog alone.
    let shared_with_me = store.get_store_signal(store_id).is_some_and(|s| untracked(|| s.with(|s| s.shared_by.is_some())));
    if !shared_with_me || untracked(|| ui.asking.get()).is_some() {
        return;
    }
    let share_name = |root: Option<NodeId>| match root {
        Some(root) => store.display_label(store_id, root),
        None => store.get_store_signal(store_id).map(|s| untracked(|| s.with(|s| s.name.clone()))).unwrap_or_default(),
    };
    for scope in &people.scopes {
        let key = (store_id, scope.root);
        if scope.my_name_chosen || untracked(|| ui.asked.get()).contains(&key) {
            continue;
        }
        ui.asked.update(|asked| {
            asked.insert(key);
        });
        ui.ask_draft.set(scope.my_name.clone());
        let name = if scope.share_name.trim().is_empty() { share_name(scope.root) } else { scope.share_name.clone() };
        ui.asking.set(Some((key, name)));
        return;
    }
}

// ── Names ────────────────────────────────────────────────────────────────

/// What `user_id` goes by in `store_id` (in the share rooted at `root` when
/// it is in several). `None` when nobody here knows: the caller shows what
/// it has (a stamped name), never an email.
pub fn name_of(store: AppStore, store_id: StoreId, root: Option<NodeId>, user_id: &str) -> Option<String> {
    store.shares_ui.people.with(|all| all.get(&store_id).and_then(|people| people.name_of(user_id, root)).map(str::to_string))
}

/// What this account goes by in `store_id`, for the name stamped on what it
/// makes there: the name in the share rooted at `root` when it names one,
/// else in any share of the store. `None` for one's own store with no
/// shares, where the account's own default applies.
pub fn my_name_in(store: AppStore, store_id: StoreId, root: Option<NodeId>) -> Option<String> {
    store.shares_ui.people.with(|all| {
        let people = all.get(&store_id)?;
        people
            .scopes
            .iter()
            .find(|scope| scope.root == root)
            .or_else(|| people.scopes.first())
            .map(|scope| scope.my_name.clone())
            .filter(|name| !name.trim().is_empty())
    })
}

/// The share `node_id` is in, as this device's people of `store_id` know
/// the shares: the nearest of the node and its ancestors that is a share's
/// root. `None` when no share holds it (or the tree above it is not here).
pub fn scope_root_of(store: AppStore, store_id: StoreId, node_id: NodeId) -> Option<NodeId> {
    let roots: Vec<NodeId> = store.shares_ui.people.with(|all| all.get(&store_id).map(|p| p.scopes.iter().filter_map(|s| s.root).collect()).unwrap_or_default());
    if roots.is_empty() {
        return None;
    }
    let mut at = Some(node_id);
    for _ in 0..256 {
        let node = at?;
        if roots.contains(&node) {
            return Some(node);
        }
        at = store.get_node_signal(store_id, node).and_then(|s| untracked(|| s.with(|n| n.parent_id)));
    }
    None
}

/// Who an edit of `node_id` is stamped as: the signed-in account, by the
/// name it goes by in the share the node is in (never anything made from
/// its email), or nobody when nobody is signed in.
pub fn author_in(store: AppStore, store_id: StoreId, node_id: NodeId) -> Option<pimble_board::Author> {
    let mut author = crate::boards::current_author()?;
    if let Some(name) = my_name_in(store, store_id, scope_root_of(store, store_id, node_id)) {
        author.name = name;
    }
    Some(author)
}

/// Turns the account ids stamped in one node's document into the names
/// their people go by in its share now (docs/IMAGE_MARKUP_CONTRACT.md section
/// 6: the current name for a known id, else the stamp).
pub struct Names {
    people: Option<StorePeople>,
    root: Option<NodeId>,
}

impl Names {
    pub fn of(store: AppStore, store_id: StoreId, node_id: NodeId) -> Self {
        let root = scope_root_of(store, store_id, node_id);
        Self { people: store.shares_ui.people.with(|all| all.get(&store_id).cloned()), root }
    }

    /// The current name of `user_id`, `None` when nobody here knows it.
    pub fn name(&self, user_id: &str) -> Option<String> {
        self.people.as_ref()?.name_of(user_id, self.root).map(str::to_string)
    }

    /// `stamped`, or the current name of the id it carries.
    pub fn named(&self, id: &str, stamped: &str) -> String {
        self.name(id).unwrap_or_else(|| stamped.to_string())
    }
}

// ── The dialog ───────────────────────────────────────────────────────────

fn row_title(row: &CloudShareRow) -> String {
    match (&row.owner_name, row.owned) {
        (_, true) => format!("{} · yours", row.name),
        (Some(owner), false) => format!("{} · shared by {owner}", row.name),
        (None, false) => row.name.clone(),
    }
}

fn role_text(row: &CloudShareRow) -> &'static str {
    match row.role.as_str() {
        "owner" => "Owner",
        "editor" => "Editor",
        "commenter" => "Commenter",
        "reader" => "Reader",
        _ => "Member",
    }
}

fn apply_default(store: AppStore) {
    let ui = store.shares_ui;
    let draft = untracked(|| ui.default_draft.get()).trim().to_string();
    let current = untracked(|| ui.listing.get()).map(|l| l.display_name).unwrap_or_default();
    if draft == current {
        return;
    }
    ui.busy.set(true);
    store.send(BackendCommand::CloudSetDisplayName { name: draft });
}

fn apply_share(store: AppStore, key: ShareKey) {
    let ui = store.shares_ui;
    let draft = untracked(|| ui.drafts.get()).get(&key).cloned().unwrap_or_default().trim().to_string();
    let current = untracked(|| ui.listing.get())
        .and_then(|l| l.shares.into_iter().find(|row| (row.store_id, row.root) == key))
        .map(|row| row.my_name)
        .unwrap_or_default();
    if draft == current {
        return;
    }
    ui.busy.set(true);
    store.send(BackendCommand::CloudSetShareName { store_id: key.0, root: key.1, name: draft });
}

fn open_share(store: AppStore, row: &CloudShareRow) {
    let node = row.root.or_else(|| store.root_node_id(row.store_id));
    if let Some(node) = node {
        store.shares_ui.open.set(false);
        store.send(BackendCommand::ResolveLink { url: PimbleUrl::node(row.store_id, node), hops: 0 });
    }
}

fn add_here(store: AppStore, row: &CloudShareRow) {
    store.shares_ui.busy.set(true);
    store.send(BackendCommand::CloudAddHostedStore { store_id: row.store_id });
}

pub(crate) fn render_modal(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let ui = store.shares_ui;
    let rows = move || ui.listing.get().map(|l| l.shares).unwrap_or_default();
    rsx! {
        Modal {
            opened_fn: move || ui.open.get(),
            onclose: move || ui.open.set(false),
            title: "Shares",
            size: "lg",

            div {
                class: "pimble-shares",
                div {
                    class: "pimble-shares__intro",
                    "In each share, the others see you by the name you choose there, never by your email."
                }
                TextInput {
                    label: "Your name",
                    description: "Used in every share where you have not chosen another.",
                    value_fn: move || ui.default_draft.get(),
                    oninput: move |val: String| ui.default_draft.set(val),
                    onchange: move |_: String| apply_default(store),
                }
                div {
                    class: "pimble-shares__hint",
                    {move || match ui.listing.get() {
                        Some(l) if l.display_name.trim().is_empty() => format!("Until you choose one, you show as {}.", l.default_name),
                        _ => String::new(),
                    }}
                }
                div { class: "pimble-shares__label", "Shares you are in" }
                div {
                    style: {move || if rows().is_empty() && !ui.busy.get() { "font-size: 12px; color: var(--rinch-color-dimmed);" } else { "display: none;" }},
                    "No shares yet. A share someone gives you, or one you make with Share..., is listed here."
                }
                for row in rows() {
                    div {
                        key: format!("{}:{:?}", row.store_id, row.root),
                        class: "pimble-shares__row",
                        div {
                            style: "flex: 1; min-width: 0;",
                            div { class: "pimble-shares__name", {row_title(&row)} }
                            div { class: "pimble-shares__role", {role_text(&row)} }
                        }
                        div {
                            style: "width: 200px;",
                            TextInput {
                                size: "xs",
                                placeholder: untracked(|| ui.listing.get()).map(|l| l.default_name).unwrap_or_default(),
                                value_fn: {
                                    let key = (row.store_id, row.root);
                                    move || ui.drafts.get().get(&key).cloned().unwrap_or_default()
                                },
                                oninput: {
                                    let key = (row.store_id, row.root);
                                    move |val: String| ui.drafts.update(|d| {
                                        d.insert(key, val);
                                    })
                                },
                                onchange: {
                                    let key = (row.store_id, row.root);
                                    move |_: String| apply_share(store, key)
                                },
                            }
                        }
                        Button {
                            size: "xs",
                            variant: "light",
                            onclick: {
                                let row = row.clone();
                                move || if row.here { open_share(store, &row) } else { add_here(store, &row) }
                            },
                            {if row.here { "Open" } else { "Add Here" }}
                        }
                    }
                }
                div {
                    class: "pimble-shares__error",
                    style: {move || if ui.error.get().is_empty() { "display: none;" } else { "" }},
                    {move || ui.error.get()}
                }
            }
        }
    }
}

/// "How should the people in <share> see you?", asked once per share whose
/// name this account never chose, the first time it is opened here.
pub(crate) fn render_question(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let ui = store.shares_ui;
    let answer = move || {
        let Some((key, _)) = untracked(|| ui.asking.get()) else { return };
        let name = untracked(|| ui.ask_draft.get()).trim().to_string();
        ui.asking.set(None);
        // Saved even when it is the default as offered: a name chosen is a
        // question answered, and it is not asked again on another device.
        if !name.is_empty() {
            store.send(BackendCommand::CloudSetShareName { store_id: key.0, root: key.1, name });
        }
    };
    rsx! {
        Modal {
            opened_fn: move || ui.asking.get().is_some(),
            onclose: answer,
            title: "Your name in this share",
            size: "sm",
            div {
                class: "pimble-shares",
                div {
                    class: "pimble-shares__name",
                    {move || ui.asking.get().map(|(_, name)| format!("How should the people in {name} see you?")).unwrap_or_default()}
                }
                div {
                    class: "pimble-shares__intro",
                    "They see this name, never your email. You can change it in Shares..."
                }
                TextInput {
                    value_fn: move || ui.ask_draft.get(),
                    oninput: move |val: String| ui.ask_draft.set(val),
                    onsubmit: answer,
                }
                div {
                    style: "display: flex; justify-content: flex-end;",
                    Button { size: "sm", variant: "filled", onclick: answer, "Done" }
                }
            }
        }
    }
}

//! A store's document themes as the API holds them (docs/THEMES_CONTRACT.md
//! section 3.1): every live theme node (`node_type = "theme"`) of the store,
//! wherever it sits in the tree, with what its document's `data.theme`
//! holds. Read from the documents themselves, the same way on every
//! backend: nothing is kept beside them.

use pimble_core::{node_types, NodeId, StoreId};
use pimble_crdt::{DataEdit, DataStep, NodeDoc};

use crate::{Addr, ApiError, Pimble};

/// One theme node of a store.
#[derive(Debug, Clone, PartialEq)]
pub struct StoreTheme {
    pub node: NodeId,
    /// The node's title: the theme's name.
    pub name: String,
    /// The node's `data.theme` as stored (its base, the tokens it sets, and
    /// `copied_from` on a copy); an empty object when it holds none.
    pub stored: serde_json::Map<String, serde_json::Value>,
}

/// The key of the `data` root a theme node keeps its theme under.
pub const DATA_KEY: &str = "theme";

/// The name a theme with no title is shown with.
pub const UNTITLED: &str = "Untitled theme";

/// `data.theme` of a document, as stored.
pub fn stored_theme(doc: &NodeDoc) -> serde_json::Map<String, serde_json::Value> {
    match doc.data_json().get(DATA_KEY) {
        Some(serde_json::Value::Object(map)) => map.clone(),
        _ => serde_json::Map::new(),
    }
}

fn theme_of(node: NodeId, doc: &NodeDoc) -> Option<StoreTheme> {
    let fields = doc.fields().ok()?;
    if fields.node_type != node_types::THEME || fields.deleted_at.is_some() {
        return None;
    }
    let name = if fields.title.trim().is_empty() { UNTITLED.to_string() } else { fields.title.clone() };
    Some(StoreTheme { node, name, stored: stored_theme(doc) })
}

impl Pimble {
    /// Every live theme node of `store` this principal may read, by name.
    /// A store whose backend holds its documents is walked through it;
    /// any other is held whole first, after which this is a read of what is
    /// held, kept current by the store's subscription.
    pub async fn themes(&self, store: StoreId) -> Result<Vec<StoreTheme>, ApiError> {
        let mut out = Vec::new();
        if self.passes_through(store) {
            let row = self.store_row(store).await.ok_or_else(|| ApiError::not_found("That store is not open here."))?;
            for walked in self.walk(&row).await? {
                if walked.addr.store != store || walked.node.node_type != node_types::THEME {
                    continue;
                }
                let doc = self.doc(Addr::new(store, walked.addr.node)).await?;
                out.extend(theme_of(walked.addr.node, &doc));
            }
        } else {
            self.hold_store(store).await?;
            let held = self.held.lock().unwrap();
            if let Some(h) = held.store(store) {
                for id in h.tree.ids() {
                    if let Some(doc) = h.tree.doc(id) {
                        out.extend(theme_of(id, doc));
                    }
                }
            }
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.node.0.cmp(&b.node.0)));
        Ok(out)
    }

    /// Whether [`Pimble::themes`] of `store` would read only what is here
    /// already: its backend holds its documents, or the whole store is held
    /// and kept current. A store's change lists its themes again only then,
    /// so a store nobody asked the themes of is never held whole for them.
    pub fn themes_at_hand(&self, store: StoreId) -> bool {
        self.passes_through(store) || self.held.lock().unwrap().store(store).is_some_and(|h| h.complete && h.live)
    }

    /// Change keys of theme `addr`'s `data.theme`, each set to its JSON or
    /// removed, in one edit: every key on its own, so two people changing
    /// different tokens both keep theirs. The values are checked by the
    /// caller (`pimble_themes::edit::typed`); anything not a theme node is
    /// refused. A theme node with no `data.theme` yet (made bare, by hand)
    /// gets an empty one first.
    pub async fn edit_theme(&self, addr: Addr, edits: Vec<(String, Option<serde_json::Value>)>) -> Result<bool, ApiError> {
        let edited = self
            .edit(addr, move |doc, _| {
                let fields = doc.fields()?;
                if fields.node_type != node_types::THEME {
                    return Err(ApiError::refused("That node is not a theme."));
                }
                if !matches!(doc.data_json().get(DATA_KEY), Some(serde_json::Value::Object(_))) {
                    doc.set_data(DATA_KEY, &serde_json::json!({}))?;
                }
                for (key, value) in &edits {
                    let path = [DataStep::Key(DATA_KEY), DataStep::Key(key)];
                    match value {
                        Some(json) => doc.edit_data(&path, DataEdit::Set(json))?,
                        None => doc.edit_data(&path, DataEdit::Remove)?,
                    };
                }
                Ok::<(), ApiError>(())
            })
            .await?;
        Ok(edited.changed)
    }

    /// Whether `addr` is a theme node's document, live or not: a change to
    /// it changes the store's list of themes. Answers from what is held (or,
    /// for a store whose backend holds its documents, from it), without
    /// asking a server.
    pub async fn is_theme(&self, addr: Addr) -> bool {
        if self.passes_through(addr.store) {
            return self.doc(addr).await.ok().and_then(|doc| doc.fields().ok()).is_some_and(|f| f.node_type == node_types::THEME);
        }
        self.held_doc_fields(addr).is_some_and(|fields| fields.node_type == node_types::THEME)
    }
}

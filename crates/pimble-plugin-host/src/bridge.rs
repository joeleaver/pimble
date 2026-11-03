//! The host's side of the API bridge (docs/PLUGINS_CONTRACT.md section 3.4, wave
//! 2): what a plugin may call, and making the call through the app's own backend.
//!
//! A plugin's `Pimble` runs over the guest SDK's `GuestBackend`, so every call
//! arrives here as an [`ApiCall`]. It is judged twice, by two different rules:
//!
//! - **here**, against what the person approved for the plugin on this device
//!   ([`Approval`]: which stores, and whether it may change them), so a plugin
//!   never reaches a store it was not given, even one the person may read;
//! - **by the server**, with the person's own role, because the call is made
//!   through the app's backend and connection exactly as the app's own would be
//!   ([`execute`]): a plugin is granted nothing the person does not have.
//!
//! Every edit is attributed to the plugin: `apply` carries
//! `client_id = "plugin:<id>"`, whatever the plugin's own `Pimble` would have said.
//!
//! Engine and platform agnostic: the desktop runs [`execute`] on its backend
//! thread, and the browser's host (wave 3) will run it in the page.

use pimble_api::backend::ChangeStream;
use pimble_api::{ApiError, Backend};
use pimble_core::StoreId;
use pimble_plugin_abi::api::{plugin_client_id, ApiCall, ApiReply, Bytes, Capabilities, Reach, Synced};
use pimble_plugin_abi::{Permissions, StoreAsk};
use serde::{Deserialize, Serialize};

/// What the person allowed a plugin on this device (decision 2): the stores it
/// may reach, and whether it may change them. Remembered per device and asked
/// once, on the plugin's first use.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Approval {
    pub stores: Approved,
    /// It may change what it reads, not only read it.
    pub write: bool,
}

/// The stores an approval covers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approved {
    /// Every store, now and later.
    All,
    /// These stores only.
    Only(Vec<StoreId>),
    /// None: what an approval is before anything was allowed.
    #[default]
    Nothing,
}

impl Approval {
    /// Whether the plugin may reach `store` this way.
    pub fn allows(&self, store: StoreId, reach: Reach) -> bool {
        let store_ok = match &self.stores {
            Approved::All => true,
            Approved::Only(stores) => stores.contains(&store),
            Approved::Nothing => false,
        };
        store_ok && (reach == Reach::Read || self.write)
    }

    /// Whether the plugin may read `store` at all.
    pub fn reads(&self, store: StoreId) -> bool {
        self.allows(store, Reach::Read)
    }

    /// Whether this approval already covers what `asks` asks for, for a
    /// plugin shown in `store`: nothing needs to be asked of the person.
    pub fn covers(&self, asks: &Permissions, store: StoreId) -> bool {
        if asks.write && !self.write {
            return false;
        }
        match asks.stores {
            StoreAsk::None => true,
            StoreAsk::All => self.stores == Approved::All,
            StoreAsk::Chosen => self.reads(store),
        }
    }

    /// This approval, grown to cover `asks` for a plugin shown in `store`: what
    /// the person's "Allow" gives.
    pub fn grown(&self, asks: &Permissions, store: StoreId) -> Approval {
        let stores = match (asks.stores, &self.stores) {
            (StoreAsk::None, s) => s.clone(),
            (StoreAsk::All, _) | (_, Approved::All) => Approved::All,
            (StoreAsk::Chosen, Approved::Only(stores)) => {
                let mut stores = stores.clone();
                if !stores.contains(&store) {
                    stores.push(store);
                }
                Approved::Only(stores)
            }
            (StoreAsk::Chosen, Approved::Nothing) => Approved::Only(vec![store]),
        };
        Approval { stores, write: self.write || asks.write }
    }

    /// The refusal for a call this approval does not cover, or `None` when it
    /// does. `plugin` is the plugin's name, for the sentence.
    pub fn check(&self, plugin: &str, call: &ApiCall) -> Result<(), ApiError> {
        for (store, reach) in call.stores() {
            if self.allows(store, reach) {
                continue;
            }
            return Err(self.refusal(plugin, store, reach));
        }
        Ok(())
    }

    fn refusal(&self, plugin: &str, store: StoreId, reach: Reach) -> ApiError {
        let sentence = if reach == Reach::Write && self.reads(store) {
            format!("The {plugin} plugin was allowed to read this store, not to change it.")
        } else {
            format!("The {plugin} plugin was not allowed to use that store.")
        };
        ApiError::NoGrant { sentence }
    }

    /// The stores to search when a plugin asks to search every store: every one
    /// it may read, or `None` for "every store" when it may read all of them.
    fn search_scope(&self) -> Option<Vec<StoreId>> {
        match &self.stores {
            Approved::All => None,
            Approved::Only(stores) => Some(stores.clone()),
            Approved::Nothing => Some(Vec::new()),
        }
    }
}

/// What a person reads when a plugin asks to be allowed `asks`: "read and change
/// all your stores", "read the store Family".
pub fn asking_sentence(plugin: &str, asks: &Permissions, store_name: &str) -> String {
    let verb = if asks.write { "read and change" } else { "read" };
    let what = match asks.stores {
        StoreAsk::All => "all your stores".to_string(),
        StoreAsk::Chosen | StoreAsk::None => format!("the store {store_name}"),
    };
    format!("The {plugin} plugin wants to {verb} {what}.")
}

/// Make `call` for plugin `plugin_id` through `backend`: the app's own backend,
/// so the server judges it with the person's role. The approval is checked again
/// (the caller checked it before forwarding; this is where the answer is made,
/// so it is also where an answer reaching past the approval is cut: a store list
/// is narrowed to the approved stores, children read through a mount into a
/// store not approved are refused). A subscription is [`subscribe`]'s.
pub async fn execute(backend: &dyn Backend, plugin_id: &str, plugin: &str, approval: &Approval, call: ApiCall) -> ApiReply {
    if let Err(e) = approval.check(plugin, &call) {
        return ApiReply::error(e);
    }
    match run(backend, plugin_id, plugin, approval, call).await {
        Ok(reply) => reply,
        Err(e) => ApiReply::error(e),
    }
}

async fn run(backend: &dyn Backend, plugin_id: &str, plugin: &str, approval: &Approval, call: ApiCall) -> Result<ApiReply, ApiError> {
    use ApiCall as C;
    Ok(match call {
        C::ListStores => {
            let stores: Vec<_> = backend.list_stores().await?.into_iter().filter(|s| approval.reads(s.id)).collect();
            // What each can do where this host reaches it (an encrypted store
            // in the browser is the vault's), for the plugin's own
            // `capabilities`.
            let capabilities = stores.iter().map(|s| Capabilities::of(s.id, backend.capabilities(s.id))).collect();
            ApiReply::Stores { stores, capabilities }
        }
        C::FetchOne { store, node } => ApiReply::Node { node: backend.fetch_one(store, node).await? },
        C::Fetch { store, nodes } => ApiReply::Nodes { nodes: backend.fetch(store, nodes).await? },
        C::Children { store, node } => {
            let (lives_in, nodes) = backend.children(store, node).await?;
            // Through a mount, the children live in the mount's source store,
            // which the approval must cover too.
            if !approval.reads(lives_in) {
                return Err(approval.refusal(plugin, lives_in, Reach::Read));
            }
            ApiReply::Children { store: lives_in, nodes }
        }
        C::Sync { store, have, list_unknown } => {
            let have = have.into_iter().map(|(n, sv)| (n, sv.0)).collect();
            let (docs, unknown) = backend.sync(store, have, list_unknown).await?;
            let docs = docs.into_iter().map(|d| Synced { node: d.node, diff: Bytes(d.diff), state_vector: Bytes(d.state_vector) }).collect();
            ApiReply::Synced { docs, unknown }
        }
        C::Apply { store, node, update } => {
            backend.apply(store, node, update.0, &plugin_client_id(plugin_id)).await?;
            ApiReply::Done
        }
        C::CreateNode { store, parent, init, place } => ApiReply::Id { node: backend.create_node(store, parent, init.into(), place).await? },
        C::CreateAttachment { store, host, init } => ApiReply::Id { node: backend.create_attachment(store, host, init.into()).await? },
        C::MoveNode { store, node, parent, place } => ApiReply::Moved { moved: backend.move_node(store, node, parent, place).await?.into() },
        C::MoveNodes { store, nodes, parent, place } => {
            let moved = backend.move_nodes(store, nodes, parent, place).await?;
            ApiReply::MovedMany { moved: moved.into_iter().map(Into::into).collect() }
        }
        C::Transplant { from, node, to, parent, place } => ApiReply::Moved { moved: backend.transplant(from, node, to, parent, place).await?.into() },
        C::Delete { store, node } => {
            backend.delete(store, node).await?;
            ApiReply::Done
        }
        C::Undelete { store, node } => {
            backend.undelete(store, node).await?;
            ApiReply::Done
        }
        C::ResolveLink { store, node } => ApiReply::Link { resolution: backend.resolve_link(store, node).await? },
        C::ListDeleted { store } => ApiReply::Deleted { nodes: backend.list_deleted(store).await? },
        C::ListAttachments { store, host } => ApiReply::Nodes { nodes: backend.list_attachments(store, host).await? },
        C::ListDiscussions { store } => ApiReply::Nodes { nodes: backend.list_discussions(store).await? },
        C::Search { query, stores, semantic, limit } => {
            // "Every store" is every store the plugin may read.
            let stores = if stores.is_empty() { approval.search_scope().unwrap_or_default() } else { stores };
            if stores.is_empty() && approval.stores != Approved::All {
                ApiReply::Results { items: Vec::new() }
            } else {
                ApiReply::Results { items: backend.search(&query, stores, semantic, limit).await? }
            }
        }
        C::PutBlob { store, node, mime, bytes } => ApiReply::BlobUrl { url: backend.put_blob(store, node, &mime, &bytes.0).await? },
        C::GetBlob { store, blob } => {
            let (mime, bytes) = backend.get_blob(store, blob).await?;
            ApiReply::Blob { mime, bytes: Bytes(bytes) }
        }
        C::WhoAmI => {
            let (sub, name) = backend.who_am_i().await?;
            ApiReply::Me { sub, name }
        }
        C::Subscribe { .. } => {
            return Err(ApiError::Failed { sentence: "A subscription is made with `subscribe`, not `execute`.".into() });
        }
        C::People { store } => ApiReply::People { people: backend.people(store).await? },
        C::CreateMount { store, parent, source_store, source_node, title } => {
            let (node, mount) = backend.create_mount(store, parent, source_store, source_node, title).await?;
            ApiReply::Mounted { node, mount }
        }
        C::MountState { store, node } => {
            let (state, mount) = backend.mount_state(store, node).await?;
            ApiReply::MountState { state, mount }
        }
    })
}

/// Subscribe a plugin to `store`'s changes through `backend`, when its approval
/// lets it read the store. Each item goes back as an [`ApiReply::Change`]; the
/// stream's end as [`ApiReply::Ended`].
pub async fn subscribe(backend: &dyn Backend, plugin: &str, approval: &Approval, store: StoreId) -> Result<ChangeStream, ApiError> {
    approval.check(plugin, &ApiCall::Subscribe { store })?;
    backend.subscribe(store).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::{NodeId, Place};

    fn ids() -> (StoreId, StoreId, NodeId) {
        (StoreId::new(), StoreId::new(), NodeId::new())
    }

    #[test]
    fn a_call_outside_the_approval_is_refused_with_a_sentence() {
        let (mine, other, node) = ids();
        let approval = Approval { stores: Approved::Only(vec![mine]), write: true };
        assert_eq!(approval.check("Hello viewer", &ApiCall::FetchOne { store: mine, node }), Ok(()));
        assert_eq!(approval.check("Hello viewer", &ApiCall::ListStores), Ok(()));
        let refused = approval.check("Hello viewer", &ApiCall::FetchOne { store: other, node }).unwrap_err();
        assert_eq!(refused.sentence(), "The Hello viewer plugin was not allowed to use that store.");
        assert!(refused.is_refusal());
        // A transplant out of an approved store into one that is not.
        let call = ApiCall::Transplant { from: mine, node, to: other, parent: node, place: Place::Last };
        assert!(approval.check("Hello viewer", &call).is_err());
        // Nothing approved yet reads nothing.
        assert!(Approval::default().check("Hello viewer", &ApiCall::FetchOne { store: mine, node }).is_err());
    }

    #[test]
    fn a_read_only_approval_refuses_writes() {
        let (mine, _, node) = ids();
        let approval = Approval { stores: Approved::All, write: false };
        assert_eq!(approval.check("Hello viewer", &ApiCall::Children { store: mine, node }), Ok(()));
        assert_eq!(approval.check("Hello viewer", &ApiCall::Subscribe { store: mine }), Ok(()));
        for call in [
            ApiCall::Apply { store: mine, node, update: Bytes(vec![1]) },
            ApiCall::MoveNode { store: mine, node, parent: node, place: Place::First },
            ApiCall::Delete { store: mine, node },
        ] {
            let refused = approval.check("Hello viewer", &call).unwrap_err();
            assert_eq!(refused.sentence(), "The Hello viewer plugin was allowed to read this store, not to change it.");
        }
    }

    #[test]
    fn allowing_grows_the_approval_to_what_was_asked_for_here() {
        let (a, b, _) = ids();
        let asks = Permissions { stores: StoreAsk::Chosen, write: true, network: false };
        let none = Approval::default();
        assert!(!none.covers(&asks, a));
        let once = none.grown(&asks, a);
        assert_eq!(once, Approval { stores: Approved::Only(vec![a]), write: true });
        assert!(once.covers(&asks, a));
        assert!(!once.covers(&asks, b), "another store is asked for again");
        assert_eq!(once.grown(&asks, b).stores, Approved::Only(vec![a, b]));
        let all = Permissions { stores: StoreAsk::All, write: false, network: false };
        assert_eq!(none.grown(&all, a), Approval { stores: Approved::All, write: false });
        assert_eq!(
            asking_sentence("Hello viewer", &asks, "Family"),
            "The Hello viewer plugin wants to read and change the store Family."
        );
        assert_eq!(asking_sentence("Hello viewer", &all, "Family"), "The Hello viewer plugin wants to read all your stores.");
    }
}

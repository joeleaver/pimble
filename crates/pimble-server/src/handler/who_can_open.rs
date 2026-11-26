//! `whoCanOpen` (docs/MCP_CONTRACT.md "Who can open a note"): the members
//! of the shares one node is in, for the operator and for an AI agent that
//! may read the node.

use http::Extensions;
use jsonrpsee::types::ErrorObjectOwned;
use pimble_core::{custom_keys, StoreKind};
use pimble_rpc::{to_rpc_error, Opener, WhoCanOpenRequest, WhoCanOpenResponse};
use pimble_store::StoreError;

use super::{require_in_scope, Reach, RpcHandler};
use crate::agents::AgentConnection;
use crate::principal::{authorize, authorize_service_only, principal_of, Access};

impl RpcHandler {
    /// Who can open `request`'s node, from the kept list of who is who
    /// (`crate::people`): Pimble Cloud is not asked, so an agent never
    /// spends the person's account session.
    pub(super) async fn openers(&self, ext: &Extensions, request: WhoCanOpenRequest) -> Result<WhoCanOpenResponse, ErrorObjectOwned> {
        let WhoCanOpenRequest { store_id, node_id } = request;
        let principal = principal_of(ext);
        // An agent is the person's own assistant on this computer. A share's
        // member on a server is not one, and learns nothing here.
        if ext.get::<AgentConnection>().is_none() {
            authorize_service_only(&principal, "whoCanOpen")?;
        }
        authorize(&principal, store_id, Access::Read)?;
        self.reject_if_vault(store_id).await?;
        {
            let manager = self.store_manager.read().await;
            require_in_scope(&Reach::of(&manager, &principal, store_id), node_id, Access::Read)?;
            let tree = manager.tree(store_id).map_err(to_rpc_error)?;
            if !tree.has_node(node_id) {
                return Err(to_rpc_error(StoreError::NodeNotFound(node_id)));
            }
        }

        // The node and what is above it, a discussion's subject included.
        let chain = crate::notices::chain_of(self, store_id, node_id).await;
        let kept = crate::people::kept(self, store_id).await.map_err(to_rpc_error)?;
        let manager = self.store_manager.read().await;
        let tree = manager.tree(store_id).map_err(to_rpc_error)?;
        // A share that ended is no longer this account's: who holds it now
        // cannot be known here, and what was kept would be stale.
        if manager.under_ended_root(store_id, node_id) {
            return Ok(WhoCanOpenResponse { people: Vec::new(), incomplete: true });
        }
        // The node may be in a share still held inside one that ended: the
        // ended one's members are left out, and may open it all the same.
        let (chain, ended): (Vec<_>, Vec<_>) = chain.into_iter().partition(|&id| !manager.under_ended_root(store_id, id));
        // A share this node is in, by the marker on its root, that the kept
        // list does not name.
        let unlisted_share = chain.iter().any(|&root| {
            tree.get_node_info(root).is_ok_and(|info| info.custom.contains_key(custom_keys::SHARE))
                && !kept.as_ref().is_some_and(|people| people.alone.contains(&root) || people.scopes.iter().any(|scope| scope.root == Some(root)))
        });
        // A store on Pimble Cloud may have members of the whole of it, which
        // no marker says.
        let never_told = kept.is_none() && self.link_kind_of_locked(&manager, store_id).await.0 == StoreKind::Vault;
        let people = kept.unwrap_or_default().members_of(&chain).into_iter().map(|person| Opener { user_id: person.user_id, name: person.name }).collect();
        Ok(WhoCanOpenResponse { people, incomplete: unlisted_share || never_told || !ended.is_empty() })
    }
}

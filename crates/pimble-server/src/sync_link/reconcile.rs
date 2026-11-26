//! The reconcile of a plain link (decision 5, per document): what the
//! remote and this store each lack of the other, by state vectors, and the
//! pushes that close the gap. Every push goes through [`push_update_remotely`],
//! which sends nothing this replica may not write ([`crate::grant::may_push`])
//! and leaves a document the remote refuses for a later connect.

use base64::Engine;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
use pimble_rpc::{EditOperation, MAX_SYNC_NODE_CONTENTS};
use tracing::debug;

use super::{apply_update_locally, Channels};
use crate::handler::{Repair, RpcHandler};

/// A full reconcile over documents (decision 5): every held document's
/// state vector goes to the remote in one `syncNodes` (batched by the
/// client) that also asks what it did not name; what comes back is merged,
/// the named unknowns are fetched in batches of [`MAX_SYNC_NODE_CONTENTS`]
/// and merged, every held document is pushed back to the extent the remote
/// lacks it (whole when the remote never heard of it), and the tree is
/// repaired once at the end (see `Repair::Later`: a repair between two
/// batches would judge a half-arrived tree). The live channels are drained
/// every batch (see [`Channels`]).
pub(super) async fn full_reconcile(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, link_id: &str, channels: &mut Channels) -> anyhow::Result<()> {
    let manager = handler.store_manager_handle();

    let local_svs: Vec<(NodeId, Vec<u8>)> = {
        let manager = manager.read().await;
        let ids = manager.doc_ids(store_id)?;
        let mut svs = Vec::with_capacity(ids.len());
        for node_id in ids {
            svs.push((node_id, manager.node_state_vector(store_id, node_id)?));
        }
        svs
    };

    let (answers, unknown) = client
        .sync_nodes(store_id, &local_svs, true)
        .await
        .map_err(|e| anyhow::anyhow!("remote syncNodes failed: {}", e))?;

    // The pull always applies (decision 7's finding: a yrs v1 diff is never
    // actually empty — `[0, 0]` at minimum plus the sender's whole delete
    // set — so an `is_empty()` gate here would always be true); the handler
    // does its own no-op check (decision 8), so applying a diff that turns
    // out to carry nothing new is cheap. Then the push back, only where the
    // remote lacks something.
    let mut answered = std::collections::HashSet::with_capacity(answers.len());
    for chunk in answers.chunks(MAX_SYNC_NODE_CONTENTS) {
        for (node_id, diff, remote_sv) in chunk {
            answered.insert(*node_id);
            apply_update_locally(handler, store_id, *node_id, link_id, diff, Repair::Later).await?;
            push_node_diff(handler, client, store_id, *node_id, link_id, remote_sv, diff).await?;
        }
        channels.drain(handler, client, store_id, link_id).await?;
    }

    // Documents the remote holds and this store does not: asked for whole
    // (an empty state vector), and nothing to push back for them.
    let empty_sv = pimble_crdt::empty_state_vector();
    for chunk in unknown.chunks(MAX_SYNC_NODE_CONTENTS) {
        let wanted: Vec<(NodeId, Vec<u8>)> = chunk.iter().map(|id| (*id, empty_sv.clone())).collect();
        let (fetched, _) = client
            .sync_nodes(store_id, &wanted, false)
            .await
            .map_err(|e| anyhow::anyhow!("remote syncNodes for unknown documents failed: {}", e))?;
        for (node_id, diff, _) in fetched {
            apply_update_locally(handler, store_id, node_id, link_id, &diff, Repair::Later).await?;
        }
        channels.drain(handler, client, store_id, link_id).await?;
    }

    // Documents this store holds and the remote does not: its `applyEdit`
    // makes a document for an id it has never seen, so the whole state goes
    // (`save`, which unlike a diff from an empty vector carries what is
    // still pending here too).
    let unanswered: Vec<NodeId> = local_svs.iter().map(|(id, _)| *id).filter(|id| !answered.contains(id)).collect();
    for chunk in unanswered.chunks(MAX_SYNC_NODE_CONTENTS) {
        for node_id in chunk {
            let whole = {
                let manager = manager.read().await;
                match manager.tree(store_id)?.doc(*node_id) {
                    Some(doc) => doc.save(),
                    None => continue,
                }
            };
            if push_update_remotely(handler, client, store_id, *node_id, link_id, &whole).await? {
                debug!("Sync link for store {} pushed the whole document of node {} to the remote", store_id, node_id);
            }
        }
        channels.drain(handler, client, store_id, link_id).await?;
    }

    handler.repair_store_tree(store_id).await;
    Ok(())
}

/// Reconcile one document with the remote (decision 5), the fallback for a
/// notification about it that carried no bytes.
pub(super) async fn reconcile_node(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, node_id: NodeId, link_id: &str) -> anyhow::Result<()> {
    let manager = handler.store_manager_handle();

    let local_sv = {
        let manager = manager.read().await;
        // Not held here yet: everything the remote has of it is wanted.
        manager.node_state_vector(store_id, node_id).unwrap_or_else(|_| pimble_crdt::empty_state_vector())
    };

    let (diff, remote_sv) = client
        .sync_node_content(store_id, node_id, &local_sv)
        .await
        .map_err(|e| anyhow::anyhow!("remote syncNodes for node {} failed: {}", node_id, e))?;

    apply_update_locally(handler, store_id, node_id, link_id, &diff, Repair::Debounced).await?;

    push_node_diff(handler, client, store_id, node_id, link_id, &remote_sv, &diff).await
}

/// Send the remote everything this node's local document has beyond
/// `remote_sv`, if anything — `remote_diff` (the diff the remote just sent
/// us, en route to `remote_sv`) is what `diff_if_peer_lacks_it` needs to
/// tell "nothing new" apart from "carries the remote's whole delete set, as
/// every yrs diff does" (decision 7).
async fn push_node_diff(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    node_id: NodeId,
    link_id: &str,
    remote_sv: &[u8],
    remote_diff: &[u8],
) -> anyhow::Result<()> {
    let local_push = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        let tree = manager.tree(store_id)?;
        match tree.doc(node_id) {
            Some(doc) => doc.diff_if_peer_lacks_it(remote_sv, remote_diff)?,
            None => None,
        }
    };

    if let Some(local_diff) = local_push {
        if push_update_remotely(handler, client, store_id, node_id, link_id, &local_diff).await? {
            debug!("Sync link for store {} pushed a diff for node {} to the remote", store_id, node_id);
        }
    }

    Ok(())
}

/// Send one document's update to the remote, answering whether it went
/// (see [`send_edit`]).
async fn push_update_remotely(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    node_id: NodeId,
    link_id: &str,
    update: &[u8],
) -> anyhow::Result<bool> {
    send_edit(handler, client, store_id, node_id, link_id, base64::engine::general_purpose::STANDARD.encode(update)).await
}

/// Send one document's update (`changes`, base64 yrs bytes) to the remote
/// as an `applyEdit`, answering whether it went. Nothing goes for a document
/// this replica may not write (a reader's, or a commenter's outside a
/// discussion), and a document the remote refuses (out of the account's
/// scope now, or under a root it only reads: `crate::vault_link::is_refusal`)
/// is left for a later connect rather than taking the link down; any other
/// failure does.
pub(super) async fn send_edit(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    node_id: NodeId,
    link_id: &str,
    changes: String,
) -> anyhow::Result<bool> {
    if !crate::grant::may_push(handler, store_id, node_id).await {
        return Ok(false);
    }
    match client.apply_edit(store_id, node_id, link_id, EditOperation::IncrementalChanges { changes }).await {
        Ok(_) => Ok(true),
        Err(e) if crate::vault_link::is_refusal(&e) => {
            debug!("Sync link for store {}: the remote refused node {} ({}); left for a later connect", store_id, node_id, e);
            Ok(false)
        }
        Err(e) => Err(anyhow::anyhow!("remote applyEdit for node {} failed: {}", node_id, e)),
    }
}

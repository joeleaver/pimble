//! The Pimble Cloud commands, on the host connection: the account,
//! notifications and watches, hosting, relaying and sharing.

use pimble_client::PimbleClient;

use super::Context;
use crate::protocol::{BackendCommand, BackendEvent, CloudOp};

/// Answer a Pimble Cloud command on the host connection (what
/// [`super::host_command`] does not answer itself).
pub(super) async fn cloud_command(ctx: &Context<'_>, cmd: BackendCommand) -> Option<BackendEvent> {
    match cmd {
        // Pimble Cloud account (docs/DESKTOP_ACCOUNT_CONTRACT.md). Every
        // failure is a `CloudError` naming its operation, never the generic
        // `Error`, so the modal that asked gets it (decision 3).
        BackendCommand::CloudStatus => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Status, message: "Not connected".into() });
            };
            match c.cloud_status().await {
                Ok(status) => Some(cloud_status_event(status)),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Status, message: e.to_string() }),
            }
        }

        BackendCommand::CloudSignIn { url, email, password } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::SignIn, message: "Not connected".into() });
            };
            if let Err(e) = c.cloud_sign_in(url.clone(), email.clone(), password).await {
                return Some(BackendEvent::CloudError { op: CloudOp::SignIn, message: e.to_string() });
            }
            // The keystore's own record of the account is the answer; the
            // typed values stand in only if reading it back fails.
            match c.cloud_status().await {
                Ok(status) => Some(cloud_status_event(status)),
                Err(_) => Some(BackendEvent::CloudStatusChanged {
                    signed_in: true,
                    email: Some(email),
                    user_id: None,
                    url: Some(url),
                }),
            }
        }

        BackendCommand::CloudSignOut => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::SignOut, message: "Not connected".into() });
            };
            match c.cloud_sign_out().await {
                Ok(()) => Some(BackendEvent::CloudStatusChanged { signed_in: false, email: None, user_id: None, url: None }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::SignOut, message: e.to_string() }),
            }
        }

        BackendCommand::ListNotices => {
            let c = ctx.host.as_ref()?;
            match c.list_notices().await {
                Ok(listed) => Some(notices_listed(listed)),
                Err(e) => {
                    tracing::debug!("notices not listed: {e}");
                    None
                }
            }
        }

        BackendCommand::AcknowledgeNotices { ids } => {
            let c = ctx.host.as_ref()?;
            if let Err(e) = c.acknowledge_notices(ids).await {
                tracing::debug!("notices not acknowledged: {e}");
            }
            None
        }

        BackendCommand::AppLinkTaken { ack } => {
            let c = ctx.host.as_ref()?;
            if let Err(e) = c.app_link_taken(ack).await {
                tracing::debug!("the link taken was not confirmed: {e}");
            }
            None
        }

        BackendCommand::SetNotifications { on } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Notifications, message: "Not connected".into() });
            };
            if let Err(e) = c.set_notifications(on).await {
                return Some(BackendEvent::CloudError { op: CloudOp::Notifications, message: e.to_string() });
            }
            match c.list_notices().await {
                Ok(listed) => Some(notices_listed(listed)),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Notifications, message: e.to_string() }),
            }
        }

        BackendCommand::SetNotificationChannels { email, push } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Notifications, message: "Not connected".into() });
            };
            if let Err(e) = c.set_notification_channels(email, push).await {
                return Some(BackendEvent::CloudError { op: CloudOp::Notifications, message: e.to_string() });
            }
            match c.list_notices().await {
                Ok(listed) => Some(notices_listed(listed)),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Notifications, message: e.to_string() }),
            }
        }

        BackendCommand::SetWatchesOn { on } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Notifications, message: "Not connected".into() });
            };
            if let Err(e) = c.set_watches_on(on).await {
                return Some(BackendEvent::CloudError { op: CloudOp::Notifications, message: e.to_string() });
            }
            match c.list_watches().await {
                Ok(listed) => Some(watches_listed(listed)),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Notifications, message: e.to_string() }),
            }
        }

        BackendCommand::WatchNode { store_id, node_id, subtree } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Watches, message: "Not connected".into() });
            };
            match c.watch_node(store_id, node_id, subtree).await {
                Ok(listed) => Some(watches_listed(listed)),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Watches, message: e.to_string() }),
            }
        }

        BackendCommand::UnwatchNode { store_id, node_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Watches, message: "Not connected".into() });
            };
            match c.unwatch_node(store_id, node_id).await {
                Ok(listed) => Some(watches_listed(listed)),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Watches, message: e.to_string() }),
            }
        }

        BackendCommand::ListWatches => {
            let c = ctx.host.as_ref()?;
            match c.list_watches().await {
                Ok(listed) => Some(watches_listed(listed)),
                Err(e) => {
                    tracing::debug!("watches not listed: {e}");
                    None
                }
            }
        }

        BackendCommand::RegisterPush { endpoint, p256dh, auth } => {
            let c = ctx.host.as_ref()?;
            if let Err(e) = c.register_push(endpoint, p256dh, auth).await {
                tracing::info!("this browser's push subscription was not registered: {e}");
            }
            None
        }

        BackendCommand::UnregisterPush { endpoint } => {
            let c = ctx.host.as_ref()?;
            if let Err(e) = c.unregister_push(endpoint).await {
                tracing::debug!("this browser's push subscription was not taken back: {e}");
            }
            None
        }

        BackendCommand::CloudHostStore { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::HostStore, message: "Not connected".into() });
            };
            match c.cloud_host_store(store_id).await {
                Ok(store_id) => Some(BackendEvent::CloudStoreHosted { store_id }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::HostStore, message: e.to_string() }),
            }
        }

        BackendCommand::CloudListHostedStores => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::ListHostedStores, message: "Not connected".into() });
            };
            // The whole answer: which rows are relay-tier rides beside them.
            match c.cloud_list_hosted_stores_response().await {
                Ok(answer) => Some(BackendEvent::CloudHostedStoresListed { stores: answer.stores, relayed: answer.relayed }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::ListHostedStores, message: e.to_string() }),
            }
        }

        // Names (docs/PRESENCE_CONTRACT.md section 3).
        BackendCommand::CloudListShares => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Shares, message: "Not connected".into() });
            };
            Some(shares_listed(c).await)
        }
        BackendCommand::CloudSetDisplayName { name } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Shares, message: "Not connected".into() });
            };
            match c.cloud_set_display_name(&name).await {
                Ok(()) => Some(shares_listed(c).await),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Shares, message: e.to_string() }),
            }
        }
        BackendCommand::CloudSetShareName { store_id, root, name } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Shares, message: "Not connected".into() });
            };
            match c.cloud_set_share_name(store_id, root, &name).await {
                Ok(()) => Some(shares_listed(c).await),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Shares, message: e.to_string() }),
            }
        }
        // Sharing from this computer (docs/RELAY_CONTRACT.md). The link's
        // state arrives afterwards as ordinary `SyncStateChanged`
        // notifications; a refusal is the server's own sentence.
        BackendCommand::CloudRelayStore { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::RelayStore, message: "Not connected".into() });
            };
            match c.cloud_relay_store(store_id).await {
                Ok(store_id) => Some(BackendEvent::CloudStoreRelayed { store_id }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::RelayStore, message: e.to_string() }),
            }
        }

        BackendCommand::CloudStopRelaying { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::StopRelaying, message: "Not connected".into() });
            };
            match c.cloud_stop_relaying(store_id).await {
                Ok(()) => Some(BackendEvent::CloudRelayingStopped { store_id }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::StopRelaying, message: e.to_string() }),
            }
        }

        BackendCommand::CloudAddHostedStore { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::AddHostedStore, message: "Not connected".into() });
            };
            // The replica arrives the way `AddRemoteStore`'s does.
            match c.cloud_add_hosted_store(store_id).await {
                Ok(store) => Some(BackendEvent::StoreOpened { store }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::AddHostedStore, message: e.to_string() }),
            }
        }

        BackendCommand::CloudShareNode { store_id, node_id, name } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Share, message: "Not connected".into() });
            };
            Some(share_answer(CloudOp::Share, store_id, node_id, c.cloud_share_node(store_id, node_id, &name).await))
        }

        BackendCommand::CloudShareInfo { store_id, node_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::ShareInfo, message: "Not connected".into() });
            };
            Some(share_answer(CloudOp::ShareInfo, store_id, node_id, c.cloud_share_info(store_id, node_id).await))
        }

        BackendCommand::CloudShareInvite { store_id, node_id, email, role } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::ShareInvite, message: "Not connected".into() });
            };
            Some(share_answer(CloudOp::ShareInvite, store_id, node_id, c.cloud_share_invite(store_id, node_id, &email, role).await))
        }

        BackendCommand::CloudShareRemoveMember { store_id, node_id, email } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::ShareRemoveMember, message: "Not connected".into() });
            };
            Some(share_answer(CloudOp::ShareRemoveMember, store_id, node_id, c.cloud_share_remove_member(store_id, node_id, &email).await))
        }

        BackendCommand::CloudStopSharing { store_id, node_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::StopSharing, message: "Not connected".into() });
            };
            match c.cloud_stop_sharing(store_id, node_id).await {
                Ok(()) => Some(BackendEvent::CloudSharingStopped { store_id, node_id }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::StopSharing, message: e.to_string() }),
            }
        }

        other => {
            tracing::warn!("a command reached the host that is the API's: {other:?}");
            None
        }
    }
}

/// The `CloudStatusChanged` event for a `cloudStatus` answer.
fn cloud_status_event(status: pimble_rpc::CloudStatusResponse) -> BackendEvent {
    BackendEvent::CloudStatusChanged { signed_in: status.signed_in, email: status.email, user_id: status.user_id, url: status.url }
}

/// The event for a sharing RPC's answer (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5): the share and its members, or the error under the operation
/// that asked.
fn share_answer(
    op: CloudOp,
    store_id: pimble_core::StoreId,
    node_id: pimble_core::NodeId,
    answer: pimble_client::Result<pimble_rpc::CloudShareInfoResponse>,
) -> BackendEvent {
    match answer {
        Ok(response) => BackendEvent::CloudShareUpdated { store_id, node_id, share: response.share, members: response.members },
        Err(e) => BackendEvent::CloudError { op, message: e.to_string() },
    }
}

/// `CloudSharesListed`, or the error that kept it from being listed.
async fn shares_listed(c: &PimbleClient) -> BackendEvent {
    match c.cloud_list_shares().await {
        Ok(shares) => BackendEvent::CloudSharesListed { shares },
        Err(e) => BackendEvent::CloudError { op: CloudOp::Shares, message: e.to_string() },
    }
}

fn notices_listed(listed: pimble_rpc::ListNoticesResponse) -> BackendEvent {
    BackendEvent::NoticesListed { notices: listed.notices, on: listed.on, email: listed.email, push: listed.push, push_key: listed.push_key, watches: listed.watches }
}

fn watches_listed(listed: pimble_rpc::ListWatchesResponse) -> BackendEvent {
    BackendEvent::WatchesListed { watches: listed.watches, on: listed.on }
}

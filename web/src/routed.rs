//! The router's write gate in front of the app's commands
//! (docs/API_CONTRACT.md waves 4 and 5). Every command that reads or writes
//! a store is answered by `pimble_app::commands::api_command` through the
//! page's `pimble_api::Pimble` over a [`Router`] (plain stores to their
//! server, encrypted ones to `crate::vault_backend`, a transplant judged by
//! the router); before any of them, a write the account may not make is
//! refused here, with the same sentence for an encrypted store and a plain
//! one ([`gate`]).

use pimble_api::router::Write;
use pimble_api::{ApiError, Router};
use pimble_app::protocol::{BackendCommand, BackendEvent};
use pimble_core::{node_types, StoreId};

/// What kind of write a command is, for the router's gate; `None` for a
/// command that changes nothing.
///
/// Listed by hand rather than by exclusion: a command added later should
/// have to be thought about once rather than quietly become a write a
/// reader may make.
pub fn write_of(cmd: &BackendCommand) -> Option<Write> {
    use BackendCommand::*;
    match cmd {
        CreateAttachment { init, .. } if init.node_type == node_types::DISCUSSION => Some(Write::Discussion),
        BroadcastChanges { .. } | ApplyUpdate { .. } => Some(Write::Content),
        CreateNode { .. }
        | CreateAttachment { .. }
        | RenameNode { .. }
        | SetNodeAppearance { .. }
        | SetMetanode { .. }
        | DeleteNode { .. }
        | UndeleteNode { .. }
        | MoveNode { .. }
        | MoveNodes { .. }
        | TransplantNode { .. }
        | PutBlob { .. }
        | CreateMount { .. }
        | MountRemoteStore { .. }
        | SetStoreSync { .. }
        | RemoveReplica { .. } => Some(Write::Structure),
        _ => None,
    }
}

/// The router's refusal of a writing command on `store`, as the event the
/// app reads it from; `None` when it may go on.
pub fn gate(router: &Router, store: StoreId, cmd: &BackendCommand) -> Option<BackendEvent> {
    let write = write_of(cmd)?;
    let refusal = router.gate(store, write).err()?;
    Some(match cmd {
        BackendCommand::PutBlob { request_id, .. } => BackendEvent::BlobStored { request_id: *request_id, result: Err(refusal.sentence()) },
        _ => error_event(refusal),
    })
}

/// An error as the app shows it (`pimble_app::commands::error_message`).
pub fn error_event(e: ApiError) -> BackendEvent {
    BackendEvent::Error { message: pimble_app::commands::error_message(&e) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::{NodeId, Place};

    #[test]
    fn only_the_commands_that_change_something_are_writes() {
        let store_id = StoreId::new();
        let node_id = NodeId::new();
        assert_eq!(
            write_of(&BackendCommand::CreateNode { store_id, parent_id: None, title: String::new(), init: None, place: Default::default() }),
            Some(Write::Structure)
        );
        assert_eq!(write_of(&BackendCommand::MoveNode { store_id, node_id, new_parent_id: node_id, place: Place::Last }), Some(Write::Structure));
        assert_eq!(write_of(&BackendCommand::BroadcastChanges { store_id, node_id, update: Vec::new() }), Some(Write::Content));
        let discussion = pimble_app::protocol::NodeInit { node_type: node_types::DISCUSSION.into(), content: Vec::new(), custom: Vec::new(), request: None };
        assert_eq!(
            write_of(&BackendCommand::CreateAttachment { store_id, host_id: node_id, title: String::new(), init: discussion }),
            Some(Write::Discussion)
        );
        assert_eq!(write_of(&BackendCommand::GetChildren { store_id, node_id }), None);
        assert_eq!(write_of(&BackendCommand::SubscribeStoreChanges { store_id }), None);
        assert_eq!(write_of(&BackendCommand::ListStores), None);
    }

    #[test]
    fn a_store_whose_owner_is_away_says_so_as_a_notice() {
        let refused = error_event(ApiError::OwnerOffline { sentence: pimble_api::router::OWNER_OFFLINE_REFUSAL.into() });
        assert!(matches!(&refused, BackendEvent::Error { message } if message == &format!("Forbidden: {}", pimble_api::router::OWNER_OFFLINE_REFUSAL)));
        let read_only = error_event(ApiError::ReadOnly);
        assert!(matches!(&read_only, BackendEvent::Error { message } if message == pimble_core::StoreAccess::READ_ONLY_REFUSAL));
    }
}

//! The names an answer carries for the request it answers: [`Asked`] for a
//! failure a modal waits on, [`CloudOp`] for every cloud outcome.

use super::{BackendCommand, BackendEvent};

/// Which request a failure answers, for the commands a modal waits on
/// (docs/API_CONTRACT.md wave 5, "answers instead of guesses"): the error
/// reaches the modal that asked because the adapter says which command it
/// answers (`BackendEvent::Failed`), not because the modal happens to be
/// waiting when some error arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// "Add Remote Store..." and "Mount Remote Store Here...".
    AddRemoteStore,
    /// The link modal's `SetStoreSync`.
    SetStoreSync,
    RemoveReplica,
    /// "New Store...": a store file on the desktop, a hosted store in the
    /// browser.
    CreateStore,
    /// The mount picker's `CreateMount`.
    CreateMount,
    /// "Recently Deleted...": its list, and putting a tombstone back.
    ListDeleted,
    UndeleteNode,
}

impl Asked {
    /// The request `cmd` is, when a failure of it has a modal to go to.
    pub fn of(cmd: &BackendCommand) -> Option<Asked> {
        Some(match cmd {
            BackendCommand::AddRemoteStore { .. } | BackendCommand::MountRemoteStore { .. } => Asked::AddRemoteStore,
            BackendCommand::SetStoreSync { .. } => Asked::SetStoreSync,
            BackendCommand::RemoveReplica { .. } => Asked::RemoveReplica,
            BackendCommand::CreateStore { .. } | BackendCommand::CreateHostedStore { .. } => Asked::CreateStore,
            BackendCommand::CreateMount { .. } => Asked::CreateMount,
            BackendCommand::ListDeleted { .. } => Asked::ListDeleted,
            BackendCommand::UndeleteNode { .. } => Asked::UndeleteNode,
            _ => return None,
        })
    }

    /// `answer` as the event the UI takes it as: an `Error` answering a
    /// request a modal waits on becomes `Failed`, naming the request.
    pub fn answer(asked: Option<Asked>, answer: Option<BackendEvent>) -> Option<BackendEvent> {
        match (asked, answer) {
            (Some(asked), Some(BackendEvent::Error { message })) => Some(BackendEvent::Failed { asked, message }),
            (_, answer) => answer,
        }
    }
}

/// Which cloud request an outcome belongs to (docs/DESKTOP_ACCOUNT_CONTRACT.md
/// decision 3): every cloud event names its operation, so the event handler
/// routes an error to the modal that asked without a pending flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudOp {
    Status,
    SignIn,
    SignOut,
    HostStore,
    ListHostedStores,
    AddHostedStore,
    RelayStore,
    StopRelaying,
    Share,
    ShareInfo,
    ShareInvite,
    ShareRemoveMember,
    StopSharing,
    /// The Shares dialog: listing shares, setting a name.
    Shares,
    /// The notifications switch (docs/NOTIFICATIONS_CONTRACT.md section 6).
    Notifications,
    /// Watching or unwatching a node (section 3, wave 3).
    Watches,
}

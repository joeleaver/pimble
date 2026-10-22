//! Help > About: this app's version, the server it talks to, and where
//! keeping it current stands (docs/UPDATE_CONTRACT.md "Help > About").

use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::{AppStore, ServerVersion, UpdateStatus};

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Open the window, asking the server for its version as it opens.
pub fn open_about(store: AppStore) {
    store.server_version.set(None);
    store.send(BackendCommand::ServerInfo);
    store.about_open.set(true);
}

/// The server line: its version, and a sentence when it is older than this
/// app (it was started by an older Pimble or pimble-mcp, docs/MCP_CONTRACT.md
/// "Which server it talks to").
fn server_line(v: &Option<ServerVersion>) -> String {
    let what = if cfg!(feature = "native") { "Server" } else { "Pimble Cloud server" };
    match v {
        None => format!("{what}: asking..."),
        Some(ServerVersion::Unknown) => format!("{what}: from before 0.12.2, older than this app"),
        Some(ServerVersion::Known(s)) if s == VERSION => format!("{what}: {s}"),
        Some(ServerVersion::Known(s)) if pimble_server_older(s) => format!("{what}: {s}, older than this app"),
        Some(ServerVersion::Known(s)) => format!("{what}: {s}"),
    }
}

/// `x.y.z` compared as numbers.
fn pimble_server_older(theirs: &str) -> bool {
    let key = |v: &str| -> (u64, u64, u64) {
        let mut p = v.split(['.', '-', '+']).map(|x| x.parse::<u64>().unwrap_or(0));
        (p.next().unwrap_or(0), p.next().unwrap_or(0), p.next().unwrap_or(0))
    };
    key(theirs) < key(VERSION)
}

/// What the update part says.
fn update_line(s: &UpdateStatus) -> String {
    if !cfg!(feature = "native") {
        return "This is the web app: each visit loads the version pimble.app serves.".to_string();
    }
    match s {
        UpdateStatus::NotInstalled => "This copy does not update itself: it is not the installed app (a development build, or run from where it was unpacked).".to_string(),
        UpdateStatus::Waiting => "Updates itself. The first check runs a moment after start.".to_string(),
        UpdateStatus::Checking => "Checking for a newer version...".to_string(),
        UpdateStatus::UpToDate { at } => format!("Up to date. Last checked {at}; it checks daily."),
        UpdateStatus::Ready { version } => format!("Pimble {version} is downloaded and checked; restart to update."),
        UpdateStatus::Failed { reason, at } => format!("The last check, {at}, failed: {reason}. It tries again daily, or now."),
    }
}

pub fn render_about(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    let can_check = move || cfg!(feature = "native") && !matches!(store.update_status.get(), UpdateStatus::NotInstalled | UpdateStatus::Checking | UpdateStatus::Ready { .. });
    let ready = move || matches!(store.update_status.get(), UpdateStatus::Ready { .. });
    rsx! {
        Modal {
            opened_fn: move || store.about_open.get(),
            onclose: move || store.about_open.set(false),
            title: "About Pimble",
            size: "sm",
            div {
                style: "display: flex; flex-direction: column; gap: 10px;",
                div { style: "font-size: 16px; font-weight: 600;", {format!("Pimble {VERSION}")} }
                div { style: "font-size: 13px;", {move || server_line(&store.server_version.get())} }
                div { style: "font-size: 13px; color: var(--rinch-color-dimmed);", {move || update_line(&store.update_status.get())} }
                div {
                    style: "display: flex; gap: 8px; justify-content: flex-end;",
                    if can_check() {
                        Button { variant: "default", size: "xs", onclick: move || check_now(), "Check Now" }
                    }
                    if ready() {
                        Button { variant: "filled", size: "xs", onclick: move || restart(store), "Restart to Update" }
                    }
                    Button { variant: "default", size: "xs", onclick: move || store.about_open.set(false), "Close" }
                }
            }
        }
    }
}

fn check_now() {
    #[cfg(feature = "native")]
    crate::update::check_now();
}

#[cfg_attr(not(feature = "native"), allow(unused_variables))]
fn restart(store: AppStore) {
    #[cfg(feature = "native")]
    crate::update::restart_to_update(store);
}

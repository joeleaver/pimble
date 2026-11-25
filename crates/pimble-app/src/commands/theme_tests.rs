//! A store's themes through the commands, end to end on a real server
//! (docs/THEMES_CONTRACT.md sections 3.1 and 6): New numbers its name past
//! the store's themes, a copy from another store folds the original's chain
//! into its own tokens and is reused, "Customize" bases a new theme on the
//! copy, and a store's themes are at hand only once something listed them.

use std::sync::Arc;

use pimble_api::rpc::RpcBackend;
use pimble_api::{Pimble, StoreTheme};
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;

use super::themes::theme_command;
use super::Context;
use crate::protocol::{BackendEvent, ThemeCommand};

/// The theme a command made, from its `ThemeMade`.
fn made(event: Option<BackendEvent>) -> NodeId {
    match event {
        Some(BackendEvent::ThemeMade { node_id, .. }) => node_id,
        other => panic!("{other:?}"),
    }
}

fn named<'a>(themes: &'a [StoreTheme], name: &str) -> &'a StoreTheme {
    themes.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no {name} in {themes:?}"))
}

#[tokio::test]
async fn themes_are_numbered_copied_once_folded_and_customized() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig::in_dir(dir.path()));
    server.start().await.unwrap();
    let url = format!("http://{}", server.addr());
    let c = Arc::new(PimbleClient::connect(&url).await.unwrap());
    let (family, _) = c.create_store(dir.path().join("family.pimble"), "Family").await.unwrap();
    let (work, work_root) = c.create_store(dir.path().join("work.pimble"), "Work").await.unwrap();
    let (quiet, _) = c.create_store(dir.path().join("quiet.pimble"), "Quiet").await.unwrap();
    let note = c.create_node(work, Some(work_root), "document", "Chapter").await.unwrap();

    let (event_tx, _events) = crossbeam_channel::unbounded();
    let signal: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let api = Arc::new(Pimble::new(RpcBackend::over(PimbleClient::connect(&url).await.unwrap()), "test"));
    let ctx = Context { api: &api, host: Some(c.clone()), event_tx: &event_tx, signal_ui: &signal, client_id: "test" };
    let run = |cmd: ThemeCommand| theme_command(&ctx, cmd);
    let new = |store_id: StoreId, base: Option<String>| ThemeCommand::New { store_id, name: "New theme".into(), base, apply_to: None };

    // In Family: Harbor (Book, 18px body), and Tide based on Harbor.
    let harbor = made(run(new(family, Some("builtin:book".into()))).await);
    let tide = made(run(new(family, Some(harbor.to_string()))).await);
    let edits = vec![("body.size".to_string(), Some(json!("18px")))];
    run(ThemeCommand::Edit { store_id: family, node_id: harbor, edits }).await;
    let listed = api.themes(family).await.unwrap();
    assert_eq!(named(&listed, "New theme").node, harbor);
    assert_eq!(named(&listed, "New theme 2").node, tide, "a second New is numbered past the first");

    // Tide copied into Work and applied to the note: Harbor folded in, the
    // built-in base kept, the original recorded.
    let copy = ThemeCommand::Copy { from_store: family, from_node: tide, to_store: work, apply_to: Some(note), customize: false };
    let Some(BackendEvent::ThemesListed { themes, .. }) = run(copy.clone()).await else { panic!("listed") };
    let [copied] = &themes[..] else { panic!("{themes:?}") };
    assert_eq!(copied.stored.get("base"), Some(&json!("builtin:book")));
    assert_eq!(copied.stored.get("body.size"), Some(&json!("18px")));
    assert!(crate::commands::copy_of(&copied.stored, family, tide));
    assert_eq!(c.get_node(work, note).await.unwrap().metadata.theme(), Some(copied.node.to_string().as_str()));

    // Chosen again: the copy is reused, not made twice.
    run(copy).await;
    assert_eq!(api.themes(work).await.unwrap().len(), 1);

    // Customized: a new theme based on the copy, applied to the note.
    let customize = ThemeCommand::Copy { from_store: family, from_node: tide, to_store: work, apply_to: Some(note), customize: true };
    let custom = made(run(customize).await);
    let listed = api.themes(work).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(named(&listed, "New theme 2 (customized)").stored.get("base"), Some(&json!(copied.node.to_string())));
    assert_eq!(c.get_node(work, note).await.unwrap().metadata.theme(), Some(custom.to_string().as_str()));

    // Only stores whose themes were listed are at hand; a change to another
    // store's documents lists nothing.
    assert!(api.themes_at_hand(work) && api.themes_at_hand(family));
    assert!(!api.themes_at_hand(quiet));
}

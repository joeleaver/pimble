//! A store's themes through the API over a real server
//! (docs/THEMES_CONTRACT.md section 3.1): theme nodes listed wherever they
//! sit, made with their tokens in the create, edited token by token, and a
//! deleted one gone from the list.

use std::time::Duration;

use pimble_api::rpc::RpcBackend;
use pimble_api::{recipes, Addr, ApiError, Change, Pimble};
use pimble_client::PimbleClient;
use pimble_core::Place;
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;

#[tokio::test]
async fn themes_are_listed_made_and_edited_token_by_token() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig::in_dir(dir.path()));
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    let (store, root) = service.create_store(dir.path().join("work.pimble"), "Work").await.unwrap();
    let folder = service.create_node(store, Some(root), "folder", "Styles").await.unwrap();
    let api = Pimble::new(RpcBackend::over(PimbleClient::connect(&url).await.unwrap()), "test:themes");
    assert!(api.themes(store).await.unwrap().is_empty());

    let stored = json!({ "base": "builtin:book", "body.size": "17px" });
    let made = api.create(Addr::new(store, root), Place::Last, recipes::theme("Reading", stored.as_object().unwrap()).unwrap()).await.unwrap();
    let nested = api.create(Addr::new(store, folder), Place::Last, recipes::theme("Another", &Default::default()).unwrap()).await.unwrap();
    let listed = api.themes(store).await.unwrap();
    assert_eq!(listed.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["Another", "Reading"], "by name, wherever they sit");
    assert_eq!(listed[1].stored, *stored.as_object().unwrap());
    assert!(api.is_theme(made).await);
    assert!(!api.is_theme(Addr::new(store, folder)).await);

    // One token set, one removed, in one edit; the rest kept.
    assert!(api.edit_theme(made, vec![("page.measure".into(), Some(json!("40em"))), ("body.size".into(), None)]).await.unwrap());
    let theme = api.themes(store).await.unwrap().into_iter().find(|t| t.node == made.node).unwrap();
    assert_eq!(theme.stored, *json!({ "base": "builtin:book", "page.measure": "40em" }).as_object().unwrap());
    assert!(!api.edit_theme(made, vec![("page.measure".into(), Some(json!("40em")))]).await.unwrap(), "setting what is there writes nothing");

    // Another device's edit reaches the list by the subscription.
    service
        .edit_node(store, nested.node, "elsewhere", |doc, _| doc.edit_data(&[pimble_crdt::DataStep::Key("theme"), pimble_crdt::DataStep::Key("body.size")], pimble_crdt::DataEdit::Set(&json!("19px"))).map(drop))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(api.next_change().await, Some(Change::Store(c)) if pimble_api::backend::document_of(&c.kind) == Some(nested.node)) {}
    })
    .await
    .expect("the change arrives");
    let theme = api.themes(store).await.unwrap().into_iter().find(|t| t.node == nested.node).unwrap();
    assert_eq!(theme.stored.get("body.size"), Some(&json!("19px")));

    // Only a theme takes a theme's edit.
    let refused = api.edit_theme(Addr::new(store, folder), vec![("body.size".into(), Some(json!("17px")))]).await;
    assert!(matches!(refused, Err(ApiError::Refused { .. })), "{refused:?}");

    // A deleted theme leaves the list.
    service.delete_node(store, nested.node).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while api.themes(store).await.unwrap().len() != 1 {
            let _ = api.next_change().await;
        }
    })
    .await
    .expect("the delete arrives");
    let _ = server.stop().await;
}

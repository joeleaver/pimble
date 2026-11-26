//! A store's themes through the API (docs/THEMES_CONTRACT.md sections 3.1
//! and 6): listing the theme nodes, making one, editing its tokens, and
//! copying one in from another open store. A theme is a node like any
//! other: it is made with its `data.theme` in the create, edited with
//! `applyEdit`s of that map's keys, and applied with `custom["theme"]` as
//! every appearance key is.

use pimble_api::{recipes, Addr, ApiError, Pimble, StoreTheme};
use pimble_core::{custom_keys, NodeId, Place, StoreId};
use pimble_themes::{edit, Theme, WithStore, COPIED_FROM};
use serde_json::{json, Map, Value as Json};

use super::changes::show_after;
use super::structure::created;
use super::{failed, Context};
use crate::protocol::{BackendEvent, ThemeCommand};

/// Answer a theme command.
pub(super) async fn theme_command(ctx: &Context<'_>, cmd: ThemeCommand) -> Option<BackendEvent> {
    let api = ctx.api;
    match cmd {
        // Asked because something needs the themes: a failure is answered
        // too, so it is asked again once the store is caught up.
        ThemeCommand::List { store_id } => Some(listed(api, store_id).await.unwrap_or(BackendEvent::ThemesNotListed { store_id })),
        ThemeCommand::Edit { store_id, node_id, edits } => match api.edit_theme(Addr::new(store_id, node_id), edits).await {
            // What changed arrives as the store's change, which lists the
            // themes again (`change_events`); this window's own edit is
            // answered at once so its editor shows it without waiting.
            Ok(true) => listed(api, store_id).await,
            Ok(false) => None,
            Err(e) => failed(e),
        },
        ThemeCommand::New { store_id, name, base, apply_to } => {
            let stored = edit::stored(base.as_deref(), &Default::default(), None);
            match make(ctx, store_id, &name, &stored, apply_to).await {
                Ok(node_id) => {
                    emit_listed(ctx, store_id).await;
                    Some(BackendEvent::ThemeMade { store_id, node_id })
                }
                Err(e) => failed(e),
            }
        }
        ThemeCommand::Copy { from_store, from_node, to_store, apply_to, customize } => {
            match copy(ctx, from_store, from_node, to_store, if customize { None } else { apply_to }).await {
                Ok((copy_id, name)) if customize => {
                    let stored = edit::stored(Some(&copy_id.to_string()), &Default::default(), None);
                    match make(ctx, to_store, &customized_name(&name), &stored, apply_to).await {
                        Ok(node_id) => {
                            emit_listed(ctx, to_store).await;
                            Some(BackendEvent::ThemeMade { store_id: to_store, node_id })
                        }
                        Err(e) => failed(e),
                    }
                }
                Ok(_) => listed(api, to_store).await,
                Err(e) => failed(e),
            }
        }
    }
}

/// The name "Customize..." gives a theme based on `base_name`.
pub fn customized_name(base_name: &str) -> String {
    format!("{base_name} (customized)")
}

/// `store_id`'s themes as the event that carries them; nothing when they
/// cannot be read (a store closing as it is asked).
pub async fn listed(api: &Pimble, store_id: StoreId) -> Option<BackendEvent> {
    match api.themes(store_id).await {
        Ok(themes) => Some(BackendEvent::ThemesListed { store_id, themes }),
        Err(e) => {
            tracing::debug!("The themes of store {store_id} could not be listed: {}", super::error_message(&e));
            None
        }
    }
}

async fn emit_listed(ctx: &Context<'_>, store_id: StoreId) {
    if let Some(event) = listed(ctx.api, store_id).await {
        ctx.emit(event);
    }
}

/// A theme node named `name` (or `name 2`, ... when a theme of the store
/// has that name) holding `stored`, made as the store root's last child
/// (section 3.1a: hidden there by its kind, and a root is in every store),
/// then applied to `apply_to` when given.
async fn make(ctx: &Context<'_>, store_id: StoreId, name: &str, stored: &Map<String, Json>, apply_to: Option<NodeId>) -> Result<NodeId, ApiError> {
    let api = ctx.api;
    let taken: Vec<String> = api.themes(store_id).await?.iter().map(|t| t.name.to_lowercase()).collect();
    let name = &crate::themes::free_name(name, &taken);
    let root = api.store_row(store_id).await.ok_or_else(|| ApiError::not_found("That store is not open here."))?.root_node_id;
    // Judged as every creation is (`plan::create_parent`): a store this
    // device may only read refuses it with the sentence that says so.
    let node_id = api.create(Addr::new(store_id, root), Place::Last, recipes::theme(name, stored)?).await?.node;
    show_after(ctx, store_id, created(node_id, root)).await;
    if let Some(target) = apply_to {
        apply(api, Addr::new(store_id, target), &node_id.to_string()).await?;
    }
    Ok(node_id)
}

/// `custom["theme"]` of `addr` set to `theme`, as "Theme..." sets it.
async fn apply(api: &Pimble, addr: Addr, theme: &str) -> Result<(), ApiError> {
    let theme = Json::String(theme.to_string());
    api.edit(addr, move |doc, now| doc.set_custom(custom_keys::THEME, &theme, now)).await?;
    Ok(())
}

/// The themes of a store as `pimble_themes` reads them: each node's id is
/// its theme's id.
pub fn as_themes(listed: &[StoreTheme]) -> Vec<Theme> {
    listed.iter().map(|t| Theme::from_json(&t.node.to_string(), &t.name, &t.stored).0).collect()
}

/// Whether `stored` says it is a copy of `from_node` in `from_store`.
pub fn copy_of(stored: &Map<String, Json>, from_store: StoreId, from_node: NodeId) -> bool {
    stored.get(COPIED_FROM).is_some_and(|from| {
        from.get("store").and_then(Json::as_str) == Some(&from_store.to_string()) && from.get("node").and_then(Json::as_str) == Some(&from_node.to_string())
    })
}

/// Theme `from_node` of `from_store` in `to_store` (section 6): a copy made
/// before is reused, otherwise one is made with the original's chain in its
/// own store folded in. Applied to `apply_to` when given. Answers the copy's
/// id and name.
async fn copy(ctx: &Context<'_>, from_store: StoreId, from_node: NodeId, to_store: StoreId, apply_to: Option<NodeId>) -> Result<(NodeId, String), ApiError> {
    let api = ctx.api;
    if from_store == to_store {
        return Err(ApiError::refused("That theme is in this store already."));
    }
    let existing = api.themes(to_store).await?;
    let (node_id, name) = match existing.iter().find(|t| copy_of(&t.stored, from_store, from_node)) {
        Some(made) => (made.node, made.name.clone()),
        None => {
            let source = api.themes(from_store).await?;
            let original = source.iter().find(|t| t.node == from_node).ok_or_else(|| ApiError::not_found("That theme is not in its store any more."))?;
            let themes = as_themes(&source);
            let (base, tokens) = edit::flatten(&from_node.to_string(), &WithStore(&themes));
            let from = json!({ "store": from_store.to_string(), "node": from_node.to_string(), "at": pimble_api::now() });
            let stored = edit::stored(base.as_deref(), &tokens, Some(from));
            let name = original.name.clone();
            (make(ctx, to_store, &name, &stored, None).await?, name)
        }
    };
    if let Some(target) = apply_to {
        apply(api, Addr::new(to_store, target), &node_id.to_string()).await?;
    }
    Ok((node_id, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_is_known_by_its_original() {
        let (store, node) = (StoreId::new(), NodeId::new());
        let stored = edit::stored(None, &Default::default(), Some(json!({ "store": store.to_string(), "node": node.to_string(), "at": "x" })));
        assert!(copy_of(&stored, store, node));
        assert!(!copy_of(&stored, store, NodeId::new()));
        assert!(!copy_of(&Map::new(), store, node));
    }

    #[test]
    fn listed_themes_read_with_their_node_ids() {
        let node = NodeId::new();
        let stored = json!({ "base": "builtin:book", "body.size": "17px" });
        let themes = as_themes(&[StoreTheme { node, name: "Mine".into(), stored: stored.as_object().unwrap().clone() }]);
        assert_eq!(themes[0].id, node.to_string());
        assert_eq!(themes[0].base.as_deref(), Some("builtin:book"));
        assert_eq!(customized_name("Book"), "Book (customized)");
    }
}

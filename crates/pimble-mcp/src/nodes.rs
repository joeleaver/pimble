//! Naming nodes (docs/MCP_CONTRACT.md "Naming a node"): a `pimble:` link, a bare
//! node id, or a path of titles from a store's name. Ambiguity is an error that
//! lists the candidates, never a guess.

use std::collections::HashSet;

use pimble_client::PimbleClient;
use pimble_core::{Node, NodeId, PimbleUrl, Store, StoreId};

/// A node by its canonical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Addr {
    pub store: StoreId,
    pub node: NodeId,
}

impl Addr {
    pub fn link(&self) -> String {
        PimbleUrl { store: self.store, node: self.node, anchor: None }.to_string()
    }
}

/// What the LLM sees of a node in a listing: enough to name it in the next call.
pub fn label(node: &Node) -> String {
    if node.metadata.title.trim().is_empty() {
        "(untitled)".to_string()
    } else {
        node.metadata.title.clone()
    }
}

/// Find a store by name or id among the open ones.
pub fn find_store<'a>(stores: &'a [Store], name: &str) -> Result<&'a Store, String> {
    if let Ok(id) = StoreId::parse(name.trim()) {
        if let Some(store) = stores.iter().find(|s| s.id == id) {
            return Ok(store);
        }
    }
    let matches: Vec<&Store> = stores.iter().filter(|s| s.name.eq_ignore_ascii_case(name.trim())).collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "No open store is called \"{name}\". Open stores: {}.",
            stores.iter().map(|s| format!("\"{}\"", s.name)).collect::<Vec<_>>().join(", ")
        )),
        several => Err(format!(
            "{} open stores are called \"{name}\"; name one by id: {}.",
            several.len(),
            several.iter().map(|s| s.id.to_string()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Resolve what a tool was given as `node`.
pub async fn resolve(client: &PimbleClient, name: &str) -> Result<Addr, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Name a node: a pimble: link, a node id, or a path like \"Store/Folder/Note\".".into());
    }
    if name.starts_with("pimble:") {
        let url = PimbleUrl::parse(name).ok_or_else(|| format!("\"{name}\" is not a Pimble link."))?;
        let addr = Addr { store: url.store, node: url.node };
        client
            .get_node(addr.store, addr.node)
            .await
            .map_err(|e| format!("The link {name} names no node open here ({e})."))?;
        return Ok(addr);
    }
    let stores = client.list_stores().await.map_err(|e| e.to_string())?;
    if let Ok(id) = NodeId::parse(name) {
        // A bare UUID: a node id, or a store id standing for its root.
        return resolve_id(client, &stores, id).await;
    }
    resolve_path(client, &stores, name).await
}

async fn resolve_id(client: &PimbleClient, stores: &[Store], id: NodeId) -> Result<Addr, String> {
    if let Some(store) = stores.iter().find(|s| s.root_node_id == id || s.id.to_string() == id.to_string()) {
        return Ok(Addr { store: store.id, node: store.root_node_id });
    }
    let mut found = Vec::new();
    for store in stores {
        if client.get_node(store.id, id).await.is_ok() {
            found.push(Addr { store: store.id, node: id });
        }
    }
    match found.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("No open store has a node with id {id}.")),
        several => Err(format!(
            "Node id {id} is in {} stores; use one of these links: {}.",
            several.len(),
            several.iter().map(Addr::link).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The children a path step looks through: a store's shown roots for a partial
/// replica, a node's children (through mounts) otherwise.
pub async fn children(client: &PimbleClient, at: Addr) -> Result<Vec<(Addr, Node)>, String> {
    let (store, nodes) = client.get_children(at.store, at.node).await.map_err(|e| e.to_string())?;
    Ok(nodes.into_iter().map(|node| (Addr { store, node: node.id }, node)).collect())
}

/// The top of a store as the LLM sees it: its root, or the roots of the shares a
/// partial replica holds.
pub fn store_tops(store: &Store) -> Vec<Addr> {
    store.shown_roots().into_iter().map(|node| Addr { store: store.id, node }).collect()
}

async fn resolve_path(client: &PimbleClient, stores: &[Store], path: &str) -> Result<Addr, String> {
    let mut steps = path.split('/').map(str::trim).filter(|s| !s.is_empty());
    let store_name = steps.next().unwrap_or_default();
    let store = find_store(stores, store_name)?;
    let tops = store_tops(store);
    let mut here: Vec<Addr> = tops.clone();
    let mut walked = store.name.clone();
    // A whole store's path steps start under its root; a partial replica's first
    // step names one of its shared roots.
    let mut first = tops.len() == 1 && store.roots.is_empty();
    for step in steps {
        let mut matches = Vec::new();
        if first {
            for (addr, node) in children(client, here[0]).await? {
                if label(&node).eq_ignore_ascii_case(step) {
                    matches.push(addr);
                }
            }
        } else {
            for top in &here {
                let node = client.get_node(top.store, top.node).await.map_err(|e| e.to_string())?;
                if label(&node).eq_ignore_ascii_case(step) {
                    matches.push(*top);
                }
            }
        }
        first = true;
        match matches.as_slice() {
            [one] => here = vec![*one],
            [] => return Err(format!("\"{walked}\" has nothing called \"{step}\" under it.")),
            several => {
                return Err(format!(
                    "\"{walked}\" has {} nodes called \"{step}\"; name one by link: {}.",
                    several.len(),
                    several.iter().map(Addr::link).collect::<Vec<_>>().join(", ")
                ))
            }
        }
        walked = format!("{walked}/{step}");
    }
    match here.as_slice() {
        [one] => Ok(*one),
        _ => Err(format!(
            "\"{}\" holds several shares; add the share's name to the path.",
            store.name
        )),
    }
}

/// A node's path of titles from its store's name, by walking `parent_id` up.
pub async fn path_of(client: &PimbleClient, stores: &[Store], addr: Addr) -> String {
    let store = stores.iter().find(|s| s.id == addr.store);
    let mut titles = Vec::new();
    let mut seen = HashSet::new();
    let mut at = Some(addr.node);
    while let Some(id) = at {
        if !seen.insert(id) || store.is_some_and(|s| s.root_node_id == id) {
            break;
        }
        let Ok(node) = client.get_node(addr.store, id).await else { break };
        titles.push(label(&node));
        if store.is_some_and(|s| s.roots.contains(&id)) {
            break;
        }
        at = node.parent_id;
    }
    titles.push(store.map_or_else(|| addr.store.to_string(), |s| s.name.clone()));
    titles.reverse();
    titles.join("/")
}

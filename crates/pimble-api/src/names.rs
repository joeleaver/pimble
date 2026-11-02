//! Naming nodes (docs/API_CONTRACT.md decision 6): a `pimble:` link, a bare
//! id, or a path of titles, from a store's name or from anywhere. A link is
//! followed where its node went (`became`, and a chain that leaves for
//! another store this client holds); a name that answers to several nodes is
//! an error that lists them, never a guess. And the other way round: a
//! node's path from its store's name, and the shortest path that names it
//! alone.

use std::collections::{HashMap, HashSet};

use pimble_core::{LinkResolution, Node, NodeId, PimbleUrl, Store, StoreId, MAX_LINK_HOPS};
use pimble_sheet::book::{find_by_path, shortest_path};
use pimble_sheet::{NodeRef, Titles};

use crate::{Addr, ApiError, Pimble};

/// A node's title as a listing shows it.
pub fn label(node: &Node) -> String {
    if node.metadata.title.trim().is_empty() {
        "(untitled)".to_string()
    } else {
        node.metadata.title.clone()
    }
}

/// The top of a store as the person sees it: its root, or the roots of the
/// shares a partial replica holds.
pub fn store_tops(store: &Store) -> Vec<Addr> {
    store.shown_roots().into_iter().map(|node| Addr { store: store.id, node }).collect()
}

/// A store by its id or its name, among `stores`.
pub fn find_store<'a>(stores: &'a [Store], name: &str) -> Result<&'a Store, ApiError> {
    let name = name.trim();
    if let Ok(id) = StoreId::parse(name) {
        if let Some(store) = stores.iter().find(|s| s.id == id) {
            return Ok(store);
        }
    }
    let matches: Vec<&Store> = stores.iter().filter(|s| s.name.eq_ignore_ascii_case(name)).collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(ApiError::not_found(format!(
            "No open store is called \"{name}\". Open stores: {}.",
            stores.iter().map(|s| format!("\"{}\"", s.name)).collect::<Vec<_>>().join(", ")
        ))),
        several => Err(ApiError::Ambiguous {
            sentence: format!(
                "{} open stores are called \"{name}\"; name one by id: {}.",
                several.len(),
                several.iter().map(|s| s.id.to_string()).collect::<Vec<_>>().join(", ")
            ),
            candidates: several.iter().map(|s| Addr { store: s.id, node: s.root_node_id }).collect(),
        }),
    }
}

/// The sentence for a link that does not lead to a live node
/// (docs/LINKS_CONTRACT.md "Following a link"). The last hop decides it: a
/// chain that moved on and stopped in a store this client does not hold
/// reads as no access, since the reader cannot reach where the note went;
/// only a link whose own store is not here says so. A chain still going
/// after `MAX_LINK_HOPS` is one that no longer leads anywhere.
pub fn stop_sentence(url: &PimbleUrl, hops: usize, resolution: &LinkResolution) -> &'static str {
    let said = match resolution {
        LinkResolution::StoreNotHere { .. } if hops >= MAX_LINK_HOPS => LinkResolution::Missing.sentence(),
        LinkResolution::StoreNotHere { store_id, node_id } if (*store_id, *node_id) != (url.store, url.node) => {
            LinkResolution::NoAccess.sentence()
        }
        other => other.sentence(),
    };
    said.unwrap_or_default()
}

/// Titles over held stores, for paths that start anywhere: every live node
/// of the stores held whole, a store's root going by the store's name (so
/// a path may start there, as a formula's may).
struct HeldTitles {
    nodes: HashMap<NodeRef, (String, Option<NodeRef>)>,
}

impl Titles for HeldTitles {
    fn candidates(&self) -> Vec<NodeRef> {
        self.nodes.keys().copied().collect()
    }

    fn title_and_parent(&self, node: &NodeRef) -> Option<(String, Option<NodeRef>)> {
        self.nodes.get(node).cloned()
    }
}

impl Pimble {
    /// What a caller named: a `pimble:` link (followed where it went), a
    /// bare node id (looked for in every store; a store's id stands for its
    /// root), or a path of titles from a store's name or from anywhere.
    pub async fn resolve(&self, name: &str) -> Result<Addr, ApiError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::refused("Name a node: a pimble: link, a node id, or a path like \"Store/Folder/Note\"."));
        }
        if name.starts_with("pimble:") {
            let url = PimbleUrl::parse(name).ok_or_else(|| ApiError::refused(format!("\"{name}\" is not a Pimble link.")))?;
            return self.follow_to_node(&url).await;
        }
        let stores = self.known_stores().await?;
        if let Ok(id) = NodeId::parse(name) {
            return self.resolve_id(&stores, id).await;
        }
        self.resolve_path(name).await
    }

    /// Where a link leads now: one resolution by its store's holder, asked
    /// again of whoever holds the store a chain left for, up to
    /// `MAX_LINK_HOPS` hops. Answers the last resolution and how many hops
    /// it took.
    pub async fn follow(&self, url: &PimbleUrl) -> Result<(LinkResolution, usize), ApiError> {
        let mut at = (url.store, url.node);
        let mut hops = 0;
        loop {
            let resolution = self.backend().resolve_link(at.0, at.1).await?;
            match resolution {
                LinkResolution::StoreNotHere { store_id, node_id } if store_id != at.0 && hops < MAX_LINK_HOPS => {
                    if self.store_row(store_id).await.is_none() {
                        return Ok((resolution, hops));
                    }
                    at = (store_id, node_id);
                    hops += 1;
                }
                other => return Ok((other, hops)),
            }
        }
    }

    /// The live node a link leads to, or why it leads nowhere.
    async fn follow_to_node(&self, url: &PimbleUrl) -> Result<Addr, ApiError> {
        let (resolution, hops) = self.follow(url).await?;
        match resolution {
            LinkResolution::Live { store_id, node_id } => Ok(Addr { store: store_id, node: node_id }),
            LinkResolution::Deleted { store_id, node_id } => Err(ApiError::Deleted { addr: Addr { store: store_id, node: node_id } }),
            LinkResolution::NoAccess => Err(ApiError::NoGrant { sentence: stop_sentence(url, hops, &resolution).to_string() }),
            other => Err(ApiError::not_found(format!("{} ({})", stop_sentence(url, hops, &other), url))),
        }
    }

    async fn resolve_id(&self, stores: &[Store], id: NodeId) -> Result<Addr, ApiError> {
        if let Some(store) = stores.iter().find(|s| s.root_node_id == id || s.id.0 == id.0) {
            return Ok(Addr { store: store.id, node: store.root_node_id });
        }
        let mut found = Vec::new();
        for store in stores {
            let addr = Addr { store: store.id, node: id };
            if self.held_node(addr).await.is_ok() {
                found.push(addr);
            }
        }
        match found.as_slice() {
            [one] => Ok(*one),
            [] => Err(ApiError::not_found(format!("No open store has a node with id {id}."))),
            several => Err(ApiError::Ambiguous {
                sentence: format!(
                    "Node id {id} is in {} stores; use one of these links: {}.",
                    several.len(),
                    several.iter().map(Addr::link).collect::<Vec<_>>().join(", ")
                ),
                candidates: found.clone(),
            }),
        }
    }

    /// A path of titles. One that starts with a store's name is walked down
    /// from it, through mounts; one that does not, or that the walk does not
    /// find, is looked for anywhere (a run of titles down the tree, as a
    /// formula's note path is), and several matches are listed.
    async fn resolve_path(&self, path: &str) -> Result<Addr, ApiError> {
        let mut stores = self.known_stores().await?;
        let first = path.split('/').map(str::trim).find(|s| !s.is_empty()).unwrap_or_default();
        if find_store(&stores, first).is_err() {
            // Perhaps a store opened since the stores were listed.
            stores = self.stores().await?;
        }
        let walked = match find_store(&stores, first) {
            Ok(store) => match self.walk_path(store, path).await {
                Ok(addr) => return Ok(addr),
                Err(e @ ApiError::Ambiguous { .. }) => return Err(e),
                Err(e) => Some(e),
            },
            Err(ApiError::Ambiguous { sentence, candidates }) => return Err(ApiError::Ambiguous { sentence, candidates }),
            Err(_) => None,
        };
        let found = self.find_anywhere(&stores, path).await?;
        match found.as_slice() {
            [one] => Ok(*one),
            [] => Err(walked.unwrap_or_else(|| {
                ApiError::not_found(format!("No open store is called \"{first}\", and no node's path ends in \"{}\".", path.trim()))
            })),
            several => {
                let mut lines = Vec::new();
                for a in several.iter().take(20) {
                    lines.push(format!("{} ({})", self.path(*a).await, a.link()));
                }
                Err(ApiError::Ambiguous {
                    sentence: format!("{} nodes match \"{}\"; name one by link: {}.", several.len(), path.trim(), lines.join(", ")),
                    candidates: several.to_vec(),
                })
            }
        }
    }

    /// A path from `store`'s name, one title per level, through mounts.
    async fn walk_path(&self, store: &Store, path: &str) -> Result<Addr, ApiError> {
        let mut steps = path.split('/').map(str::trim).filter(|s| !s.is_empty());
        steps.next();
        let tops = store_tops(store);
        let mut here: Vec<Addr> = tops.clone();
        let mut walked = store.name.clone();
        // A whole store's path steps start under its root; a partial
        // replica's first step names one of its shared roots. A store shown
        // with one root as its own (an agent granted one subtree,
        // docs/AGENTS_CONTRACT.md) is walked like a whole one, as `path`
        // names its nodes.
        let single_root = store.roots.len() == 1 && store.roots[0] == store.root_node_id;
        let mut first = tops.len() == 1 && (store.roots.is_empty() || single_root);
        for step in steps {
            let mut matches = Vec::new();
            if first {
                for (addr, node) in self.children(here[0]).await? {
                    if label(&node).eq_ignore_ascii_case(step) {
                        matches.push(addr);
                    }
                }
            } else {
                for top in &here {
                    let node = self.held_node(*top).await?;
                    if label(&node).eq_ignore_ascii_case(step) {
                        matches.push(*top);
                    }
                }
            }
            first = true;
            match matches.as_slice() {
                [one] => here = vec![*one],
                [] => return Err(ApiError::not_found(format!("\"{walked}\" has nothing called \"{step}\" under it."))),
                several => {
                    return Err(ApiError::Ambiguous {
                        sentence: format!(
                            "\"{walked}\" has {} nodes called \"{step}\"; name one by link: {}.",
                            several.len(),
                            several.iter().map(Addr::link).collect::<Vec<_>>().join(", ")
                        ),
                        candidates: matches.clone(),
                    })
                }
            }
            walked = format!("{walked}/{step}");
        }
        match here.as_slice() {
            [one] => Ok(*one),
            _ => Err(ApiError::refused(format!("\"{}\" holds several shares; add the share's name to the path.", store.name))),
        }
    }

    /// Every node whose path of titles ends in `path`, in the stores held
    /// whole for it.
    async fn find_anywhere(&self, stores: &[Store], path: &str) -> Result<Vec<Addr>, ApiError> {
        for store in stores {
            if let Err(e) = self.hold_store(store.id).await {
                tracing::debug!("store {} could not be held to look for \"{path}\": {e}", store.name);
            }
        }
        let titles = self.held_titles(stores);
        Ok(find_by_path(&titles, path).into_iter().map(|r| Addr { store: r.store, node: r.node }).collect())
    }

    fn held_titles(&self, stores: &[Store]) -> HeldTitles {
        let held = self.held.lock().unwrap();
        let mut nodes = HashMap::new();
        for store in stores {
            let Some(h) = held.store(store.id).filter(|h| h.complete) else { continue };
            for id in h.tree.list_node_ids() {
                let Ok(fields) = h.tree.live_fields(id) else { continue };
                let title = if id == store.root_node_id {
                    store.name.clone()
                } else if fields.title.trim().is_empty() {
                    "(untitled)".to_string()
                } else {
                    fields.title.clone()
                };
                // A shared root's parent is not held, so it starts a path.
                let parent = fields.parent_id.filter(|p| h.tree.has_node(*p));
                nodes.insert(NodeRef { store: store.id, node: id }, (title, parent.map(|p| NodeRef { store: store.id, node: p })));
            }
        }
        HeldTitles { nodes }
    }

    /// A node's path of titles from its store's name, by its parents. Nodes
    /// above it are read as they are needed and held from then on.
    pub async fn path(&self, addr: Addr) -> String {
        let store = self.store_row(addr.store).await;
        let mut titles = Vec::new();
        let mut seen = HashSet::new();
        let mut at = Some(addr.node);
        while let Some(id) = at {
            if !seen.insert(id) || store.as_ref().is_some_and(|s| s.root_node_id == id) {
                break;
            }
            let Ok(node) = self.held_node(Addr { store: addr.store, node: id }).await else { break };
            titles.push(label(&node));
            if store.as_ref().is_some_and(|s| s.roots.contains(&id)) {
                break;
            }
            at = node.parent_id;
        }
        titles.push(store.map_or_else(|| addr.store.to_string(), |s| s.name));
        titles.reverse();
        titles.join("/")
    }

    /// The shortest path of titles that names `addr` alone among the stores
    /// held whole (its title, when that is unique).
    pub async fn short_name(&self, addr: Addr) -> String {
        let Ok(stores) = self.known_stores().await else { return "That note".to_string() };
        if self.hold_store(addr.store).await.is_err() {
            return "That note".to_string();
        }
        shortest_path(&self.held_titles(&stores), &NodeRef { store: addr.store, node: addr.node })
    }

    /// The nodes above `addr`, nearest first, up to its store's top.
    pub async fn ancestors(&self, addr: Addr) -> Result<Vec<Addr>, ApiError> {
        let store = self.store_row(addr.store).await;
        let mut out = Vec::new();
        let mut seen = HashSet::from([addr.node]);
        let mut at = self.held_node(addr).await?.parent_id;
        while let Some(id) = at {
            if !seen.insert(id) {
                break;
            }
            let here = Addr { store: addr.store, node: id };
            out.push(here);
            if store.as_ref().is_some_and(|s| s.root_node_id == id || s.roots.contains(&id)) {
                break;
            }
            at = self.held_node(here).await.ok().and_then(|n| n.parent_id);
        }
        Ok(out)
    }
}

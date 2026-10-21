//! AI agents (docs/AGENTS_CONTRACT.md): the apps a person connects to Pimble
//! (Claude Desktop, Claude Code, a remote connector), each with grants of the
//! shape a share has, `(store, root, reader | editor)`, judged by the same
//! `Reach` a share's member is.
//!
//! An agent presents its key; [`AgentRegistry::principal_for_key`] turns it
//! into a `Principal::User` whose `sub` is `agent:<id>` and whose grants are
//! the agent's, so every judgement the server already makes of a scoped
//! principal (reads, writes, search, notifications, the `Service`-only
//! lifecycle RPCs) applies to it unchanged.
//!
//! Agents live in `<config dir>/pimble/agents.json`, mode 0600, on this
//! device only: never in a store, never sent to Pimble Cloud, never synced
//! (Joe, 2026-09-18: nothing is hosted unless the person asked). A key is
//! shown once, when the agent is made, and kept only as its SHA-256.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::RwLock;

use base64::Engine;
use pimble_core::{NodeId, StoreId};
use pimble_rpc::{AgentGrant, AgentInfo, AgentRole};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tracing::warn;
use uuid::Uuid;

use crate::principal::{Grant, Principal, Role};

/// What every agent key starts with, so a key is recognisable for what it is
/// (and never mistaken for a JWT or the server's static token).
pub const KEY_PREFIX: &str = "pmbl_agent_";

/// The `sub` prefix of an agent's principal.
pub const AGENT_SUB_PREFIX: &str = "agent:";

pub fn default_agents_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("agents.json")
}

/// Whether `principal` is an agent's.
pub fn is_agent(principal: &Principal) -> bool {
    matches!(principal, Principal::User { sub, .. } if sub.starts_with(AGENT_SUB_PREFIX))
}

/// What an agent's connection carries: which agent it is, and where to ask
/// what that agent may do now (`principal::principal_of`).
#[derive(Clone)]
pub struct AgentConnection {
    id: Uuid,
    registry: std::sync::Arc<AgentRegistry>,
}

impl AgentConnection {
    /// The agent's principal as of now; one with no grants (refused
    /// everything) once it is removed.
    pub fn principal(&self) -> Principal {
        self.registry.principal_of_id(self.id).unwrap_or_else(|| Principal::User {
            sub: format!("{AGENT_SUB_PREFIX}{}", self.id),
            email: String::new(),
            grants: HashMap::new(),
        })
    }
}

/// One agent as saved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct AgentRecord {
    id: Uuid,
    name: String,
    /// Hex SHA-256 of the whole key.
    key_sha256: String,
    grants: Vec<AgentGrant>,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_used_at: Option<String>,
}

impl AgentRecord {
    fn info(&self) -> AgentInfo {
        AgentInfo {
            id: self.id.to_string(),
            name: self.name.clone(),
            grants: self.grants.clone(),
            created_at: self.created_at.clone(),
            last_used_at: self.last_used_at.clone(),
        }
    }

    fn principal(&self) -> Principal {
        Principal::User {
            sub: format!("{AGENT_SUB_PREFIX}{}", self.id),
            email: self.name.clone(),
            grants: grants_of(&self.grants),
        }
    }
}

/// An agent's grants as the server judges them: per store, the whole store
/// at one role, or roots each with a role (a root's subtree, the union of
/// every root that reaches a node, as for a share's member).
fn grants_of(grants: &[AgentGrant]) -> HashMap<StoreId, Grant> {
    let mut whole: HashMap<StoreId, Role> = HashMap::new();
    let mut scoped: HashMap<StoreId, HashMap<NodeId, Role>> = HashMap::new();
    for g in grants {
        let role = role_of(g.role);
        match g.node {
            None => {
                let e = whole.entry(g.store).or_insert(role);
                *e = max_role(*e, role);
            }
            Some(node) => {
                let e = scoped.entry(g.store).or_default().entry(node).or_insert(role);
                *e = max_role(*e, role);
            }
        }
    }
    let mut out: HashMap<StoreId, Grant> = HashMap::new();
    for (store, role) in whole {
        // A whole store at editor covers every root inside it; a whole store
        // at reader with editor roots inside is normalized away by
        // `normalize` before it is saved, so it never reaches here.
        out.insert(store, Grant::whole(role));
    }
    for (store, roots) in scoped {
        out.entry(store).or_insert_with(|| Grant::scoped(roots));
    }
    out
}

fn role_of(role: AgentRole) -> Role {
    match role {
        AgentRole::Reader => Role::Reader,
        AgentRole::Editor => Role::Editor,
    }
}

fn max_role(a: Role, b: Role) -> Role {
    if a == Role::Editor || b == Role::Editor { Role::Editor } else { Role::Reader }
}

/// Grants as they are saved: one entry per (store, root), the higher role
/// winning; a whole-store grant that a root inside it would widen becomes a
/// grant on the store's root node (`root_of` answers it), so the judgement
/// stays a plain union of roots.
pub fn normalize(grants: Vec<AgentGrant>, root_of: impl Fn(StoreId) -> Option<NodeId>) -> Vec<AgentGrant> {
    let mut by_store: HashMap<StoreId, Vec<AgentGrant>> = HashMap::new();
    for g in grants {
        by_store.entry(g.store).or_default().push(g);
    }
    let mut out = Vec::new();
    for (store, mut list) in by_store {
        let whole: Option<AgentRole> = list.iter().filter(|g| g.node.is_none()).map(|g| g.role).max();
        let roots_editing = list.iter().any(|g| g.node.is_some() && g.role == AgentRole::Editor);
        match whole {
            Some(AgentRole::Editor) => {
                out.push(AgentGrant { store, node: None, role: AgentRole::Editor });
                continue;
            }
            Some(AgentRole::Reader) if !roots_editing => {
                out.push(AgentGrant { store, node: None, role: AgentRole::Reader });
                continue;
            }
            Some(AgentRole::Reader) => {
                list.retain(|g| g.node.is_some());
                if let Some(root) = root_of(store) {
                    list.push(AgentGrant { store, node: Some(root), role: AgentRole::Reader });
                }
            }
            None => {}
        }
        let mut roots: HashMap<NodeId, AgentRole> = HashMap::new();
        for g in list {
            if let Some(node) = g.node {
                let e = roots.entry(node).or_insert(g.role);
                *e = (*e).max(g.role);
            }
        }
        let mut roots: Vec<_> = roots.into_iter().collect();
        roots.sort_by_key(|(n, _)| n.to_string());
        out.extend(roots.into_iter().map(|(node, role)| AgentGrant { store, node: Some(node), role }));
    }
    out.sort_by_key(|g| (g.store.to_string(), g.node.map(|n| n.to_string())));
    out
}

fn sha256_hex(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A new key for agent `id`: the prefix, the id (so a key names its agent),
/// and 32 random bytes.
fn new_key(id: Uuid) -> String {
    let mut secret = [0u8; 32];
    rand::fill(&mut secret);
    let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret);
    format!("{KEY_PREFIX}{}_{secret}", id.simple())
}

/// The agent id a key names, if it is shaped like an agent key.
fn id_of_key(key: &str) -> Option<Uuid> {
    let rest = key.strip_prefix(KEY_PREFIX)?;
    let (id, _) = rest.split_once('_')?;
    Uuid::parse_str(id).ok()
}

/// The agents of this device.
pub struct AgentRegistry {
    path: PathBuf,
    agents: RwLock<Vec<AgentRecord>>,
}

impl AgentRegistry {
    /// The agents saved at `path` (none when there is no file yet; a file
    /// that does not parse is left alone and logged, and holds no agent
    /// until the next save rewrites it).
    pub fn load(path: PathBuf) -> Self {
        let agents = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Vec<AgentRecord>>(&bytes).unwrap_or_else(|e| {
                warn!("Agents file {} could not be parsed ({e}); no agent can connect until it is fixed or rewritten", path.display());
                Vec::new()
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                warn!("Agents file {} could not be read: {e}", path.display());
                Vec::new()
            }
        };
        Self { path, agents: RwLock::new(agents) }
    }

    /// The connection `key` opens, when it is a current agent's key.
    pub fn connect(self: &std::sync::Arc<Self>, key: &str) -> Option<AgentConnection> {
        let Some(Principal::User { sub, .. }) = self.principal_for_key(key) else { return None };
        let id = Uuid::parse_str(sub.strip_prefix(AGENT_SUB_PREFIX)?).ok()?;
        Some(AgentConnection { id, registry: std::sync::Arc::clone(self) })
    }

    fn principal_of_id(&self, id: Uuid) -> Option<Principal> {
        self.agents.read().ok()?.iter().find(|a| a.id == id).map(AgentRecord::principal)
    }

    /// The principal `key` stands for, when it is a current agent's key.
    pub fn principal_for_key(&self, key: &str) -> Option<Principal> {
        let id = id_of_key(key)?;
        let hash = sha256_hex(key);
        let agents = self.agents.read().ok()?;
        let agent = agents.iter().find(|a| a.id == id)?;
        bool::from(agent.key_sha256.as_bytes().ct_eq(hash.as_bytes())).then(|| agent.principal())
    }

    /// Record that agent `principal` connected (its `last_used_at`), saved
    /// at most once a minute per agent.
    pub fn touch(&self, principal: &Principal) {
        let Principal::User { sub, .. } = principal else { return };
        let Some(id) = sub.strip_prefix(AGENT_SUB_PREFIX).and_then(|s| Uuid::parse_str(s).ok()) else { return };
        let now = chrono::Utc::now();
        let changed = {
            let Ok(mut agents) = self.agents.write() else { return };
            let Some(agent) = agents.iter_mut().find(|a| a.id == id) else { return };
            let stale = agent
                .last_used_at
                .as_deref()
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .is_none_or(|t| now.signed_duration_since(t) > chrono::Duration::minutes(1));
            if stale {
                agent.last_used_at = Some(now.to_rfc3339());
            }
            stale
        };
        if changed {
            let _ = self.save();
        }
    }

    pub fn list(&self) -> Vec<AgentInfo> {
        self.agents.read().map(|a| a.iter().map(AgentRecord::info).collect()).unwrap_or_default()
    }

    /// A new agent; answers it and its key, which is never shown again.
    pub fn create(&self, name: &str, grants: Vec<AgentGrant>) -> io::Result<(AgentInfo, String)> {
        let id = Uuid::new_v4();
        let key = new_key(id);
        let record = AgentRecord {
            id,
            name: name.to_string(),
            key_sha256: sha256_hex(&key),
            grants,
            created_at: chrono::Utc::now().to_rfc3339(),
            last_used_at: None,
        };
        let info = record.info();
        self.agents.write().map_err(|_| io::Error::other("agents lock poisoned"))?.push(record);
        self.save()?;
        Ok((info, key))
    }

    /// Rename agent `id` or replace its grants; `None` leaves that alone.
    /// Answers `None` when there is no such agent.
    pub fn update(&self, id: &str, name: Option<&str>, grants: Option<Vec<AgentGrant>>) -> io::Result<Option<AgentInfo>> {
        let Ok(id) = Uuid::parse_str(id) else { return Ok(None) };
        let info = {
            let mut agents = self.agents.write().map_err(|_| io::Error::other("agents lock poisoned"))?;
            let Some(agent) = agents.iter_mut().find(|a| a.id == id) else { return Ok(None) };
            if let Some(name) = name {
                agent.name = name.to_string();
            }
            if let Some(grants) = grants {
                agent.grants = grants;
            }
            agent.info()
        };
        self.save()?;
        Ok(Some(info))
    }

    /// Remove agent `id`: its key stops working on its next call. Answers
    /// whether there was one.
    pub fn remove(&self, id: &str) -> io::Result<bool> {
        let Ok(id) = Uuid::parse_str(id) else { return Ok(false) };
        let removed = {
            let mut agents = self.agents.write().map_err(|_| io::Error::other("agents lock poisoned"))?;
            let before = agents.len();
            agents.retain(|a| a.id != id);
            agents.len() != before
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> io::Result<()> {
        let json = {
            let agents = self.agents.read().map_err(|_| io::Error::other("agents lock poisoned"))?;
            serde_json::to_vec_pretty(&*agents).map_err(io::Error::other)?
        };
        crate::fs_util::write_atomic_0600(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StoreId {
        StoreId::new()
    }

    #[test]
    fn a_key_finds_its_agent_and_nothing_else_does() {
        let dir = tempfile::tempdir().unwrap();
        let reg = AgentRegistry::load(dir.path().join("agents.json"));
        let s = store();
        let (info, key) = reg.create("Claude", vec![AgentGrant { store: s, node: None, role: AgentRole::Reader }]).unwrap();
        assert!(key.starts_with(KEY_PREFIX));
        let Some(Principal::User { sub, grants, .. }) = reg.principal_for_key(&key) else { panic!("the key is the agent's") };
        assert_eq!(sub, format!("agent:{}", info.id));
        assert_eq!(grants.get(&s), Some(&Grant::whole(Role::Reader)));
        // A key with the right id and the wrong secret is nobody.
        let forged = format!("{}x", &key[..key.len() - 1]);
        assert!(reg.principal_for_key(&forged).is_none());
        assert!(reg.principal_for_key("not a key").is_none());
        // The file keeps the hash, never the key, and is 0600.
        let text = std::fs::read_to_string(dir.path().join("agents.json")).unwrap();
        assert!(!text.contains(&key));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("agents.json")).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // Reloaded, the key still works; removed, it stops.
        let reg = AgentRegistry::load(dir.path().join("agents.json"));
        assert!(reg.principal_for_key(&key).is_some());
        assert!(reg.remove(&info.id).unwrap());
        assert!(reg.principal_for_key(&key).is_none());
    }

    #[test]
    fn normalizing_keeps_one_entry_per_root_and_widens_whole_readers_by_their_root() {
        let s = store();
        let (root, a, b) = (NodeId::new(), NodeId::new(), NodeId::new());
        let grants = vec![
            AgentGrant { store: s, node: None, role: AgentRole::Reader },
            AgentGrant { store: s, node: Some(a), role: AgentRole::Editor },
            AgentGrant { store: s, node: Some(a), role: AgentRole::Reader },
            AgentGrant { store: s, node: Some(b), role: AgentRole::Reader },
        ];
        let out = normalize(grants, |_| Some(root));
        let find = |n: NodeId| out.iter().find(|g| g.node == Some(n)).map(|g| g.role);
        assert_eq!(find(root), Some(AgentRole::Reader), "the whole store at reader, as its root");
        assert_eq!(find(a), Some(AgentRole::Editor), "the higher role wins");
        assert_eq!(find(b), Some(AgentRole::Reader));
        assert!(out.iter().all(|g| g.node.is_some()));

        // A whole store at editor covers everything inside it.
        let out = normalize(vec![AgentGrant { store: s, node: None, role: AgentRole::Editor }, AgentGrant { store: s, node: Some(a), role: AgentRole::Reader }], |_| Some(root));
        assert_eq!(out, vec![AgentGrant { store: s, node: None, role: AgentRole::Editor }]);
    }
}

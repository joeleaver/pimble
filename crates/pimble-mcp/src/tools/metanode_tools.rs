//! The tools that read and set a metanode (docs/QUERY_METANODES_CONTRACT.md):
//! its viewer and its source, a query's conditions given as an agent writes them.


use pimble_api::names::label;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;


use super::{NodeParam, Pimble};

#[derive(Deserialize, JsonSchema)]
pub struct SetMetanodeParams {
    /// The node.
    pub node: String,
    /// The viewer to show it with (a plugin's id, such as "kanban"). Leave it
    /// out, or give "", to show it as an ordinary node again (its outline):
    /// its source is kept for next time.
    #[serde(default)]
    pub viewer: Option<String>,
    /// How many levels below the node it shows: 1 its children (the
    /// default), 2 its children and theirs (a kanban's columns and cards).
    /// Leave it out to keep the source it has.
    #[serde(default)]
    pub depth: Option<u32>,
    /// Make it a metanode with a query instead: it shows its own children, then the
    /// nodes this finds (live, never copied). Its viewer is "list" unless
    /// `viewer` says otherwise. Not together with `depth`.
    #[serde(default)]
    pub query: Option<MatchParams>,
}

/// What a metanode's query finds: the nodes every condition holds for.
#[derive(Deserialize, JsonSchema)]
pub struct MatchParams {
    /// Where it looks: leave it out for the node's own store; "everywhere"
    /// for every open store; a store's name; or a node (link, id or path) for
    /// everything under it.
    #[serde(default)]
    pub within: Option<String>,
    /// The conditions, all of which hold for a node it finds. Empty finds
    /// every node where it looks.
    #[serde(default)]
    pub all: Vec<ConditionParams>,
    /// "title" (the default), "modified", "created", or "field:<name>".
    #[serde(default)]
    pub order: Option<String>,
    /// Largest, newest or last first.
    #[serde(default)]
    pub descending: bool,
    /// At most this many (default 50, at most 500).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// One condition: give exactly one of `tag`, `no_tag`, `text`, `field`,
/// `links_to`, `type`.
#[derive(Deserialize, JsonSchema, Default)]
pub struct ConditionParams {
    /// Tagged this.
    #[serde(default)]
    pub tag: Option<String>,
    /// Not tagged this.
    #[serde(default)]
    pub no_tag: Option<String>,
    /// Every word of this in the title or text, any case.
    #[serde(default)]
    pub text: Option<String>,
    /// A field's name, compared with `is` and `value`.
    #[serde(default)]
    pub field: Option<String>,
    /// For `field`: "<", "<=", "=", ">=", ">", "set" or "empty".
    #[serde(default)]
    pub is: Option<String>,
    /// For `field`: the value, as a person types it ("Friday", "$12", "Open").
    #[serde(default)]
    pub value: Option<String>,
    /// Content that links to this node (link, id or path).
    #[serde(default)]
    pub links_to: Option<String>,
    /// "document", "canvas", "sheet", "discussion", "kanban", or a node type.
    #[serde(default, rename = "type")]
    pub node_type: Option<String>,
}

#[tool_router(router = metanode_router, vis = "pub(super)")]
impl Pimble {
    /// What a metanode shows (docs/METANODES_CONTRACT.md): its source's nodes as an
    /// indented outline in the tree's own order, each line with its link. A node
    /// with no viewer is read as its children. A metanode with a query lists its
    /// own children, then the nodes it finds, each marked found with where it lives.
    #[tool(name = "get_metanode")]
    pub(super) async fn get_metanode(&self, Parameters(p): Parameters<NodeParam>) -> Result<String, String> {
        let addr = self.resolve(&p.node).await?;
        let forest = self.api.metanode_source(addr).await?;
        self.present(addr).await;
        let name = label(&forest.node);
        let mut out = match &forest.spec {
            Some(metanode) => format!(
                "{} ({}) is a metanode: {}, shown with the {} viewer.\n",
                self.api.path(addr).await,
                addr.link(),
                self.api.describe_source(addr, &metanode.source).await,
                metanode.viewer
            ),
            None => format!(
                "{} ({}) is not a metanode (set_metanode makes it one); {}:\n",
                self.api.path(addr).await,
                addr.link(),
                forest.source.describe(&name)
            ),
        };
        if let Some(why) = forest.not_understood() {
            out.push_str(&format!("{why}\n"));
        }
        if forest.entries.is_empty() {
            out.push_str("(it shows nothing: no children)\n");
        }
        for entry in forest.entries.iter().filter(|e| !e.found) {
            let indent = "  ".repeat(entry.depth.saturating_sub(1) as usize);
            out.push_str(&format!("{indent}- {} ({})\n", label(&entry.node), entry.addr.link()));
        }
        if forest.source.is_match() {
            // docs/QUERY_METANODES_CONTRACT.md section 7: the finds after its
            // own children, each marked; they live elsewhere.
            let found: Vec<_> = forest.found().collect();
            match found.len() {
                0 => out.push_str("Found: nothing matches now.\n"),
                n => out.push_str(&format!(
                    "Found ({n}; each lives elsewhere: move_node and delete_node work on it where it lives, not here):\n"
                )),
            }
            for entry in found {
                out.push_str(&format!("- {} ({}) [found, in {}]\n", label(&entry.node), entry.addr.link(), self.api.lives_in(entry.addr).await));
            }
        }
        Ok(out)
    }

    /// Make a node a metanode shown with a viewer and a source, or an ordinary node
    /// again. Every gesture in a viewer is an ordinary edit of real nodes, which the
    /// other tools already make (move_node, create_node, set_fields, set_tags).
    /// With `query` it becomes a metanode with a query: its own children, then the nodes the
    /// query finds (tags, absent tags, words, field values, links, types), live.
    #[tool(name = "set_metanode")]
    pub(super) async fn set_metanode(&self, Parameters(p): Parameters<SetMetanodeParams>) -> Result<String, String> {
        use pimble_core::metanode::{Source, MAX_SUBTREE_DEPTH};
        let addr = self.resolve(&p.node).await?;
        let mut viewer = p.viewer.as_deref().map(str::trim).filter(|v| !v.is_empty());
        let source = match (p.depth, &p.query) {
            (Some(_), Some(_)) => return Err("Give `depth` or `query`, not both.".into()),
            (_, Some(query)) => {
                // A query metanode is shown as a list (docs/QUERY_METANODES_CONTRACT.md section 2).
                viewer = viewer.or(Some("list"));
                Some(self.match_source(query).await?)
            }
            (Some(0), _) => return Err("`depth` is at least 1 (the node's children).".into()),
            (Some(d), _) if d > MAX_SUBTREE_DEPTH => return Err(format!("`depth` is at most {MAX_SUBTREE_DEPTH}.")),
            (Some(1), _) => Some(Source::children()),
            (Some(d), _) => Some(Source::subtree(d)),
            (None, None) => None,
        };
        let edited = self.api.set_metanode(addr, viewer, source.as_ref()).await?;
        if !edited.changed {
            return Err("That would change nothing, so nothing was written.".into());
        }
        let node = self.api.node(addr).await?;
        Ok(match &node.metadata.metanode {
            Some(metanode) => {
                let mut said = format!(
                    "{} is now a metanode: {}, shown with the {} viewer.",
                    addr.link(),
                    self.api.describe_source(addr, &metanode.source).await,
                    metanode.viewer
                );
                let known = super::viewer_tools::known_viewers();
                if !known.contains(&metanode.viewer) {
                    said.push_str(&format!(
                        " No plugin installed here says it provides that viewer, so Pimble shows the outline until one does; the viewers here: {}.",
                        known.join(", ")
                    ));
                }
                said
            }
            None => format!("{} is an ordinary node again, shown as its outline.", addr.link()),
        })
    }
}

impl Pimble {
    /// A query metanode's source from what an agent gave (docs/QUERY_METANODES_CONTRACT.md
    /// section 3), with its nodes and stores named as the source names them.
    async fn match_source(&self, q: &MatchParams) -> Result<pimble_core::metanode::Source, String> {
        use pimble_core::metanode::{node_ref, Condition, FieldOp, Order, Query, Source, Within, DEFAULT_LIMIT, MAX_LIMIT};
        let within = match q.within.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
            None => None,
            Some(w) if w.eq_ignore_ascii_case("everywhere") => Some(Within::Everywhere),
            Some(w) => match self.find_store(w).await {
                Ok(store) => Some(Within::Store(store.id)),
                Err(_) => {
                    let under = self.resolve(w).await?;
                    Some(Within::Under(node_ref(under.store, under.node)))
                }
            },
        };
        let mut all = Vec::new();
        for c in &q.all {
            let given = [c.tag.is_some(), c.no_tag.is_some(), c.text.is_some(), c.field.is_some(), c.links_to.is_some(), c.node_type.is_some()];
            if given.iter().filter(|g| **g).count() != 1 {
                return Err("Each condition gives exactly one of `tag`, `no_tag`, `text`, `field`, `links_to`, `type`.".into());
            }
            all.push(if let Some(tag) = &c.tag {
                Condition::Tag { tag: tag.clone() }
            } else if let Some(no_tag) = &c.no_tag {
                Condition::NoTag { no_tag: no_tag.clone() }
            } else if let Some(text) = &c.text {
                Condition::Text { text: text.clone() }
            } else if let Some(field) = &c.field {
                let is = match c.is.as_deref().map(str::trim).unwrap_or("set") {
                    "<" => FieldOp::Less,
                    "<=" => FieldOp::AtMost,
                    "=" | "==" => FieldOp::Equal,
                    ">=" => FieldOp::AtLeast,
                    ">" => FieldOp::More,
                    "set" => FieldOp::Set,
                    "empty" => FieldOp::Empty,
                    other => return Err(format!("`is` is one of <, <=, =, >=, >, set, empty (not \"{other}\").")),
                };
                if !matches!(is, FieldOp::Set | FieldOp::Empty) && c.value.as_deref().is_none_or(|v| v.trim().is_empty()) {
                    return Err(format!("Comparing the field \"{field}\" needs a `value`."));
                }
                Condition::Field { field: field.clone(), is, value: c.value.clone() }
            } else if let Some(node) = &c.links_to {
                let target = self.resolve(node).await?;
                Condition::LinksTo { links_to: node_ref(target.store, target.node) }
            } else {
                Condition::Type { node_type: c.node_type.clone().unwrap_or_default() }
            });
        }
        let order = match q.order.as_deref().map(str::trim).unwrap_or("title") {
            "title" | "" => Order::Title,
            "modified" => Order::Modified,
            "created" => Order::Created,
            other => match other.strip_prefix("field:") {
                Some(name) if !name.trim().is_empty() => Order::Field(name.trim().to_string()),
                _ => return Err("`order` is \"title\", \"modified\", \"created\" or \"field:<name>\".".into()),
            },
        };
        let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 || limit > MAX_LIMIT {
            return Err(format!("`limit` is from 1 to {MAX_LIMIT}."));
        }
        Ok(Source { query: Query::Match { within, all, order, descending: q.descending, limit }, later: Default::default() })
    }
}

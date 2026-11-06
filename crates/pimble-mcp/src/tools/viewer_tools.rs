//! What the installed viewers take (docs/AGENT_GUIDES_CONTRACT.md): each
//! plugin's guide, read from the `pimble-guide` section of its `.wasm` file in
//! this computer's plugins folder (nothing runs), and a metanode's settings,
//! read and written as one edit of its `data` under the viewer's id, checked
//! against the viewer's schema first.

use std::path::PathBuf;
use std::sync::Mutex;

use pimble_api::{label, Addr};
use pimble_crdt::NodeDoc;
use pimble_plugin_abi::guide::{self, ViewerGuide};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{crdt, Pimble};

/// The viewer Pimble itself provides: a query metanode's list.
const LIST: &str = "list";

/// Where the guides are read from in a test; this computer's plugins folder
/// otherwise.
static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(test)]
pub(super) fn set_plugins_dir(dir: PathBuf) {
    *DIR.lock().unwrap() = Some(dir);
}

/// The folder the desktop app loads plugins from.
fn plugins_dir() -> PathBuf {
    if let Some(dir) = DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return dir;
    }
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("plugins")
}

/// One `.wasm` file in the plugins folder and the guide it carries.
struct Installed {
    file: String,
    path: PathBuf,
    guide: Option<Result<guide::Guide, String>>,
}

/// Every plugin installed here, read now: a handful of file reads, so a
/// plugin added or updated since is seen at once.
fn installed() -> Vec<Installed> {
    let mut found: Vec<Installed> = std::fs::read_dir(plugins_dir())
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "wasm"))
                .filter_map(|path| {
                    let bytes = std::fs::read(&path).ok()?;
                    let file = path.file_name()?.to_string_lossy().into_owned();
                    Some(Installed { file, guide: guide::read(&bytes), path })
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort_by(|a, b| a.file.cmp(&b.file));
    found
}

/// The guide for viewer `id`, from whichever installed plugin provides it.
fn viewer_guide(id: &str) -> Option<ViewerGuide> {
    provider(id).and_then(|(_, g)| g.viewer(id).cloned())
}

/// The installed plugin that provides viewer `id`, or whose file is `id`
/// (`timeline_viewer`, `timeline-viewer`), with its guide.
fn provider(id: &str) -> Option<(PathBuf, guide::Guide)> {
    let stem = id.trim().trim_end_matches(".wasm").replace('-', "_");
    let all: Vec<(PathBuf, String, guide::Guide)> =
        installed().into_iter().filter_map(|p| Some((p.path, p.file.trim_end_matches(".wasm").to_string(), p.guide?.ok()?))).collect();
    all.iter()
        .find(|(_, _, g)| g.viewer(id).is_some())
        .or_else(|| all.iter().find(|(_, file, _)| *file == stem))
        .map(|(path, _, g)| (path.clone(), g.clone()))
}

// ── Running a plugin's tools (docs/AGENT_GUIDES_CONTRACT.md section 8) ─────

/// What a file was when it was compiled: its length and modified time.
type Stamp = (u64, Option<std::time::SystemTime>);

/// Plugins compiled so far, for as long as this process runs.
static COMPILED: Mutex<Option<std::collections::HashMap<PathBuf, (Stamp, pimble_plugin_host::wasmtime_engine::Plugin)>>> = Mutex::new(None);

/// The plugin at `path`, compiled once for each version of the file.
fn compiled(path: &std::path::Path) -> Result<pimble_plugin_host::wasmtime_engine::Plugin, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let stamp: Stamp = (meta.len(), meta.modified().ok());
    if let Some((was, plugin)) = COMPILED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).get(path) {
        if *was == stamp {
            return Ok(plugin.clone());
        }
    }
    let plugin = pimble_plugin_host::wasmtime_engine::load_file(path, pimble_plugin_host::wasmtime_engine::DEFAULT_BUDGET).map_err(|(_, sentence)| sentence)?;
    COMPILED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(path.to_path_buf(), (stamp, plugin.clone()));
    Ok(plugin)
}

/// Compile the installed plugins that have tools in the background, so the
/// first check an agent's write makes does not wait for the compiler.
pub(super) fn warm() {
    let _ = std::thread::Builder::new().name("pimble-mcp-plugins".into()).spawn(|| {
        for plugin in installed() {
            if plugin.guide.as_ref().is_some_and(|g| g.as_ref().is_ok_and(|g| !g.tools.is_empty())) {
                if let Err(e) = compiled(&plugin.path) {
                    tracing::warn!("plugin {} cannot run its tools: {e}", plugin.file);
                }
            }
        }
    });
}

impl Pimble {
    /// Run `tool` of the plugin at `path` with `args`, on a thread of its own
    /// (a plugin runs on one thread), through this agent's connection.
    async fn run_tool(&self, path: PathBuf, tool: &str, args: serde_json::Value) -> Result<guide::ToolAnswer, String> {
        let backend = self.tool_backend.clone().ok_or("This MCP server runs no plugin tools.")?;
        let request = guide::ToolRequest { tool: tool.to_string(), args };
        tokio::task::spawn_blocking(move || {
            let plugin = compiled(&path)?;
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, pimble_plugin_host::tools::run(&plugin, backend, &request))
        })
        .await
        .map_err(|e| format!("the plugin's tool did not finish: {e}"))?
    }

    /// The checks a viewer's plugin makes before `fields` are written on
    /// `addr`: for every metanode showing it (a parent within its source's
    /// depth) whose viewer's plugin has `check_field`. Err is the refusal;
    /// Ok is what the checks said about the fields their viewers read.
    pub(super) async fn field_checks(&self, addr: Addr, fields: &std::collections::BTreeMap<String, Option<String>>) -> Result<Vec<String>, String> {
        let mut said = Vec::new();
        let written = self.api.node(addr).await.map(|n| label(&n)).unwrap_or_default();
        let mut at = addr;
        for distance in 1..=pimble_core::metanode::MAX_SUBTREE_DEPTH {
            let Ok(node) = self.api.node(at).await else { break };
            let Some(parent) = node.parent_id else { break };
            at = Addr { store: at.store, node: parent };
            let Ok(shower) = self.api.node(at).await else { break };
            let Some(metanode) = &shower.metadata.metanode else { continue };
            if metanode.source.depth() < distance {
                continue;
            }
            let Some((path, g)) = provider(&metanode.viewer) else { continue };
            if g.tool(guide::CHECK_FIELD).is_none() {
                continue;
            }
            let listed: Vec<&str> = g.viewer(&metanode.viewer).map(|v| v.fields.iter().map(|f| f.name.as_str()).collect()).unwrap_or_default();
            for (field, value) in fields {
                let Some(value) = value else { continue };
                let args = serde_json::json!({ "viewer": metanode.viewer, "metanode": at.link(), "node": addr.link(), "field": field, "value": value });
                let answer = self.run_tool(path.clone(), guide::CHECK_FIELD, args).await?;
                if !answer.ok {
                    return Err(format!("Nothing was written: {field} on {written} is shown by the {} viewer of {}, which says: {}", metanode.viewer, label(&shower), answer.said));
                }
                if listed.contains(&field.as_str()) {
                    said.push(answer.said);
                }
            }
        }
        Ok(said)
    }
}

/// The ids of every viewer a guide names here, and Pimble's own.
pub(super) fn known_viewers() -> Vec<String> {
    let mut ids: Vec<String> = installed().into_iter().filter_map(|p| p.guide?.ok()).flat_map(|g| g.viewers.into_iter().map(|v| v.viewer)).collect();
    ids.push(LIST.to_string());
    ids
}

fn describe_viewer(v: &ViewerGuide, from: &str) -> String {
    let mut out = format!("## {} ({from})\n\n{}\n", v.viewer, v.about);
    if let Some(source) = &v.source {
        out.push_str(&format!("\nSource: {source}.\n"));
    }
    if !v.fields.is_empty() {
        out.push_str("\nFields it reads on the nodes it shows (set_fields writes them):\n");
        for f in &v.fields {
            match &f.example {
                Some(e) => out.push_str(&format!("- {}: {} For example \"{e}\".\n", f.name, f.about)),
                None => out.push_str(&format!("- {}: {}\n", f.name, f.about)),
            }
        }
    }
    match &v.settings {
        Some(s) => {
            out.push_str(&format!("\nSettings (get_viewer_settings, set_viewer_settings): {}\n", s.about));
            out.push_str(&format!("\nSchema:\n```json\n{}\n```\n", serde_json::to_string(&s.schema).unwrap_or_default()));
            if let Some(example) = &s.example {
                out.push_str(&format!("\nExample:\n```json\n{}\n```\n", serde_json::to_string_pretty(example).unwrap_or_default()));
            }
        }
        None => out.push_str("\nIt takes no settings.\n"),
    }
    out
}

#[derive(Deserialize, JsonSchema)]
pub struct ViewerSettingsParam {
    /// The metanode: a pimble: link, a node id, or a path.
    pub node: String,
    /// Which viewer's settings; leave it out for the viewer the node is shown with.
    #[serde(default)]
    pub viewer: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CallPluginToolParams {
    /// A viewer the plugin provides ("timeline"), or its file ("timeline_viewer").
    pub plugin: String,
    /// The tool's name, as list_viewers lists it.
    pub tool: String,
    /// The tool's arguments, as list_viewers says.
    #[serde(default)]
    pub arguments: serde_json::Value,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetViewerSettingsParams {
    /// The metanode: a pimble: link, a node id, or a path.
    pub node: String,
    /// The whole settings object, as the viewer's schema in list_viewers says.
    /// It replaces what is there: read get_viewer_settings first to change one thing.
    pub settings: serde_json::Value,
    /// Which viewer's settings; leave it out for the viewer the node is shown with.
    #[serde(default)]
    pub viewer: Option<String>,
}

impl Pimble {
    /// The viewer a settings call means, and refusals in words: the one named,
    /// which the node must be shown with, or the node's own.
    async fn settings_target(&self, node: &str, viewer: Option<&str>) -> Result<(Addr, String, String), String> {
        let addr = self.api.resolve(node).await?;
        let n = self.api.node(addr).await?;
        let shown = n.metadata.metanode.as_ref().map(|m| m.viewer.clone());
        let name = label(&n);
        let viewer = match (viewer.map(str::trim).filter(|v| !v.is_empty()), shown) {
            (Some(v), Some(s)) if v != s => {
                return Err(format!("{name} ({}) is shown with the {s} viewer, not {v}. set_metanode changes its viewer.", addr.link()));
            }
            (Some(_), None) | (None, None) => {
                return Err(format!("{name} ({}) is not a metanode, so no viewer reads settings from it. set_metanode makes it one.", addr.link()));
            }
            (_, Some(s)) => s,
        };
        Ok((addr, viewer, name))
    }
}

#[tool_router(router = viewer_router, vis = "pub(super)")]
impl Pimble {
    /// Every viewer installed here, from each plugin's own guide: what it shows
    /// and how it reads the tree, the node fields it reads (a timeline's date,
    /// a todo list's done), and its settings with their schema and an example
    /// (a timeline's calendar, a calendar node's whole definition). Read it
    /// before making a metanode or writing a viewer's settings.
    #[tool(name = "list_viewers")]
    async fn list_viewers(&self) -> Result<String, String> {
        let mut out = String::from("The viewers installed on this computer (set_metanode's viewer):\n\n");
        let mut without = Vec::new();
        for plugin in installed() {
            match plugin.guide {
                Some(Ok(g)) => {
                    for v in &g.viewers {
                        out.push_str(&describe_viewer(v, &plugin.file));
                        out.push('\n');
                    }
                    if !g.tools.is_empty() {
                        out.push_str(&format!("### Tools of {} (call_plugin_tool)\n\n", plugin.file));
                        for t in &g.tools {
                            let auto = match t.name.as_str() {
                                guide::CHECK_SETTINGS => " (run by set_viewer_settings before it writes)",
                                guide::CHECK_FIELD => " (run by set_fields before it writes a field this plugin's viewers read)",
                                _ => "",
                            };
                            out.push_str(&format!("- {}{auto}: {} Arguments: {}\n", t.name, t.about, serde_json::to_string(&t.params).unwrap_or_default()));
                        }
                        out.push('\n');
                    }
                }
                Some(Err(e)) => without.push(format!("{} (its guide does not read: {e})", plugin.file)),
                None => without.push(plugin.file),
            }
        }
        out.push_str(&format!(
            "## {LIST} (Pimble's own)\n\nShows a metanode with a query: its own children, then the nodes its query finds, as a list. \
             set_metanode with query makes one. It takes no settings.\n"
        ));
        if !without.is_empty() {
            out.push_str(&format!("\nInstalled without a guide (their viewers work in Pimble, but say nothing here): {}.\n", without.join(", ")));
        }
        Ok(out)
    }

    /// Run one of a plugin's tools (list_viewers lists them with their
    /// arguments): `plugin` is a viewer it provides ("timeline") or its file
    /// ("timeline_viewer"). Its reads and writes are bounded by your grant and
    /// by what the plugin asks for.
    #[tool(name = "call_plugin_tool")]
    async fn call_plugin_tool(&self, Parameters(p): Parameters<CallPluginToolParams>) -> Result<String, String> {
        let Some((path, g)) = provider(&p.plugin) else {
            return Err(format!("No plugin installed here provides {} or is called that. list_viewers lists them.", p.plugin));
        };
        let Some(tool) = g.tool(&p.tool) else {
            let names: Vec<&str> = g.tools.iter().map(|t| t.name.as_str()).collect();
            return Err(format!("{} has no tool {}; its tools: {}.", p.plugin, p.tool, if names.is_empty() { "none".to_string() } else { names.join(", ") }));
        };
        let args = if p.arguments.is_null() { serde_json::json!({}) } else { p.arguments.clone() };
        if let Err(e) = guide::check(&tool.params, &args) {
            return Err(format!("Nothing was run: {}. Its arguments: {}", e.replace("the settings object", "the arguments"), serde_json::to_string(&tool.params).unwrap_or_default()));
        }
        let answer = self.run_tool(path, &p.tool, args).await?;
        if answer.ok { Ok(answer.said) } else { Err(answer.said) }
    }

    /// A metanode's viewer settings, as JSON: `{}` when none are set.
    #[tool(name = "get_viewer_settings")]
    async fn get_viewer_settings(&self, Parameters(p): Parameters<ViewerSettingsParam>) -> Result<String, String> {
        let (addr, viewer, name) = self.settings_target(&p.node, p.viewer.as_deref()).await?;
        let node = self.api.node(addr).await?;
        self.present(addr).await;
        let doc = NodeDoc::load(&node.content).map_err(|e| e.to_string())?;
        let settings = doc.data_json().get(&viewer).cloned().unwrap_or_else(|| serde_json::json!({}));
        Ok(format!(
            "The {viewer} viewer's settings on {name} ({}):\n```json\n{}\n```",
            addr.link(),
            serde_json::to_string_pretty(&settings).unwrap_or_default()
        ))
    }

    /// Write a metanode's viewer settings, whole, after checking them against
    /// the viewer's schema (list_viewers shows it). Nothing is written when
    /// they do not fit; the answer says where and what was wanted.
    #[tool(name = "set_viewer_settings")]
    async fn set_viewer_settings(&self, Parameters(p): Parameters<SetViewerSettingsParams>) -> Result<String, String> {
        let (addr, viewer, name) = self.settings_target(&p.node, p.viewer.as_deref()).await?;
        let Some(guide) = viewer_guide(&viewer) else {
            return Err(format!(
                "No plugin installed here says what the {viewer} viewer's settings are, so nothing was written. list_viewers lists the ones that do."
            ));
        };
        let Some(settings) = &guide.settings else {
            return Err(format!("The {viewer} viewer takes no settings, so nothing was written."));
        };
        if let Err(e) = guide::check(&settings.schema, &p.settings) {
            return Err(format!("Nothing was written: {e}. list_viewers shows the {viewer} viewer's schema and an example."));
        }
        // The plugin's own check, which knows what the settings mean.
        let mut said = String::new();
        if let Some((path, g)) = provider(&viewer).filter(|(_, g)| g.tool(guide::CHECK_SETTINGS).is_some()) {
            let _ = g;
            let args = serde_json::json!({ "viewer": viewer, "metanode": addr.link(), "settings": p.settings });
            let answer = self.run_tool(path, guide::CHECK_SETTINGS, args).await?;
            if !answer.ok {
                return Err(format!("Nothing was written: the {viewer} viewer says {}", answer.said));
            }
            said = format!(" {}", answer.said);
        }
        let value = p.settings.clone();
        let key = viewer.clone();
        self.edit(addr, move |doc| doc.set_data(&key, &value).map_err(crdt)).await?;
        Ok(format!("Wrote the {viewer} viewer's settings on {name} ({}).{said}", addr.link()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_viewer_reads_as_an_agent_needs_it() {
        let g = guide::Guide::parse(
            br#"{"v":1,"viewers":[{"viewer":"todo","about":"A checklist.","source":"its subtree","fields":[{"name":"done","about":"When it was checked.","example":"2026-11-06T18:30:00Z"}]}]}"#,
        )
        .unwrap();
        let text = describe_viewer(&g.viewers[0], "todo_viewer.wasm");
        assert!(text.starts_with("## todo (todo_viewer.wasm)\n\nA checklist."), "{text}");
        assert!(text.contains("- done: When it was checked. For example \"2026-11-06T18:30:00Z\"."), "{text}");
        assert!(text.contains("It takes no settings."), "{text}");
    }
}

/// The viewer tools against a real server, with a plugins folder holding a
/// module whose guide is the timeline plugin's own.
#[cfg(test)]
mod against_a_server {
    use std::sync::Arc;

    use pimble_client::PimbleClient;
    use pimble_server::{PimbleServer, ServerConfig};
    use rmcp::handler::server::wrapper::Parameters;
    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::super::Pimble;
    use crate::connection::Connection;

    fn p<T: DeserializeOwned>(v: serde_json::Value) -> Parameters<T> {
        Parameters(serde_json::from_value(v).expect("params"))
    }

    /// A module of nothing but a `pimble-guide` section.
    fn module_with(guide: &[u8]) -> Vec<u8> {
        let name = pimble_plugin_abi::guide::SECTION.as_bytes();
        let mut body = vec![name.len() as u8];
        body.extend(name);
        body.extend(guide);
        let mut m = b"\0asm\x01\0\0\0\0".to_vec();
        let mut size = body.len() as u32;
        loop {
            let byte = (size & 0x7f) as u8;
            size >>= 7;
            if size == 0 {
                m.push(byte);
                break;
            }
            m.push(byte | 0x80);
        }
        m.extend(body);
        m
    }

    #[tokio::test]
    async fn an_agent_reads_a_guide_and_writes_settings_that_fit_it() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        // The real plugin, whose checks run: built by `cd plugins && cargo
        // build --release --target wasm32-wasip1`.
        let built = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/target/wasm32-wasip1/release/timeline_viewer.wasm");
        if !built.is_file() {
            eprintln!("skipped: build the plugins first; no {}", built.display());
            return;
        }
        std::fs::copy(&built, plugins.join("timeline_viewer.wasm")).unwrap();
        std::fs::write(plugins.join("hello_viewer.wasm"), module_with(b"")[..12].to_vec()).unwrap();
        super::set_plugins_dir(plugins);

        let mut server = PimbleServer::with_config(ServerConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            replicas_dir: Some(dir.path().join("replicas")),
            ..Default::default()
        });
        server.start().await.expect("server starts");
        let url = format!("http://{}", server.addr());
        let service = PimbleClient::connect(&url).await.unwrap();
        service.create_store(dir.path().join("world.pimble"), "World").await.unwrap();
        let pimble = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));

        let listed = pimble.list_viewers().await.unwrap();
        assert!(listed.contains("## timeline (timeline_viewer.wasm)") && listed.contains("## calendar (timeline_viewer.wasm)"), "{listed}");
        assert!(listed.contains("- date: One moment") && listed.contains("## list (Pimble's own)"), "{listed}");
        assert!(listed.contains("Installed without a guide") && listed.contains("hello_viewer.wasm"), "{listed}");

        pimble.create_node(p(json!({ "parent": "World", "title": "Founding Reckoning" }))).await.unwrap();
        let refused = pimble.set_viewer_settings(p(json!({ "node": "World/Founding Reckoning", "settings": {} }))).await.unwrap_err();
        assert!(refused.contains("is not a metanode"), "{refused}");
        let made = pimble.set_metanode(p(json!({ "node": "World/Founding Reckoning", "viewer": "calendar", "depth": 1 }))).await.unwrap();
        assert!(!made.contains("No plugin installed here"), "{made}");

        let bad = json!({ "name": "Founding", "day": { "hours": 24, "minutes": 60, "seconds": 60 }, "year": [{ "month": "Firstmonth", "days": "thirty" }], "eras": [{ "name": "Year of the Founding", "short": "YF", "counts": "up" }] });
        let refused = pimble.set_viewer_settings(p(json!({ "node": "World/Founding Reckoning", "settings": bad }))).await.unwrap_err();
        assert!(refused.starts_with("Nothing was written: year[0] must be one of: a month"), "{refused}");
        let read = pimble.get_viewer_settings(p(json!({ "node": "World/Founding Reckoning" }))).await.unwrap();
        assert!(read.contains("```json\n{}\n```"), "nothing was written: {read}");

        let good = json!({ "name": "Founding", "day": { "hours": 24, "minutes": 60, "seconds": 60 }, "year": [{ "month": "Firstmonth", "short": "Fir", "days": 30 }, { "day": "Year's Gate" }], "eras": [{ "name": "Year of the Founding", "short": "YF", "from": 0, "counts": "up", "quiet": true }] });
        let wrote = pimble.set_viewer_settings(p(json!({ "node": "World/Founding Reckoning", "settings": good }))).await.unwrap();
        assert!(wrote.starts_with("Wrote the calendar viewer's settings on Founding Reckoning"), "{wrote}");
        let read = pimble.get_viewer_settings(p(json!({ "node": "World/Founding Reckoning" }))).await.unwrap();
        assert!(read.contains("\"Firstmonth\"") && read.contains("\"Year's Gate\""), "{read}");

        // Another viewer's settings on it are refused, naming the one it has.
        let refused = pimble.get_viewer_settings(p(json!({ "node": "World/Founding Reckoning", "viewer": "timeline" }))).await.unwrap_err();
        assert!(refused.contains("is shown with the calendar viewer, not timeline"), "{refused}");
        // get_node points at the guide.
        let node = pimble.get_node(p(json!({ "node": "World/Founding Reckoning" }))).await.unwrap();
        assert!(node.contains("list_viewers says what its viewer reads"), "{node}");
        // A viewer nothing provides is written and said.
        pimble.create_node(p(json!({ "parent": "World", "title": "Map" }))).await.unwrap();
        let made = pimble.set_metanode(p(json!({ "node": "World/Map", "viewer": "atlas" }))).await.unwrap();
        assert!(made.contains("No plugin installed here says it provides that viewer") && made.contains("timeline, calendar, list"), "{made}");

        // The plugin's own checks (docs/AGENT_GUIDES_CONTRACT.md section 8).
        let listed = pimble.list_viewers().await.unwrap();
        assert!(listed.contains("### Tools of timeline_viewer.wasm") && listed.contains("- read_date: "), "{listed}");
        let calendar = pimble.get_node(p(json!({ "node": "World/Founding Reckoning" }))).await.unwrap();
        let cal_link = calendar.lines().find_map(|l| l.strip_prefix("link: ")).unwrap().to_string();
        pimble.create_node(p(json!({ "parent": "World", "title": "Chronicle" }))).await.unwrap();
        pimble.set_metanode(p(json!({ "node": "World/Chronicle", "viewer": "timeline", "depth": 2 }))).await.unwrap();
        let refused = pimble.set_viewer_settings(p(json!({ "node": "World/Chronicle", "settings": { "calendar": "not a link" } }))).await.unwrap_err();
        assert!(refused.starts_with("Nothing was written: the timeline viewer says calendar must be a calendar node's pimble: link"), "{refused}");
        let wrote = pimble.set_viewer_settings(p(json!({ "node": "World/Chronicle", "settings": { "calendar": cal_link } }))).await.unwrap();
        assert!(wrote.contains("It counts in the Founding calendar."), "{wrote}");

        pimble.create_node(p(json!({ "parent": "World/Chronicle", "title": "The Founding" }))).await.unwrap();
        let refused = pimble.set_fields(p(json!({ "node": "World/Chronicle/The Founding", "fields": { "date": "Blorpuary 99, banana" } }))).await.unwrap_err();
        assert!(refused.starts_with("Nothing was written: date on The Founding is shown by the timeline viewer of Chronicle"), "{refused}");
        assert!(refused.contains("does not read as a date in the Founding calendar"), "{refused}");
        let fields = pimble.get_fields(p(json!({ "node": "World/Chronicle/The Founding" }))).await.unwrap();
        assert!(!fields.contains("Blorpuary"), "nothing was written: {fields}");
        let wrote = pimble.set_fields(p(json!({ "node": "World/Chronicle/The Founding", "fields": { "date": "Fir 3, 1 YF", "mood": "grand" } }))).await.unwrap();
        assert!(wrote.contains("\u{201c}Fir 3, 1 YF\u{201d} reads as Fir 3, 1 in the Founding calendar"), "{wrote}");
        assert!(!wrote.contains("does not read mood"), "a field the viewer does not read is not talked about: {wrote}");

        let tried = pimble.call_plugin_tool(p(json!({ "plugin": "timeline", "tool": "read_date", "arguments": { "calendar": cal_link, "text": "Year's Gate, 3 YF" } }))).await.unwrap();
        assert!(tried.contains("reads as Year's Gate, 3"), "{tried}");
        let refused = pimble.call_plugin_tool(p(json!({ "plugin": "timeline", "tool": "read_date", "arguments": {} }))).await.unwrap_err();
        assert!(refused.starts_with("Nothing was run: the arguments needs text"), "{refused}");
        let refused = pimble.call_plugin_tool(p(json!({ "plugin": "timeline", "tool": "fly", "arguments": {} }))).await.unwrap_err();
        assert!(refused.contains("has no tool fly; its tools: check_settings, check_field, read_date"), "{refused}");
        let _ = server.stop().await;
    }
}

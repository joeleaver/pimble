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
                    Some(Installed { file, guide: guide::read(&bytes) })
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort_by(|a, b| a.file.cmp(&b.file));
    found
}

/// The guide for viewer `id`, from whichever installed plugin provides it.
fn viewer_guide(id: &str) -> Option<ViewerGuide> {
    installed().into_iter().find_map(|p| p.guide?.ok()?.viewer(id).cloned())
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
        let value = p.settings.clone();
        let key = viewer.clone();
        self.edit(addr, move |doc| doc.set_data(&key, &value).map_err(crdt)).await?;
        Ok(format!("Wrote the {viewer} viewer's settings on {name} ({}).", addr.link()))
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
        let guide = include_bytes!("../../../../plugins/timeline-viewer/guide.json");
        std::fs::write(plugins.join("timeline_viewer.wasm"), module_with(guide)).unwrap();
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
        let _ = server.stop().await;
    }
}

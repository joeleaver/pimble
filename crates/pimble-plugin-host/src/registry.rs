//! The plugins an app has loaded, and the ones it refused (docs/PLUGINS_CONTRACT.md
//! section 3.5), whatever engine compiled them.
//!
//! Each engine has its own loaded plugin: the desktop's holds a wasmtime module
//! (`crate::wasmtime_engine::Plugin`), the browser's a `WebAssembly.Module` and an
//! instance made ready ahead of time (`crate::js_engine::Plugin`). What the app does
//! with the set is the same on both: find the plugin that provides a viewer, or say
//! why none will run.

use std::path::PathBuf;

use pimble_plugin_abi::{ChildViewerDecl, Manifest, ManifestHead};

/// A plugin an engine loaded: what it said it is.
pub trait Loaded {
    fn manifest(&self) -> &Manifest;
}

/// The plugins loaded, and the files refused.
#[derive(Clone)]
pub struct Registry<P> {
    pub plugins: Vec<P>,
    /// Files that are not plugins this app can run, each with why, and the viewers
    /// it said it provides when it could say.
    pub refused: Vec<Refused>,
}

impl<P> Default for Registry<P> {
    fn default() -> Self {
        Registry { plugins: Vec::new(), refused: Vec::new() }
    }
}

/// Why a file did not load as a plugin this app runs, and what it said it is
/// as far as that could be read.
#[derive(Debug, Clone)]
pub struct NotLoaded {
    /// The whole manifest, when it decoded.
    pub manifest: Option<Manifest>,
    /// Its id, name, version and ABI, when at least those decoded.
    pub head: Option<ManifestHead>,
    /// What a person reads.
    pub sentence: String,
}

impl NotLoaded {
    /// Refused before it could say what it is.
    pub fn new(sentence: String) -> NotLoaded {
        NotLoaded { manifest: None, head: None, sentence }
    }

    /// Refused although its whole manifest read.
    pub fn described(manifest: Manifest, sentence: String) -> NotLoaded {
        NotLoaded { head: Some(manifest.head()), manifest: Some(manifest), sentence }
    }

    /// Refused with only the head of its manifest read.
    pub fn headed(head: ManifestHead, sentence: String) -> NotLoaded {
        NotLoaded { manifest: None, head: Some(head), sentence }
    }
}

/// A plugin file the app does not run.
#[derive(Debug, Clone)]
pub struct Refused {
    /// Where it came from: a path in the plugins folder, or (in the browser) the
    /// name of the file the person added.
    pub path: PathBuf,
    pub viewers: Vec<String>,
    pub sentence: String,
    /// What it said it is, when it could say.
    pub head: Option<ManifestHead>,
}

impl Refused {
    /// The file's name, as a person knows it.
    pub fn file(&self) -> String {
        self.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The plugin it is: the id its manifest gave, else the file's name read
    /// as one (`kanban_viewer.wasm`, as the build names it, is `kanban-viewer`).
    pub fn id(&self) -> String {
        match &self.head {
            Some(head) if !head.id.is_empty() => head.id.clone(),
            _ => id_from_file(&self.path),
        }
    }
}

/// The plugin id a file's name reads as: its stem, with `_` for `-`.
pub fn id_from_file(path: &std::path::Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().replace('_', "-")).unwrap_or_default()
}

/// What the app has for a viewer id.
pub enum ViewerLookup<'a, P> {
    Ready(&'a P),
    /// A plugin provides it but was refused; the sentence says why.
    Refused(&'a str),
    Missing,
}

impl<P: Loaded> Registry<P> {
    /// Take what loading one file came to: the plugin, or why it is refused (with
    /// what it said it is when it could say). One plugin is one id: a second
    /// file of a loaded plugin keeps the later version of the two, and a
    /// refused file of a plugin that loaded is set aside (both said in the log).
    pub fn take(&mut self, path: PathBuf, loaded: Result<P, NotLoaded>) {
        match loaded {
            Ok(plugin) => {
                let m = plugin.manifest();
                if m.rinch != crate::HOST_RINCH_REVISION {
                    tracing::info!("plugin {} was built against rinch {}; this app runs rinch {}", m.id, m.rinch, crate::HOST_RINCH_REVISION);
                }
                if let Some(at) = self.plugins.iter().position(|p| p.manifest().id == m.id) {
                    let had = self.plugins[at].manifest();
                    tracing::warn!("plugin {} is in two files ({} loaded first, {} in {}); only the later version runs", m.id, had.version, m.version, path.display());
                    if crate::library::newer(&m.version, &had.version) {
                        self.plugins[at] = plugin;
                    }
                    return;
                }
                tracing::info!("plugin {} {} loaded from {}", m.id, m.version, path.display());
                let id = m.id.clone();
                self.refused.retain(|r| r.id() != id);
                self.plugins.push(plugin);
            }
            Err(refusal) => {
                tracing::warn!("{}", refusal.sentence);
                let viewers = refusal
                    .manifest
                    .map(|m| m.viewers().map(|v| v.id.clone()).chain(m.child_viewers().map(|c| c.id.clone())).collect())
                    .unwrap_or_default();
                let refused = Refused { path, viewers, sentence: refusal.sentence, head: refusal.head };
                if self.plugin(&refused.id()).is_some() {
                    return tracing::warn!("{} is set aside: the {} plugin loaded from another file", refused.file(), refused.id());
                }
                self.refused.push(refused);
            }
        }
    }

    /// The plugin providing viewer `id`, or why there is none to run.
    pub fn viewer(&self, id: &str) -> ViewerLookup<'_, P> {
        if let Some(plugin) = self.plugins.iter().find(|p| p.manifest().mounts(id)) {
            return ViewerLookup::Ready(plugin);
        }
        match self.refused.iter().find(|r| r.viewers.iter().any(|v| v == id)) {
            Some(refused) => ViewerLookup::Refused(&refused.sentence),
            None => ViewerLookup::Missing,
        }
    }

    /// The viewer for the children of viewer `of`'s metanodes, when a loaded
    /// plugin provides one: what a kanban's column opens as.
    pub fn child_viewer(&self, of: &str) -> Option<&ChildViewerDecl> {
        self.plugins.iter().flat_map(|p| p.manifest().child_viewers()).find(|c| c.of == of)
    }

    /// The plugin whose id is `id`.
    pub fn plugin(&self, id: &str) -> Option<&P> {
        self.plugins.iter().find(|p| p.manifest().id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_plugin_abi::{Provision, SourceShape, ViewerDecl, ABI_VERSION};

    struct Fake(Manifest);

    impl Loaded for Fake {
        fn manifest(&self) -> &Manifest {
            &self.0
        }
    }

    fn kanban() -> Manifest {
        Manifest {
            id: "kanban-viewer".into(),
            name: "Kanban".into(),
            version: "0.1.0".into(),
            abi: ABI_VERSION,
            rinch: String::new(),
            provides: vec![
                Provision::Viewer(ViewerDecl { id: "kanban".into(), name: "Kanban".into(), takes: vec![SourceShape::Children] }),
                Provision::ChildViewer(ChildViewerDecl {
                    id: "kanban-column".into(),
                    name: "Kanban column".into(),
                    of: "kanban".into(),
                    icon: Some("layout-columns".into()),
                }),
            ],
            asks: Default::default(),
        }
    }

    #[test]
    fn a_child_viewer_is_found_by_its_parents_viewer_and_mounts_as_a_viewer() {
        let mut loaded = Registry::default();
        loaded.take(PathBuf::from("kanban.wasm"), Ok(Fake(kanban())));
        assert_eq!(loaded.child_viewer("kanban").map(|c| c.id.as_str()), Some("kanban-column"));
        assert!(loaded.child_viewer("hello").is_none());
        assert!(matches!(loaded.viewer("kanban-column"), ViewerLookup::Ready(_)));

        // A refused plugin's child viewer says why, as its viewers do.
        let mut refused: Registry<Fake> = Registry::default();
        refused.take(PathBuf::from("kanban.wasm"), Err(NotLoaded::described(kanban(), "Refused.".into())));
        assert!(matches!(refused.viewer("kanban-column"), ViewerLookup::Refused("Refused.")));
        assert!(refused.child_viewer("kanban").is_none());
    }

    fn kanban_at(version: &str) -> Manifest {
        Manifest { version: version.into(), ..kanban() }
    }

    #[test]
    fn one_plugin_in_two_files_runs_its_later_version() {
        let mut loaded = Registry::default();
        loaded.take(PathBuf::from("kanban-viewer.wasm"), Ok(Fake(kanban_at("0.2.0"))));
        loaded.take(PathBuf::from("kanban_viewer.wasm"), Ok(Fake(kanban_at("0.1.2"))));
        assert_eq!(loaded.plugins.len(), 1);
        assert_eq!(loaded.plugins[0].0.version, "0.2.0");

        let mut other_order = Registry::default();
        other_order.take(PathBuf::from("a.wasm"), Ok(Fake(kanban_at("0.1.2"))));
        other_order.take(PathBuf::from("b.wasm"), Ok(Fake(kanban_at("0.2.0"))));
        assert_eq!(other_order.plugins.len(), 1);
        assert_eq!(other_order.plugins[0].0.version, "0.2.0");
    }

    #[test]
    fn a_refused_file_is_known_by_its_head_or_its_name_and_yields_to_one_that_loads() {
        let head = ManifestHead { id: "kanban-viewer".into(), name: "Kanban".into(), version: "0.1.2".into(), abi: 6 };
        let mut loaded: Registry<Fake> = Registry::default();
        loaded.take(PathBuf::from("old.wasm"), Err(NotLoaded::headed(head, "Old.".into())));
        loaded.take(PathBuf::from("todo_viewer.wasm"), Err(NotLoaded::new("Junk.".into())));
        let ids: Vec<String> = loaded.refused.iter().map(Refused::id).collect();
        assert_eq!(ids, vec!["kanban-viewer".to_string(), "todo-viewer".to_string()]);

        // The library's version arrives beside it: the old file is set aside.
        loaded.take(PathBuf::from("kanban-viewer.wasm"), Ok(Fake(kanban_at("0.2.0"))));
        assert_eq!(loaded.refused.iter().map(Refused::id).collect::<Vec<_>>(), vec!["todo-viewer".to_string()]);
        // And one refused after it is too.
        loaded.take(PathBuf::from("kanban_viewer.wasm"), Err(NotLoaded::described(kanban_at("0.1.0"), "Old.".into())));
        assert_eq!(loaded.refused.len(), 1);
        assert!(matches!(loaded.viewer("kanban"), ViewerLookup::Ready(_)));
    }
}

//! The plugins an app has loaded, and the ones it refused (docs/PLUGINS_CONTRACT.md
//! section 3.5), whatever engine compiled them.
//!
//! Each engine has its own loaded plugin: the desktop's holds a wasmtime module
//! (`crate::wasmtime_engine::Plugin`), the browser's a `WebAssembly.Module` and an
//! instance made ready ahead of time (`crate::js_engine::Plugin`). What the app does
//! with the set is the same on both: find the plugin that provides a viewer, or say
//! why none will run.

use std::path::PathBuf;

use pimble_plugin_abi::{ChildViewerDecl, Manifest};

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

/// A plugin file the app does not run.
#[derive(Debug, Clone)]
pub struct Refused {
    /// Where it came from: a path in the plugins folder, or (in the browser) the
    /// name of the file the person added.
    pub path: PathBuf,
    pub viewers: Vec<String>,
    pub sentence: String,
}

impl Refused {
    /// The file's name, as a person knows it.
    pub fn file(&self) -> String {
        self.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
    }
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
    /// the manifest when it could describe itself).
    pub fn take(&mut self, path: PathBuf, loaded: Result<P, (Option<Manifest>, String)>) {
        match loaded {
            Ok(plugin) => {
                let m = plugin.manifest();
                if m.rinch != crate::HOST_RINCH_REVISION {
                    tracing::info!("plugin {} was built against rinch {}; this app runs rinch {}", m.id, m.rinch, crate::HOST_RINCH_REVISION);
                }
                tracing::info!("plugin {} {} loaded from {}", m.id, m.version, path.display());
                self.plugins.push(plugin);
            }
            Err((manifest, sentence)) => {
                tracing::warn!("{sentence}");
                let viewers = manifest
                    .map(|m| m.viewers().map(|v| v.id.clone()).chain(m.child_viewers().map(|c| c.id.clone())).collect())
                    .unwrap_or_default();
                self.refused.push(Refused { path, viewers, sentence });
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
        refused.take(PathBuf::from("kanban.wasm"), Err((Some(kanban()), "Refused.".into())));
        assert!(matches!(refused.viewer("kanban-column"), ViewerLookup::Refused("Refused.")));
        assert!(refused.child_viewer("kanban").is_none());
    }
}

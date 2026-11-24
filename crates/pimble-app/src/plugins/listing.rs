//! Settings > Plugins as one list (docs/PLUGINS_CONTRACT.md section 7): each
//! plugin once, from what is installed here and what the library offers, with
//! its one state and what its row can do. Pure: the window draws it, and the
//! library client keeps what feeds it.

use std::collections::{BTreeMap, HashMap};

use pimble_plugin_host::abi::{Manifest, Permissions, StoreAsk};
use pimble_plugin_host::library::{self, Entry, Have, Index, Offer, Provenance, Runs};

/// A plugin installed here, as loading left it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Installed {
    pub(super) id: String,
    /// What a person calls it ("Kanban"); its file's name when it could not say.
    pub(super) name: String,
    /// Its version, when it could say.
    pub(super) version: Option<String>,
    /// The plugin ABI it was built for, when it could say.
    pub(super) abi: Option<u32>,
    /// Why this Pimble does not run it; `None` when it runs.
    pub(super) refusal: Option<String>,
    /// The viewers it provides ("Kanban"), when it runs.
    pub(super) viewers: Vec<String>,
    /// What it asks for, when it runs.
    pub(super) asks: Option<Permissions>,
    /// Its file (the browser's: the name it is kept under).
    pub(super) file: String,
}

impl Installed {
    pub(super) fn running(manifest: &Manifest, file: String) -> Installed {
        Installed {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: Some(manifest.version.clone()),
            abi: Some(manifest.abi),
            refusal: None,
            viewers: manifest.viewers().map(|v| v.name.clone()).collect(),
            asks: Some(manifest.asks),
            file,
        }
    }

    pub(super) fn refused(refused: &pimble_plugin_host::registry::Refused) -> Installed {
        let head = refused.head.as_ref();
        Installed {
            id: refused.id(),
            name: head.map(|h| h.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| refused.file()),
            version: head.map(|h| h.version.clone()),
            abi: head.map(|h| h.abi),
            refusal: Some(refused.sentence.clone()),
            viewers: Vec::new(),
            asks: None,
            file: refused.file(),
        }
    }

    /// Whether this Pimble runs it, as the library judges it.
    pub(super) fn runs(&self) -> Runs {
        Runs::judged(self.refusal.is_some(), self.abi)
    }

    fn is_running(&self) -> bool {
        self.refusal.is_none()
    }
}

/// What a row's buttons do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Install,
    Update,
    /// Replace a file's plugin with the library's version.
    UseLibrary,
    TurnOn,
    TurnOff,
    Remove,
}

impl Action {
    pub(super) fn label(self) -> &'static str {
        match self {
            Action::Install => "Install",
            Action::Update => "Update",
            Action::UseLibrary => "Use Library Version",
            Action::TurnOn => "Turn On",
            Action::TurnOff => "Turn Off",
            Action::Remove => "Remove",
        }
    }
}

/// One plugin in the list.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Row {
    pub(super) id: String,
    /// "Kanban 0.2.0": the version installed, else the library's.
    pub(super) title: String,
    /// The library's summary, else what it provides.
    pub(super) summary: String,
    /// Its state in a few words ("Installed", "Updating to 0.2.0...", why it
    /// does not run).
    pub(super) state: String,
    /// The row's one button, when its state has one.
    pub(super) button: Option<Action>,
    /// What Details shows, line by line (what it asks, where it came from, its
    /// file), and the actions under them.
    pub(super) details: Vec<String>,
    pub(super) more: Vec<Action>,
    pub(super) installed: bool,
    /// A link asked about it.
    pub(super) asked: bool,
}

/// Everything the list is made from.
pub(super) struct Inputs<'a> {
    pub(super) installed: &'a [Installed],
    pub(super) index: Option<&'a Index>,
    pub(super) provenance: &'a Provenance,
    /// What a row says now instead of its state (downloading, what came of an
    /// install, an update made on its own), by id.
    pub(super) lines: &'a HashMap<String, String>,
    pub(super) asked: Option<&'a str>,
}

/// What a plugin asks for, as a row says it: "asks to read the stores you
/// choose".
pub(super) fn asks_sentence(asks: &Permissions) -> &'static str {
    match (asks.stores, asks.write) {
        (StoreAsk::None, _) => "asks for nothing",
        (StoreAsk::All, true) => "asks to read and change all your stores",
        (StoreAsk::All, false) => "asks to read all your stores",
        (StoreAsk::Chosen, true) => "asks to read and change the stores you choose",
        (StoreAsk::Chosen, false) => "asks to read the stores you choose",
    }
}

fn capitalized(s: &str) -> String {
    let mut out = s.to_string();
    if let Some(first) = out.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    out
}

/// The list: installed plugins first, then the library's others, each group
/// by name.
pub(super) fn rows(inputs: &Inputs<'_>) -> Vec<Row> {
    let mut by_id: BTreeMap<&str, (Option<&Installed>, Option<&Entry>)> = BTreeMap::new();
    for have in inputs.installed {
        by_id.entry(have.id.as_str()).or_default().0 = Some(have);
    }
    for entry in inputs.index.map(|i| i.plugins.as_slice()).unwrap_or_default() {
        by_id.entry(entry.id.as_str()).or_default().1 = Some(entry);
    }
    let mut rows: Vec<Row> = by_id.into_values().map(|(have, entry)| row(inputs, have, entry)).collect();
    rows.sort_by(|a, b| b.installed.cmp(&a.installed).then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase())));
    rows
}

fn row(inputs: &Inputs<'_>, have: Option<&Installed>, entry: Option<&Entry>) -> Row {
    let id = have.map(|h| h.id.clone()).or_else(|| entry.map(|e| e.id.clone())).unwrap_or_default();
    let from_library = inputs.provenance.from_library(&id);
    let off = inputs.provenance.is_off(&id);
    let asked = inputs.asked == Some(id.as_str());
    let name = have.map(|h| h.name.clone()).or_else(|| entry.map(|e| e.name.clone())).unwrap_or_else(|| id.clone());
    let version = have.and_then(|h| h.version.clone()).or_else(|| entry.map(|e| e.version.clone())).unwrap_or_default();
    let title = if version.is_empty() { name.clone() } else { format!("{name} {version}") };
    let viewers = have.map(|h| h.viewers.join(", ")).unwrap_or_default();
    let summary = match entry {
        Some(e) if !e.summary.is_empty() => e.summary.clone(),
        _ if !viewers.is_empty() => format!("The {viewers} viewer."),
        _ => String::new(),
    };
    let offer = entry.map(|e| {
        let have = have.map(|h| Have { version: h.version.as_deref().unwrap_or_default(), from_library, runs: h.runs() });
        library::offer(e, have)
    });

    let (mut state, button) = match (have, &offer) {
        (None, Some(Offer::NeedsNewer)) => ("Needs a newer Pimble".to_string(), None),
        (None, _) => (entry.map(|e| capitalized(asks_sentence(&e.asks)) + ".").unwrap_or_default(), Some(Action::Install)),
        (Some(h), Some(Offer::Update { .. })) if !h.is_running() => (format!("{} The library's version replaces it.", h.refusal.clone().unwrap_or_default()), Some(Action::Update)),
        (Some(h), Some(Offer::Replace { .. })) => (h.refusal.clone().unwrap_or_default(), Some(Action::UseLibrary)),
        (Some(h), _) if !h.is_running() => (h.refusal.clone().unwrap_or_default(), None),
        (Some(_), _) if off => ("Turned off".to_string(), Some(Action::TurnOn)),
        (Some(_), Some(Offer::Update { .. })) => (format!("Version {} is in the library.", entry.map(|e| e.version.as_str()).unwrap_or_default()), Some(Action::Update)),
        (Some(_), Some(Offer::FromFile)) => ("From a file".to_string(), Some(Action::UseLibrary)),
        (Some(_), None) if !from_library => ("From a file".to_string(), None),
        (Some(_), _) => ("Installed".to_string(), None),
    };
    if asked {
        state = match (have, &offer) {
            (None, Some(Offer::Install)) => format!("Press Install to add {name}."),
            (None, _) => format!("{name} needs a newer Pimble. Update Pimble first."),
            (Some(_), Some(Offer::Update { .. })) => format!("Press Update to get {}.", entry.map(|e| e.version.as_str()).unwrap_or_default()),
            (Some(_), Some(Offer::Replace { .. })) => format!("Press Use Library Version to get {}.", entry.map(|e| e.version.as_str()).unwrap_or_default()),
            (Some(_), _) => format!("{name} is installed already."),
        };
    }
    if let Some(line) = inputs.lines.get(&id).filter(|l| !l.is_empty()) {
        state = line.clone();
    }

    let mut details = Vec::new();
    if let Some(asks) = have.and_then(|h| h.asks).or_else(|| entry.map(|e| e.asks)) {
        details.push(format!("{}.", capitalized(asks_sentence(&asks))));
    }
    if let Some(h) = have {
        let came = if from_library { "From the library" } else { "From a file" };
        details.push(format!("{came}: {}.", h.file));
        if let (Some(e), Some(v)) = (entry, h.version.as_deref()) {
            if e.version != v {
                details.push(format!("The library has {}.", e.version));
            }
        }
    }
    let mut more = Vec::new();
    if let Some(h) = have {
        if h.is_running() {
            more.push(if off { Action::TurnOn } else { Action::TurnOff });
        }
        more.push(Action::Remove);
    }
    more.retain(|a| Some(*a) != button);

    Row { id, title, summary, state, button, details, more, installed: have.is_some(), asked }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_plugin_host::abi::ABI_VERSION;
    use pimble_plugin_host::registry::Refused;

    fn entry(id: &str, name: &str, version: &str, abi: u32) -> Entry {
        Entry {
            id: id.into(),
            name: name.into(),
            version: version.into(),
            abi,
            author: "Pimble".into(),
            summary: format!("{name} from the library."),
            asks: Permissions { stores: StoreAsk::Chosen, write: true, ..Default::default() },
            wasm: format!("{id}-{version}.wasm"),
            sig: format!("{id}-{version}.wasm.sig"),
            shot: None,
            bytes: 1,
            sha256: String::new(),
        }
    }

    fn running(id: &str, name: &str, version: &str) -> Installed {
        Installed {
            id: id.into(),
            name: name.into(),
            version: Some(version.into()),
            abi: Some(ABI_VERSION),
            refusal: None,
            viewers: vec![name.into()],
            asks: Some(Permissions::default()),
            file: format!("{id}.wasm"),
        }
    }

    fn list(installed: &[Installed], index: &Index, provenance: &Provenance, lines: &HashMap<String, String>, asked: Option<&str>) -> Vec<Row> {
        rows(&Inputs { installed, index: Some(index), provenance, lines, asked })
    }

    fn by_id<'a>(rows: &'a [Row], id: &str) -> &'a Row {
        rows.iter().find(|r| r.id == id).unwrap_or_else(|| panic!("no row for {id}"))
    }

    #[test]
    fn each_plugin_is_listed_once_installed_first() {
        let index = Index {
            v: 1,
            plugins: vec![
                entry("kanban-viewer", "Kanban", "0.2.0", ABI_VERSION),
                entry("todo-viewer", "Todo list", "0.1.2", ABI_VERSION),
                entry("timeline-viewer", "Timeline", "0.1.2", ABI_VERSION),
                entry("future-viewer", "Future", "1.0.0", ABI_VERSION + 1),
            ],
        };
        let mut provenance = Provenance::default();
        provenance.plugins.insert("todo-viewer".into(), "0.1.2".into());
        let installed = vec![running("todo-viewer", "Todo list", "0.1.2"), running("kanban-viewer", "Kanban", "0.1.2"), running("hello-viewer", "Hello", "0.1.0")];
        let rows = list(&installed, &index, &provenance, &HashMap::new(), None);
        let order: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(order, vec!["hello-viewer", "kanban-viewer", "todo-viewer", "future-viewer", "timeline-viewer"]);

        let hello = by_id(&rows, "hello-viewer");
        assert_eq!((hello.state.as_str(), hello.button), ("From a file", None));
        assert_eq!(hello.summary, "The Hello viewer.");
        let kanban = by_id(&rows, "kanban-viewer");
        assert_eq!((kanban.title.as_str(), kanban.button), ("Kanban 0.1.2", Some(Action::Update)), "a file install follows the library");
        assert!(kanban.details.contains(&"The library has 0.2.0.".to_string()));
        let todo = by_id(&rows, "todo-viewer");
        assert_eq!((todo.state.as_str(), todo.button), ("Installed", None));
        assert_eq!(todo.more, vec![Action::TurnOff, Action::Remove]);
        assert!(todo.details.contains(&"From the library: todo-viewer.wasm.".to_string()));
        let timeline = by_id(&rows, "timeline-viewer");
        assert_eq!((timeline.state.as_str(), timeline.button, timeline.installed), ("Asks to read and change the stores you choose.", Some(Action::Install), false));
        assert!(timeline.more.is_empty());
        let future = by_id(&rows, "future-viewer");
        assert_eq!((future.state.as_str(), future.button), ("Needs a newer Pimble", None));
    }

    #[test]
    fn a_plugin_that_does_not_run_says_why_and_the_library_replaces_it() {
        let head = pimble_plugin_host::abi::ManifestHead { id: "kanban-viewer".into(), name: "Kanban".into(), version: "0.1.2".into(), abi: ABI_VERSION - 1 };
        let refused = Refused { path: "kanban_viewer.wasm".into(), viewers: Vec::new(), sentence: "Built for ABI 6.".into(), head: Some(head) };
        let junk = Refused { path: "junk.wasm".into(), viewers: Vec::new(), sentence: "Not a module.".into(), head: None };
        let installed = vec![Installed::refused(&refused), Installed::refused(&junk)];
        let index = Index { v: 1, plugins: vec![entry("kanban-viewer", "Kanban", "0.1.2", ABI_VERSION)] };
        let rows = list(&installed, &index, &Provenance::default(), &HashMap::new(), None);
        let kanban = by_id(&rows, "kanban-viewer");
        assert_eq!(kanban.button, Some(Action::Update));
        assert_eq!(kanban.state, "Built for ABI 6. The library's version replaces it.");
        assert_eq!(kanban.more, vec![Action::Remove], "it cannot be turned off: it does not run");
        let junk = by_id(&rows, "junk");
        assert_eq!((junk.title.as_str(), junk.state.as_str(), junk.button), ("junk.wasm", "Not a module.", None));
        assert_eq!(junk.more, vec![Action::Remove]);
    }

    #[test]
    fn a_build_ahead_of_this_pimble_or_one_refused_otherwise_is_only_offered_the_library() {
        let ahead_head = pimble_plugin_host::abi::ManifestHead { id: "kanban-viewer".into(), name: "Kanban".into(), version: "0.3.0".into(), abi: ABI_VERSION + 1 };
        let ahead = Refused { path: "kanban-viewer.wasm".into(), viewers: Vec::new(), sentence: "Built for a later ABI.".into(), head: Some(ahead_head) };
        // It ran past its time describing itself: nothing of it was read.
        let stuck = Refused { path: "todo-viewer.wasm".into(), viewers: Vec::new(), sentence: "It ran past its time.".into(), head: None };
        let installed = vec![Installed::refused(&ahead), Installed::refused(&stuck)];
        let index = Index { v: 1, plugins: vec![entry("kanban-viewer", "Kanban", "0.2.0", ABI_VERSION), entry("todo-viewer", "Todo list", "0.1.2", ABI_VERSION)] };
        let rows = list(&installed, &index, &Provenance::default(), &HashMap::new(), None);
        for id in ["kanban-viewer", "todo-viewer"] {
            let row = by_id(&rows, id);
            assert_eq!(row.button, Some(Action::UseLibrary), "{id}");
            assert_eq!(row.more, vec![Action::Remove], "{id}");
        }
        assert_eq!(by_id(&rows, "kanban-viewer").state, "Built for a later ABI.");
        assert_eq!(by_id(&rows, "todo-viewer").state, "It ran past its time.");
    }

    #[test]
    fn turned_off_a_development_build_a_link_and_what_happened_say_so() {
        let index = Index { v: 1, plugins: vec![entry("kanban-viewer", "Kanban", "0.2.0", ABI_VERSION), entry("todo-viewer", "Todo list", "0.1.2", ABI_VERSION)] };
        let mut provenance = Provenance::default();
        provenance.plugins.insert("todo-viewer".into(), "0.1.2".into());
        provenance.off.insert("todo-viewer".into());
        let installed = vec![running("kanban-viewer", "Kanban", "0.2.0"), running("todo-viewer", "Todo list", "0.1.2")];
        let rows = list(&installed, &index, &provenance, &HashMap::new(), None);
        let todo = by_id(&rows, "todo-viewer");
        assert_eq!((todo.state.as_str(), todo.button), ("Turned off", Some(Action::TurnOn)));
        assert_eq!(todo.more, vec![Action::Remove]);
        let kanban = by_id(&rows, "kanban-viewer");
        assert_eq!((kanban.state.as_str(), kanban.button), ("From a file", Some(Action::UseLibrary)));

        let lines = HashMap::from([("kanban-viewer".to_string(), "Updating to 0.2.1...".to_string())]);
        let rows = list(&installed, &index, &provenance, &lines, Some("todo-viewer"));
        assert_eq!(by_id(&rows, "kanban-viewer").state, "Updating to 0.2.1...");
        let todo = by_id(&rows, "todo-viewer");
        assert!(todo.asked);
        assert_eq!(todo.state, "Todo list is installed already.");
    }

    #[test]
    fn without_the_library_the_installed_plugins_are_the_list() {
        let installed = vec![running("kanban-viewer", "Kanban", "0.2.0")];
        let rows = rows(&Inputs { installed: &installed, index: None, provenance: &Provenance::default(), lines: &HashMap::new(), asked: None });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, "From a file");
    }
}

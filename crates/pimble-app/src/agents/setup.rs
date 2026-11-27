//! Setting an agent up in the apps Pimble knows, and taking it out again
//! (docs/AGENTS_CONTRACT.md section 5, "Setting up the app").
//!
//! **Claude is one choice, written to two files.** The Claude app's Chat tab
//! reads `claude_desktop_config.json`; the terminal `claude` reads
//! `~/.claude.json`; the app's Code tab reads both, and loads an entry once
//! when both files name it alike (tested 2026-11-24). So "Add to Claude"
//! writes one entry, the same name, command and key, into each file whose
//! Claude is installed, and takes every other Pimble entry out of both:
//! Claude keeps exactly one Pimble agent, and no tool is loaded twice.
//!
//! **The Claude app installed as a Windows package** (MSIX) does not read
//! `%APPDATA%\Claude`: Windows hands it a private copy of that folder under
//! `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\Claude`
//! once that copy exists, and the real folder until then (2026-11-26: a
//! tester's Claude never started Pimble's MCP server). So when the package
//! is there Pimble writes the same entry to both, and the app sees it either
//! way.
//!
//! What an "Add" or a removal did is logged under `pimble_app::agents`, one
//! of the targets diagnostics send at `info` (docs/DIAGNOSTICS_CONTRACT.md
//! section 2): files by path, agents by id, never their names or keys.
//!
//! An entry's key names its agent (`pimble_rpc::agent_id_of_key`), which is
//! how Pimble finds the entries of an agent being removed and the agents an
//! "Add" replaced.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use uuid::Uuid;

use super::client_config::{self, Change, Entry, Format, NewEntry};
use super::ClientApp;

/// A config file Pimble knows how to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConfigFile {
    /// The Claude app's: its Chat tab, and its Code tab too.
    ClaudeApp,
    /// The Claude app's when it is installed as a Windows package: its
    /// private copy of `ClaudeApp`'s folder.
    ClaudeAppPackaged,
    /// `~/.claude.json`, user scope: `claude` in a terminal, and the Claude
    /// app's Code tab.
    ClaudeCli,
    /// `config.toml` of the ChatGPT desktop app and Codex.
    Codex,
    Cursor,
}

impl ConfigFile {
    pub const ALL: [ConfigFile; 5] = [ConfigFile::ClaudeApp, ConfigFile::ClaudeAppPackaged, ConfigFile::ClaudeCli, ConfigFile::Codex, ConfigFile::Cursor];

    /// The files an app's setup writes.
    pub fn of(app: ClientApp) -> &'static [ConfigFile] {
        match app {
            ClientApp::Claude => &[ConfigFile::ClaudeApp, ConfigFile::ClaudeAppPackaged, ConfigFile::ClaudeCli],
            ClientApp::ChatGpt => &[ConfigFile::Codex],
            ClientApp::Cursor => &[ConfigFile::Cursor],
            ClientApp::Other => &[],
        }
    }

    pub fn app(self) -> ClientApp {
        match self {
            ConfigFile::ClaudeApp | ConfigFile::ClaudeAppPackaged | ConfigFile::ClaudeCli => ClientApp::Claude,
            ConfigFile::Codex => ClientApp::ChatGpt,
            ConfigFile::Cursor => ClientApp::Cursor,
        }
    }

    pub fn format(self) -> Format {
        if self == ConfigFile::Codex { Format::Toml } else { Format::Json }
    }

    /// `claude mcp add` writes `"type": "stdio"`; the others write none.
    fn stdio_type(self) -> bool {
        self == ConfigFile::ClaudeCli
    }
}

/// Where this computer's apps keep their config: the folders the paths hang
/// from, so a test can put them anywhere.
#[derive(Clone, Debug)]
pub struct Locations {
    pub home: PathBuf,
    /// `~/.config`, `%APPDATA%` or `~/Library/Application Support`.
    pub config: PathBuf,
    /// `CODEX_HOME`, else `~/.codex`.
    pub codex: PathBuf,
    /// The Claude app's Windows package folder, when it is installed as one
    /// (`%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc`, found by
    /// [`claude_package_in`]); never elsewhere.
    pub claude_package: Option<PathBuf>,
    /// Where the Claude app's program is when it is installed the ordinary
    /// way: any of these there means the app is installed, though it may
    /// never have run and made its config folder.
    pub claude_programs: Vec<PathBuf>,
}

/// The Claude app's Windows package family name: its package folder's name,
/// the same on every computer (the part after `_` is derived from the
/// publisher).
pub const CLAUDE_PACKAGE: &str = "Claude_pzs8sxrjxfjjc";

/// The Claude app's package folder in `packages` (`%LOCALAPPDATA%\Packages`):
/// [`CLAUDE_PACKAGE`], else any folder named `Claude_<publisher id>`
/// (ignoring case), so a package signed anew still counts. The first by name when
/// several are there.
pub fn claude_package_in(packages: &std::path::Path) -> Option<PathBuf> {
    let known = packages.join(CLAUDE_PACKAGE);
    if known.is_dir() {
        return Some(known);
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(packages)
        .ok()?
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            name.strip_prefix("claude_").is_some_and(is_publisher_id)
        })
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Whether `id` has the shape of a Windows package's publisher id: 13
/// characters of Crockford base32 (no i, l, o or u).
fn is_publisher_id(id: &str) -> bool {
    id.len() == 13 && id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='z') && !matches!(c, 'i' | 'l' | 'o' | 'u'))
}

impl Locations {
    /// This computer's, when it has a home folder.
    #[cfg(feature = "native")]
    pub fn here() -> Option<Locations> {
        let home = dirs::home_dir()?;
        let config = dirs::config_dir()?;
        // `CODEX_HOME` moves it, as Codex itself honours.
        let codex = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
        let local = dirs::data_local_dir();
        let (claude_package, claude_programs) = if cfg!(windows) {
            // The package (Microsoft Store, or the MSIX installer), or the
            // older installer's per-user folder.
            let package = local.as_ref().and_then(|l| claude_package_in(&l.join("Packages")));
            (package, local.iter().map(|l| l.join("AnthropicClaude")).collect())
        } else if cfg!(target_os = "macos") {
            (None, vec![PathBuf::from("/Applications/Claude.app"), home.join("Applications").join("Claude.app")])
        } else {
            (None, Vec::new())
        };
        Some(Locations { home, config, codex, claude_package, claude_programs })
    }

    /// Where `file` is, or `None` when this computer has no such file (the
    /// Windows package's, when the package is not installed).
    pub fn path(&self, file: ConfigFile) -> Option<PathBuf> {
        Some(match file {
            ConfigFile::ClaudeApp => self.config.join("Claude").join("claude_desktop_config.json"),
            ConfigFile::ClaudeAppPackaged => {
                self.claude_package.as_ref()?.join("LocalCache").join("Roaming").join("Claude").join("claude_desktop_config.json")
            }
            ConfigFile::ClaudeCli => self.home.join(".claude.json"),
            ConfigFile::Codex => self.codex.join("config.toml"),
            ConfigFile::Cursor => self.home.join(".cursor").join("mcp.json"),
        })
    }

    /// The files this computer can have, with where each is.
    pub fn files(&self) -> impl Iterator<Item = (ConfigFile, PathBuf)> + '_ {
        ConfigFile::ALL.into_iter().filter_map(|f| self.path(f).map(|p| (f, p)))
    }

    /// Whether the app that reads `file` looks installed. Only Claude's files
    /// are judged so; for the others, choosing the app is asking for its
    /// file. The Claude app is installed when its config folder, its program
    /// or its Windows package is there; a packaged app also gets the plain
    /// file, which is the one its "Edit Config" opens and the one it reads
    /// until it has made its own copy.
    pub fn installed(&self, file: ConfigFile) -> bool {
        let packaged = self.claude_package.as_ref().is_some_and(|d| d.is_dir());
        match file {
            ConfigFile::ClaudeApp => self.config.join("Claude").is_dir() || packaged || self.claude_programs.iter().any(|p| p.exists()),
            ConfigFile::ClaudeAppPackaged => packaged,
            ConfigFile::ClaudeCli => self.home.join(".claude.json").exists() || self.home.join(".claude").is_dir(),
            ConfigFile::Codex | ConfigFile::Cursor => true,
        }
    }
}

/// What adding an agent to an app did.
#[derive(Debug, Default)]
pub struct Added {
    /// The files written.
    pub written: Vec<ConfigFile>,
    /// A sentence per file that could not be written.
    pub failed: Vec<String>,
    /// The entries it took out that were not the added agent's own, by name,
    /// with the agent each one's key names (none for an entry without one).
    pub dropped: Vec<(String, Option<Uuid>)>,
}

impl Added {
    /// The agents, other than the one added, whose entries it replaced.
    pub fn replaced(&self) -> BTreeSet<Uuid> {
        self.dropped.iter().filter_map(|(_, id)| *id).collect()
    }
}

/// Add agent `agent` (its entry `entry`) to `app`. For Claude, every Pimble
/// entry already in Claude's files comes out; for the others, an entry of
/// the same name is replaced.
pub fn add(at: &Locations, app: ClientApp, entry: NewEntry, agent: Uuid) -> Added {
    let mut added = Added::default();
    let files: Vec<(ConfigFile, PathBuf)> =
        ConfigFile::of(app).iter().filter(|f| at.installed(**f)).filter_map(|f| at.path(*f).map(|p| (*f, p))).collect();
    if files.is_empty() && app == ClientApp::Claude {
        added.failed.push("Pimble found no Claude on this computer".into());
    }
    let one_agent = |_: &str, e: &Entry| e.runs_pimble();
    let same_name = |name: &str, _: &Entry| name == entry.name;
    let drop: &dyn Fn(&str, &Entry) -> bool = if app == ClientApp::Claude { &one_agent } else { &same_name };
    for (file, path) in files {
        if file == ConfigFile::ClaudeAppPackaged {
            if let Err(e) = seed_packaged_copy(at, &path) {
                added.failed.push(e);
                continue;
            }
        }
        let change = Change { drop, add: Some(NewEntry { stdio_type: file.stdio_type(), ..entry }) };
        match client_config::apply(&path, file.format(), &change) {
            Ok(changed) => {
                added.written.push(file);
                for (name, e) in changed.dropped {
                    let id = e.agent();
                    if id != Some(agent) && !added.dropped.iter().any(|(n, i)| *n == name && *i == id) {
                        added.dropped.push((name, id));
                    }
                }
            }
            Err(e) => added.failed.push(e),
        }
    }
    added
}

/// Before the packaged app's own copy of its config is first written, make it
/// a copy of the plain file: once the copy exists the app reads it instead,
/// so a copy holding only Pimble's entry would hide every other server and
/// setting the person has. The plain file is already changed by this "Add"
/// (it comes first), so the change applied to the copy then rewrites nothing.
fn seed_packaged_copy(at: &Locations, packaged: &std::path::Path) -> Result<(), String> {
    let Some(plain) = at.path(ConfigFile::ClaudeApp) else { return Ok(()) };
    if packaged.exists() || !plain.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&plain).map_err(|e| format!("Could not read {}: {e}", plain.display()))?;
    if let Some(dir) = packaged.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not make {}: {e}", dir.display()))?;
    }
    std::fs::write(packaged, text).map_err(|e| format!("Could not write {}: {e}", packaged.display()))
}

/// Log what adding agent `agent` to `app` did: each file written, each that
/// failed, the agents it replaced and, when it wrote nothing, every place it
/// looked.
pub fn log_added(at: &Locations, app: ClientApp, agent: Uuid, added: &Added, settled: &Settled) {
    for line in added_log(at, app, agent, added, settled) {
        match line {
            Logged::Info(l) => tracing::info!("{l}"),
            Logged::Warn(l) => tracing::warn!("{l}"),
        }
    }
}

/// A line of [`log_added`], at its level.
#[derive(Debug, PartialEq)]
pub enum Logged {
    Info(String),
    Warn(String),
}

/// The lines [`log_added`] writes. Paths are written with `display`, never
/// `{:?}`: a Windows path in its debug form doubles every backslash.
pub fn added_log(at: &Locations, app: ClientApp, agent: Uuid, added: &Added, settled: &Settled) -> Vec<Logged> {
    let shown = |f: ConfigFile| at.path(f).map(|p| p.display().to_string()).unwrap_or_else(|| format!("{f:?}"));
    let mut out = Vec::new();
    for f in &added.written {
        out.push(Logged::Info(format!("Agent {agent} added to {}: wrote {}", app.name(), shown(*f))));
    }
    for e in &added.failed {
        out.push(Logged::Warn(format!("Agent {agent} not added to {}: {e}", app.name())));
    }
    if added.written.is_empty() {
        let looked: Vec<String> = ConfigFile::of(app).iter().filter_map(|f| at.path(*f)).map(|p| p.display().to_string()).collect();
        out.push(Logged::Warn(format!("Agent {agent}: nothing written for {}; looked at {}", app.name(), looked.join(", "))));
    }
    for id in &settled.remove {
        out.push(Logged::Info(format!("Agent {agent} replaced agent {id}, removed from Pimble")));
    }
    if !settled.kept.is_empty() || !settled.others.is_empty() {
        out.push(Logged::Info(format!(
            "Agent {agent}: {} replaced agents kept, {} entries for no agent here taken out",
            settled.kept.len(),
            settled.others.len()
        )));
    }
    out
}

/// What taking an agent out did: the files changed, and a sentence per file
/// that could not be.
#[derive(Debug, Default)]
pub struct Forgotten {
    pub changed: Vec<ConfigFile>,
    pub failed: Vec<String>,
}

/// Take every entry whose key names `agent` out of every file Pimble knows,
/// and log what that changed.
pub fn forget(at: &Locations, agent: Uuid) -> Forgotten {
    let mut out = Forgotten::default();
    let theirs = |_: &str, e: &Entry| e.agent() == Some(agent);
    for (file, path) in at.files() {
        match client_config::apply(&path, file.format(), &Change { drop: &theirs, add: None }) {
            Ok(changed) if changed.written => {
                tracing::info!("Agent {agent} taken out of {}", path.display());
                out.changed.push(file);
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("Agent {agent} not taken out of {}: {e}", path.display());
                out.failed.push(e);
            }
        }
    }
    out
}

/// Which apps each agent is set up in, read from every file Pimble knows.
#[derive(Debug, Default)]
pub struct Holders {
    pub by_agent: HashMap<Uuid, Vec<ClientApp>>,
    /// Some file could not be read, so any agent may be in it.
    pub unreadable: bool,
}

pub fn holders(at: &Locations) -> Holders {
    let mut out = Holders::default();
    for (file, path) in at.files() {
        let Ok(entries) = client_config::read_entries(&path, file.format()) else {
            out.unreadable = true;
            continue;
        };
        for (_, entry) in entries {
            if let (true, Some(id)) = (entry.runs_pimble(), entry.agent()) {
                let apps = out.by_agent.entry(id).or_default();
                if !apps.contains(&file.app()) {
                    apps.push(file.app());
                }
            }
        }
    }
    out
}

/// What becomes of the agents an "Add" replaced.
#[derive(Debug, Default, PartialEq)]
pub struct Settled {
    /// To remove from Pimble: in no file Pimble knows any more.
    pub remove: Vec<Uuid>,
    /// Their names, to say so.
    pub removed: Vec<String>,
    /// Replaced but kept: still set up in another app, or a file could not
    /// be read or written, so Pimble cannot tell.
    pub kept: Vec<String>,
    /// Pimble entries taken out that name no agent of this computer.
    pub others: Vec<String>,
}

/// Decide what becomes of the agents adding to `app` replaced, given who
/// still holds what and this computer's agents (id and name). Only Claude
/// removes them: it keeps one Pimble agent, so the ones it replaced were
/// Claude's. The other apps replace an entry of the same name and remove
/// nobody, as before. Nothing is removed when a file failed or could not be
/// read, since a replaced agent may still be set up there.
pub fn settle(app: ClientApp, added: &Added, held: &Holders, agents: &[(Uuid, String)]) -> Settled {
    let mut out = Settled::default();
    if app != ClientApp::Claude {
        return out;
    }
    let unsure = held.unreadable || !added.failed.is_empty();
    let mut seen = BTreeSet::new();
    for (entry, id) in &added.dropped {
        let Some((id, name)) = id.and_then(|id| agents.iter().find(|(a, _)| *a == id)) else {
            out.others.push(entry.clone());
            continue;
        };
        if !seen.insert(*id) {
            continue;
        }
        if unsure || held.by_agent.contains_key(id) {
            out.kept.push(name.clone());
        } else {
            out.remove.push(*id);
            out.removed.push(name.clone());
        }
    }
    out
}

/// Apps named in a sentence: "Claude", "Claude and Cursor", "Claude, Cursor
/// and ChatGPT / Codex".
pub fn app_list(apps: &[ClientApp]) -> String {
    let names: Vec<&str> = apps.iter().map(|a| a.name()).collect();
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// Where an agent is set up, in a line under its name.
pub fn placed_line(apps: Option<&Vec<ClientApp>>) -> String {
    match apps {
        Some(apps) if !apps.is_empty() => format!("Set up in {}", app_list(apps)),
        _ => "Not in the settings of any app Pimble knows here".into(),
    }
}

/// What "Add to <app>" did, in sentences: where it went, how to connect it,
/// what it replaced.
pub fn added_sentence(app: ClientApp, added: &Added, settled: &Settled) -> String {
    let mut out = Vec::new();
    let wrote = |f| added.written.contains(&f);
    if app == ClientApp::Claude {
        let claude_app = wrote(ConfigFile::ClaudeApp) || wrote(ConfigFile::ClaudeAppPackaged);
        match (claude_app, wrote(ConfigFile::ClaudeCli)) {
            (true, true) => out.push("Added to Claude: the Chat and Code tabs of the Claude app, and claude in a terminal.".to_string()),
            (true, false) => out.push("Added to the Claude app, for its Chat and Code tabs.".to_string()),
            (false, true) => out.push("Added to Claude Code, for claude in a terminal.".to_string()),
            (false, false) => {}
        }
        if claude_app {
            // The Claude app on Windows keeps running in the tray when its
            // window closes, and reads its config only when it starts
            // (2026-11-13: a closed window looked like a restart).
            out.push(if cfg!(windows) {
                "Quit the Claude app from its tray icon (right-click, Quit: closing its window leaves it running), then open it again to connect it.".to_string()
            } else {
                "Quit the Claude app and open it again to connect it.".to_string()
            });
        }
        if wrote(ConfigFile::ClaudeCli) {
            out.push("A claude session started from now on has it.".to_string());
        }
    } else if !added.written.is_empty() {
        out.push(format!("Added to {}. Quit {} and open it again to connect it.", app.name(), app.name()));
    }
    if !settled.removed.is_empty() {
        out.push(format!("It replaces {}, removed from Pimble.", quoted(&settled.removed)));
    }
    if !settled.kept.is_empty() {
        out.push(format!("{} kept in Pimble: another app, or a file Pimble could not change, may still use {}.", quoted(&settled.kept), if settled.kept.len() == 1 { "it" } else { "them" }));
    }
    if !settled.others.is_empty() {
        out.push(format!(
            "It also took out {}, Pimble's for no agent on this computer (the old file is kept beside it as .pimble-backup).",
            quoted(&settled.others)
        ));
    }
    for e in &added.failed {
        out.push(format!("{e}. Copy the setup below into {} instead.", app.name()));
    }
    out.join(" ")
}

fn quoted(names: &[String]) -> String {
    let q: Vec<String> = names.iter().map(|n| format!("“{n}”")).collect();
    match q.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;

//! Setting an agent up in the apps Pimble knows, and taking it out again
//! (docs/AGENTS_CONTRACT.md section 5, "Setting up the app").
//!
//! **Claude is one choice, written to every file a Claude here reads.** The
//! Claude app's Chat tab reads `claude_desktop_config.json`; the terminal
//! `claude` reads `~/.claude.json`; the app's Code tab reads both, and loads
//! an entry once when both files name it alike (tested 2026-11-24). So "Add
//! to Claude" writes one entry, the same name, command and key, into each
//! file an installed Claude reads, and takes every other Pimble entry out of
//! all of them: Claude keeps exactly one Pimble agent, and no tool is loaded
//! twice. Which Claudes are installed, and where each reads its config (the
//! Windows package reads a private copy of `%APPDATA%\Claude` once it has
//! made one), is [`super::locations`]'.
//!
//! What an "Add" or a removal did is logged under `pimble_app::agents`, one
//! of the targets diagnostics send at `info` (docs/DIAGNOSTICS_CONTRACT.md
//! section 2): the Claudes found, and each file written, skipped or changed
//! with why, by path; agents by id, never their names or keys.
//!
//! An entry's key names its agent (`pimble_rpc::agent_id_of_key`), which is
//! how Pimble finds the entries of an agent being removed and the agents an
//! "Add" replaced.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::client_config::{self, Change, Entry, NewEntry};
use super::ClientApp;
pub use super::locations::{claude_package_in, ConfigFile, Install, InstallKind, Locations, Target, CLAUDE_PACKAGE};

/// What adding an agent to an app did.
#[derive(Debug, Default)]
pub struct Added {
    /// The files written, each with why.
    pub written: Vec<(ConfigFile, &'static str)>,
    /// The files made from another's content first: the packaged Claude's,
    /// from the plain file.
    pub carried: Vec<ConfigFile>,
    /// The app's files left alone, each with why.
    pub skipped: Vec<(ConfigFile, &'static str)>,
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

    /// Whether it wrote `file`.
    pub fn wrote(&self, file: ConfigFile) -> bool {
        self.written.iter().any(|(f, _)| *f == file)
    }
}

/// Add agent `agent` (its entry `entry`) to `app`. For Claude, every Pimble
/// entry already in Claude's files comes out; for the others, an entry of
/// the same name is replaced.
pub fn add(at: &Locations, app: ClientApp, entry: NewEntry, agent: Uuid) -> Added {
    let mut added = Added::default();
    let mut files: Vec<(ConfigFile, PathBuf, &'static str)> = Vec::new();
    for file in ConfigFile::of(app) {
        let Some(path) = at.path(*file) else { continue };
        match at.target(*file) {
            Target::Write(why) => files.push((*file, path, why)),
            Target::Skip(why) => added.skipped.push((*file, why)),
        }
    }
    if files.is_empty() && app == ClientApp::Claude {
        added.failed.push("Pimble found no Claude on this computer".into());
    }
    let one_agent = |_: &str, e: &Entry| e.runs_pimble();
    let same_name = |name: &str, _: &Entry| name == entry.name;
    let drop: &dyn Fn(&str, &Entry) -> bool = if app == ClientApp::Claude { &one_agent } else { &same_name };
    for (file, path, why) in files {
        // The packaged Claude's file, when the app has made its folder and
        // no file in it, starts as the plain file: whatever the person set
        // there stays theirs once the app reads this one instead.
        if file == ConfigFile::ClaudeAppPackaged {
            match carry_over(at, &path) {
                Ok(true) => added.carried.push(file),
                Ok(false) => {}
                Err(e) => {
                    added.failed.push(e);
                    continue;
                }
            }
        }
        let change = Change { drop, add: Some(NewEntry { stdio_type: file.stdio_type(), ..entry }) };
        match client_config::apply(&path, file.format(), &change) {
            Ok(changed) => {
                added.written.push((file, why));
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

/// Make the packaged Claude's missing file at `path` a copy of the plain
/// one, when there is a plain one. Whether it made one.
fn carry_over(at: &Locations, path: &Path) -> Result<bool, String> {
    if path.exists() {
        return Ok(false);
    }
    let Some(plain) = at.path(ConfigFile::ClaudeApp).filter(|p| p.is_file()) else { return Ok(false) };
    std::fs::copy(&plain, path).map(|_| true).map_err(|e| format!("Could not copy {} to {}: {e}", plain.display(), path.display()))
}

/// Log what adding agent `agent` to `app` did: the Claudes found, each file
/// written or skipped and why, each that failed, the agents it replaced
/// and, when it wrote nothing, every place it looked.
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

/// A file by its path, for the log. Paths are written with `display`, never
/// `{:?}`: a Windows path in its debug form doubles every backslash.
fn shown(at: &Locations, f: ConfigFile) -> String {
    at.path(f).map(|p| p.display().to_string()).unwrap_or_else(|| format!("{f:?}"))
}

/// The lines [`log_added`] writes.
pub fn added_log(at: &Locations, app: ClientApp, agent: Uuid, added: &Added, settled: &Settled) -> Vec<Logged> {
    let mut out = Vec::new();
    if app == ClientApp::Claude {
        out.push(Logged::Info(format!("Agent {agent}, adding to Claude: {}", at.installs_line())));
    }
    for (f, why) in &added.written {
        let from = if added.carried.contains(f) { format!(", made from {}", shown(at, ConfigFile::ClaudeApp)) } else { String::new() };
        out.push(Logged::Info(format!("Agent {agent} added to {}: wrote {} ({why}{from})", app.name(), shown(at, *f))));
    }
    for (f, why) in &added.skipped {
        out.push(Logged::Info(format!("Agent {agent}: skipped {} ({why})", shown(at, *f))));
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
/// installed or not, and log what that changed.
pub fn forget(at: &Locations, agent: Uuid) -> Forgotten {
    let out = forget_quietly(at, agent);
    for line in forgotten_log(at, agent, &out) {
        match line {
            Logged::Info(l) => tracing::info!("{l}"),
            Logged::Warn(l) => tracing::warn!("{l}"),
        }
    }
    out
}

fn forget_quietly(at: &Locations, agent: Uuid) -> Forgotten {
    let mut out = Forgotten::default();
    let theirs = |_: &str, e: &Entry| e.agent() == Some(agent);
    for (file, path) in at.files() {
        match client_config::apply(&path, file.format(), &Change { drop: &theirs, add: None }) {
            Ok(changed) if changed.written => out.changed.push(file),
            Ok(_) => {}
            Err(e) => out.failed.push(e),
        }
    }
    out
}

/// The lines [`forget`] writes: the Claudes found, each file changed, each
/// that could not be, and when nothing changed, every place it looked.
pub fn forgotten_log(at: &Locations, agent: Uuid, out: &Forgotten) -> Vec<Logged> {
    let mut lines = vec![Logged::Info(format!("Agent {agent}, taking out: {}", at.installs_line()))];
    for f in &out.changed {
        lines.push(Logged::Info(format!("Agent {agent} taken out of {}", shown(at, *f))));
    }
    for e in &out.failed {
        lines.push(Logged::Warn(format!("Agent {agent} not taken out: {e}")));
    }
    if out.changed.is_empty() && out.failed.is_empty() {
        let looked: Vec<String> = at.files().map(|(_, p)| p.display().to_string()).collect();
        lines.push(Logged::Info(format!("Agent {agent} was in none of {}", looked.join(", "))));
    }
    lines
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
    let wrote = |f| added.wrote(f);
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

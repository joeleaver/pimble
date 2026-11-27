//! Where this computer's AI apps are installed, and which config file each
//! one reads (docs/AGENTS_CONTRACT.md section 6, "Where each Claude reads its
//! config").
//!
//! **Claude can be installed several ways at once**, and each reads
//! `claude_desktop_config.json` from its own place:
//!
//! - The Claude app as a **Windows package** (MSIX: the Microsoft Store, and
//!   the installer from claude.ai/download today), folder
//!   `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc`. Windows runs it with its
//!   `AppData` virtualized: a file or folder it *creates* under `%APPDATA%`
//!   lands in the package's private `LocalCache\Roaming`, a file that
//!   already exists in the real `%APPDATA%` is changed in place, and a file
//!   it opens is looked for in the private copy first and the real folder
//!   after. So a packaged Claude that first ran with no `%APPDATA%\Claude`
//!   made its folder privately, `LocalCache\Roaming\Claude`, and keeps its
//!   config there; one that found `%APPDATA%\Claude` already there uses that.
//!   Pimble writes the private file whenever that private folder exists,
//!   making the file when the app has not (a tester's Claude, 2026-11-27, had
//!   the folder and no file, and never read the plain one Pimble wrote), from
//!   the plain file's content so nothing the person set there is lost; and
//!   it always writes the plain file too, which the app reads while it has
//!   no folder of its own (and which older versions' "Edit Config" opened).
//!   Once Pimble has made the private file, the app reads that and no longer
//!   the plain one: a hand edit of the plain file stops reaching it, and only
//!   Pimble's own entry is kept alike in both.
//! - The Claude app from the **older installer** (Squirrel, program in
//!   `%LOCALAPPDATA%\AnthropicClaude`), not virtualized: `%APPDATA%\Claude`.
//! - The **macOS** app (`Claude.app`): `~/Library/Application Support/Claude`.
//! - A Claude app Pimble knows no program of (**Linux** builds, or one
//!   installed somewhere else) that made its config folder: that folder.
//! - **Claude Code** (`claude` in a terminal, and the app's Code tab):
//!   `~/.claude.json`, when it or `~/.claude/` is there.

use std::path::{Path, PathBuf};

use super::client_config::Format;
use super::ClientApp;

/// A config file Pimble knows how to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConfigFile {
    /// The Claude app's plain `claude_desktop_config.json`: its Chat tab,
    /// and its Code tab too.
    ClaudeApp,
    /// The Claude app's when it is installed as a Windows package: the file
    /// in the package's private copy of `ClaudeApp`'s folder.
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
    pub(super) fn stdio_type(self) -> bool {
        self == ConfigFile::ClaudeCli
    }
}

/// A way Claude is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// The Claude app as a Windows package (MSIX).
    Packaged,
    /// The Claude app from the older Windows installer.
    Installer,
    /// The macOS app.
    MacApp,
    /// A Claude app Pimble knows no program of (Linux, or installed
    /// elsewhere), found by the config folder it made.
    ConfigFolder,
    /// Claude Code.
    Cli,
}

impl InstallKind {
    fn label(self) -> &'static str {
        match self {
            InstallKind::Packaged => "the Claude app as a Windows package",
            InstallKind::Installer => "the Claude app from its installer",
            InstallKind::MacApp => "the Claude app",
            InstallKind::ConfigFolder => "a Claude app, by its config folder",
            InstallKind::Cli => "Claude Code",
        }
    }
}

/// A Claude found on this computer: how it is installed, where (its package
/// folder, program, config folder or file), and the config file it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    pub kind: InstallKind,
    pub at: PathBuf,
    pub reads: ConfigFile,
}

/// Whether "Add to Claude" writes a file, and why, in words for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Write(&'static str),
    Skip(&'static str),
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
    /// way, with how: any of these there means the app is installed, though
    /// it may never have run and made its config folder.
    pub claude_programs: Vec<(InstallKind, PathBuf)>,
}

/// The Claude app's Windows package family name: its package folder's name,
/// the same on every computer (the part after `_` is derived from the
/// publisher).
pub const CLAUDE_PACKAGE: &str = "Claude_pzs8sxrjxfjjc";

/// The Claude app's package folder in `packages` (`%LOCALAPPDATA%\Packages`):
/// [`CLAUDE_PACKAGE`], else any folder named `Claude_<publisher id>`
/// (ignoring case), so a package signed anew still counts. The first by name
/// when several are there.
pub fn claude_package_in(packages: &Path) -> Option<PathBuf> {
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
            let package = local.as_ref().and_then(|l| claude_package_in(&l.join("Packages")));
            (package, local.iter().map(|l| (InstallKind::Installer, l.join("AnthropicClaude"))).collect())
        } else if cfg!(target_os = "macos") {
            let apps = [PathBuf::from("/Applications/Claude.app"), home.join("Applications").join("Claude.app")];
            (None, apps.into_iter().map(|a| (InstallKind::MacApp, a)).collect())
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
            ConfigFile::ClaudeAppPackaged => self.packaged_folder()?.join("claude_desktop_config.json"),
            ConfigFile::ClaudeCli => self.home.join(".claude.json"),
            ConfigFile::Codex => self.codex.join("config.toml"),
            ConfigFile::Cursor => self.home.join(".cursor").join("mcp.json"),
        })
    }

    /// The files this computer can have, with where each is.
    pub fn files(&self) -> impl Iterator<Item = (ConfigFile, PathBuf)> + '_ {
        ConfigFile::ALL.into_iter().filter_map(|f| self.path(f).map(|p| (f, p)))
    }

    /// The package's private copy of the Claude app's config folder.
    fn packaged_folder(&self) -> Option<PathBuf> {
        Some(self.claude_package.as_ref()?.join("LocalCache").join("Roaming").join("Claude"))
    }

    /// Whether the packaged Claude has made its config folder privately, so
    /// it reads its config from there.
    pub fn packaged_folder_made(&self) -> bool {
        self.claude_package.as_ref().is_some_and(|p| p.is_dir()) && self.packaged_folder().is_some_and(|f| f.is_dir())
    }

    /// Every Claude installed here, with the file each reads.
    pub fn claude_installs(&self) -> Vec<Install> {
        let mut out = Vec::new();
        if let Some(package) = self.claude_package.as_ref().filter(|p| p.is_dir()) {
            let reads = if self.packaged_folder_made() { ConfigFile::ClaudeAppPackaged } else { ConfigFile::ClaudeApp };
            out.push(Install { kind: InstallKind::Packaged, at: package.clone(), reads });
        }
        for (kind, program) in self.claude_programs.iter().filter(|(_, p)| p.exists()) {
            out.push(Install { kind: *kind, at: program.clone(), reads: ConfigFile::ClaudeApp });
        }
        let folder = self.config.join("Claude");
        if out.is_empty() && folder.is_dir() {
            out.push(Install { kind: InstallKind::ConfigFolder, at: folder, reads: ConfigFile::ClaudeApp });
        }
        if self.home.join(".claude.json").exists() || self.home.join(".claude").is_dir() {
            out.push(Install { kind: InstallKind::Cli, at: self.home.join(".claude.json"), reads: ConfigFile::ClaudeCli });
        }
        out
    }

    /// Whether "Add" writes `file`, and why. Only Claude's files are judged;
    /// for the others, choosing the app is asking for its file.
    pub fn target(&self, file: ConfigFile) -> Target {
        let installs = self.claude_installs();
        let read_by = |f: ConfigFile| installs.iter().any(|i| i.reads == f);
        let packaged = installs.iter().any(|i| i.kind == InstallKind::Packaged);
        match file {
            ConfigFile::ClaudeApp if read_by(file) => Target::Write("a Claude app installed here reads it"),
            ConfigFile::ClaudeApp if packaged => Target::Write("the packaged Claude reads it while it has no private config folder"),
            ConfigFile::ClaudeApp => Target::Skip("no Claude app is installed here"),
            ConfigFile::ClaudeAppPackaged if read_by(file) => Target::Write("the packaged Claude made its config folder here, and reads it first"),
            ConfigFile::ClaudeAppPackaged if packaged => Target::Skip("the packaged Claude has made no private config folder, so it reads the plain file"),
            ConfigFile::ClaudeAppPackaged => Target::Skip("Claude is not installed as a Windows package"),
            ConfigFile::ClaudeCli if read_by(file) => Target::Write("Claude Code is installed here"),
            ConfigFile::ClaudeCli => Target::Skip("Claude Code is not installed here"),
            ConfigFile::Codex | ConfigFile::Cursor => Target::Write("chosen"),
        }
    }

    /// The Claude installs found, in one line for the log: how each is
    /// installed, where, and the file it reads.
    pub fn installs_line(&self) -> String {
        let installs = self.claude_installs();
        if installs.is_empty() {
            return "no Claude found".into();
        }
        let shown = |f: ConfigFile| self.path(f).map(|p| p.display().to_string()).unwrap_or_default();
        let each: Vec<String> = installs.iter().map(|i| format!("{} at {} reads {}", i.kind.label(), i.at.display(), shown(i.reads))).collect();
        each.join("; ")
    }
}

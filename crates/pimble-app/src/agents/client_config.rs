//! An MCP app's config file, as Pimble reads and changes it: the `mcpServers`
//! object of a JSON file (Claude, Cursor) or the `[mcp_servers]` table of
//! ChatGPT / Codex's `config.toml`. Pimble changes only the entries it is
//! asked to, keeps the rest of the file as written, keeps the old file beside
//! it as `.pimble-backup`, and writes the new one in one rename. A file that
//! does not parse is left alone.

use std::path::{Path, PathBuf};

/// The environment variable that carries an agent's key to `pimble-mcp`.
pub const KEY_VAR: &str = "PIMBLE_AGENT_KEY";

/// How a config file is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// `mcpServers` in a JSON object.
    Json,
    /// `[mcp_servers]` in TOML.
    Toml,
}

/// What Pimble reads of an entry: the command it runs and the agent key in
/// its environment, either missing when the entry has none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub command: Option<String>,
    pub key: Option<String>,
}

impl Entry {
    /// Whether the entry runs Pimble's MCP server: its command is a file
    /// named `pimble-mcp` (or `pimble-mcp.exe`), wherever it is installed.
    pub fn runs_pimble(&self) -> bool {
        self.command.as_deref().is_some_and(|c| {
            let name = c.rsplit(['/', '\\']).next().unwrap_or(c);
            name == "pimble-mcp" || name.eq_ignore_ascii_case("pimble-mcp.exe")
        })
    }

    /// The agent the entry's key names, if it carries an agent key.
    pub fn agent(&self) -> Option<uuid::Uuid> {
        self.key.as_deref().and_then(pimble_rpc::agent_id_of_key)
    }
}

/// An entry to write: `command` with the key in its environment, and
/// `"type": "stdio"` where the app's own tools write one (`~/.claude.json`).
#[derive(Clone, Copy, Debug)]
pub struct NewEntry<'a> {
    pub name: &'a str,
    pub command: &'a str,
    pub key: &'a str,
    pub stdio_type: bool,
}

/// A change to one file: the entries `drop` says yes to come out, then `add`
/// (if any) goes in under its name, replacing an entry of that name.
pub struct Change<'a> {
    pub drop: &'a dyn Fn(&str, &Entry) -> bool,
    pub add: Option<NewEntry<'a>>,
}

/// The entries a change took out, by name, and whether it wrote the file.
#[derive(Debug, Default)]
pub struct Changed {
    pub dropped: Vec<(String, Entry)>,
    pub written: bool,
}

/// The name an agent's entry takes in an app's config: `pimble-<name>`.
pub fn entry_name(agent_name: &str) -> String {
    let slug: String = agent_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() { "pimble".into() } else { format!("pimble-{slug}") }
}

/// The JSON of an entry.
pub fn entry_json(entry: &NewEntry) -> serde_json::Value {
    let mut json = serde_json::json!({ "command": entry.command, "env": { KEY_VAR: entry.key } });
    if entry.stdio_type {
        json["type"] = serde_json::json!("stdio");
    }
    json
}

/// The whole snippet to paste, in the file's format.
pub fn snippet(format: Format, entry: &NewEntry) -> String {
    match format {
        Format::Toml => {
            let mut doc = toml_edit::Document::new();
            if let Some(servers) = toml_servers(&mut doc) {
                servers.insert(entry.name, toml_entry(entry));
            }
            doc.to_string()
        }
        Format::Json => {
            let config = serde_json::json!({ "mcpServers": { entry.name: entry_json(entry) } });
            serde_json::to_string_pretty(&config).unwrap_or_default()
        }
    }
}

/// The entries of the file at `path`; none when there is no file.
pub fn read_entries(path: &Path, format: Format) -> Result<Vec<(String, Entry)>, String> {
    let Some(text) = read(path)? else { return Ok(Vec::new()) };
    match format {
        Format::Json => {
            let config = parse_json(path, &text)?;
            Ok(config.get("mcpServers").and_then(|s| s.as_object()).map(|s| s.iter().map(|(n, v)| (n.clone(), json_entry(v))).collect()).unwrap_or_default())
        }
        Format::Toml => {
            let doc = parse_toml(path, &text)?;
            Ok(doc.get("mcp_servers").and_then(|s| s.as_table_like()).map(|s| s.iter().map(|(n, v)| (n.to_string(), toml_entry_of(v))).collect()).unwrap_or_default())
        }
    }
}

/// Make `change` to the file at `path`. A missing file is made only when
/// there is an entry to add; a file the change leaves as it was is not
/// written again.
pub fn apply(path: &Path, format: Format, change: &Change) -> Result<Changed, String> {
    let text = read(path)?;
    if text.is_none() && change.add.is_none() {
        return Ok(Changed::default());
    }
    let (new_text, dropped) = match format {
        Format::Json => change_json(path, text.as_deref(), change)?,
        Format::Toml => change_toml(path, text.as_deref(), change)?,
    };
    // Nothing changed (the same entry added again): the file, and the backup
    // of what it was before Pimble's last real change, stay as they are.
    if (dropped.is_empty() && change.add.is_none()) || text.as_deref() == Some(new_text.as_str()) {
        return Ok(Changed { dropped, written: false });
    }
    // Agent keys sit in these files: the new file and the backup keep the
    // old file's permissions (a 0600 file stays 0600).
    let permissions = std::fs::metadata(path).ok().map(|m| m.permissions());
    if let Some(old) = &text {
        let backup = backup_path(path);
        std::fs::write(&backup, old).map_err(|e| format!("Could not keep a backup of {}: {e}", path.display()))?;
        if let Some(p) = &permissions {
            std::fs::set_permissions(&backup, p.clone()).map_err(|e| format!("Could not keep a backup of {}: {e}", path.display()))?;
        }
    }
    write_replacing(path, &new_text, permissions)?;
    Ok(Changed { dropped, written: true })
}

fn change_json(path: &Path, text: Option<&str>, change: &Change) -> Result<(String, Vec<(String, Entry)>), String> {
    let mut config = match text {
        Some(text) if !text.trim().is_empty() => parse_json(path, text)?,
        _ => serde_json::json!({}),
    };
    let Some(object) = config.as_object_mut() else {
        return Err(format!("{} is not a settings object, so it was left alone", path.display()));
    };
    let servers = object.entry("mcpServers").or_insert_with(|| serde_json::json!({}));
    let Some(servers) = servers.as_object_mut() else {
        return Err(format!("{}'s mcpServers is not a list of servers, so it was left alone", path.display()));
    };
    let doomed: Vec<(String, Entry)> = servers.iter().map(|(n, v)| (n.clone(), json_entry(v))).filter(|(n, e)| (change.drop)(n, e)).collect();
    for (name, _) in &doomed {
        servers.remove(name);
    }
    if let Some(entry) = &change.add {
        servers.insert(entry.name.to_string(), entry_json(entry));
    }
    let text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    Ok((text, doomed))
}

fn change_toml(path: &Path, text: Option<&str>, change: &Change) -> Result<(String, Vec<(String, Entry)>), String> {
    let mut doc = match text {
        Some(text) => parse_toml(path, text)?,
        None => toml_edit::Document::new(),
    };
    let Some(servers) = toml_servers(&mut doc) else {
        return Err(format!("{}'s mcp_servers is not a table, so it was left alone", path.display()));
    };
    let doomed: Vec<(String, Entry)> = servers.iter().map(|(n, v)| (n.to_string(), toml_entry_of(v))).filter(|(n, e)| (change.drop)(n, e)).collect();
    for (name, _) in &doomed {
        servers.remove(name);
    }
    if let Some(entry) = &change.add {
        servers.insert(entry.name, toml_entry(entry));
    }
    Ok((doc.to_string(), doomed))
}

fn json_entry(value: &serde_json::Value) -> Entry {
    Entry {
        command: value.get("command").and_then(|c| c.as_str()).map(str::to_string),
        key: value.get("env").and_then(|e| e.get(KEY_VAR)).and_then(|k| k.as_str()).map(str::to_string),
    }
}

fn toml_entry_of(item: &toml_edit::Item) -> Entry {
    Entry {
        command: item.get("command").and_then(|c| c.as_str()).map(str::to_string),
        key: item.get("env").and_then(|e| e.get(KEY_VAR)).and_then(|k| k.as_str()).map(str::to_string),
    }
}

/// `[mcp_servers]`, made when missing; `None` when it is something else.
fn toml_servers(doc: &mut toml_edit::Document) -> Option<&mut toml_edit::Table> {
    let servers = doc.entry("mcp_servers").or_insert(toml_edit::table()).as_table_mut()?;
    servers.set_implicit(true);
    Some(servers)
}

/// `[mcp_servers.<name>]` with the command and the key in its `env`.
fn toml_entry(entry: &NewEntry) -> toml_edit::Item {
    let mut table = toml_edit::Table::new();
    table.insert("command", toml_edit::value(entry.command));
    let mut env = toml_edit::Table::new();
    env.insert(KEY_VAR, toml_edit::value(entry.key));
    table.insert("env", toml_edit::Item::Table(env));
    toml_edit::Item::Table(table)
}

fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Could not read {}: {e}", path.display())),
    }
}

fn parse_json(path: &Path, text: &str) -> Result<serde_json::Value, String> {
    if text.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(text).map_err(|_| format!("{} is not JSON Pimble can read, so it was left alone", path.display()))
}

fn parse_toml(path: &Path, text: &str) -> Result<toml_edit::Document, String> {
    text.parse().map_err(|_| format!("{} is not TOML Pimble can read, so it was left alone", path.display()))
}

fn write_replacing(path: &Path, text: &str, permissions: Option<std::fs::Permissions>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not make {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("pimble-tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    if let Some(p) = permissions {
        std::fs::set_permissions(&tmp, p).map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("Could not write {}: {e}", path.display()))
}

/// Where the file at `path` is kept as it was before Pimble's last change.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".pimble-backup");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new<'a>(name: &'a str, key: &'a str, stdio_type: bool) -> NewEntry<'a> {
        NewEntry { name, command: "/opt/pimble-mcp", key, stdio_type }
    }

    fn by_name(name: &str) -> impl Fn(&str, &Entry) -> bool + '_ {
        move |n, _| n == name
    }

    #[test]
    fn an_entry_joins_an_apps_config_and_leaves_the_rest_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude_desktop_config.json");
        std::fs::write(&path, r#"{ "theme": "dark", "mcpServers": { "other": { "command": "x" } } }"#).unwrap();
        let drop = by_name("pimble-claude");
        apply(&path, Format::Json, &Change { drop: &drop, add: Some(new("pimble-claude", "pmbl_agent_k", false)) }).unwrap();
        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["mcpServers"]["other"]["command"], "x");
        assert_eq!(config["mcpServers"]["pimble-claude"]["command"], "/opt/pimble-mcp");
        assert_eq!(config["mcpServers"]["pimble-claude"]["env"]["PIMBLE_AGENT_KEY"], "pmbl_agent_k");
        assert!(config["mcpServers"]["pimble-claude"].get("type").is_none());
        let backup = std::fs::read_to_string(backup_path(&path)).unwrap();
        assert!(backup.contains("\"theme\""), "the old file is kept");
    }

    #[test]
    fn a_missing_config_is_made_and_a_broken_one_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("mcp.json");
        let drop = by_name("pimble-x");
        apply(&path, Format::Json, &Change { drop: &drop, add: Some(new("pimble-x", "k", true)) }).unwrap();
        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["pimble-x"]["type"], "stdio");

        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, "{ not json").unwrap();
        assert!(apply(&broken, Format::Json, &Change { drop: &drop, add: Some(new("pimble-x", "k", false)) }).is_err());
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "{ not json", "untouched");
    }

    #[test]
    fn taking_out_without_adding_makes_no_file_and_writes_only_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.json");
        let all = |_: &str, _: &Entry| true;
        let changed = apply(&missing, Format::Json, &Change { drop: &all, add: None }).unwrap();
        assert!(!changed.written && !missing.exists());

        let path = dir.path().join("c.json");
        std::fs::write(&path, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        let none = |_: &str, _: &Entry| false;
        let changed = apply(&path, Format::Json, &Change { drop: &none, add: None }).unwrap();
        assert!(!changed.written && !backup_path(&path).exists(), "nothing to change, nothing written");
    }

    #[test]
    fn adding_the_same_entry_again_keeps_the_file_and_its_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        std::fs::write(&path, r#"{"mine":1}"#).unwrap();
        let drop = by_name("pimble-x");
        let change = Change { drop: &drop, add: Some(new("pimble-x", "k", false)) };
        assert!(apply(&path, Format::Json, &change).unwrap().written);
        let again = apply(&path, Format::Json, &change).unwrap();
        assert!(!again.written, "nothing changed");
        assert_eq!(std::fs::read_to_string(backup_path(&path)).unwrap(), r#"{"mine":1}"#, "the backup still holds the person's own file");
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_stays_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let drop = by_name("pimble-x");
        apply(&path, Format::Json, &Change { drop: &drop, add: Some(new("pimble-x", "k", false)) }).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(backup_path(&path)).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_codex_config_keeps_its_comments_and_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# my settings\nmodel = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"x\" # keep me\n").unwrap();
        let drop = by_name("pimble-chatgpt");
        apply(&path, Format::Toml, &Change { drop: &drop, add: Some(new("pimble-chatgpt", "pmbl_agent_k", false)) }).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my settings") && text.contains("# keep me"), "comments kept:\n{text}");
        let doc: toml_edit::Document = text.parse().unwrap();
        assert_eq!(doc["model"].as_str(), Some("gpt"));
        assert_eq!(doc["mcp_servers"]["other"]["command"].as_str(), Some("x"));
        assert_eq!(doc["mcp_servers"]["pimble-chatgpt"]["command"].as_str(), Some("/opt/pimble-mcp"));
        assert_eq!(doc["mcp_servers"]["pimble-chatgpt"]["env"]["PIMBLE_AGENT_KEY"].as_str(), Some("pmbl_agent_k"));
        assert!(std::fs::read_to_string(backup_path(&path)).unwrap().contains("# my settings"));
        let entries = read_entries(&path, Format::Toml).unwrap();
        assert!(entries.iter().any(|(n, e)| n == "pimble-chatgpt" && e.runs_pimble() && e.key.as_deref() == Some("pmbl_agent_k")));

        let gone = apply(&path, Format::Toml, &Change { drop: &drop, add: None }).unwrap();
        assert_eq!(gone.dropped.len(), 1);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("pimble-chatgpt") && text.contains("# keep me"), "taken out, the rest kept:\n{text}");

        let broken = dir.path().join("broken.toml");
        std::fs::write(&broken, "[[[ not toml").unwrap();
        assert!(apply(&broken, Format::Toml, &Change { drop: &drop, add: Some(new("pimble-x", "k", false)) }).is_err());
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "[[[ not toml");
    }

    #[test]
    fn pimbles_own_server_is_recognised_wherever_it_is_installed() {
        let e = |c: &str| Entry { command: Some(c.into()), key: None };
        assert!(e("/home/j/.local/share/pimble/pimble-mcp").runs_pimble());
        assert!(e(r"C:\Program Files\Pimble\pimble-mcp.exe").runs_pimble());
        assert!(e("pimble-mcp").runs_pimble());
        assert!(!e("/usr/bin/pimble-mcp-proxy").runs_pimble());
        assert!(!e("npx").runs_pimble());
        assert!(!Entry::default().runs_pimble());
    }

    #[test]
    fn entry_names_are_tidy() {
        assert_eq!(entry_name("Claude"), "pimble-claude");
        assert_eq!(entry_name("Claude Desktop"), "pimble-claude-desktop");
        assert_eq!(entry_name("  Work GPT!! "), "pimble-work-gpt");
        assert_eq!(entry_name("✨"), "pimble");
    }
}

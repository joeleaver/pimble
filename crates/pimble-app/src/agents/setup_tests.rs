use super::*;

/// A computer with the Claude app and Claude Code installed, nothing set up.
fn computer() -> (tempfile::TempDir, Locations) {
    let dir = tempfile::tempdir().unwrap();
    let at = Locations { home: dir.path().join("home"), config: dir.path().join("config"), codex: dir.path().join("home/.codex") };
    std::fs::create_dir_all(at.config.join("Claude")).unwrap();
    std::fs::create_dir_all(at.home.join(".claude")).unwrap();
    (dir, at)
}

fn key(id: Uuid) -> String {
    pimble_rpc::agent_key(id, "secret")
}

const MCP: &str = "/opt/pimble/pimble-mcp";

fn entry<'a>(name: &'a str, key: &'a str) -> NewEntry<'a> {
    NewEntry { name, command: MCP, key, stdio_type: false }
}

fn servers(at: &Locations, file: ConfigFile) -> serde_json::Map<String, serde_json::Value> {
    let text = std::fs::read_to_string(at.path(file)).unwrap();
    let config: serde_json::Value = serde_json::from_str(&text).unwrap();
    config["mcpServers"].as_object().cloned().unwrap_or_default()
}

fn write(at: &Locations, file: ConfigFile, json: serde_json::Value) {
    let path = at.path(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string(&json).unwrap()).unwrap();
}

fn pimble(key: &str) -> serde_json::Value {
    serde_json::json!({ "command": MCP, "env": { "PIMBLE_AGENT_KEY": key } })
}

#[test]
fn claude_is_one_entry_written_alike_to_both_files() {
    let (_dir, at) = computer();
    write(&at, ConfigFile::ClaudeCli, serde_json::json!({ "numStartups": 4, "mcpServers": { "other": { "command": "x" } } }));
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert!(added.failed.is_empty(), "{:?}", added.failed);
    assert_eq!(added.written, vec![ConfigFile::ClaudeApp, ConfigFile::ClaudeCli]);
    assert!(added.replaced.is_empty());

    let app = servers(&at, ConfigFile::ClaudeApp);
    let cli = servers(&at, ConfigFile::ClaudeCli);
    assert_eq!(app.keys().collect::<Vec<_>>(), vec!["pimble-claude"]);
    assert_eq!(app["pimble-claude"]["command"], cli["pimble-claude"]["command"]);
    assert_eq!(app["pimble-claude"]["env"], cli["pimble-claude"]["env"]);
    assert_eq!(cli["pimble-claude"]["type"], "stdio");
    assert!(app["pimble-claude"].get("type").is_none());
    assert_eq!(cli["other"]["command"], "x", "other servers stay");
    let text = std::fs::read_to_string(at.path(ConfigFile::ClaudeCli)).unwrap();
    assert!(text.contains("numStartups"), "the rest of ~/.claude.json stays");

    assert_eq!(holders(&at).get(&id), Some(&vec![ClientApp::Claude]), "one app, not two");
}

#[test]
fn claude_is_written_only_where_it_is_installed() {
    let dir = tempfile::tempdir().unwrap();
    let at = Locations { home: dir.path().join("home"), config: dir.path().join("config"), codex: dir.path().join("codex") };
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert!(added.written.is_empty());
    assert_eq!(added.failed.len(), 1, "no Claude here says so");
    assert!(!at.path(ConfigFile::ClaudeApp).exists() && !at.path(ConfigFile::ClaudeCli).exists());

    std::fs::create_dir_all(at.home.join(".claude")).unwrap();
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert_eq!(added.written, vec![ConfigFile::ClaudeCli]);
    assert!(!at.path(ConfigFile::ClaudeApp).exists(), "no Claude app, no config made for it");
}

#[test]
fn the_old_split_entries_are_replaced_and_their_agents_reported() {
    let (_dir, at) = computer();
    let (desktop, code) = (Uuid::new_v4(), Uuid::new_v4());
    write(&at, ConfigFile::ClaudeApp, serde_json::json!({ "mcpServers": { "pimble-claude-desktop": pimble(&key(desktop)), "postrinch-claude-desktop": { "command": "/opt/postrinch-mcp" } } }));
    write(&at, ConfigFile::ClaudeCli, serde_json::json!({ "mcpServers": { "pimble-claude-code": pimble(&key(code)) } }));

    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert_eq!(added.replaced, [desktop, code].into_iter().collect());
    let app = servers(&at, ConfigFile::ClaudeApp);
    let cli = servers(&at, ConfigFile::ClaudeCli);
    assert_eq!(app.keys().filter(|n| n.starts_with("pimble")).collect::<Vec<_>>(), vec!["pimble-claude"]);
    assert!(app.contains_key("postrinch-claude-desktop"), "another app's server stays");
    assert_eq!(cli.keys().collect::<Vec<_>>(), vec!["pimble-claude"]);
    let held = holders(&at);
    assert!(!held.contains_key(&desktop) && !held.contains_key(&code));
}

#[test]
fn a_hand_made_pair_under_an_old_name_is_replaced_too() {
    // Joe's machine on 2026-11-24: one `pimble-claude-desktop` in both files,
    // the old desktop agent's key, and the old Claude Code agent in no file.
    let (_dir, at) = computer();
    let desktop = Uuid::new_v4();
    let pair = serde_json::json!({ "mcpServers": { "pimble-claude-desktop": pimble(&key(desktop)) } });
    write(&at, ConfigFile::ClaudeApp, pair.clone());
    write(&at, ConfigFile::ClaudeCli, pair);
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert_eq!(added.replaced, [desktop].into_iter().collect());
    for file in [ConfigFile::ClaudeApp, ConfigFile::ClaudeCli] {
        assert_eq!(servers(&at, file).keys().collect::<Vec<_>>(), vec!["pimble-claude"], "{file:?}");
    }
}

#[test]
fn adding_again_leaves_one_entry_and_replaces_the_stale_one() {
    let (_dir, at) = computer();
    let first = Uuid::new_v4();
    let k1 = key(first);
    add(&at, ClientApp::Claude, entry("pimble-claude", &k1), first);
    // Added again under the same name with the same key: nothing replaced.
    let again = add(&at, ClientApp::Claude, entry("pimble-claude", &k1), first);
    assert!(again.replaced.is_empty());

    let second = Uuid::new_v4();
    let k2 = key(second);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k2), second);
    assert_eq!(added.replaced, [first].into_iter().collect(), "the stale entry's agent");
    for file in [ConfigFile::ClaudeApp, ConfigFile::ClaudeCli] {
        let s = servers(&at, file);
        assert_eq!(s.len(), 1);
        assert_eq!(s["pimble-claude"]["env"]["PIMBLE_AGENT_KEY"], k2.as_str());
    }
}

#[test]
fn removing_an_agent_takes_it_out_of_every_app() {
    let (_dir, at) = computer();
    let (gone, stays) = (Uuid::new_v4(), Uuid::new_v4());
    let (kg, ks) = (key(gone), key(stays));
    add(&at, ClientApp::Claude, entry("pimble-claude", &kg), gone);
    add(&at, ClientApp::Cursor, entry("pimble-cursor", &kg), gone);
    add(&at, ClientApp::ChatGpt, entry("pimble-chatgpt", &ks), stays);
    add(&at, ClientApp::Cursor, entry("pimble-other", &ks), stays);
    assert_eq!(holders(&at)[&gone].len(), 2);

    let out = forget(&at, gone);
    assert!(out.failed.is_empty(), "{:?}", out.failed);
    assert_eq!(out.changed, vec![ConfigFile::ClaudeApp, ConfigFile::ClaudeCli, ConfigFile::Cursor]);
    let held = holders(&at);
    assert!(!held.contains_key(&gone));
    assert_eq!(held[&stays], vec![ClientApp::ChatGpt, ClientApp::Cursor], "another agent's entries stay");
    assert!(servers(&at, ConfigFile::ClaudeApp).is_empty());

    let again = forget(&at, gone);
    assert!(again.changed.is_empty(), "nothing left to take out, nothing written");
}

#[test]
fn other_apps_replace_only_an_entry_of_the_same_name() {
    let (_dir, at) = computer();
    let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (ka, kb, kc) = (key(a), key(b), key(c));
    add(&at, ClientApp::Cursor, entry("pimble-work", &ka), a);
    add(&at, ClientApp::Cursor, entry("pimble-home", &kb), b);
    let added = add(&at, ClientApp::Cursor, entry("pimble-work", &kc), c);
    assert_eq!(added.replaced, [a].into_iter().collect());
    let s = servers(&at, ConfigFile::Cursor);
    assert_eq!(s.len(), 2);
    assert_eq!(s["pimble-home"]["env"]["PIMBLE_AGENT_KEY"], kb.as_str());
}

#[test]
fn sentences_say_where_it_went_and_what_it_replaced() {
    let added = Added { written: vec![ConfigFile::ClaudeApp, ConfigFile::ClaudeCli], ..Added::default() };
    let s = added_sentence(ClientApp::Claude, &added, &["Claude Desktop".into(), "Claude Code".into()], &[]);
    assert!(s.starts_with("Added to Claude: the Chat and Code tabs"), "{s}");
    assert!(s.contains("It replaces “Claude Desktop” and “Claude Code”, removed from Pimble."), "{s}");
    assert_eq!(app_list(&[ClientApp::Claude, ClientApp::Cursor, ClientApp::ChatGpt]), "Claude, Cursor and ChatGPT / Codex");
    assert_eq!(placed_line(None), "Not in the settings of any app Pimble knows here");
}

use super::*;

/// A computer with the Claude app and Claude Code installed, nothing set up.
fn computer() -> (tempfile::TempDir, Locations) {
    let dir = tempfile::tempdir().unwrap();
    let at = Locations { home: dir.path().join("home"), config: dir.path().join("config"), codex: dir.path().join("home/.codex"), claude_package: None, claude_programs: Vec::new() };
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
    let text = std::fs::read_to_string(at.path(file).unwrap()).unwrap();
    let config: serde_json::Value = serde_json::from_str(&text).unwrap();
    config["mcpServers"].as_object().cloned().unwrap_or_default()
}

fn write(at: &Locations, file: ConfigFile, json: serde_json::Value) {
    let path = at.path(file).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string(&json).unwrap()).unwrap();
}

/// The files an "Add" wrote, without why.
fn written(added: &Added) -> Vec<ConfigFile> {
    added.written.iter().map(|(f, _)| *f).collect()
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
    assert_eq!(written(&added), vec![ConfigFile::ClaudeApp, ConfigFile::ClaudeCli]);
    assert!(added.replaced().is_empty());

    let app = servers(&at, ConfigFile::ClaudeApp);
    let cli = servers(&at, ConfigFile::ClaudeCli);
    assert_eq!(app.keys().collect::<Vec<_>>(), vec!["pimble-claude"]);
    assert_eq!(app["pimble-claude"]["command"], cli["pimble-claude"]["command"]);
    assert_eq!(app["pimble-claude"]["env"], cli["pimble-claude"]["env"]);
    assert_eq!(cli["pimble-claude"]["type"], "stdio");
    assert!(app["pimble-claude"].get("type").is_none());
    assert_eq!(cli["other"]["command"], "x", "other servers stay");
    let text = std::fs::read_to_string(at.path(ConfigFile::ClaudeCli).unwrap()).unwrap();
    assert!(text.contains("numStartups"), "the rest of ~/.claude.json stays");

    assert_eq!(holders(&at).by_agent.get(&id), Some(&vec![ClientApp::Claude]), "one app, not two");
}

#[test]
fn claude_is_written_only_where_it_is_installed() {
    let dir = tempfile::tempdir().unwrap();
    let at = Locations { home: dir.path().join("home"), config: dir.path().join("config"), codex: dir.path().join("codex"), claude_package: None, claude_programs: Vec::new() };
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert!(added.written.is_empty());
    assert_eq!(added.failed.len(), 1, "no Claude here says so");
    assert!(!at.path(ConfigFile::ClaudeApp).unwrap().exists() && !at.path(ConfigFile::ClaudeCli).unwrap().exists());

    std::fs::create_dir_all(at.home.join(".claude")).unwrap();
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert_eq!(written(&added), vec![ConfigFile::ClaudeCli]);
    assert!(!at.path(ConfigFile::ClaudeApp).unwrap().exists(), "no Claude app, no config made for it");
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
    assert_eq!(added.replaced(), [desktop, code].into_iter().collect());
    let app = servers(&at, ConfigFile::ClaudeApp);
    let cli = servers(&at, ConfigFile::ClaudeCli);
    assert_eq!(app.keys().filter(|n| n.starts_with("pimble")).collect::<Vec<_>>(), vec!["pimble-claude"]);
    assert!(app.contains_key("postrinch-claude-desktop"), "another app's server stays");
    assert_eq!(cli.keys().collect::<Vec<_>>(), vec!["pimble-claude"]);
    let held = holders(&at).by_agent;
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
    assert_eq!(added.replaced(), [desktop].into_iter().collect());
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
    assert!(again.replaced().is_empty());

    let second = Uuid::new_v4();
    let k2 = key(second);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k2), second);
    assert_eq!(added.replaced(), [first].into_iter().collect(), "the stale entry's agent");
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
    assert_eq!(holders(&at).by_agent[&gone].len(), 2);

    let out = forget(&at, gone);
    assert!(out.failed.is_empty(), "{:?}", out.failed);
    assert_eq!(out.changed, vec![ConfigFile::ClaudeApp, ConfigFile::ClaudeCli, ConfigFile::Cursor]);
    let held = holders(&at).by_agent;
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
    assert_eq!(added.replaced(), [a].into_iter().collect());
    let s = servers(&at, ConfigFile::Cursor);
    assert_eq!(s.len(), 2);
    assert_eq!(s["pimble-home"]["env"]["PIMBLE_AGENT_KEY"], kb.as_str());
}

#[test]
fn sentences_say_where_it_went_and_what_it_replaced() {
    let added = Added { written: vec![(ConfigFile::ClaudeApp, "test"), (ConfigFile::ClaudeCli, "test")], ..Added::default() };
    let settled = Settled { removed: vec!["Claude Desktop".into(), "Claude Code".into()], ..Settled::default() };
    let s = added_sentence(ClientApp::Claude, &added, &settled);
    assert!(s.starts_with("Added to Claude: the Chat and Code tabs"), "{s}");
    assert!(s.contains("It replaces “Claude Desktop” and “Claude Code”, removed from Pimble."), "{s}");
    assert_eq!(app_list(&[ClientApp::Claude, ClientApp::Cursor, ClientApp::ChatGpt]), "Claude, Cursor and ChatGPT / Codex");
    assert_eq!(placed_line(None), "Not in the settings of any app Pimble knows here");
}

#[test]
fn replaced_claude_agents_are_removed_unless_still_used_or_unsure() {
    let (_dir, at) = computer();
    let (desktop, code, foreign) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    write(&at, ConfigFile::ClaudeApp, serde_json::json!({ "mcpServers": { "pimble-claude-desktop": pimble(&key(desktop)), "pimble-dev": pimble(&key(foreign)) } }));
    write(&at, ConfigFile::ClaudeCli, serde_json::json!({ "mcpServers": { "pimble-claude-code": pimble(&key(code)) } }));
    // The old Claude Code agent's key is in Cursor too.
    write(&at, ConfigFile::Cursor, serde_json::json!({ "mcpServers": { "pimble-cursor": pimble(&key(code)) } }));
    let agents = vec![(desktop, "Claude Desktop".to_string()), (code, "Claude Code".to_string())];

    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    let settled = settle(ClientApp::Claude, &added, &holders(&at), &agents);
    assert_eq!(settled.remove, vec![desktop]);
    assert_eq!(settled.removed, vec!["Claude Desktop".to_string()]);
    assert_eq!(settled.kept, vec!["Claude Code".to_string()], "still in Cursor");
    assert_eq!(settled.others, vec!["pimble-dev".to_string()], "named, not silently gone");

    // A file that could not be read might hold any of them: nobody is removed.
    let unsure = Holders { unreadable: true, ..Holders::default() };
    let settled = settle(ClientApp::Claude, &added, &unsure, &agents);
    assert!(settled.remove.is_empty());
    assert_eq!(settled.kept.len(), 2);
}

#[test]
fn one_claude_file_failing_removes_nobody() {
    let (_dir, at) = computer();
    let old = Uuid::new_v4();
    let pair = serde_json::json!({ "mcpServers": { "pimble-claude-desktop": pimble(&key(old)) } });
    write(&at, ConfigFile::ClaudeApp, pair);
    std::fs::write(at.path(ConfigFile::ClaudeCli).unwrap(), "{ half written").unwrap();
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert_eq!(written(&added), vec![ConfigFile::ClaudeApp]);
    assert_eq!(added.failed.len(), 1);
    let held = holders(&at);
    assert!(held.unreadable);
    let settled = settle(ClientApp::Claude, &added, &held, &[(old, "Claude Desktop".into())]);
    assert!(settled.remove.is_empty(), "it may still be in the file that failed");
    assert_eq!(std::fs::read_to_string(at.path(ConfigFile::ClaudeCli).unwrap()).unwrap(), "{ half written", "left alone");
}

#[test]
fn other_apps_remove_no_agent_they_replace() {
    let (_dir, at) = computer();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let (ka, kb) = (key(a), key(b));
    add(&at, ClientApp::Cursor, entry("pimble-cursor", &ka), a);
    let added = add(&at, ClientApp::Cursor, entry("pimble-cursor", &kb), b);
    assert_eq!(added.replaced(), [a].into_iter().collect());
    let settled = settle(ClientApp::Cursor, &added, &holders(&at), &[(a, "Cursor".into())]);
    assert_eq!(settled, Settled::default(), "as before: the old agent stays");
}

#[test]
fn removing_takes_an_agent_out_of_codex_too() {
    let (_dir, at) = computer();
    std::fs::create_dir_all(&at.codex).unwrap();
    std::fs::write(at.path(ConfigFile::Codex).unwrap(), "# mine\n[mcp_servers.other]\ncommand = \"x\"\n").unwrap();
    let id = Uuid::new_v4();
    let k = key(id);
    add(&at, ClientApp::ChatGpt, entry("pimble-chatgpt", &k), id);
    assert_eq!(holders(&at).by_agent[&id], vec![ClientApp::ChatGpt]);
    assert_eq!(forget(&at, id).changed, vec![ConfigFile::Codex]);
    let text = std::fs::read_to_string(at.path(ConfigFile::Codex).unwrap()).unwrap();
    assert!(!text.contains("pimble-chatgpt") && text.contains("# mine") && text.contains("[mcp_servers.other]"), "{text}");
}

#[cfg(unix)]
#[test]
fn a_claude_file_that_cannot_be_written_removes_nobody() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, at) = computer();
    let old = Uuid::new_v4();
    let pair = serde_json::json!({ "mcpServers": { "pimble-claude-desktop": pimble(&key(old)) } });
    write(&at, ConfigFile::ClaudeApp, pair.clone());
    write(&at, ConfigFile::ClaudeCli, pair);
    // The home folder is readable but takes no new file: ~/.claude.json
    // reads, and cannot be replaced.
    std::fs::set_permissions(&at.home, std::fs::Permissions::from_mode(0o555)).unwrap();
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    std::fs::set_permissions(&at.home, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(written(&added), vec![ConfigFile::ClaudeApp]);
    assert_eq!(added.failed.len(), 1);
    assert_eq!(added.replaced(), [old].into_iter().collect(), "taken out of the Claude app's file");
    let held = holders(&at);
    assert!(!held.unreadable, "every file reads; only a write failed");
    let settled = settle(ClientApp::Claude, &added, &held, &[(old, "Claude Desktop".into())]);
    assert!(settled.remove.is_empty(), "its entry is still in the file that failed");
    assert_eq!(settled.kept, vec!["Claude Desktop".to_string()]);
    // Even were the files read as holding nothing, a failed write is reason
    // enough to remove nobody.
    let settled = settle(ClientApp::Claude, &added, &Holders::default(), &[(old, "Claude Desktop".into())]);
    assert!(settled.remove.is_empty());
}

/// A Windows computer with the Claude app installed as a package, which has
/// made its own copy of its config (`{}`), and never its plain config folder.
fn packaged_computer() -> (tempfile::TempDir, Locations) {
    let dir = tempfile::tempdir().unwrap();
    let packages = dir.path().join("local/Packages");
    std::fs::create_dir_all(packages.join(CLAUDE_PACKAGE)).unwrap();
    let at = Locations {
        home: dir.path().join("home"),
        config: dir.path().join("roaming"),
        codex: dir.path().join("home/.codex"),
        claude_package: claude_package_in(&packages),
        claude_programs: Vec::new(),
    };
    write(&at, ConfigFile::ClaudeAppPackaged, serde_json::json!({}));
    (dir, at)
}

#[test]
fn a_packaged_claude_gets_its_own_copy_and_the_plain_one() {
    let (_dir, at) = packaged_computer();
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert!(added.failed.is_empty(), "{:?}", added.failed);
    assert_eq!(written(&added), vec![ConfigFile::ClaudeApp, ConfigFile::ClaudeAppPackaged]);
    let packaged = at.path(ConfigFile::ClaudeAppPackaged).unwrap();
    assert!(packaged.ends_with("LocalCache/Roaming/Claude/claude_desktop_config.json"), "{}", packaged.display());
    assert_eq!(servers(&at, ConfigFile::ClaudeAppPackaged), servers(&at, ConfigFile::ClaudeApp));
    assert_eq!(holders(&at).by_agent.get(&id), Some(&vec![ClientApp::Claude]), "one app");
    let sentence = added_sentence(ClientApp::Claude, &added, &Settled::default());
    assert!(sentence.starts_with("Added to the Claude app"), "{sentence}");

    let gone = forget(&at, id);
    assert!(gone.failed.is_empty());
    assert!(servers(&at, ConfigFile::ClaudeAppPackaged).is_empty() && servers(&at, ConfigFile::ClaudeApp).is_empty());
}

#[test]
fn the_packaged_copy_loses_the_old_pimble_entries_too() {
    let (_dir, at) = packaged_computer();
    let old = Uuid::new_v4();
    write(&at, ConfigFile::ClaudeAppPackaged, serde_json::json!({ "preferences": { "x": 1 }, "mcpServers": { "pimble-claude-desktop": pimble(&key(old)) } }));
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert_eq!(added.replaced(), [old].into_iter().collect());
    assert_eq!(servers(&at, ConfigFile::ClaudeAppPackaged).keys().collect::<Vec<_>>(), vec!["pimble-claude"]);
    let text = std::fs::read_to_string(at.path(ConfigFile::ClaudeAppPackaged).unwrap()).unwrap();
    assert!(text.contains("preferences"), "the rest of the app's file stays");
}

#[test]
fn a_claude_package_is_found_by_its_prefix() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(claude_package_in(dir.path()), None);
    assert_eq!(claude_package_in(&dir.path().join("missing")), None);
    for name in ["ClaudeHelper", "Claude_", "Claude_x.y", "Claude_abcdefghjkmn", "Claude_abcdefghjkmno", "Other_pzs8sxrjxfjjc"] {
        std::fs::create_dir_all(dir.path().join(name)).unwrap();
    }
    assert_eq!(claude_package_in(dir.path()), None, "only Claude_<publisher id>: 13 characters, no i, l, o or u");
    std::fs::create_dir_all(dir.path().join("Claude_abcdefghjkmnp")).unwrap();
    assert_eq!(claude_package_in(dir.path()), Some(dir.path().join("Claude_abcdefghjkmnp")), "a package signed anew");
    std::fs::create_dir_all(dir.path().join(CLAUDE_PACKAGE)).unwrap();
    assert_eq!(claude_package_in(dir.path()), Some(dir.path().join(CLAUDE_PACKAGE)), "the known one first");
}

#[test]
fn a_claude_app_that_never_ran_is_found_by_its_program() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("local/AnthropicClaude");
    let at = Locations {
        home: dir.path().join("home"),
        config: dir.path().join("roaming"),
        codex: dir.path().join("home/.codex"),
        claude_package: None,
        claude_programs: vec![(InstallKind::Installer, program.clone())],
    };
    assert!(matches!(at.target(ConfigFile::ClaudeApp), Target::Skip(_)));
    std::fs::create_dir_all(&program).unwrap();
    assert!(matches!(at.target(ConfigFile::ClaudeApp), Target::Write(_)));
    assert!(matches!(at.target(ConfigFile::ClaudeAppPackaged), Target::Skip(_)));
    assert_eq!(at.claude_installs(), vec![Install { kind: InstallKind::Installer, at: program.clone(), reads: ConfigFile::ClaudeApp }]);
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert_eq!(written(&added), vec![ConfigFile::ClaudeApp], "its config folder is made");
    assert!(at.path(ConfigFile::ClaudeApp).unwrap().exists());
}

#[test]
fn what_an_add_did_is_logged_by_path_and_id() {
    let (_dir, at) = packaged_computer();
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("my private name", &k), id);
    let lines = added_log(&at, ClientApp::Claude, id, &added, &Settled::default());
    assert_eq!(lines.len(), 4, "the Claudes found, two files written, one skipped: {lines:?}");
    let Logged::Info(found) = &lines[0] else { panic!("{lines:?}") };
    assert!(found.contains("Windows package") && found.contains(&at.claude_package.as_ref().unwrap().display().to_string()), "{found}");
    assert!(found.contains(&at.path(ConfigFile::ClaudeAppPackaged).unwrap().display().to_string()), "and the file it reads: {found}");
    assert!(matches!(&lines[3], Logged::Info(l) if l.contains("skipped") && l.contains(".claude.json") && l.contains("not installed")), "{lines:?}");
    for (line, file) in lines[1..3].iter().zip([ConfigFile::ClaudeApp, ConfigFile::ClaudeAppPackaged]) {
        let Logged::Info(line) = line else { panic!("{line:?}") };
        assert!(line.contains(&id.to_string()) && line.contains(&at.path(file).unwrap().display().to_string()), "{line}");
        assert!(!line.contains("my private name") && !line.contains(&k), "no names, no keys: {line}");
    }

    let nowhere = Locations { home: at.home.join("x"), config: at.config.join("x"), codex: at.codex.clone(), claude_package: None, claude_programs: Vec::new() };
    let added = add(&nowhere, ClientApp::Claude, entry("pimble-claude", &k), id);
    let lines = added_log(&nowhere, ClientApp::Claude, id, &added, &Settled::default());
    assert!(matches!(&lines[0], Logged::Info(l) if l.contains("no Claude found")), "{lines:?}");
    assert!(lines.iter().any(|l| matches!(l, Logged::Warn(l) if l.contains("found no Claude"))), "{lines:?}");
    assert!(matches!(lines.last(), Some(Logged::Warn(l)) if l.contains("looked at") && l.contains(".claude.json") && !l.contains("Packaged")), "{lines:?}");
}

#[test]
fn a_packaged_claude_whose_folder_has_no_file_gets_one_made_from_the_plain_file() {
    // The tester's computer (2026-11-27): the app made its private folder
    // and no config file in it, and never read the plain one.
    let (_dir, at) = packaged_computer();
    std::fs::remove_file(at.path(ConfigFile::ClaudeAppPackaged).unwrap()).unwrap();
    write(&at, ConfigFile::ClaudeApp, serde_json::json!({ "preferences": { "x": 1 }, "mcpServers": { "mine": { "command": "x" } } }));
    assert_eq!(at.claude_installs()[0].reads, ConfigFile::ClaudeAppPackaged, "the app reads its private folder");
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert!(added.failed.is_empty(), "{:?}", added.failed);
    assert_eq!(written(&added), vec![ConfigFile::ClaudeApp, ConfigFile::ClaudeAppPackaged]);
    assert_eq!(added.carried, vec![ConfigFile::ClaudeAppPackaged]);
    assert_eq!(servers(&at, ConfigFile::ClaudeAppPackaged).keys().collect::<Vec<_>>(), vec!["mine", "pimble-claude"], "the person's own servers come along");
    let text = std::fs::read_to_string(at.path(ConfigFile::ClaudeAppPackaged).unwrap()).unwrap();
    assert!(text.contains("preferences"), "and the rest of the plain file");
    let lines = added_log(&at, ClientApp::Claude, id, &added, &Settled::default());
    assert!(lines.iter().any(|l| matches!(l, Logged::Info(l) if l.contains("made from"))), "{lines:?}");

    let gone = forget(&at, id);
    assert_eq!(gone.changed, vec![ConfigFile::ClaudeApp, ConfigFile::ClaudeAppPackaged]);
    assert_eq!(servers(&at, ConfigFile::ClaudeAppPackaged).keys().collect::<Vec<_>>(), vec!["mine"]);
}

#[test]
fn a_packaged_claude_with_no_private_folder_reads_and_gets_the_plain_file() {
    // Never run since installing, or it found the plain folder there first.
    let (_dir, at) = packaged_computer();
    let folder = at.path(ConfigFile::ClaudeAppPackaged).unwrap().parent().unwrap().to_path_buf();
    std::fs::remove_dir_all(&folder).unwrap();
    assert_eq!(at.claude_installs(), vec![Install { kind: InstallKind::Packaged, at: at.claude_package.clone().unwrap(), reads: ConfigFile::ClaudeApp }]);
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert!(added.failed.is_empty(), "{:?}", added.failed);
    assert_eq!(written(&added), vec![ConfigFile::ClaudeApp]);
    assert!(added.skipped.iter().any(|(f, _)| *f == ConfigFile::ClaudeAppPackaged));
    assert!(!folder.exists(), "Pimble makes no folder in the package's copy of AppData");
    let sentence = added_sentence(ClientApp::Claude, &added, &Settled::default());
    assert!(sentence.starts_with("Added to the Claude app"), "{sentence}");
}

#[test]
fn an_installer_claude_alone_gets_the_plain_file() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("local/AnthropicClaude");
    std::fs::create_dir_all(&program).unwrap();
    let at = Locations {
        home: dir.path().join("home"),
        config: dir.path().join("roaming"),
        codex: dir.path().join("home/.codex"),
        claude_package: claude_package_in(&dir.path().join("local/Packages")),
        claude_programs: vec![(InstallKind::Installer, program)],
    };
    assert_eq!(at.path(ConfigFile::ClaudeAppPackaged), None, "no package, no packaged file");
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert_eq!(written(&added), vec![ConfigFile::ClaudeApp]);
    assert_eq!(servers(&at, ConfigFile::ClaudeApp).keys().collect::<Vec<_>>(), vec!["pimble-claude"]);
}

#[test]
fn both_claude_apps_and_claude_code_each_get_the_one_entry_and_lose_it_together() {
    let (dir, mut at) = packaged_computer();
    let program = dir.path().join("local/AnthropicClaude");
    std::fs::create_dir_all(&program).unwrap();
    std::fs::create_dir_all(at.home.join(".claude")).unwrap();
    at.claude_programs = vec![(InstallKind::Installer, program)];
    let kinds: Vec<InstallKind> = at.claude_installs().iter().map(|i| i.kind).collect();
    assert_eq!(kinds, vec![InstallKind::Packaged, InstallKind::Installer, InstallKind::Cli]);

    let old = Uuid::new_v4();
    write(&at, ConfigFile::ClaudeApp, serde_json::json!({ "mcpServers": { "pimble-claude-desktop": pimble(&key(old)) } }));
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert!(added.failed.is_empty(), "{:?}", added.failed);
    assert_eq!(written(&added), vec![ConfigFile::ClaudeApp, ConfigFile::ClaudeAppPackaged, ConfigFile::ClaudeCli]);
    assert!(added.carried.is_empty(), "the app's own file is kept, not replaced");
    assert_eq!(added.replaced(), [old].into_iter().collect());
    for file in [ConfigFile::ClaudeApp, ConfigFile::ClaudeAppPackaged, ConfigFile::ClaudeCli] {
        assert_eq!(servers(&at, file).keys().collect::<Vec<_>>(), vec!["pimble-claude"], "{file:?}: exactly one Pimble agent");
    }
    assert_eq!(holders(&at).by_agent.get(&id), Some(&vec![ClientApp::Claude]));

    let gone = forget(&at, id);
    assert!(gone.failed.is_empty(), "{:?}", gone.failed);
    assert_eq!(gone.changed, vec![ConfigFile::ClaudeApp, ConfigFile::ClaudeAppPackaged, ConfigFile::ClaudeCli]);
    for file in [ConfigFile::ClaudeApp, ConfigFile::ClaudeAppPackaged, ConfigFile::ClaudeCli] {
        assert!(servers(&at, file).is_empty(), "{file:?}");
    }
    let lines = forgotten_log(&at, id, &gone);
    assert!(matches!(&lines[0], Logged::Info(l) if l.contains("Windows package") && l.contains("installer") && l.contains("Claude Code")), "{lines:?}");
    assert_eq!(lines.len(), 4, "{lines:?}");
    let again = forgotten_log(&at, id, &forget(&at, id));
    assert!(matches!(again.last(), Some(Logged::Info(l)) if l.contains("was in none of")), "{again:?}");
}

#[test]
fn a_broken_plain_file_is_not_copied_into_the_package() {
    let (_dir, at) = packaged_computer();
    let private = at.path(ConfigFile::ClaudeAppPackaged).unwrap();
    std::fs::remove_file(&private).unwrap();
    let id = Uuid::new_v4();
    let k = key(id);
    let plain = at.path(ConfigFile::ClaudeApp).unwrap();
    std::fs::create_dir_all(plain.parent().unwrap()).unwrap();
    for broken in ["{ \"mcpServers\": {}, }", "[1, 2]"] {
        std::fs::write(at.path(ConfigFile::ClaudeApp).unwrap(), broken).unwrap();
        let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
        assert_eq!(added.failed.len(), 2, "{broken}: the plain file and the copy both fail: {:?}", added.failed);
        assert!(!private.exists(), "{broken}: no copy left for the app to read in place of the plain file");
        let left: Vec<_> = std::fs::read_dir(private.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert!(left.is_empty(), "{broken}: nothing else left in the package's folder: {left:?}");
        assert!(added.carried.is_empty());
        assert_eq!(std::fs::read_to_string(at.path(ConfigFile::ClaudeApp).unwrap()).unwrap(), broken, "the plain file is left alone");
    }
}

#[test]
fn a_plain_file_that_is_not_settings_is_never_copied() {
    // The check itself, apart from the clean-up after a failed change.
    let dir = tempfile::tempdir().unwrap();
    let (from, to) = (dir.path().join("plain.json"), dir.path().join("private/copy.json"));
    for broken in ["{ \"a\": 1, }", "[1, 2]"] {
        std::fs::write(&from, broken).unwrap();
        assert!(client_config::copy_settings(&from, &to).is_err(), "{broken}");
        assert!(!to.exists(), "{broken}");
    }
}

#[cfg(unix)]
#[test]
fn the_packaged_copy_keeps_the_plain_files_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, at) = packaged_computer();
    std::fs::remove_file(at.path(ConfigFile::ClaudeAppPackaged).unwrap()).unwrap();
    write(&at, ConfigFile::ClaudeApp, serde_json::json!({ "preferences": {} }));
    let plain = at.path(ConfigFile::ClaudeApp).unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o600)).unwrap();
    let id = Uuid::new_v4();
    let k = key(id);
    let added = add(&at, ClientApp::Claude, entry("pimble-claude", &k), id);
    assert_eq!(added.carried, vec![ConfigFile::ClaudeAppPackaged]);
    let mode = std::fs::metadata(at.path(ConfigFile::ClaudeAppPackaged).unwrap()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "it holds an agent key");
}

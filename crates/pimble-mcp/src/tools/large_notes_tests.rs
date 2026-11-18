//! Long notes (docs/MCP_CONTRACT.md "Reading part of a note", "split_note",
//! "search"): an outline, a section by its heading path, a size warning, a
//! split into child notes that keeps each section's Markdown, search hits
//! that say where they are, and an unscoped search that puts the store this
//! session last worked in first.

use std::sync::Arc;

use pimble_client::PimbleClient;
use pimble_server::{PimbleServer, ServerConfig};
use rmcp::handler::server::wrapper::Parameters;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::Pimble;
use crate::connection::Connection;

fn p<T: DeserializeOwned>(v: serde_json::Value) -> Parameters<T> {
    Parameters(serde_json::from_value(v).expect("params"))
}

/// Ten sections, with tables, code, nested lists, quotes and a subsection.
fn long_note() -> (String, Vec<(String, String)>) {
    let intro = "An introduction that stays in the note.\n\nWith a **second** paragraph.".to_string();
    let mut sections = Vec::new();
    for i in 1..=10 {
        let body = match i % 5 {
            0 => format!("| Name | Count |\n| --- | --- |\n| apples | {i} |\n| pears | {} |", i * 2),
            1 => format!("```rust\nfn section_{i}() -> u32 {{\n    {i}\n}}\n```\n\nAfter the code."),
            2 => "- one\n  - one point one\n    - deeper\n- two\n\n1. first\n2. second".to_string(),
            3 => format!("> A quote in section {i}.\n\nText with *italic*, `code` and a [link](https://example.com/{i})."),
            _ => format!("Plain words for section {i}.\n\n### A subsection {i}\n\nUnder the subsection.\n\n---\n\nAfter a rule."),
        };
        sections.push((format!("Section {i}"), body));
    }
    let mut md = intro.clone();
    for (title, body) in &sections {
        md.push_str(&format!("\n\n## {title}\n\n{body}"));
    }
    (md, sections)
}

#[tokio::test]
async fn outlines_sections_and_split_note_on_a_long_note() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        replicas_dir: Some(dir.path().join("replicas")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    service.create_store(dir.path().join("notes.pimble"), "Notes").await.unwrap();
    let pimble = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));

    let (md, sections) = long_note();
    pimble.create_node(p(json!({ "parent": "Notes", "title": "Atlas", "markdown": md }))).await.unwrap();
    let before = pimble.get_node(p(json!({ "node": "Notes/Atlas" }))).await.unwrap();
    let link = before.lines().find_map(|l| l.strip_prefix("link: ")).unwrap().to_string();

    // The outline: every heading, its block and size, no content.
    let outline = pimble.get_node(p(json!({ "node": "Notes/Atlas", "outline": true }))).await.unwrap();
    assert!(outline.contains("- ## Section 1 (block 2,") && outline.contains("  - ### A subsection 4"), "{outline}");
    assert!(outline.contains("(before the first heading: blocks 0-1"), "{outline}");
    assert!(!outline.contains("After the code."), "{outline}");

    // A section by its text, and a nested one by its heading path.
    let one = pimble.get_node(p(json!({ "node": "Notes/Atlas", "section": "Section 5" }))).await.unwrap();
    assert!(one.contains("section: Section 5 (") && one.contains("| apples | 5 |") && !one.contains("Section 6"), "{one}");
    let nested = pimble.get_node(p(json!({ "node": "Notes/Atlas", "section": "Section 4 > A subsection 4" }))).await.unwrap();
    assert!(nested.contains("section: Section 4 > A subsection 4") && nested.contains("Under the subsection."), "{nested}");
    let several = pimble.get_node(p(json!({ "node": "Notes/Atlas", "section": "subsection" }))).await.unwrap_err();
    assert!(several.contains("\"Section 4 > A subsection 4\"") && several.contains("\"Section 9 > A subsection 9\""), "{several}");
    let blocks = pimble.get_node(p(json!({ "node": "Notes/Atlas", "blocks": "0-1" }))).await.unwrap();
    assert!(blocks.contains("An introduction") && !blocks.contains("Section 1"), "{blocks}");

    // A long note says so first, and still shows all of it.
    let long = format!("{}\n\n{}", "A long paragraph of words. ".repeat(700), "## The end\n\nLast.");
    pimble.create_node(p(json!({ "parent": "Notes", "title": "Long", "markdown": long }))).await.unwrap();
    let read = pimble.get_node(p(json!({ "node": "Notes/Long" }))).await.unwrap();
    assert!(read.starts_with("(This note is ") && read.contains("`outline: true`") && read.contains("Last."), "{}", &read[..300]);
    assert!(!before.starts_with("(This note is "), "the atlas is short");

    // Each section as the note holds it before the split.
    let mut was = Vec::new();
    for (title, _) in &sections {
        let read = pimble.get_node(p(json!({ "node": "Notes/Atlas", "section": title }))).await.unwrap();
        let body = read.split_once("---\n").unwrap().1.strip_prefix(&format!("## {title}\n\n")).unwrap().trim_end().to_string();
        was.push(body);
    }

    // Split at its top headings, with links left behind.
    let split = pimble.split_note(p(json!({ "node": "Notes/Atlas", "links": true }))).await.unwrap();
    assert!(split.contains("at its level-2 headings into 10 notes"), "{split}");
    let kept = pimble.get_node(p(json!({ "node": link.clone() }))).await.unwrap();
    assert!(kept.contains("title: Atlas") && kept.contains("children: 10") && kept.contains("An introduction"), "{kept}");
    assert!(!kept.contains("## Section"), "{kept}");
    for ((title, written), was) in sections.iter().zip(&was) {
        let child = pimble.get_node(p(json!({ "node": format!("Notes/Atlas/{title}") }))).await.unwrap();
        let content = child.split_once("---\n").unwrap().1.trim_end();
        // Byte for byte what the section was before the split; and what was
        // written in it, but for the blank lines rinch writes inside a nested
        // list (the Markdown round trip's own).
        assert_eq!(content, was, "{title}");
        let lines = |md: &str| md.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect::<Vec<_>>();
        assert_eq!(lines(content), lines(written), "{title}");
        assert!(kept.contains(&format!("- [{title}](pimble:")), "{kept}");
    }
    let refused = pimble.split_note(p(json!({ "node": "Notes/Atlas/Section 1" }))).await.unwrap_err();
    assert!(refused.contains("no headings"), "{refused}");
    let _ = server.stop().await;
}

#[tokio::test]
async fn search_says_where_and_puts_the_last_store_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        replicas_dir: Some(dir.path().join("replicas")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    service.create_store(dir.path().join("a.pimble"), "Alpha").await.unwrap();
    service.create_store(dir.path().join("b.pimble"), "Beta").await.unwrap();
    // Written by another session, so this one has worked in no store yet.
    let writer = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));
    let strong = "lighthouse lighthouse lighthouse";
    for store in ["Alpha", "Beta"] {
        for i in 0..2 {
            let md = if store == "Beta" { "# Design\n\n## The coast\n\nA lighthouse.".to_string() } else { format!("{strong} {i}") };
            writer.create_node(p(json!({ "parent": store, "title": format!("{store} {i}"), "markdown": md }))).await.unwrap();
        }
    }
    let pimble = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));
    let mut found = String::new();
    for _ in 0..60 {
        found = pimble.search(p(json!({ "query": "lighthouse" }))).await.unwrap_or_default();
        if found.matches("\n- ").count() + found.starts_with("- ") as usize >= 4 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    // No store touched: the ranking as it was (Alpha's denser notes first), no line.
    assert!(!found.contains("first (the store"), "{found}");
    assert!(found.find("Alpha 0").unwrap() < found.find("Beta 0").unwrap(), "{found}");
    // Each hit says where: a heading path get_node's `section` takes.
    assert!(found.contains("  in: Design > The coast\n"), "{found}");
    let section = pimble.get_node(p(json!({ "node": "Beta/Beta 0", "section": "Design > The coast" }))).await.unwrap();
    assert!(section.contains("A lighthouse."), "{section}");

    // Having read in Beta, its hits come first, and the answer says so.
    let found = pimble.search(p(json!({ "query": "lighthouse" }))).await.unwrap();
    assert!(found.starts_with("Hits from Beta first (the store this session last worked in); pass `store` to search one store.\n"), "{found}");
    let last_beta = found.rfind("Beta ").unwrap();
    assert!(found.find("Alpha ").unwrap() > last_beta, "{found}");
    // A store named searches that store, and says nothing of the order.
    let only = pimble.search(p(json!({ "query": "lighthouse", "store": "Alpha" }))).await.unwrap();
    assert!(!only.contains("Beta") && !only.contains("first (the store"), "{only}");
    let _ = server.stop().await;
}

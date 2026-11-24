//! The text edits through the tools, against a real server
//! (docs/MCP_CONTRACT.md "Naming what an edit touches", "What an edit says it
//! did", "Dry runs"): a block named by its number, a number-like quote that
//! removes nothing, an ambiguous quote refused with every place, `occurrence`,
//! the echo in every answer, a dry run that writes nothing, the old parameter
//! names refused rather than read as something else, `expect` on a block
//! number, and a heading or text removed by too little of it refused.

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

fn parse<T: DeserializeOwned>(v: serde_json::Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

async fn read(pimble: &Pimble) -> String {
    pimble.get_node(p(json!({ "node": "Notes/List" }))).await.unwrap()
}

const NOTE: &str = "# Shopping\n\nApples for the pie.\n\nWe need 13 eggs for the cake.\n\nFlour, the strong kind.\n\nAsk about block 13 at the shop.";

/// A server in `dir` with the store "Notes" holding the node "List" of `note`.
async fn serve(dir: &std::path::Path, note: &str) -> (PimbleServer, Pimble) {
    let mut server = PimbleServer::with_config(ServerConfig { ..ServerConfig::in_dir(dir) });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    service.create_store(dir.join("notes.pimble"), "Notes").await.unwrap();
    let pimble = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));
    pimble.create_node(p(json!({ "parent": "Notes", "title": "List", "markdown": note }))).await.unwrap();
    (server, pimble)
}

#[tokio::test]
async fn edits_name_blocks_surely_and_say_what_they_took_out() {
    let dir = tempfile::tempdir().unwrap();
    let (mut server, pimble) = serve(dir.path(), NOTE).await;

    // The incident: "13" meant block 13. Now it removes nothing.
    let refused = pimble.replace_block(p(json!({ "node": "Notes/List", "quote": "13", "markdown": "" }))).await.unwrap_err();
    assert!(refused.contains("reads like a block number") && refused.contains("block_index"), "{refused}");
    assert!(read(&pimble).await.contains("We need 13 eggs"));
    // The old parameter names are refused, never read as a quote.
    assert!(parse::<super::text_edit_tools::ReplaceBlockParams>(json!({ "node": "x", "block": "13" })).err().unwrap().contains("unknown field `block`"));
    assert!(parse::<super::text_edit_tools::InsertAfterParams>(json!({ "node": "x", "after": "a", "markdown": "b" })).err().unwrap().contains("unknown field `after`"));
    // Both ways of naming, or neither, are refused.
    let both = pimble.replace_block(p(json!({ "node": "Notes/List", "quote": "Apples", "block_index": 1, "markdown": "x" }))).await.unwrap_err();
    assert!(both.contains("not both"), "{both}");
    let neither = pimble.remove_section(p(json!({ "node": "Notes/List" }))).await.unwrap_err();
    assert!(neither.contains("Name the heading"), "{neither}");

    // A dry run says what would go and writes nothing.
    let before = read(&pimble).await;
    let dry = pimble.replace_block(p(json!({ "node": "Notes/List", "block_index": 2, "markdown": "", "dry_run": true }))).await.unwrap();
    assert!(dry.starts_with("Dry run, nothing written: this would remove block 2"), "{dry}");
    assert!(dry.contains("was: \"We need 13 eggs for the cake.\"") && dry.contains("before it: \"Apples for the pie.\""), "{dry}");
    assert_eq!(read(&pimble).await, before);

    // By its number, the block goes, and the answer says what it was.
    let removed = pimble.replace_block(p(json!({ "node": "Notes/List", "block_index": 2, "markdown": "" }))).await.unwrap();
    assert!(removed.starts_with("Removed block 2 of pimble:"), "{removed}");
    assert!(removed.contains("was: \"We need 13 eggs for the cake.\"") && removed.contains("after it: \"Flour, the strong kind.\""), "{removed}");
    assert!(!read(&pimble).await.contains("13 eggs"));

    // A quote in several places is refused with each; `occurrence` picks one.
    pimble.append(p(json!({ "node": "Notes/List", "markdown": "Flour, the strong kind." }))).await.unwrap();
    let ambiguous = pimble.replace_block(p(json!({ "node": "Notes/List", "quote": "Flour, the strong", "markdown": "Rye flour." }))).await.unwrap_err();
    assert!(ambiguous.contains("appears 2 times") && ambiguous.contains("(1) block 2") && ambiguous.contains("(2) block 4"), "{ambiguous}");
    let picked = pimble.replace_block(p(json!({ "node": "Notes/List", "quote": "Flour, the strong", "occurrence": "last", "markdown": "Rye flour." }))).await.unwrap();
    assert!(picked.starts_with("Replaced block 4"), "{picked}");
    let text = pimble.replace_text(p(json!({ "node": "Notes/List", "quote": "strong", "occurrence": 1, "text": "white" }))).await.unwrap();
    assert!(text.contains("was: \"strong\"") && text.contains("before it: \"Flour, the \""), "{text}");
    assert!(read(&pimble).await.contains("Flour, the white kind.\n\nAsk about block 13 at the shop.\n\nRye flour."));

    // A section by its heading's number; insert_after by a block's.
    let inserted = pimble.insert_after(p(json!({ "node": "Notes/List", "block_index": 0, "markdown": "Bring bags." }))).await.unwrap();
    assert!(inserted.contains("after block 0") && inserted.contains("before it: \"Shopping\""), "{inserted}");
    let dry = pimble.remove_section(p(json!({ "node": "Notes/List", "block_index": 0, "dry_run": true }))).await.unwrap();
    assert!(dry.contains("this would remove blocks 0-5"), "{dry}");
    let not_heading = pimble.replace_section(p(json!({ "node": "Notes/List", "block_index": 1, "markdown": "x" }))).await.unwrap_err();
    assert!(not_heading.contains("not a heading"), "{not_heading}");

    // A whole rewrite lists each changed stretch.
    let content = pimble.replace_content(p(json!({ "node": "Notes/List", "markdown": "# Shopping\n\nBring bags.\n\nApples for the pie." }))).await.unwrap();
    assert!(content.contains("changed stretch") && content.contains("was \"Flour, the white kind."), "{content}");
    let _ = server.stop().await;
}

#[tokio::test]
async fn a_block_number_is_checked_and_loose_names_remove_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut server, pimble) = serve(dir.path(), "# Plan\n\nintro\n\n## Phase 3\n\nbuild it\n\nWe need 13 eggs.").await;
    // `expect` catches a number that names another block now.
    let stale = pimble.replace_block(p(json!({ "node": "Notes/List", "block_index": 1, "expect": "build", "markdown": "" }))).await.unwrap_err();
    assert!(stale.contains("block 1 does not contain \"build\"") && stale.contains("reads \"intro\""), "{stale}");
    let with_quote = pimble.replace_block(p(json!({ "node": "Notes/List", "quote": "intro", "expect": "intro", "markdown": "x" }))).await.unwrap_err();
    assert!(with_quote.contains("with `quote` leave it out"), "{with_quote}");
    let with_heading = pimble.remove_section(p(json!({ "node": "Notes/List", "heading": "Plan", "expect": "Plan" }))).await.unwrap_err();
    assert!(with_heading.contains("with `heading` leave it out"), "{with_heading}");
    let heading = pimble.replace_section(p(json!({ "node": "Notes/List", "block_index": 2, "expect": "Phase", "markdown": "rebuilt", "dry_run": true }))).await.unwrap();
    assert!(heading.starts_with("Dry run"), "{heading}");
    // A heading named by a short part of its text is not removed or emptied.
    let loose = pimble.remove_section(p(json!({ "node": "Notes/List", "heading": "3" }))).await.unwrap_err();
    assert!(loose.contains("\"Phase 3\" (block 2)") && loose.contains("block number, 2"), "{loose}");
    let empty = pimble.replace_section(p(json!({ "node": "Notes/List", "heading": "Phase", "markdown": "" }))).await.unwrap_err();
    assert!(empty.contains("to empty that section"), "{empty}");
    // Text deleted by a short quote: refused, and the words around it work.
    let short = pimble.replace_text(p(json!({ "node": "Notes/List", "quote": "13 ", "text": "" }))).await.unwrap_err();
    assert!(short.contains("Nothing was deleted") && short.contains("words either side"), "{short}");
    pimble.replace_text(p(json!({ "node": "Notes/List", "quote": "need 13 eggs", "text": "need eggs" }))).await.unwrap();
    pimble.remove_section(p(json!({ "node": "Notes/List", "block_index": 2, "expect": "Phase 3" }))).await.unwrap();
    let left = read(&pimble).await;
    assert!(left.trim_end().ends_with("---\n# Plan\n\nintro"), "{left}");
    let _ = server.stop().await;
}

//! A plugin's tools run for an agent with no window
//! (docs/AGENT_GUIDES_CONTRACT.md section 8): the built `hello` plugin's tools
//! through `pimble_plugin_host::tools::run`, over a real server.
#![cfg(feature = "wasm-host")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use pimble_api::rpc::RpcBackend;
use pimble_client::PimbleClient;
use pimble_core::PimbleUrl;
use pimble_plugin_abi::guide::ToolRequest;
use pimble_plugin_host::tools::{agent_approval, run};
use pimble_plugin_host::wasmtime_engine::load_file;
use serde_json::json;

use common::{built_plugin, fixture};

#[test]
fn hellos_tools_answer_read_write_and_check() {
    let Some(path) = built_plugin("hello_viewer") else { return };
    let f = fixture("hello");
    let plugin = load_file(&path, Duration::from_secs(2)).expect("the plugin loads");
    let board = PimbleUrl::node(f.store, f.board).to_string();
    let plan = PimbleUrl::node(f.store, f.plan).to_string();
    let url = f.url.clone();

    // One thread and a LocalSet, as the MCP runs a tool.
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    let answers = local.block_on(&rt, async {
        let backend: Arc<dyn pimble_api::Backend> = Arc::new(RpcBackend::over(PimbleClient::connect(&url).await.unwrap()));
        let ask = |tool: &str, args: serde_json::Value| ToolRequest { tool: tool.into(), args };
        let mut out = Vec::new();
        for request in [
            ask("echo", json!({ "text": "hi" })),
            ask("count_children", json!({ "node": board })),
            ask("rename", json!({ "node": plan, "title": "Plan B" })),
            ask("check_field", json!({ "field": "mood", "value": "bad" })),
            ask("check_field", json!({ "field": "mood", "value": "sunny" })),
            ask("nothing", json!({})),
        ] {
            out.push(run(&plugin, backend.clone(), &request).await.expect("the tool answers"));
        }
        out
    });
    let said: Vec<(bool, &str)> = answers.iter().map(|a| (a.ok, a.said.as_str())).collect();
    assert_eq!(
        said,
        vec![
            (true, "hi"),
            (true, "2 children"),
            (true, "renamed to Plan B"),
            (false, "a mood cannot be bad"),
            (true, "sunny is a fine mood"),
            (false, "hello has no tool nothing"),
        ]
    );
    let renamed = f.rt.block_on(f.service.get_node(f.store, f.plan)).unwrap();
    assert_eq!(renamed.metadata.title, "Plan B", "the write went through the server");
    // What a tool may do comes from the manifest: hello asks to write the stores chosen.
    let approval = agent_approval(&plugin.manifest);
    assert!(approval.write);
}

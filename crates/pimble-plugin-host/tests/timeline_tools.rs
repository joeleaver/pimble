//! The timeline's tools for agents (docs/AGENT_GUIDES_CONTRACT.md section 8),
//! run headless on the built plugin over a real server: dates read as the
//! viewer reads them, and timeline and calendar settings checked before an
//! agent writes them.
#![cfg(feature = "wasm-host")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use pimble_api::rpc::RpcBackend;
use pimble_api::Addr;
use pimble_client::PimbleClient;
use pimble_core::PimbleUrl;
use pimble_plugin_abi::guide::{ToolAnswer, ToolRequest};
use pimble_plugin_host::tools::run;
use pimble_plugin_host::wasmtime_engine::load_file;
use serde_json::{json, Value};

use common::{built_plugin, fixture};

#[test]
fn the_timeline_reads_dates_and_checks_settings_for_an_agent() {
    let Some(path) = built_plugin("timeline_viewer") else { return };
    let f = fixture("timeline");
    let plugin = load_file(&path, Duration::from_secs(5)).expect("the plugin loads");
    let guide: Value = serde_json::from_slice(include_bytes!("../../../plugins/timeline-viewer/guide.json")).unwrap();
    let calendar = guide["viewers"][1]["settings"]["example"].clone();
    let timeline = PimbleUrl::node(f.store, f.board).to_string();
    let plain = PimbleUrl::node(f.store, f.plan).to_string();

    // A calendar node, made as the MCP makes one: a metanode shown with the
    // calendar viewer, its definition its settings.
    let cal_node = f.rt.block_on(async {
        let node = f.service.create_node(f.store, Some(f.board), "document", "Reckoning").await.unwrap();
        let addr = Addr::new(f.store, node);
        f.app.set_metanode(addr, Some("calendar"), None).await.unwrap();
        let def = calendar.clone();
        f.app.edit(addr, move |doc, _| -> Result<(), pimble_api::ApiError> { Ok(doc.set_data("calendar", &def)?) }).await.unwrap();
        node
    });
    let cal_link = PimbleUrl::node(f.store, cal_node).to_string();
    let url = f.url.clone();

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    let ask = |tool: &str, args: Value| ToolRequest { tool: tool.into(), args };
    let mut bad_season = calendar.clone();
    bad_season["seasons"] = json!([{ "name": "Wet", "from": "Mudmonth", "to": "Rainmonth" }]);
    let requests = vec![
        ask("check_field", json!({ "viewer": "timeline", "metanode": timeline, "field": "date", "value": "Mar 15, 44 BC" })),
        ask("check_field", json!({ "viewer": "timeline", "metanode": timeline, "field": "date", "value": "Blorpuary 99, banana" })),
        ask("check_field", json!({ "viewer": "timeline", "metanode": timeline, "field": "mood", "value": "anything" })),
        ask("check_settings", json!({ "viewer": "calendar", "settings": calendar })),
        ask("check_settings", json!({ "viewer": "calendar", "settings": bad_season })),
        ask("check_settings", json!({ "viewer": "timeline", "settings": { "calendar": "not a link" } })),
        ask("check_settings", json!({ "viewer": "timeline", "settings": { "calendar": plain } })),
        ask("check_settings", json!({ "viewer": "timeline", "settings": { "calendar": cal_link, "also": ["gregorian"] } })),
        ask("read_date", json!({ "calendar": cal_link, "text": "Fir 3, 1300 YF" })),
        ask("read_date", json!({ "calendar": cal_link, "text": "Year's Gate, 12 BF" })),
        ask("read_date", json!({ "text": "c. 1066" })),
    ];
    let answers: Vec<ToolAnswer> = local.block_on(&rt, async {
        let backend: Arc<dyn pimble_api::Backend> = Arc::new(RpcBackend::over(PimbleClient::connect(&url).await.unwrap()));
        let mut out = Vec::new();
        for r in &requests {
            out.push(run(&plugin, backend.clone(), r).await.expect("the tool answers"));
        }
        out
    });
    let a = |i: usize| (answers[i].ok, answers[i].said.as_str());
    for (i, answer) in answers.iter().enumerate() {
        eprintln!("{i}: {} {}", answer.ok, answer.said);
    }
    assert_eq!(a(0), (true, "\u{201c}Mar 15, 44 BC\u{201d} reads as Mar 15, 44 BC in the Gregorian calendar."));
    assert!(!a(1).0 && a(1).1.contains("does not read as a date in the Gregorian calendar"), "{:?}", a(1));
    assert_eq!(a(2), (true, "The timeline does not read mood."));
    assert!(a(3).0 && a(3).1.starts_with("the Reckoning of the Founding calendar reads: 181 days in a year (183 in a leap year") && a(3).1.contains("The absolute year 0 is written \u{201c}1\u{201d} and -1 \u{201c}1 BF\u{201d}"), "{:?}", a(3));
    assert_eq!(a(4), (false, "the season Wet names the month Mudmonth, which the year does not have"));
    assert!(!a(5).0 && a(5).1.starts_with("calendar must be a calendar node's pimble: link"), "{:?}", a(5));
    assert!(!a(6).0 && a(6).1.contains("is not a calendar"), "{:?}", a(6));
    assert_eq!(a(7), (true, "It counts in the Reckoning of the Founding calendar, with the Gregorian calendar beside it."));
    assert!(a(8).0 && a(8).1.contains("reads as Fir 3, 1300") && a(8).1.contains("in the Gregorian calendar"), "{:?}", a(8));
    assert!(a(9).0 && a(9).1.contains("Year's Gate"), "{:?}", a(9));
    assert!(a(10).0 && a(10).1.contains("1066"), "{:?}", a(10));
}

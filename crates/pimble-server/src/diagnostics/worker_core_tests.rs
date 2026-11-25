use std::sync::Mutex;
use std::time::Duration;

use pimble_host::http::{HttpError, HttpRequest, HttpResponse};
use serde_json::json;
use tokio::sync::Notify;

use super::*;

/// Every wait in these tests ends by this.
const DEADLINE: Duration = Duration::from_secs(10);

fn dir() -> PathBuf {
    PathBuf::from("/data/diagnostics")
}

fn alice() -> SendAs<'static> {
    SendAs { url: "https://cloud.test", session: "alice-session", user_id: "alice-id" }
}

fn bob() -> SendAs<'static> {
    SendAs { url: "https://cloud.test", session: "bob-session", user_id: "bob-id" }
}

/// Pimble Cloud's records endpoint: takes everything, keeps what it was sent
/// (the bearer and the records' kinds), and, when `held`, holds each request
/// until the test lets it go.
#[derive(Debug, Default)]
struct Service {
    sent: Mutex<Vec<(Option<String>, Vec<Value>)>>,
    held: bool,
    arrived: Notify,
    release: Notify,
}

impl Service {
    fn holding() -> Self {
        Service { held: true, ..Service::default() }
    }

    fn sent(&self) -> Vec<(Option<String>, Vec<Value>)> {
        self.sent.lock().unwrap().clone()
    }

    /// The kinds and accounts of every record sent, per request.
    fn bearers(&self) -> Vec<Option<String>> {
        self.sent().into_iter().map(|(bearer, _)| bearer).collect()
    }
}

#[async_trait::async_trait]
impl Http for Service {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let body = String::from_utf8(request.body.expect("a body").bytes).unwrap();
        let lines = body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        self.sent.lock().unwrap().push((request.bearer, lines));
        if self.held {
            self.arrived.notify_one();
            self.release.notified().await;
        }
        Ok(HttpResponse { status: 200, body: Vec::new() })
    }

    async fn websocket_close_code(&self, _url: &str, _token: &str) -> Option<u16> {
        None
    }
}

async fn waiting(disk: &Disk) -> usize {
    Queue::new(disk.clone(), dir()).waiting().await.len()
}

#[tokio::test]
async fn nothing_is_sent_before_the_page_says_or_while_off() {
    let disk = Disk::memory();
    let service = Service::default();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    assert_eq!(d.attach(&disk, dir()).await, Applied::Done);
    d.record("stores", json!({ "stores": 1 }), "t0".into());
    d.pump(&disk, &service, Some(alice()), 1).await;
    assert!(service.sent().is_empty(), "the page has not said");
    assert_eq!(waiting(&disk).await, 0, "nor is anything written");
    assert!(d.ids().is_none());

    assert_eq!(d.configure(&disk, false, None).await, Applied::Done);
    d.record("stores", json!({ "stores": 2 }), "t1".into());
    d.pump(&disk, &service, Some(alice()), 2).await;
    assert!(service.sent().is_empty(), "off");
    assert_eq!(waiting(&disk).await, 0);
    assert!(d.ids().is_none());

    assert_eq!(d.configure(&disk, true, None).await, Applied::FirstOn, "on for the first time, even after an off");
    d.record("stores", json!({ "stores": 3 }), "t2".into());
    d.pump(&disk, &service, Some(alice()), 3).await;
    let sent = service.sent();
    assert_eq!(sent.len(), 1);
    let records: Vec<i64> = sent[0].1.iter().map(|r| r["data"]["stores"].as_i64().unwrap()).collect();
    assert_eq!(records, [3], "what was recorded before the off went with it");
    assert!(d.ids().is_some());
    assert_eq!(d.configure(&disk, true, None).await, Applied::Done, "the program is said once");
}

#[tokio::test]
async fn an_off_during_a_pump_sends_nothing_after_it_and_leaves_nothing() {
    let disk = Disk::memory();
    let service = Service::holding();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    d.attach(&disk, dir()).await;
    d.configure(&disk, true, None).await;
    // Three batches wait: two left by earlier pumps (signed out), one taken
    // by the pump the off interrupts.
    for millis in [1, 2] {
        d.record("stores", json!({ "at": millis }), "t".into());
        d.pump(&disk, &service, None, millis).await;
    }
    assert_eq!(waiting(&disk).await, 2);
    d.record("stores", json!({ "at": 3 }), "t".into());

    let pump = d.pump(&disk, &service, Some(alice()), 3);
    let off = async {
        // The pump is suspended in its first request when the person saves
        // "off".
        tokio::time::timeout(DEADLINE, service.arrived.notified()).await.expect("the pump's first request");
        let configured = d.configure(&disk, false, None);
        tokio::pin!(configured);
        // The off waits for the pump: poll it once, then let the request go.
        assert!(futures::poll!(configured.as_mut()).is_pending(), "the off waits for the pump in flight");
        assert!(!d.enabled(), "but the switch is off at once");
        service.release.notify_one();
        configured.await
    };
    let ((), applied) = tokio::time::timeout(DEADLINE, async { tokio::join!(pump, off) }).await.expect("the pump and the off end");
    assert_eq!(applied, Applied::Done);
    assert_eq!(service.sent().len(), 1, "only the request already in flight when the switch went off");
    assert_eq!(waiting(&disk).await, 0, "nothing is left on the disk");

    d.record("stores", json!({}), "t".into());
    d.pump(&disk, &service, Some(alice()), 4).await;
    assert_eq!(service.sent().len(), 1, "and nothing after");
}

#[tokio::test]
async fn batches_never_cross_accounts() {
    let disk = Disk::memory();
    let service = Service::default();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    d.attach(&disk, dir()).await;
    d.configure(&disk, true, None).await;
    d.set_account(Some("alice-id".into()));
    d.record("stores", json!({ "by": "alice" }), "t".into());
    // Alice is signed out before a pump sends her records.
    d.pump(&disk, &service, None, 1).await;
    d.set_account(None);
    d.record("stores", json!({ "by": "no one" }), "t".into());
    d.set_account(Some("bob-id".into()));
    d.record("stores", json!({ "by": "bob" }), "t".into());
    d.pump(&disk, &service, Some(bob()), 2).await;

    let sent = service.sent();
    for (bearer, records) in &sent {
        assert_eq!(bearer.as_deref(), Some("bob-session"));
        for record in records {
            assert_ne!(record["account"], "alice-id", "Alice's records are never sent as Bob: {record}");
        }
    }
    let by: Vec<&str> = sent.iter().flat_map(|(_, r)| r).filter_map(|r| r["data"]["by"].as_str()).collect();
    assert_eq!(by, ["no one", "bob"]);
    assert_eq!(waiting(&disk).await, 1, "Alice's batch waits for her");

    d.pump(&disk, &service, Some(alice()), 3).await;
    assert_eq!(service.bearers().last().cloned().flatten().as_deref(), Some("alice-session"));
    let last = service.sent().pop().unwrap().1;
    assert!(last.iter().filter(|r| r["kind"] == "stores").all(|r| r["account"] == "alice-id"));
    assert_eq!(waiting(&disk).await, 0);
}

#[tokio::test]
async fn a_choice_before_the_queue_s_folder_is_applied_with_it() {
    let disk = Disk::memory();
    let service = Service::default();
    let page_install = "c".repeat(32);

    // Off before the folder: the switch turns at once, and what an earlier
    // run left is cleared once the folder is known.
    let earlier = Queue::new(disk.clone(), dir());
    earlier.append(&["{\"kind\":\"left\"}".into()], 1, "old", None).await.unwrap();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    assert_eq!(d.configure(&disk, false, Some(page_install.clone())).await, Applied::Done);
    assert!(!d.enabled());
    assert_eq!(waiting(&disk).await, 1, "nothing is touched without the folder");
    assert_eq!(d.attach(&disk, dir()).await, Applied::Done);
    assert_eq!(waiting(&disk).await, 0, "the off is applied once the folder is known");
    d.pump(&disk, &service, Some(alice()), 2).await;
    assert!(service.sent().is_empty());

    // On before the folder: the page's install number is used once it is.
    let d = WorkerDiagnostics::new("1.0.0", "web");
    assert_eq!(d.configure(&disk, true, Some(page_install.clone())).await, Applied::Done, "nothing to apply to yet");
    assert!(d.ids().is_none());
    assert_eq!(d.attach(&disk, dir()).await, Applied::FirstOn);
    assert_eq!(d.ids().map(|(install, _)| install), Some(page_install.clone()));
    d.record("stores", json!({}), "t".into());
    d.pump(&disk, &service, Some(alice()), 3).await;
    let sent = service.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1[0]["install"], page_install.as_str());
}

#[tokio::test]
async fn a_report_carries_the_install_in_use_and_the_worker_s_log() {
    let disk = Disk::memory();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    assert!(d.dir().is_none());
    d.log("WARN", Some(Severity::Warning), "pimble_server::vault_link", "lost the link", "t".into());
    let before = d.report("a".repeat(32), "t", "note", Vec::new(), json!([]));
    assert_eq!(before["install"], "a".repeat(32), "before a choice, the one handed over");
    d.attach(&disk, dir()).await;
    assert_eq!(d.dir(), Some(dir()));
    let page_install = "d".repeat(32);
    d.configure(&disk, true, Some(page_install.clone())).await;
    let report = d.report("a".repeat(32), "t", "note", vec![("page.log".into(), "from the page".into())], json!([]));
    assert_eq!(report["install"], page_install.as_str(), "the install in use");
    assert_eq!(report["logs"][1]["name"], "worker.log");
    assert!(d.recent_log().contains("lost the link"));
    assert!(report["logs"][1]["text"].as_str().unwrap().contains("lost the link"));
}

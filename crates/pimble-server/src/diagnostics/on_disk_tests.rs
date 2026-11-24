use std::sync::Mutex;

use pimble_host::http::{HttpError, HttpResponse};

use super::*;

/// Answers each call with the next status (200 once they run out), and
/// keeps what it was sent.
#[derive(Debug, Default)]
struct Service {
    answers: Mutex<Vec<u16>>,
    sent: Mutex<Vec<(String, Option<String>, &'static str, String)>>,
}

impl Service {
    fn answering(statuses: &[u16]) -> Self {
        Service { answers: Mutex::new(statuses.iter().rev().copied().collect()), sent: Mutex::default() }
    }
}

#[async_trait::async_trait]
impl Http for Service {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let body = request.body.expect("a body");
        self.sent.lock().unwrap().push((request.url, request.bearer, body.content_type, String::from_utf8(body.bytes).unwrap()));
        let status = self.answers.lock().unwrap().pop().unwrap_or(200);
        if status == 0 {
            return Err(HttpError("offline".into()));
        }
        Ok(HttpResponse { status, body: br#"{"report":"ABCD1234"}"#.to_vec() })
    }

    async fn websocket_close_code(&self, _url: &str, _token: &str) -> Option<u16> {
        None
    }
}

fn lines_of(text: &str) -> Vec<Value> {
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[test]
fn records_wait_for_the_install_and_carry_the_account() {
    let mut r = Recorder::new("1.2.3", "web");
    r.record("system", json!({ "program": "worker" }), "t0".into());
    r.set_account(Some("u-1".into()));
    r.log("WARN", Some(Severity::Warning), "pimble_server::vault_link", "lost a@b.co", "t1".into());
    r.log("INFO", None, "pimble_store", "opened", "t2".into());
    let lines: Vec<Value> = r.take(&"a".repeat(32)).iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "the info line of another target is not recorded");
    assert_eq!(lines[0]["kind"], "system");
    assert!(lines[0].get("account").is_none(), "taken before the account was known");
    assert_eq!(lines[1]["account"], "u-1");
    assert_eq!(lines[1]["data"]["message"], "lost <email>");
    assert_eq!(lines[1]["install"], "a".repeat(32));
    assert_eq!(lines[1]["os"], "web");
    assert!(r.take("x").is_empty(), "taken once");
    assert_eq!(r.recent().lines().count(), 2, "both lines are kept for a report");
    assert!(r.recent().contains("<email>"));
}

#[test]
fn turned_off_it_records_nothing_but_keeps_the_log_for_a_report() {
    let mut r = Recorder::new("1", "web");
    r.record("system", json!({}), "t".into());
    r.set_enabled(false);
    r.record("system", json!({}), "t".into());
    r.log("ERROR", Some(Severity::Warning), "x", "broke", "t".into());
    assert!(r.take("i").is_empty());
    let report = r.report("i", "t", "note", vec![("page.log".into(), "from the page".into())], "worker.log", json!([]));
    assert_eq!(report["logs"][0]["name"], "page.log");
    assert_eq!(report["logs"][1]["name"], "worker.log");
    assert!(report["logs"][1]["text"].as_str().unwrap().contains("broke"));
}

#[test]
fn what_waits_in_memory_is_bounded() {
    let mut r = Recorder::new("1", "web");
    // Words, not one long run, which the scrubber would take for a secret.
    let big = "some words ".repeat(1_000);
    for _ in 0..300 {
        r.record("log", json!({ "message": big }), "t".into());
    }
    let kept = r.take("i");
    assert!(kept.len() < 300 && !kept.is_empty(), "{}", kept.len());
    assert!(kept.iter().map(String::len).sum::<usize>() <= MAX_PENDING_BYTES + 300 * 400);
}

#[tokio::test]
async fn the_install_is_the_page_s_when_it_offers_one() {
    let queue = Queue::new(Disk::memory(), PathBuf::from("/d/diagnostics"));
    let made = queue.install(None).await;
    assert!(valid_install(&made));
    assert_eq!(queue.install(None).await, made, "kept");
    assert_eq!(queue.install(Some("not an id")).await, made, "a bad offer is ignored");
    let page = "b".repeat(32);
    assert_eq!(queue.install(Some(&page)).await, page);
    assert_eq!(queue.install(None).await, page, "and kept from then on");
}

#[tokio::test]
async fn batches_go_oldest_first_and_stop_at_a_refusal() {
    let queue = Queue::new(Disk::memory(), PathBuf::from("/d/diagnostics"));
    queue.append(&["{\"seq\":0}".into(), "{\"seq\":1}".into()], 2, "s").await.unwrap();
    queue.append(&["{\"seq\":2}".into()], 1, "s").await.unwrap();
    queue.append(&["{\"seq\":3}".into()], 3, "s").await.unwrap();
    queue.append(&[], 4, "s").await.unwrap();
    assert_eq!(queue.waiting().await.len(), 3, "an empty batch writes nothing");

    let service = Service::answering(&[200, 503]);
    assert_eq!(queue.send(&service, &records_url("https://cloud.test/"), Some("sess")).await, Sent::Stopped);
    let sent = service.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].0, "https://cloud.test/api/v1/diagnostics/records");
    assert_eq!(sent[0].1.as_deref(), Some("sess"));
    assert_eq!(sent[0].2, "application/x-ndjson");
    assert_eq!(lines_of(&sent[0].3)[0]["seq"], 2, "the oldest batch first");
    assert_eq!(queue.waiting().await.len(), 2, "the refused batch and the one after it wait");

    let service = Service::answering(&[400, 0]);
    assert_eq!(queue.send(&service, "u", None).await, Sent::Stopped, "no network");
    assert_eq!(queue.waiting().await.len(), 1, "a batch refused as unreadable is dropped");
    assert_eq!(queue.send(&Service::default(), "u", None).await, Sent::All);
    assert!(queue.waiting().await.is_empty());
}

#[tokio::test]
async fn the_queue_drops_the_oldest_past_its_cap_and_clears() {
    let queue = Queue::new(Disk::memory(), PathBuf::from("/d/diagnostics"));
    for i in 0..5 {
        queue.append(&["x".repeat(99)], i, "s").await.unwrap();
    }
    queue.cap(250).await;
    let left: Vec<String> = queue.waiting().await.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["records-0000000000003-s.ndjson", "records-0000000000004-s.ndjson"]);
    let install = queue.install(None).await;
    queue.clear().await;
    assert!(queue.waiting().await.is_empty());
    assert_eq!(queue.install(None).await, install, "clearing keeps the install number");
}

#[tokio::test]
async fn a_report_answers_its_code_or_a_sentence() {
    let service = Service::default();
    let code = send_report(&service, "https://cloud.test", "sess", &json!({ "note": "n" })).await.unwrap();
    assert_eq!(code, "ABCD1234");
    let sent = service.sent.lock().unwrap().clone();
    assert_eq!(sent[0].0, "https://cloud.test/api/v1/diagnostics/reports");
    assert_eq!(sent[0].2, "application/json");
    let refused = send_report(&Service::answering(&[401]), "https://cloud.test", "sess", &json!({})).await.unwrap_err();
    assert!(refused.starts_with("Pimble Cloud did not take the report (401"), "{refused}");
    let offline = send_report(&Service::answering(&[0]), "https://cloud.test", "sess", &json!({})).await.unwrap_err();
    assert!(offline.contains("could not be reached"), "{offline}");
}

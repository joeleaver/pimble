use std::time::Duration;

use pimble_host::Runtime;
use serde_json::json;

use super::*;

#[path = "worker_core_seams.rs"]
mod seams;
use seams::{HeldDisk, Service, SetClock};

use pimble_core::diagnostics::SavedChoice;

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

fn rt() -> Runtime {
    Runtime::default()
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
    d.pump(&disk, &service, &rt(), Some(alice())).await;
    assert!(service.sent().is_empty(), "the page has not said");
    assert_eq!(waiting(&disk).await, 0, "nor is anything written");
    assert!(d.ids().is_none());

    assert_eq!(d.configure(&disk, false, None).await, Applied::Done);
    d.record("stores", json!({ "stores": 2 }), "t1".into());
    d.pump(&disk, &service, &rt(), Some(alice())).await;
    assert!(service.sent().is_empty(), "off");
    assert_eq!(waiting(&disk).await, 0);
    assert!(d.ids().is_none());

    assert_eq!(d.configure(&disk, true, None).await, Applied::FirstOn, "on for the first time, even after an off");
    d.record("stores", json!({ "stores": 3 }), "t2".into());
    d.pump(&disk, &service, &rt(), Some(alice())).await;
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
    let service = Service::holding_first();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    d.attach(&disk, dir()).await;
    d.configure(&disk, true, None).await;
    // Three batches wait: two left by earlier pumps (signed out), one taken
    // by the pump the off interrupts.
    for millis in [1, 2] {
        d.record("stores", json!({ "at": millis }), "t".into());
        d.pump(&disk, &service, &rt(), None).await;
    }
    assert_eq!(waiting(&disk).await, 2);
    d.record("stores", json!({ "at": 3 }), "t".into());

    let runtime = rt();
    let pump = d.pump(&disk, &service, &runtime, Some(alice()));
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
    d.pump(&disk, &service, &rt(), Some(alice())).await;
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
    d.pump(&disk, &service, &rt(), None).await;
    d.set_account(None);
    d.record("stores", json!({ "by": "no one" }), "t".into());
    d.set_account(Some("bob-id".into()));
    d.record("stores", json!({ "by": "bob" }), "t".into());
    d.pump(&disk, &service, &rt(), Some(bob())).await;

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

    d.pump(&disk, &service, &rt(), Some(alice())).await;
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
    d.pump(&disk, &service, &rt(), Some(alice())).await;
    assert!(service.sent().is_empty());

    // On before the folder: the page's install number is used once it is.
    let d = WorkerDiagnostics::new("1.0.0", "web");
    assert_eq!(d.configure(&disk, true, Some(page_install.clone())).await, Applied::Done, "nothing to apply to yet");
    assert!(d.ids().is_none());
    assert_eq!(d.attach(&disk, dir()).await, Applied::FirstOn);
    assert_eq!(d.ids().map(|(install, _)| install), Some(page_install.clone()));
    d.record("stores", json!({}), "t".into());
    d.pump(&disk, &service, &rt(), Some(alice())).await;
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

/// Three batches of records `at` 1, 2 and 3 on the disk, written by pumps
/// signed out, with diagnostics on.
async fn three_batches_waiting(disk: &Disk, service: &Service, d: &WorkerDiagnostics) {
    d.attach(disk, dir()).await;
    d.configure(disk, true, None).await;
    for at in [1, 2, 3] {
        d.record("stores", json!({ "at": at }), "t".into());
        d.pump(disk, service, &rt(), None).await;
    }
    assert_eq!(waiting(disk).await, 3);
}

#[tokio::test]
async fn an_off_then_an_on_during_a_send_send_nothing_from_before_the_off_and_clear_it() {
    let disk = Disk::memory();
    let service = Service::holding_first();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    three_batches_waiting(&disk, &service, &d).await;

    let runtime = rt();
    let pump = d.pump(&disk, &service, &runtime, Some(alice()));
    let say = async {
        // The first batch is on the wire when the person saves "off", then
        // "on", before it is answered.
        tokio::time::timeout(DEADLINE, service.arrived.notified()).await.expect("the pump's first request");
        let off = d.configure(&disk, false, None);
        let on = d.configure(&disk, true, None);
        tokio::pin!(off, on);
        assert!(futures::poll!(off.as_mut()).is_pending(), "the off waits for the pump in flight");
        assert!(futures::poll!(on.as_mut()).is_pending(), "and so does the on");
        assert!(d.enabled(), "the switch is on again");
        d.record("stores", json!({ "at": 100 }), "t".into());
        service.release.notify_one();
        tokio::join!(off, on)
    };
    tokio::time::timeout(DEADLINE, async { tokio::join!(pump, say) }).await.expect("the pump and both says end");
    assert_eq!(service.ats(), [1], "only the request already on the wire when the off was saved");
    assert_eq!(waiting(&disk).await, 0, "the off's clear ran although an on came after it");

    d.pump(&disk, &service, &rt(), Some(alice())).await;
    assert_eq!(service.ats(), [1, 100], "what was recorded after the on is sent, and nothing from before the off");
    assert_eq!(waiting(&disk).await, 0);
}

/// The person saves "off" (and then "on", when `then_on`) while a pump is
/// writing its first batch: nothing recorded before the off is sent, now or
/// later, and none of it stays on the disk.
async fn an_off_during_a_pump_s_write(then_on: bool) {
    let held: &'static HeldDisk = Box::leak(Box::new(HeldDisk::new()));
    let disk = Disk::new(held);
    let service = Service::default();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    d.attach(&disk, dir()).await;
    d.configure(&disk, true, None).await;
    // A batch left by an earlier pump, and no one's and Alice's records in
    // memory, so the pump writes two batches.
    d.record("stores", json!({ "at": 1 }), "t".into());
    d.pump(&disk, &service, &rt(), None).await;
    d.record("stores", json!({ "at": 2 }), "t".into());
    d.set_account(Some("alice-id".into()));
    d.record("stores", json!({ "at": 3 }), "t".into());

    held.arm();
    let runtime = rt();
    let pump = d.pump(&disk, &service, &runtime, Some(alice()));
    let say = async {
        tokio::time::timeout(DEADLINE, held.arrived.notified()).await.expect("the pump's first write");
        let off = d.configure(&disk, false, None);
        let on = async {
            if then_on {
                d.configure(&disk, true, None).await;
            }
        };
        tokio::pin!(off, on);
        assert!(futures::poll!(off.as_mut()).is_pending(), "the off waits for the pump writing");
        if then_on {
            assert!(futures::poll!(on.as_mut()).is_pending(), "and so does the on");
        }
        held.release.notify_one();
        tokio::join!(off, on)
    };
    tokio::time::timeout(DEADLINE, async { tokio::join!(pump, say) }).await.expect("the pump and the says end");
    assert!(service.sent().is_empty(), "nothing recorded before the off is sent");
    assert_eq!(waiting(&disk).await, 0, "and none of it stays on the disk");

    if !then_on {
        d.configure(&disk, true, None).await;
    }
    d.record("stores", json!({ "at": 10 }), "t".into());
    d.pump(&disk, &service, &rt(), Some(alice())).await;
    assert_eq!(service.ats(), [10], "only what was recorded after the on");
    assert_eq!(waiting(&disk).await, 0);
}

#[tokio::test]
async fn an_off_during_a_pump_s_write_sends_and_keeps_nothing_from_before_it() {
    an_off_during_a_pump_s_write(false).await;
}

#[tokio::test]
async fn an_off_then_an_on_during_a_pump_s_write_send_and_keep_nothing_from_before_the_off() {
    an_off_during_a_pump_s_write(true).await;
}

#[tokio::test]
async fn an_off_during_a_stalled_send_clears_within_the_timeout_and_sends_nothing_after() {
    const LIMIT: Duration = Duration::from_millis(300);
    let disk = Disk::memory();
    // The first request is never answered.
    let service = Service::holding_first();
    let d = WorkerDiagnostics::new("1.0.0", "web").with_send_timeout(LIMIT);
    three_batches_waiting(&disk, &service, &d).await;

    let runtime = rt();
    let pump = d.pump(&disk, &service, &runtime, Some(alice()));
    let off = async {
        tokio::time::timeout(DEADLINE, service.arrived.notified()).await.expect("the pump's first request");
        let saved = std::time::Instant::now();
        tokio::time::timeout(LIMIT + Duration::from_secs(2), d.configure(&disk, false, None)).await.expect("the off is applied within the send timeout");
        let took = saved.elapsed();
        assert_eq!(waiting(&disk).await, 0, "the queue is cleared");
        took
    };
    let ((), took) = tokio::time::timeout(DEADLINE, async { tokio::join!(pump, off) }).await.expect("the pump and the off end");
    assert!(took <= LIMIT + Duration::from_secs(2), "the clear waited {took:?}");
    assert_eq!(service.ats(), [1], "only the stalled request, which was on the wire before the off");

    d.record("stores", json!({ "at": 9 }), "t".into());
    d.pump(&disk, &service, &rt(), Some(alice())).await;
    assert_eq!(service.ats(), [1], "nothing after the off");
    assert_eq!(waiting(&disk).await, 0);
}

/// The offs written down and not yet cleared for.
async fn clear_marks(disk: &Disk) -> usize {
    Queue::new(disk.clone(), dir()).clear_marks().await.len()
}

/// The person saves "off" and then "on" while a send is in flight, and the
/// tab closes before the off's clear could run (its step is dropped). The
/// clear is written down, so the next worker, started with the page's saved
/// "on", clears it before anything is sent: nothing from before the off is
/// sent, and none of it is left on the disk.
#[tokio::test]
async fn an_off_owed_when_the_worker_ends_is_cleared_by_the_next_one_before_it_sends() {
    let disk = Disk::memory();
    let service = Service::holding_first();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    three_batches_waiting(&disk, &service, &d).await;

    let runtime = rt();
    {
        let pump = d.pump(&disk, &service, &runtime, Some(alice()));
        tokio::pin!(pump);
        tokio::select! {
            biased;
            arrived = tokio::time::timeout(DEADLINE, service.arrived.notified()) => arrived.expect("the pump's first request"),
            () = &mut pump => panic!("the first request is held, so the pump cannot end"),
        }
        let off = d.configure(&disk, false, None);
        let on = d.configure(&disk, true, None);
        tokio::pin!(off, on);
        assert!(futures::poll!(off.as_mut()).is_pending(), "the off waits for the pump in flight");
        assert!(futures::poll!(on.as_mut()).is_pending(), "and so does the on");
        // The tab closes: the pump, the off and the on are dropped mid-way.
    }
    drop(d);
    assert_eq!(waiting(&disk).await, 3, "the clear was still owed when the worker ended");
    assert_eq!(clear_marks(&disk).await, 1, "and it is written down");

    let next = WorkerDiagnostics::new("1.0.0", "web");
    let next_service = Service::default();
    // A pump before the folder is known, or while a mark waits, does nothing.
    next.pump(&disk, &next_service, &rt(), Some(alice())).await;
    assert_eq!(next.attach(&disk, dir()).await, Applied::Done, "the page has not said yet");
    assert_eq!(waiting(&disk).await, 0, "the next worker clears what the off owed as soon as it has the folder");
    assert_eq!(clear_marks(&disk).await, 0, "and then the mark");
    assert_eq!(next.configure(&disk, true, None).await, Applied::FirstOn, "the page's saved choice");
    next.record("stores", json!({ "at": 10 }), "t".into());
    next.pump(&disk, &next_service, &rt(), Some(alice())).await;
    assert_eq!(next_service.ats(), [10], "only what was recorded after the on");
    assert_eq!(service.ats(), [1], "the first worker sent only the request on the wire before the off");
    assert_eq!(waiting(&disk).await, 0);
}

/// While an off written down waits to be cleared for, no pump writes or
/// sends anything, even one with diagnostics on.
#[tokio::test]
async fn no_pump_runs_while_an_off_is_written_down() {
    let disk = Disk::memory();
    let service = Service::default();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    d.attach(&disk, dir()).await;
    d.configure(&disk, true, None).await;
    // An off from another session's worker, written down and not cleared for.
    Queue::new(disk.clone(), dir()).mark_cleared_through(0, "other-1").await.unwrap();
    d.record("stores", json!({ "at": 2 }), "t".into());
    d.pump(&disk, &service, &rt(), Some(alice())).await;
    assert!(service.sent().is_empty(), "nothing is sent while the mark waits");
    assert_eq!(waiting(&disk).await, 0, "nor written");
    // The next say clears for it, and pumps go on.
    d.configure(&disk, true, None).await;
    assert_eq!(clear_marks(&disk).await, 0);
    d.pump(&disk, &service, &rt(), Some(alice())).await;
    assert_eq!(service.ats(), [2], "what was recorded after the off");
}

/// The wall clock steps back between a pump and an off, and the worker ends
/// before the off's clear runs (a send is in flight). Batches are numbered
/// by the queue's own sequence, so the off's mark covers the batch written
/// "later" by the clock: the next worker clears it before it sends anything.
#[tokio::test]
async fn a_clock_that_steps_back_never_lets_a_batch_from_before_an_off_survive_it() {
    let clock: &'static SetClock = Box::leak(Box::new(SetClock::default()));
    let runtime = Runtime::new(clock);
    let disk = Disk::memory();
    let service = Service::holding_first();
    let d = WorkerDiagnostics::new("1.0.0", "web");
    d.attach(&disk, dir()).await;
    d.configure(&disk, true, None).await;
    clock.set(1_000_000);
    d.record("stores", json!({ "at": 1 }), "t".into());
    d.pump(&disk, &service, &runtime, None).await;
    // The clock steps back ten minutes before the next pump and the off.
    clock.set(400_000);
    d.record("stores", json!({ "at": 2 }), "t".into());
    d.pump(&disk, &service, &runtime, None).await;
    assert_eq!(waiting(&disk).await, 2);
    {
        let pump = d.pump(&disk, &service, &runtime, Some(alice()));
        tokio::pin!(pump);
        tokio::select! {
            biased;
            arrived = tokio::time::timeout(DEADLINE, service.arrived.notified()) => arrived.expect("the pump's first request"),
            () = &mut pump => panic!("the first request is held, so the pump cannot end"),
        }
        clock.set(0);
        let off = d.configure(&disk, false, None);
        tokio::pin!(off);
        assert!(futures::poll!(off.as_mut()).is_pending(), "the off waits for the pump in flight");
        // The tab closes with the off's clear still owed.
    }
    drop(d);
    assert_eq!(clear_marks(&disk).await, 1, "the off is written down");

    let next = WorkerDiagnostics::new("1.0.0", "web");
    let next_service = Service::default();
    next.attach(&disk, dir()).await;
    assert_eq!(waiting(&disk).await, 0, "both batches from before the off go, whatever the clock said");
    next.configure(&disk, true, None).await;
    next.record("stores", json!({ "at": 3 }), "t".into());
    next.pump(&disk, &next_service, &runtime, Some(alice())).await;
    assert_eq!(next_service.ats(), [3], "only what was recorded after the on");
    assert_eq!(service.ats(), [1], "the first worker sent only the request on the wire before the off");
    assert_eq!(clock.reads(), 0, "nothing in the queue is named by the wall clock");
}

/// A worker hears "off" and then "on" before it has the queue's folder, and
/// ends before it gets it: the off was never written down. The next worker
/// hears the page's say (on), with the page's note of the clear owed when
/// `page_notes`; what it sends (each record's `at`), and whether it
/// confirmed the clear.
async fn a_restart_after_an_off_said_before_the_folder(page_notes: bool) -> (Vec<i64>, bool) {
    let disk = Disk::memory();
    let service = Service::default();
    // An earlier session left a batch waiting (signed out).
    let earlier = WorkerDiagnostics::new("1.0.0", "web");
    earlier.attach(&disk, dir()).await;
    earlier.configure(&disk, true, None).await;
    earlier.record("stores", json!({ "at": 1 }), "t".into());
    earlier.pump(&disk, &service, &rt(), None).await;
    drop(earlier);
    assert_eq!(waiting(&disk).await, 1);

    // The person turns diagnostics off and on again while the next worker
    // starts; it ends before it attaches.
    let mut page = SavedChoice::default().saved(false, true, || "off-1".into());
    let ended = WorkerDiagnostics::new("1.0.0", "web");
    assert_eq!(ended.configure(&disk, false, None).await, Applied::Done);
    page = page.saved(true, true, || unreachable!("an on owes nothing"));
    assert_eq!(ended.configure(&disk, true, None).await, Applied::Done);
    drop(ended);
    assert_eq!(clear_marks(&disk).await, 0, "the off never reached the disk");
    assert_eq!(waiting(&disk).await, 1);

    // The next worker hears the page before it attaches, as the page tells
    // it at connecting.
    let next = WorkerDiagnostics::new("1.0.0", "web");
    let owed = page.clear_owed.clone().filter(|_| page_notes);
    let say = async {
        let cleared = match owed.is_some() {
            true => next.clear_owed_by_the_page(&disk).await,
            false => true,
        };
        (cleared, next.configure(&disk, page.enabled, None).await)
    };
    let ((cleared, said), attached) = tokio::time::timeout(DEADLINE, async { tokio::join!(say, next.attach(&disk, dir())) }).await.expect("the say and the attach end");
    // Whichever applied the say (it came first, so the attach does unless
    // the say waited for it) says the program is on for the first time.
    assert_eq!([said, attached].iter().filter(|a| **a == Applied::FirstOn).count(), 1, "{said:?} {attached:?}");
    next.record("stores", json!({ "at": 2 }), "t".into());
    next.pump(&disk, &service, &rt(), Some(alice())).await;
    if let (true, Some(tag)) = (cleared, owed) {
        assert_eq!(page.confirmed(&tag).clear_owed, None, "the page drops its note");
    }
    (service.ats(), cleared)
}

#[tokio::test]
async fn an_off_the_worker_never_wrote_down_is_cleared_by_the_next_from_the_page_s_note() {
    let (sent, cleared) = a_restart_after_an_off_said_before_the_folder(true).await;
    assert_eq!(sent, [2], "nothing from before the off is sent");
    assert!(cleared, "the worker confirms the clear");
}

/// The control for the test above: the worker alone cannot know of an off
/// it never wrote down, so without the page's note the earlier session's
/// batch would be sent. This is the case the note exists for.
#[tokio::test]
async fn the_restart_case_is_only_caught_by_the_page_s_note() {
    let (sent, _) = a_restart_after_an_off_said_before_the_folder(false).await;
    assert_eq!(sent, [1, 2]);
}

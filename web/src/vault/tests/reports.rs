//! State reports and out of step (docs/CONVERGENCE_CONTRACT.md).

use super::*;

// ── State reports (docs/CONVERGENCE_CONTRACT.md, wave 1) ────────────────

/// Two pages of one store, as two devices: the same scope keys, the same
/// documents, reporting.
fn two_pages(peer: &Tree) -> (VaultStore, VaultStore) {
    let mut a = opened(peer);
    let mut b = VaultStore::assemble(listed(peer.root()), same_keys(&a), Vec::new(), pull_of(peer));
    a.reports_ok = true;
    b.reports_ok = true;
    (a, b)
}

/// The 2026-11-19 gap, in the browser: page A holds an edit the server
/// never got. Page B's report lacks it, so A answers with exactly what B
/// lacks; once B holds it, its report asks nothing more; the same report
/// is never answered twice; a page's own report is only noted.
#[test]
fn a_report_that_lacks_what_this_page_holds_is_answered_once() {
    let (peer, root) = origin();
    let store_id = StoreId::new();
    let (mut a, mut b) = two_pages(&peer);
    // An edit A made and never appended.
    a.tree.set_title(root, "Twelve structs", T1).unwrap();

    let sealed = b.reports_for(store_id, vec![root]);
    assert_eq!(sealed.len(), 1, "B reports the document the server holds");
    let (_, plaintext, blob) = sealed[0].clone();
    b.sent_report(root, plaintext);
    assert!(b.reports_for(store_id, vec![root]).is_empty(), "a report the server holds as it is is not sent again");

    let (diff, reported) = a.answer_report(store_id, "device-a", root, "device-b", &blob, None).expect("B lacks what A holds");
    b.tree.apply_update(root, &diff).unwrap();
    assert_eq!(b.tree.get_node_info(root).unwrap().title, "Twelve structs", "the answer is what B lacked");
    a.answered_report(root, "device-b", &reported);
    assert!(a.answer_report(store_id, "device-a", root, "device-b", &blob, None).is_none(), "the same report is not answered twice");

    let (_, _, again) = b.reports_for(store_id, vec![root]).pop().expect("B's vector moved: a new report");
    assert!(a.answer_report(store_id, "device-a", root, "device-b", &again, None).is_none(), "equal now: nothing owed");
    assert!(b.answer_report(store_id, "device-b", root, "device-a", &again, None).is_none(), "and nothing the other way");

    // A page's own report, read back at a connect, says what the server
    // holds of it and asks nothing.
    let mut fresh = VaultStore::assemble(listed(root), same_keys(&a), Vec::new(), pull_of(&peer));
    fresh.reports_ok = true;
    assert!(fresh.answer_report(store_id, "device-b", root, "device-b", &blob, None).is_none());
    assert!(fresh.reported.contains_key(&root));

    // A document the server has never seen is never reported: a report
    // must not be what creates it.
    let x = NodeId::new();
    b.tree.add_node(x, Some(root), Place::Last, "document", "x", T2).unwrap();
    assert!(b.reports_for(store_id, vec![x]).is_empty());
    // A report that does not open here asks nothing.
    assert!(a.answer_report(store_id, "device-a", root, "device-b", "not a blob", None).is_none());
}

/// A page that may only read the document answers no report: a reader's
/// page never appends. It still reports.
#[test]
fn a_readers_page_reports_and_answers_nothing() {
    let (peer, root) = origin();
    let store_id = StoreId::new();
    let (mut a, mut b) = two_pages(&peer);
    a.tree.set_title(root, "Held here only", T1).unwrap();
    let reader = row(vec![grant(None, "reader", "Vault")], None);
    let (_, _, blob) = b.reports_for(store_id, vec![root]).pop().expect("a reader's page reports");
    assert!(a.answer_report(store_id, "device-a", root, "device-b", &blob, Some(&reader)).is_none());
    let editor = row(vec![grant(None, "editor", "Vault")], None);
    assert!(a.answer_report(store_id, "device-a", root, "device-b", &blob, Some(&editor)).is_some());
}

/// A document's report is due a debounce after the last change to it,
/// merged from elsewhere or made here, and only once reports are on.
#[test]
fn a_report_is_due_a_debounce_after_the_last_change() {
    let (mut peer, root) = origin();
    let store_id = StoreId::new();
    let mut page = opened(&peer);
    let edit = peer.set_title(root, "Elsewhere", T1).unwrap();
    page.merge(store_id, root, vec![(Mark::One(2), Ok(edit.touched[0].1.clone()))], true, None);
    assert!(page.report_due.is_empty(), "nothing is due before the server is known to take reports");

    page.reports_ok = true;
    let edit = peer.set_title(root, "Elsewhere again", T2).unwrap();
    let before = now_ms();
    page.merge(store_id, root, vec![(Mark::One(3), Ok(edit.touched[0].1.clone()))], true, None);
    assert!(page.take_due_reports(before).is_empty(), "not before the debounce");
    assert!(!page.reports_waiting(before));
    let after = now_ms() + protocol::REPORT_DEBOUNCE_MS as f64 + 1.0;
    assert!(page.reports_waiting(after));
    assert_eq!(page.take_due_reports(after), vec![root]);
    assert!(page.report_due.is_empty());

    // A change made here counts the same.
    let local = page.tree.set_title(root, "Here", T2).unwrap();
    page.prepare(store_id, root, &local.touched[0].1).unwrap();
    assert!(page.report_due.contains_key(&root));
}

/// Two pages holding the same structs and different deletes: page B's copy
/// missed a delete A holds. Their vectors are equal, the delete digests
/// tell them apart, each answers the other once, and both end with the
/// delete and equal reports.
#[test]
fn a_delete_one_page_missed_is_answered_by_its_digest() {
    let (mut peer, root) = origin();
    let store_id = StoreId::new();
    peer.set_title(root, "A title to delete", T1).unwrap();
    let (mut a, mut b) = two_pages(&peer);
    // A delete-only update: what a peer's delete carries when it brings
    // no structs.
    let mut scratch = NodeDoc::load(&a.tree.doc(root).unwrap().save()).unwrap();
    scratch.set_title("", T2).unwrap();
    let deletes = scratch.diff_since(&scratch.state_vector()).unwrap();
    a.tree.apply_update(root, &deletes).unwrap();
    assert_eq!(a.tree.doc(root).unwrap().state_vector(), b.tree.doc(root).unwrap().state_vector(), "a delete moves no clock");
    assert_ne!(a.tree.doc(root).unwrap().delete_set_digest(), b.tree.doc(root).unwrap().delete_set_digest());

    let (_, _, a_blob) = a.reports_for(store_id, vec![root]).pop().unwrap();
    let (_, _, b_blob) = b.reports_for(store_id, vec![root]).pop().unwrap();
    let (to_b, b_report) = a.answer_report(store_id, "device-a", root, "device-b", &b_blob, None).expect("A holds a delete B lacks");
    let (to_a, a_report) = b.answer_report(store_id, "device-b", root, "device-a", &a_blob, None).expect("and B cannot tell whose set is larger");
    b.tree.apply_update(root, &to_b).unwrap();
    a.tree.apply_update(root, &to_a).unwrap();
    a.answered_report(root, "device-b", &b_report);
    b.answered_report(root, "device-a", &a_report);
    assert_eq!(a.tree.doc(root).unwrap().delete_set_digest(), b.tree.doc(root).unwrap().delete_set_digest(), "both hold the union");
    assert!(a.answer_report(store_id, "device-a", root, "device-b", &b_blob, None).is_none(), "the same report is not answered twice");
    let (_, _, b_now) = b.reports_for(store_id, vec![root]).pop().expect("B reports what it holds now");
    assert!(a.answer_report(store_id, "device-a", root, "device-b", &b_now, None).is_none(), "equal now");
}

// ── Out of step, said (docs/CONVERGENCE_CONTRACT.md section 4) ──────────

/// A page holding an update parked behind a gap says the note is out of
/// step once the window has passed (not before), with the reason and since
/// when the difference began, and in step again as soon as the gap fills.
#[test]
fn a_page_says_a_note_out_of_step_after_the_window_and_clears_it() {
    let (peer, root) = origin();
    let (mut page, _) = two_pages(&peer);
    let mut scratch = NodeDoc::load(&page.tree.doc(root).unwrap().save()).unwrap();
    let lost = scratch.append_markdown("nobody holds this").unwrap();
    let waits = scratch.append_markdown("waits for it").unwrap();
    page.tree.apply_update(root, &waits).unwrap();
    assert!(page.tree.doc(root).unwrap().missing().is_some(), "parked");

    let after = 1_000.0;
    assert!(!page.judge_steps("me", None, None, 10_000.0, 0.0, after), "a difference just seen has lasted no time");
    assert!(!page.judge_steps("me", None, None, 10_600.0, 600.0, after), "nor half the window");
    page.held_changed = false;
    assert!(page.judge_steps("me", None, None, 11_200.0, 600.0, after), "the window has passed");
    let list = page.out_of_step_list();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].node_id, list[0].reason.as_str(), list[0].kind), (root, protocol::WAITING_REASON, pimble_core::OutOfStepKind::Waiting));
    assert!(!list[0].is_shown(), "nobody can act on it: said only in Properties (Joe, 2026-11-20)");
    assert_eq!(list[0].since.timestamp_millis(), 10_000, "since the difference began");
    assert!(page.held_changed, "the store's sync answer says it again");
    assert!(!page.judge_steps("me", None, None, 11_800.0, 600.0, after), "said once");

    page.tree.apply_update(root, &lost).unwrap();
    assert!(page.judge_steps("me", None, None, 12_400.0, 600.0, after), "filled: in step at once");
    assert!(page.out_of_step_list().is_empty());
}

/// Another device's report holding what this page lacks makes the note out
/// of step, naming whose device as far as the page knows; this person's
/// own other device by that name.
#[test]
fn a_report_beyond_this_page_names_whose_device_holds_more() {
    let (peer, root) = origin();
    let store_id = StoreId::new();
    let (mut a, mut b) = two_pages(&peer);
    b.tree.set_title(root, "B holds more", T1).unwrap();
    let (_, _, blob) = b.reports_for(store_id, vec![root]).pop().unwrap();
    assert!(a.answer_report_from(store_id, "device-a", root, "device-b", Some("acct-me"), 0.0, &blob, None).is_none(), "nothing of A's to send");
    a.judge_steps("device-a", Some("acct-me"), None, 1.0, 0.0, 10.0);
    a.judge_steps("device-a", Some("acct-me"), None, 20.0, 19.0, 10.0);
    assert_eq!(a.out_of_step_list()[0].reason, "Your other device holds changes this one doesn't have yet. They'll arrive when you open Pimble there.");
    assert!(a.out_of_step_list()[0].is_shown());
    a.answer_report_from(store_id, "device-a", root, "device-b", Some("acct-will"), 0.0, &blob, None);
    a.judge_steps("device-a", Some("acct-me"), None, 40.0, 20.0, 10.0);
    assert_eq!(
        a.out_of_step_list()[0].reason,
        "Another device holds changes this one doesn't have yet. They'll arrive when that device is online again.",
        "no one is known by that account yet"
    );
    // The store's people, as the accounts service lists them: named.
    a.people.insert("acct-will".to_string(), "Will".to_string());
    a.judge_steps("device-a", Some("acct-me"), None, 60.0, 20.0, 10.0);
    assert_eq!(a.out_of_step_list()[0].reason, "Will's device holds changes this one doesn't have yet. They'll arrive when Will opens Pimble.", "named as the desktop names it");
}

/// Section 5 in the browser (decided 2026-11-20, building wave 3): a page
/// never gives up on its own, however long it waits; it sets a parked
/// update aside when another device's report says that device set aside
/// one waiting for exactly the same, says so quietly, and says it in its
/// own report; one it set aside and parks again goes at once; and a
/// device that reported holding what it waits for keeps it.
#[test]
fn a_page_sets_aside_only_what_another_device_set_aside() {
    let (peer, root) = origin();
    let store_id = StoreId::new();
    let (mut a, mut b) = two_pages(&peer);
    let base = a.tree.doc(root).unwrap().save();
    let mut scratch = NodeDoc::load(&base).unwrap();
    let _lost = scratch.append_markdown("nobody holds this").unwrap();
    let waits = scratch.append_markdown("waits for it").unwrap();
    a.tree.apply_update(root, &waits).unwrap();
    b.tree.apply_update(root, &waits).unwrap();
    let missing = a.tree.doc(root).unwrap().missing().unwrap();

    // A day of judging, and nothing is given up on the page's own word.
    a.judge_steps("device-a", None, None, 0.0, 0.0, 10.0);
    a.judge_steps("device-a", None, None, 90_000_000.0, 600.0, 10.0);
    assert!(a.tree.doc(root).unwrap().missing().is_some(), "a page never sets aside on its own");

    // B set it aside (a desktop's 24 hours, say): its report says so.
    b.set_aside.insert(root, vec![protocol::SetAsideRecord::new(&missing, chrono::Utc::now())]);
    let (_, _, blob) = b.reports_for(store_id, vec![root]).pop().unwrap();
    a.answer_report_from(store_id, "device-a", root, "device-b", None, 90_000_000.0, &blob, None);
    assert!(a.judge_steps("device-a", None, None, 90_000_600.0, 600.0, 10.0), "the list changed");
    assert_eq!(a.tree.doc(root).unwrap().missing(), None, "set aside, on another device's word");
    let list = a.out_of_step_list();
    assert_eq!((list.len(), list[0].kind, list[0].reason.as_str()), (1, pimble_core::OutOfStepKind::SetAside, protocol::SET_ASIDE_REASON));
    assert!(!list[0].is_shown(), "quiet");
    let (_, plaintext, _) = a.reports_for(store_id, vec![root]).pop().unwrap();
    assert_eq!(protocol::Report::decode(&plaintext).unwrap().aside, vec![missing.clone()], "and its own report says so");

    // Read again from a log that still carries it: set aside at once.
    a.tree.apply_update(root, &waits).unwrap();
    assert!(a.tree.doc(root).unwrap().missing().is_some());
    a.judge_steps("device-a", None, None, 90_001_200.0, 600.0, 10.0);
    assert_eq!(a.tree.doc(root).unwrap().missing(), None, "parked again, set aside again");

    // A device that holds what it waits for keeps a page from giving up.
    let page = |keys_of: &VaultStore| {
        let mut page = VaultStore::assemble(listed(peer.root()), same_keys(keys_of), Vec::new(), pull_of(&peer));
        page.reports_ok = true;
        page
    };
    let mut c = page(&a);
    c.tree.apply_update(root, &waits).unwrap();
    let mut holder = page(&a);
    holder.tree.apply_update(root, &scratch.diff_since(&holder.tree.doc(root).unwrap().state_vector()).unwrap()).unwrap();
    let (_, _, holds) = holder.reports_for(store_id, vec![root]).pop().unwrap();
    c.answer_report_from(store_id, "device-c", root, "device-b", None, 0.0, &blob, None);
    c.answer_report_from(store_id, "device-c", root, "device-h", None, 0.0, &holds, None);
    c.judge_steps("device-c", None, None, 1.0, 0.0, 10.0);
    assert!(c.tree.doc(root).unwrap().missing().is_some(), "held by a device that reported: kept");
}

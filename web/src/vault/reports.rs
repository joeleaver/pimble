//! State reports and out-of-step judgements (docs/CONVERGENCE_CONTRACT.md).

use std::collections::HashMap;
use std::sync::Arc;

use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
use pimble_rpc::vault as protocol;
use pimble_rpc::VaultDocId;

use super::{AccountStore, Side, StepCandidate, VaultClient, VaultStore, OUT_OF_STEP_AFTER_MS, PEOPLE_EVERY_MS, STEPS_EVERY_MS};
use super::helpers::{now_ms, save_answered};

impl VaultClient {
    // ── State reports (docs/CONVERGENCE_CONTRACT.md, wave 1) ────────────────

    /// At an open or a connect: read every device's reports of the documents
    /// this page holds and answer each that lacks something held here, then
    /// say what this page holds of every one of them. A server from before
    /// reports answers method-not-found, and the page goes on without them.
    pub(super) async fn compare_reports(&mut self, client: &Arc<PimbleClient>, store_id: StoreId) {
        let held: Vec<NodeId> = {
            let Some(store) = self.stores.get_mut(&store_id) else { return };
            store.reports_ok = true;
            store.reported.clear();
            store.report_due.clear();
            store.reports_heard.clear();
            store.tree.ids().into_iter().filter(|id| store.heads.contains_key(id)).collect()
        };
        for batch in held.chunks(protocol::REPORTS_BATCH) {
            match client.vault_reports(store_id, batch.iter().map(|id| VaultDocId::Node(*id)).collect()).await {
                Ok(reports) => {
                    for report in reports {
                        if let VaultDocId::Node(node_id) = report.doc_id {
                            self.answer_one(client, store_id, node_id, &report.device, report.account.as_deref(), report.at.timestamp_millis() as f64, &report.blob).await;
                        }
                    }
                }
                Err(e) if e.is_method_not_found() => {
                    tracing::info!("Store {}: its server takes no state reports (an older Pimble); syncing without them", store_id);
                    if let Some(store) = self.stores.get_mut(&store_id) {
                        store.reports_ok = false;
                    }
                    return;
                }
                Err(e) => {
                    tracing::warn!("Reading the state reports of {} failed: {}", store_id, e);
                    return;
                }
            }
        }
        self.send_reports(client, store_id, held).await;
    }

    /// The stores whose documents are due to be judged in step or not
    /// (docs/CONVERGENCE_CONTRACT.md section 4).
    pub fn steps_due(&self) -> Vec<StoreId> {
        let now = now_ms();
        self.stores.iter().filter(|(_, store)| store.reports_ok && store.steps_judged_at.is_none_or(|at| now - at >= STEPS_EVERY_MS)).map(|(id, _)| *id).collect()
    }

    /// Judge one store's documents, its endpoint connected: the time since
    /// the last judgment counts toward the window. What changed reaches the
    /// UI with the store's sync answer, through `pump`.
    pub fn judge_steps(&mut self, store_id: StoreId) {
        let own = self.device.clone();
        let own_account = crate::session::user_id();
        let row = self.rows.get(&store_id).cloned();
        let Some(store) = self.stores.get_mut(&store_id) else { return };
        let now = now_ms();
        let elapsed = store.steps_judged_at.map_or(0.0, |at| (now - at).min(2.0 * STEPS_EVERY_MS));
        store.steps_judged_at = Some(now);
        store.judge_steps(&own, own_account.as_deref(), row.as_ref(), now, elapsed, OUT_OF_STEP_AFTER_MS);
    }

    /// Whether the store's people are due to be asked again.
    pub fn people_due(&self, store_id: StoreId) -> bool {
        self.stores.get(&store_id).is_some_and(|s| s.people_at.is_none_or(|at| now_ms() - at >= PEOPLE_EVERY_MS))
    }

    /// Who is who in the store's shares, as the accounts service said.
    pub fn set_people(&mut self, store_id: StoreId, people: &pimble_rpc::StorePeople) {
        if let Some(store) = self.stores.get_mut(&store_id) {
            store.people = people.scopes.iter().flat_map(|scope| scope.people.iter()).map(|p| (p.user_id.clone(), p.name.clone())).collect();
            store.people_at = Some(now_ms());
        }
    }

    /// Asked and failed: not again until the next round.
    pub fn people_asked(&mut self, store_id: StoreId) {
        if let Some(store) = self.stores.get_mut(&store_id) {
            store.people_at = Some(now_ms());
        }
    }

    /// The store's endpoint is down: the time until it is up again does not
    /// count toward any document's window.
    pub fn pause_steps(&mut self, store_id: StoreId) {
        if let Some(store) = self.stores.get_mut(&store_id) {
            store.steps_judged_at = Some(now_ms());
        }
    }

    /// The stores with reports to send or to answer now.
    pub fn reports_due(&self) -> Vec<StoreId> {
        let now = now_ms();
        self.stores.iter().filter(|(_, store)| store.reports_waiting(now)).map(|(id, _)| *id).collect()
    }

    /// Answer the reports heard live since the last pass, and send the ones
    /// whose debounce has run out.
    pub async fn exchange_reports(&mut self, client: &Arc<PimbleClient>, store_id: StoreId) {
        let (incoming, due) = {
            let Some(store) = self.stores.get_mut(&store_id) else { return };
            (std::mem::take(&mut store.reports_in), store.take_due_reports(now_ms()))
        };
        for (node_id, device, blob, account) in incoming {
            self.answer_one(client, store_id, node_id, &device, account.as_deref(), now_ms(), &blob).await;
        }
        if !due.is_empty() {
            self.send_reports(client, store_id, due).await;
        }
    }

    /// One other device's report: when it lacks something this page holds,
    /// append the diff from it.
    #[allow(clippy::too_many_arguments)]
    async fn answer_one(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, node_id: NodeId, device: &str, account: Option<&str>, at_ms: f64, blob: &str) {
        let own = self.device.clone();
        let row = self.rows.get(&store_id).cloned();
        let Some(store) = self.stores.get_mut(&store_id) else { return };
        let Some((diff, reported)) = store.answer_report_from(store_id, &own, node_id, device, account, at_ms, blob, row.as_ref()) else { return };
        tracing::info!("Store {}: device {} lacks part of the document {} that this page holds; sending {} bytes", store_id, device, node_id, diff.len());
        match self.append_prepared(client, store_id, node_id, &diff).await {
            Ok(()) => {
                if let Some(store) = self.stores.get_mut(&store_id) {
                    store.answered_report(node_id, device, &reported);
                }
            }
            Err(e) => tracing::warn!("Sending what device {} lacks of {} failed: {}", device, node_id, e),
        }
    }

    /// Seal and send this page's reports of `nodes`, several at a time.
    async fn send_reports(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, nodes: Vec<NodeId>) {
        let sealed = {
            let Some(store) = self.stores.get_mut(&store_id).filter(|store| store.reports_ok) else { return };
            store.reports_for(store_id, nodes)
        };
        let device = self.device.clone();
        for batch in sealed.chunks(protocol::REPORTS_IN_FLIGHT) {
            let sent = futures::future::join_all(batch.iter().map(|(node_id, _, blob)| client.vault_report(store_id, VaultDocId::Node(*node_id), &device, blob.clone()))).await;
            let Some(store) = self.stores.get_mut(&store_id) else { return };
            for ((node_id, plaintext, _), result) in batch.iter().zip(sent) {
                match result {
                    Ok(()) => store.sent_report(*node_id, plaintext.clone()),
                    Err(e) if e.is_method_not_found() => {
                        store.reports_ok = false;
                        return;
                    }
                    Err(e) => tracing::debug!("Store {}: the report of {} was not taken ({})", store_id, node_id, e),
                }
            }
        }
    }
}

impl VaultStore {
    // ── State reports (docs/CONVERGENCE_CONTRACT.md, wave 1) ────────────────

    /// A change to `node_id`, made here or merged from elsewhere: its report
    /// is due a debounce from `now`.
    pub(super) fn note_report_due(&mut self, node_id: NodeId, now: f64) {
        if self.reports_ok {
            self.report_due.insert(node_id, now + protocol::REPORT_DEBOUNCE_MS as f64);
        }
    }

    /// The documents whose reports are due at `now`, taken off the list.
    pub(super) fn take_due_reports(&mut self, now: f64) -> Vec<NodeId> {
        let due: Vec<NodeId> = self.report_due.iter().filter(|(_, at)| **at <= now).map(|(id, _)| *id).collect();
        for id in &due {
            self.report_due.remove(id);
        }
        due
    }

    /// Whether anything about reports is waiting for the backend loop.
    pub(super) fn reports_waiting(&self, now: f64) -> bool {
        !self.reports_in.is_empty() || self.report_due.values().any(|at| *at <= now)
    }

    /// This page's reports of `nodes`, sealed, each with its plaintext: only
    /// documents the server holds (a report never creates one), only those
    /// whose report the server does not hold as it is, and only those with a
    /// key this page may seal under.
    pub(super) fn reports_for(&mut self, store_id: StoreId, nodes: Vec<NodeId>) -> Vec<(NodeId, Vec<u8>, String)> {
        let mut out = Vec::new();
        for node_id in nodes {
            if !self.heads.contains_key(&node_id) {
                continue;
            }
            let Some(doc) = self.tree.doc(node_id) else { continue };
            let aside = self.set_aside.get(&node_id).map(|records| records.iter().filter_map(|r| r.missing_bytes()).collect::<Vec<_>>()).unwrap_or_default();
            let report = protocol::Report::of(doc).with_aside(aside);
            let plaintext = report.encode();
            if self.reported.get(&node_id) == Some(&plaintext) {
                continue;
            }
            let Some((key_id, key)) = self.keys.encrypt_key(store_id, node_id) else { continue };
            let sealed = protocol::seal_report(store_id, node_id, key_id, &key, &report);
            out.push((node_id, plaintext, sealed));
        }
        out
    }

    /// The server stored this page's report of `node_id`.
    pub(super) fn sent_report(&mut self, node_id: NodeId, plaintext: Vec<u8>) {
        self.reported.insert(node_id, plaintext);
    }

    /// Another device's report of `node_id` (`own` is this page's device):
    /// the diff to append and the report it answers, when this page holds
    /// something that report lacks (structs, or deletes of the same structs)
    /// and has not answered that same
    /// report already (`pimble_rpc::vault::Comparing`). A report that will not
    /// open here, this page's own, and any report of a document this account
    /// may only read ask nothing of it.
    #[cfg(test)]
    pub(super) fn answer_report(&mut self, store_id: StoreId, own: &str, node_id: NodeId, device: &str, blob: &str, row: Option<&AccountStore>) -> Option<(Vec<u8>, protocol::Report)> {
        self.answer_report_from(store_id, own, node_id, device, None, now_ms(), blob, row)
    }

    /// [`VaultStore::answer_report`] knowing whose device it is and when the
    /// server stored the report: heard either way, for section 4.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn answer_report_from(&mut self, store_id: StoreId, own: &str, node_id: NodeId, device: &str, account: Option<&str>, at_ms: f64, blob: &str, row: Option<&AccountStore>) -> Option<(Vec<u8>, protocol::Report)> {
        let report = match self.keys.open_report(store_id, node_id, blob) {
            Ok(report) => report,
            Err(reason) => {
                tracing::debug!("Store {}: device {}'s report of {} does not open here ({}); ignoring it", store_id, device, node_id, reason);
                return None;
            }
        };
        if device == own {
            self.reported.insert(node_id, report.encode());
            return None;
        }
        self.reports_heard.entry(node_id).or_default().insert(device.to_string(), (report.clone(), account.map(str::to_string), at_ms));
        if !self.may_write(node_id, row) {
            return None;
        }
        let doc = self.tree.doc(node_id)?;
        let from = self.answered.owed(own, &node_id.to_string(), device, &protocol::Report::of(doc), &report)?;
        let diff = doc.diff_since(&from).ok()?;
        Some((diff, report))
    }

    /// `device`'s `reported` report of `node_id` was answered with an append.
    pub(super) fn answered_report(&mut self, node_id: NodeId, device: &str, reported: &protocol::Report) {
        self.answered.answered(&node_id.to_string(), device, reported);
        save_answered(self.listed.id, &self.answered);
    }

    // ── Out of step, said (docs/CONVERGENCE_CONTRACT.md section 4) ─────────

    /// Judge every document again at `now` (ms), `elapsed` (ms) of connected
    /// time after the last judgment: a difference that has lasted `after`
    /// makes a document out of step, and one that closes clears it at once
    /// (`pimble_rpc::vault::out_of_step_reason`, the desktop link's
    /// judgement). Answers whether the list changed; the store's sync answer
    /// then says it again (`held_back_events`).
    pub(super) fn judge_steps(&mut self, own: &str, own_account: Option<&str>, row: Option<&AccountStore>, now: f64, elapsed: f64, after: f64) -> bool {
        let mut changed = self.settle_parked(now);
        // The person's own other devices, and everyone else's by the name
        // the store's people list gives them.
        let people = &self.people;
        let name = |account: Option<&str>| protocol::whose(account, own_account, |id| people.get(id).cloned());
        let mut reasons: HashMap<NodeId, (pimble_core::OutOfStepKind, String)> = HashMap::new();
        let mut parked: HashMap<NodeId, Vec<u8>> = HashMap::new();
        for node_id in self.tree.ids() {
            let Some(doc) = self.tree.doc(node_id) else { continue };
            let local = protocol::Report::of(doc);
            if let Some(missing) = &local.missing {
                parked.insert(node_id, missing.clone());
            }
            let known = self.docs.get(&node_id).map(|d| (d.unsent, d.known_sv.clone()));
            let unsent = match &known {
                Some((unsent, known_sv)) => *unsent || pimble_crdt::state_vector_exceeds(&local.state_vector, known_sv).unwrap_or(true),
                None => true,
            };
            let heard = self.reports_heard.get(&node_id).into_iter().flatten().map(|(device, (report, account, at))| (device.as_str(), report, account.as_deref(), ((now - at) / 86_400_000.0) as i64));
            let pushes = self.may_write(node_id, row);
            if let Some(reason) = protocol::out_of_step_reason(own, &node_id.to_string(), &local, doc.missing().is_some(), heard, &self.answered, unsent, pushes, &name) {
                reasons.insert(node_id, reason);
            }
        }
        // Since when each parked update has waited for what it waits for.
        self.parked_since.retain(|id, (missing, _)| parked.get(id) == Some(missing));
        for (node_id, missing) in parked {
            self.parked_since.entry(node_id).or_insert((missing, now));
        }
        let since_of = |ms: f64| chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64).unwrap_or_else(chrono::Utc::now);
        let gone: Vec<NodeId> = self.steps.keys().filter(|id| !reasons.contains_key(id)).copied().collect();
        for node_id in gone {
            if self.steps.remove(&node_id).is_some_and(|c| c.marked) {
                tracing::info!("Store {}: the document {} is in step again", self.listed.id, node_id);
            }
        }
        for (node_id, (kind, reason)) in reasons {
            let candidate = match self.steps.entry(node_id) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    let candidate = entry.into_mut();
                    candidate.held_ms += elapsed;
                    candidate
                }
                std::collections::hash_map::Entry::Vacant(entry) => entry.insert(StepCandidate { since_ms: now, held_ms: 0.0, reason: reason.clone(), kind, marked: false }),
            };
            candidate.reason = reason;
            candidate.kind = kind;
            if candidate.held_ms >= after && !candidate.marked {
                tracing::warn!("Store {}: the document {} is out of step: {}", self.listed.id, node_id, candidate.reason);
                candidate.marked = true;
            }
        }
        let mut list: HashMap<NodeId, pimble_core::OutOfStep> = HashMap::new();
        for (node_id, candidate) in self.steps.iter().filter(|(_, c)| c.marked) {
            let since_ms = match candidate.kind {
                pimble_core::OutOfStepKind::Waiting => self.parked_since.get(node_id).map(|(_, at)| *at).unwrap_or(candidate.since_ms),
                _ => candidate.since_ms,
            };
            list.insert(*node_id, pimble_core::OutOfStep { node_id: *node_id, since: since_of(since_ms), reason: candidate.reason.clone(), kind: candidate.kind });
        }
        // What this page set aside, said quietly where nothing else is.
        for (node_id, records) in &self.set_aside {
            if list.contains_key(node_id) || self.parked_since.contains_key(node_id) {
                continue;
            }
            if let Some(record) = records.iter().max_by_key(|r| r.at) {
                list.insert(*node_id, pimble_core::OutOfStep { node_id: *node_id, since: record.at, reason: protocol::SET_ASIDE_REASON.to_string(), kind: pimble_core::OutOfStepKind::SetAside });
            }
        }
        if list != self.out_of_step {
            self.out_of_step = list;
            changed = true;
        }
        if changed {
            self.held_changed = true;
        }
        changed
    }

    /// Section 5 in the browser: a parked update is set aside when another
    /// device's report says it set aside one waiting for exactly the same
    /// (and no device that reported holds any of it), or when this page set
    /// it aside before and parked it again. A page never gives up on its own:
    /// it does not live the 24 hours, and the device that does takes the
    /// log entry away with a snapshot. Answers whether anything was set aside.
    fn settle_parked(&mut self, now: f64) -> bool {
        let mut any = false;
        let ids: Vec<NodeId> = self.tree.ids();
        for node_id in ids {
            let Some(missing) = self.tree.doc(node_id).and_then(|d| d.missing()) else { continue };
            let before = self.set_aside.get(&node_id).is_some_and(|records| records.iter().any(|r| r.missing_bytes().as_deref() == Some(missing.as_slice())));
            let elsewhere = || {
                let heard = self.reports_heard.get(&node_id).into_iter().flatten().map(|(_, (report, _, at))| (report, ((now - at) / 86_400_000.0) as i64));
                // The page's own clock never decides: only another device's word.
                let never = chrono::Duration::MAX;
                protocol::set_aside_due(&missing, chrono::Utc::now(), chrono::Utc::now(), never, heard) == protocol::SetAsideDue::SetAsideElsewhere
            };
            if !before && !elsewhere() {
                continue;
            }
            let Some(dropped) = self.tree.doc_mut(node_id).and_then(|d| d.set_aside_parked()) else { continue };
            tracing::warn!(
                "Store {}: set aside an update of the document {} from another device, waiting for {} ({})",
                self.listed.id,
                node_id,
                pimble_crdt::describe_state_vector(&dropped),
                if before { "this page set it aside before" } else { "another device set it aside" }
            );
            let at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now as i64).unwrap_or_else(chrono::Utc::now);
            let records = self.set_aside.entry(node_id).or_default();
            let record = protocol::SetAsideRecord::new(&dropped, at);
            if !records.iter().any(|r| r.missing == record.missing) {
                records.push(record);
            }
            self.parked_since.remove(&node_id);
            self.note_report_due(node_id, now - protocol::REPORT_DEBOUNCE_MS as f64);
            any = true;
        }
        any
    }

    /// The documents out of step, in a steady order; what this page could
    /// not send with what holds its send back, when that is known.
    pub(super) fn out_of_step_list(&self) -> Vec<pimble_core::OutOfStep> {
        let mut list: Vec<pimble_core::OutOfStep> = self
            .out_of_step
            .values()
            .cloned()
            .map(|mut entry| {
                if entry.kind == pimble_core::OutOfStepKind::Unsent {
                    entry.reason = protocol::with_step(&entry.reason, self.held_back.get(&(entry.node_id, Side::Send)).map(String::as_str));
                }
                entry
            })
            .collect();
        list.sort_by_key(|o| o.node_id.to_string());
        list
    }
}

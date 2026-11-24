//! What an owner's relay face saw others write, reported to the accounts
//! service (docs/NOTIFICATIONS_CONTRACT.md section 3, wave 3).
//!
//! A relayed store lives on its owner's computer, and its members' writes
//! reach the owner's relay face (`crate::relay_face`) through the tunnel,
//! never the hosted server. So the face gathers them as any token-verifying
//! server does (`crate::writes`), and the owner's own server hears them in
//! its process and posts them (`POST /api/v1/watches/writes`) under the
//! owner's session: the owner's device does not know who watches what, so
//! it reports writes and the service evaluates watches as it does for a
//! hosted store. Only stores the tunnel serves are reported (a twin being
//! rebuilt is filled from the store itself, and those are nobody's new
//! writes). This works only while the owner's computer is on, as the relay
//! does; a write the service could not be told after a few tries is lost,
//! as is one gathered in the last moments before the owner's app quits.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use pimble_core::StoreId;
use pimble_rpc::WriteSeen;
use tokio::sync::{broadcast, watch};
use tracing::debug;

use crate::cloud::ReportedWrite;
use crate::handler::RpcHandler;

/// How many times one report is tried.
const ATTEMPTS: u32 = 3;

/// Hear `writes` (the face's) and report those to `served` stores, under
/// whoever is signed in to `host` (the owner's server). Ends when the face
/// goes.
pub(crate) async fn run(host: RpcHandler, mut writes: broadcast::Receiver<WriteSeen>, served: watch::Receiver<HashSet<StoreId>>) {
    loop {
        let first = match writes.recv().await {
            Ok(seen) => seen,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                debug!("Relay writes: {} were not reported (fell behind)", n);
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        // Whatever else is waiting goes in the same reports, per store.
        let mut by_store: BTreeMap<StoreId, Vec<ReportedWrite>> = BTreeMap::new();
        let mut take = |seen: WriteSeen| {
            if served.borrow().contains(&seen.store_id) {
                by_store.entry(seen.store_id).or_default().push(ReportedWrite { doc: seen.doc_id.to_string(), writer: seen.writer, count: seen.count });
            }
        };
        take(first);
        while let Ok(seen) = writes.try_recv() {
            take(seen);
        }
        for (store_id, reported) in by_store {
            report(&host, store_id, &reported).await;
        }
    }
}

async fn report(host: &RpcHandler, store_id: StoreId, writes: &[ReportedWrite]) {
    let mut wait = Duration::from_secs(2);
    for attempt in 1..=ATTEMPTS {
        let Some(account) = host.keystore().account().await else { return };
        match crate::cloud::post_relay_writes(host.http(), &account.url, &account.session, &store_id.to_string(), writes).await {
            Ok(()) => return,
            Err(e) if e.status() == Some(401) => {
                host.keystore().mark_session_expired();
                return;
            }
            Err(e) if matches!(e.status(), Some(400 | 403 | 404)) => {
                debug!("Relay writes: the service refused a report for store {}: {}", store_id, e);
                return;
            }
            Err(e) => {
                debug!("Relay writes: report {} of {} for store {} failed: {}", attempt, ATTEMPTS, store_id, e);
                host.runtime().sleep(wait).await;
                wait *= 2;
            }
        }
    }
}

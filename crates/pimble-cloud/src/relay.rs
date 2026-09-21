//! The relay (docs/RELAY_CONTRACT.md, "Accounts service"): how people reach
//! a store that is **not** hosted on Pimble Cloud. The owner's own machine
//! serves it; this module pipes WebSocket messages between that machine and
//! the members' devices, and keeps none of them.
//!
//! Two routes, both WebSockets:
//!
//! - `GET /api/v1/relay`, **the owner's tunnel**, authenticated with the
//!   account's session and never from a browser. Its first message is
//!   `{"serve": ["<store id>", ...]}`; the answer is `{"serving": [...]}`,
//!   the announced ids that are relay-tier stores this account owns. From
//!   then on "store S is behind this connection" — in [`Relay`]'s memory and
//!   nowhere else.
//! - `GET /api/v1/relay/<store id>`, **a member's connection**, authenticated
//!   with the account's JWT exactly as `/rpc` on a Pimble server is. It
//!   becomes a *virtual connection* over the store's tunnel.
//!
//! Tunnel framing (binary messages on the tunnel only):
//! `[conn: u32 BE][kind: u8][payload]`, kinds [`KIND_OPEN`] (relay to owner,
//! payload the member's JWT), [`KIND_TEXT`] (either way, one text message),
//! [`KIND_CLOSE`] (either way, empty). The owner's machine verifies the JWT
//! itself: the relay is a gate that keeps strangers off the tunnel, never the
//! authority on who somebody is or what they may do.
//!
//! What the relay holds is what is in flight: every queue here is a small
//! bounded channel, a reader that cannot hand its message on stops reading
//! (so TCP pushes back on the sender), and a peer that stays stuck is closed —
//! that one virtual connection, or the tunnel if it is the owner. Nothing
//! about a connection is written to the database, and the log lines name
//! store ids and counts only: never a token, an account or a payload.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::header::{AUTHORIZATION, ORIGIN};
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum::response::Response;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};

use pimble_core::StoreId;

use crate::error::{CloudError, CloudResult};
use crate::jwt::JwtRejection;
use crate::session::AuthedUser;
use crate::state::AppState;

// ── The wire ─────────────────────────────────────────────────────────────

/// Relay to owner: a member connected. Payload: the member's JWT, UTF-8.
pub const KIND_OPEN: u8 = 1;
/// Either way: one text (JSON-RPC) message of that virtual connection.
pub const KIND_TEXT: u8 = 2;
/// Either way: that virtual connection is over. Payload empty.
pub const KIND_CLOSE: u8 = 3;
/// `[conn: u32 BE][kind: u8]`.
const FRAME_HEADER_BYTES: usize = 5;

/// A member's connection when no tunnel serves the store (docs/
/// RELAY_CONTRACT.md) — and when the tunnel that did goes away under it. A
/// member's link reads this as "offline, try again later", not as an error.
pub const CLOSE_OWNER_OFFLINE: u16 = 4404;
const REASON_OWNER_OFFLINE: &str = "owner offline";
/// A member's connection, at the moment its token's `exp` passes.
pub const CLOSE_TOKEN_EXPIRED: u16 = 4401;
/// Over a limit: a 65th member connection on one tunnel, a 17th tunnel for one
/// account. A refusal, not a queue.
pub const CLOSE_OVER_LIMIT: u16 = 4429;
/// A tunnel whose every store has been taken over by a later tunnel (the
/// owner's server restarted and the old socket had not died yet).
pub const CLOSE_REPLACED: u16 = 4409;
/// A tunnel that did not open with a readable `{"serve": [...]}`.
pub const CLOSE_BAD_ANNOUNCE: u16 = 4400;
/// A member's connection that stopped taking what the owner sent it.
pub const CLOSE_TOO_SLOW: u16 = 4408;
/// RFC 6455's own codes, where one says it already.
const CLOSE_NORMAL: u16 = 1000;
const CLOSE_GOING_AWAY: u16 = 1001;
const CLOSE_PROTOCOL: u16 = 1002;
const CLOSE_UNSUPPORTED: u16 = 1003;
const CLOSE_INVALID_DATA: u16 = 1007;
const CLOSE_TOO_BIG: u16 = 1009;
const CLOSE_INTERNAL: u16 = 1011;

/// Frames queued for one tunnel's socket, from all of its members together.
/// Small on purpose: past it a member's reader waits, which is the
/// backpressure the contract asks for.
const TUNNEL_QUEUE: usize = 8;
/// Text messages queued for one member's socket.
const MEMBER_QUEUE: usize = 2;
/// How many store ids one `serve` may announce. Each costs two database
/// reads, so the list is bounded like everything else a caller sends.
const MAX_ANNOUNCED_STORES: usize = 256;

/// The relay's numbers. Every default is docs/RELAY_CONTRACT.md's (or, where
/// the contract names none, stated here), and none is an environment
/// variable: like [`crate::config::Config::max_members_per_store`] they are
/// fields only so a test can reach a limit without sixty-four sockets or a
/// ninety-second wait.
#[derive(Debug, Clone)]
pub struct RelayLimits {
    /// "64 member connections per tunnel".
    pub max_connections_per_tunnel: usize,
    /// "16 tunnels per account".
    pub max_tunnels_per_account: usize,
    /// "a message over 16 MiB closes the virtual connection": above the 10 MiB
    /// a Pimble server accepts or sends, so the relay is never the stricter of the two.
    pub max_message_bytes: usize,
    /// "pings every tunnel and every member connection every 30 s".
    pub ping_interval: Duration,
    /// "closes one that has not answered in 90 s".
    pub idle_timeout: Duration,
    /// How long a new tunnel has to say what it serves. Not in the contract;
    /// an authenticated socket that says nothing is not worth holding.
    pub announce_timeout: Duration,
    /// How long a peer may refuse to take a message before it is given up
    /// on: a socket write that does not complete, or a member whose queue
    /// stays full while the owner has more for it. Not in the contract,
    /// which says only that a slow side applies backpressure; this is what
    /// keeps one stuck member from holding up a whole tunnel for ever. It is
    /// also the longest one message may take to leave, so it is not small: a
    /// largest-allowed message in thirty seconds is a little over 2 Mbit/s.
    pub stall_timeout: Duration,
}

impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            max_connections_per_tunnel: 64,
            max_tunnels_per_account: 16,
            max_message_bytes: 16 * 1024 * 1024,
            ping_interval: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(90),
            announce_timeout: Duration::from_secs(10),
            stall_timeout: Duration::from_secs(30),
        }
    }
}

/// Why a socket is being closed, and whether the owner still has to be told
/// that the virtual connection is over (it does not when it said so itself,
/// or when its tunnel is what went away).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Close {
    code: u16,
    reason: &'static str,
    tell_owner: bool,
}

impl Close {
    const fn new(code: u16, reason: &'static str) -> Self {
        Self { code, reason, tell_owner: true }
    }

    const fn from_owner_side(code: u16, reason: &'static str) -> Self {
        Self { code, reason, tell_owner: false }
    }

    /// The tunnel went away under a member: there is no owner to tell.
    const OWNER_OFFLINE: Close = Close::from_owner_side(CLOSE_OWNER_OFFLINE, REASON_OWNER_OFFLINE);

    /// The store stopped being behind a tunnel that is itself still there
    /// (withdrawn, taken over by a later tunnel, its record deleted). To the
    /// member it is the same thing, `owner offline`; but the owner's end still
    /// holds its side of the virtual connection and has to be told to let go.
    const STORE_WITHDRAWN: Close = Close::new(CLOSE_OWNER_OFFLINE, REASON_OWNER_OFFLINE);

    fn frame(self) -> Message {
        Message::Close(Some(CloseFrame { code: self.code, reason: self.reason.into() }))
    }
}

/// A close signal both halves of a socket watch, set at most once: the first
/// reason wins, since whatever comes after is a consequence of it.
type CloseSignal = Arc<watch::Sender<Option<Close>>>;

fn close_signal() -> CloseSignal {
    Arc::new(watch::channel(None).0)
}

fn signal_close(signal: &watch::Sender<Option<Close>>, close: Close) {
    signal.send_if_modified(|current| {
        if current.is_some() {
            return false;
        }
        *current = Some(close);
        true
    });
}

/// Waits until `signal` is set. The value is copied out before returning, so
/// no `watch::Ref` (a lock guard, not `Send`) is ever held across an await.
async fn closed(signal: &mut watch::Receiver<Option<Close>>) -> Close {
    match signal.wait_for(Option::is_some).await {
        Ok(close) => (*close).unwrap_or(Close::OWNER_OFFLINE),
        // Every sender is gone: whatever this socket belonged to is too.
        Err(_) => Close::OWNER_OFFLINE,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Nothing here panics while holding a lock, and if something ever did,
    // the maps are still maps: carry on rather than poison every connection.
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn frame(conn: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
    out.extend_from_slice(&conn.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

// ── The registry ─────────────────────────────────────────────────────────

/// One owner's tunnel: the queue to its socket, and the virtual connections
/// running over it.
struct Tunnel {
    id: u64,
    user_rid: u64,
    /// To the tunnel's socket: `[conn][kind][payload]` frames as binary
    /// messages, and the `{"serving": [...]}` answers, the only text a tunnel
    /// is ever sent.
    out: mpsc::Sender<Message>,
    close: CloseSignal,
    conns: Mutex<Conns>,
}

struct Conns {
    /// The next id to try. Counts up and never goes back, so a frame still in
    /// flight for a connection that has ended cannot land on a new one.
    next: u32,
    open: HashMap<u32, Conn>,
}

/// The relay's end of one member's connection.
struct Conn {
    store_id: String,
    /// To the member's socket: text messages from the owner.
    texts: mpsc::Sender<Message>,
    close: CloseSignal,
}

impl Tunnel {
    /// Signals every connection `doomed` picks, and forgets them.
    fn close_conns(&self, close: Close, mut doomed: impl FnMut(&Conn) -> bool) -> usize {
        let mut conns = lock(&self.conns);
        let ids: Vec<u32> = conns.open.iter().filter(|(_, conn)| doomed(conn)).map(|(id, _)| *id).collect();
        for id in &ids {
            if let Some(conn) = conns.open.remove(id) {
                signal_close(&conn.close, close);
            }
        }
        ids.len()
    }

    fn remove_conn(&self, conn: u32) {
        lock(&self.conns).open.remove(&conn);
    }

    fn conn(&self, conn: u32) -> Option<(mpsc::Sender<Message>, CloseSignal)> {
        lock(&self.conns).open.get(&conn).map(|c| (c.texts.clone(), c.close.clone()))
    }
}

struct TunnelEntry {
    tunnel: Arc<Tunnel>,
    stores: HashSet<String>,
}

#[derive(Default)]
struct Registry {
    next_tunnel_id: u64,
    tunnels: HashMap<u64, TunnelEntry>,
    /// Store id (canonical hyphenated form) to the tunnel it is behind.
    by_store: HashMap<String, u64>,
}

/// What the relay holds right now. All of it is gone the moment the sockets
/// are: this is the whole of the relay's state, and none of it is a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RelayCounts {
    pub tunnels: usize,
    pub stores: usize,
    pub connections: usize,
}

/// Why a member's connection could not be put on a tunnel.
enum OpenRefused {
    OwnerOffline,
    TunnelFull,
}

/// A member's connection as the registry hands it over.
struct OpenedConn {
    tunnel: Arc<Tunnel>,
    conn: u32,
    texts: mpsc::Receiver<Message>,
    close: CloseSignal,
}

/// The in-memory registry: store id to tunnel, tunnel to virtual connections.
/// Lock order is always the registry, then a tunnel's connections; neither is
/// ever held across an await. Joining a tunnel and tearing one down both
/// happen under the registry's lock, so a member can never be added to a
/// tunnel after the sweep that closed its connections.
pub struct Relay {
    limits: RelayLimits,
    registry: Mutex<Registry>,
}

impl Relay {
    pub fn new(limits: RelayLimits) -> Self {
        Self { limits, registry: Mutex::new(Registry::default()) }
    }

    pub fn limits(&self) -> &RelayLimits {
        &self.limits
    }

    pub fn counts(&self) -> RelayCounts {
        let registry = lock(&self.registry);
        RelayCounts {
            tunnels: registry.tunnels.len(),
            stores: registry.by_store.len(),
            connections: registry.tunnels.values().map(|entry| lock(&entry.tunnel.conns).open.len()).sum(),
        }
    }

    /// A new tunnel for `user_rid`, serving nothing yet — or `None` when the
    /// account already has as many as it may.
    fn register_tunnel(&self, user_rid: u64) -> Option<(Arc<Tunnel>, mpsc::Receiver<Message>)> {
        let mut registry = lock(&self.registry);
        let held = registry.tunnels.values().filter(|entry| entry.tunnel.user_rid == user_rid).count();
        if held >= self.limits.max_tunnels_per_account {
            return None;
        }
        registry.next_tunnel_id += 1;
        let (out, out_rx) = mpsc::channel(TUNNEL_QUEUE);
        let tunnel = Arc::new(Tunnel {
            id: registry.next_tunnel_id,
            user_rid,
            out,
            close: close_signal(),
            conns: Mutex::new(Conns { next: 1, open: HashMap::new() }),
        });
        registry.tunnels.insert(tunnel.id, TunnelEntry { tunnel: tunnel.clone(), stores: HashSet::new() });
        Some((tunnel, out_rx))
    }

    /// `tunnel` now serves exactly `stores`. A store another tunnel was
    /// serving moves here ("a later tunnel for the same store replaces the
    /// earlier one") and that tunnel's members of it are closed; a tunnel left
    /// serving nothing by that is closed too — it is the socket the owner's
    /// restarted server left behind. A store this tunnel served and no longer
    /// announces is withdrawn.
    fn announce(&self, tunnel: &Arc<Tunnel>, stores: HashSet<String>) {
        let mut guard = lock(&self.registry);
        let registry = &mut *guard;
        let Some(previous) = registry.tunnels.get(&tunnel.id).map(|entry| entry.stores.clone()) else {
            return; // torn down while the announce was being checked
        };

        for withdrawn in previous.difference(&stores) {
            if registry.by_store.get(withdrawn) == Some(&tunnel.id) {
                registry.by_store.remove(withdrawn);
            }
            tunnel.close_conns(Close::STORE_WITHDRAWN, |conn| &conn.store_id == withdrawn);
        }
        for store_id in &stores {
            let Some(earlier_id) = registry.by_store.insert(store_id.clone(), tunnel.id).filter(|id| *id != tunnel.id) else {
                continue;
            };
            let Some(earlier) = registry.tunnels.get_mut(&earlier_id) else { continue };
            earlier.stores.remove(store_id);
            let closed = earlier.tunnel.close_conns(Close::STORE_WITHDRAWN, |conn| &conn.store_id == store_id);
            tracing::info!(store_id = %store_id, members_closed = closed, "relay: a later tunnel took over the store");
            if earlier.stores.is_empty() {
                signal_close(&earlier.tunnel.close, Close::new(CLOSE_REPLACED, "replaced by a later tunnel"));
            }
        }
        if let Some(entry) = registry.tunnels.get_mut(&tunnel.id) {
            entry.stores = stores;
        }
    }

    /// The tunnel is gone: its stores are behind nothing, and every member
    /// connection through it is closed.
    fn remove_tunnel(&self, tunnel: &Tunnel) {
        let mut guard = lock(&self.registry);
        let registry = &mut *guard;
        let Some(entry) = registry.tunnels.remove(&tunnel.id) else { return };
        for store_id in &entry.stores {
            if registry.by_store.get(store_id) == Some(&tunnel.id) {
                registry.by_store.remove(store_id);
            }
        }
        let closed = tunnel.close_conns(Close::OWNER_OFFLINE, |_| true);
        tracing::info!(stores = entry.stores.len(), members_closed = closed, tunnels = registry.tunnels.len(), "relay: tunnel closed");
    }

    /// The store's record is gone (`DELETE /stores/{id}`): stop piping to it
    /// now, not when its members' tokens run out. The tunnel itself stays, for
    /// whatever else it serves.
    pub fn withdraw_store(&self, store_id: &str) {
        let mut guard = lock(&self.registry);
        let registry = &mut *guard;
        let Some(tunnel_id) = registry.by_store.remove(store_id) else { return };
        let Some(entry) = registry.tunnels.get_mut(&tunnel_id) else { return };
        entry.stores.remove(store_id);
        let closed = entry.tunnel.close_conns(Close::STORE_WITHDRAWN, |conn| conn.store_id == store_id);
        tracing::info!(store_id = %store_id, members_closed = closed, "relay: store withdrawn");
    }

    /// A virtual connection to whichever tunnel serves `store_id`.
    fn open_conn(&self, store_id: &str) -> Result<OpenedConn, OpenRefused> {
        let registry = lock(&self.registry);
        let tunnel = registry
            .by_store
            .get(store_id)
            .and_then(|id| registry.tunnels.get(id))
            .map(|entry| entry.tunnel.clone())
            .ok_or(OpenRefused::OwnerOffline)?;
        let mut conns = lock(&tunnel.conns);
        if conns.open.len() >= self.limits.max_connections_per_tunnel {
            return Err(OpenRefused::TunnelFull);
        }
        let mut conn = conns.next;
        while conns.open.contains_key(&conn) {
            conn = conn.wrapping_add(1);
        }
        conns.next = conn.wrapping_add(1);
        let (texts, texts_rx) = mpsc::channel(MEMBER_QUEUE);
        let close = close_signal();
        conns.open.insert(conn, Conn { store_id: store_id.to_string(), texts, close: close.clone() });
        drop(conns);
        Ok(OpenedConn { tunnel, conn, texts: texts_rx, close })
    }
}

// ── Sockets ──────────────────────────────────────────────────────────────

/// The write half of a socket, as a task of its own so that reading never
/// waits on writing (a reader stuck behind its own writer is how two relayed
/// peers deadlock each other). Sends what `queue` yields, pings on the
/// interval, and ends on the close signal by sending the close frame. A write
/// that does not complete within `stall_timeout` sets the signal itself: the
/// peer has stopped reading, and the reader half must hear of it.
async fn write_half(mut sink: SplitSink<WebSocket, Message>, mut queue: mpsc::Receiver<Message>, close: CloseSignal, limits: RelayLimits) {
    let mut close_rx = close.subscribe();
    let mut ping = tokio::time::interval_at(Instant::now() + limits.ping_interval, limits.ping_interval);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut drained = false;
    loop {
        let message = tokio::select! {
            biased;
            close = closed(&mut close_rx) => {
                // An orderly end (the owner closed the connection) comes
                // after whatever the owner said before it: the last answer is
                // delivered, then the close. Any other end is abrupt by
                // nature and what is queued goes with it.
                if close.code == CLOSE_NORMAL {
                    while let Ok(message) = queue.try_recv() {
                        if !matches!(tokio::time::timeout(limits.stall_timeout, sink.send(message)).await, Ok(Ok(()))) {
                            break;
                        }
                    }
                }
                // Best effort and bounded: a peer that is not reading never
                // gets to hold this task open.
                let _ = tokio::time::timeout(limits.stall_timeout, sink.send(close.frame())).await;
                return;
            }
            queued = queue.recv(), if !drained => match queued {
                Some(message) => message,
                // Every sender is gone; the close signal is on its way.
                None => {
                    drained = true;
                    continue;
                }
            },
            _ = ping.tick() => Message::Ping(Vec::new()),
        };
        match tokio::time::timeout(limits.stall_timeout, sink.send(message)).await {
            Ok(Ok(())) => {}
            // The socket is broken or the peer is not taking anything. Either
            // way this end is over; the close frame above is then a formality
            // that fails fast or times out.
            Ok(Err(_)) => signal_close(&close, Close::new(CLOSE_GOING_AWAY, "connection lost")),
            Err(_) => signal_close(&close, Close::new(CLOSE_TOO_SLOW, "not reading")),
        }
    }
}

/// What a socket's reader saw next.
enum Incoming {
    Message(Message),
    /// The peer closed, vanished, or broke the protocol. `too_big` when what
    /// it broke was the message size limit.
    Gone { too_big: bool },
    /// Nothing at all for `idle_timeout`, pings included.
    Idle,
    /// The close signal was set by somebody else.
    Closed(Close),
}

async fn read_next(stream: &mut SplitStream<WebSocket>, close_rx: &mut watch::Receiver<Option<Close>>, idle_at: Instant) -> Incoming {
    tokio::select! {
        biased;
        close = closed(close_rx) => Incoming::Closed(close),
        next = stream.next() => match next {
            Some(Ok(Message::Close(_))) | None => Incoming::Gone { too_big: false },
            Some(Ok(message)) => Incoming::Message(message),
            // axum hands the size limit's breach over as an opaque error;
            // its text is all there is to tell it by.
            Some(Err(e)) => Incoming::Gone { too_big: e.to_string().to_ascii_lowercase().contains("too long") },
        },
        _ = tokio::time::sleep_until(idle_at) => Incoming::Idle,
    }
}

/// How long a socket that has been sent its close frame is still read from.
const LINGER: Duration = Duration::from_secs(2);

/// After the close frame, keep reading (and discarding) until the peer has
/// closed too, briefly. Dropping a socket that still has unread bytes makes
/// the kernel reset the connection, and a reset can overtake the close frame
/// on its way to the peer — who would then see a broken connection where it
/// should have seen `owner offline`, and report an error instead of waiting.
async fn linger(stream: &mut SplitStream<WebSocket>) {
    let _ = tokio::time::timeout(LINGER, async {
        while let Some(Ok(message)) = stream.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// Refuse a socket that never became anything: say why, and let go of it.
async fn refuse(mut sink: SplitSink<WebSocket, Message>, mut stream: SplitStream<WebSocket>, close: Close, limits: &RelayLimits) {
    if let Ok(Ok(())) = tokio::time::timeout(limits.stall_timeout, sink.send(close.frame())).await {
        linger(&mut stream).await;
    }
}

// ── The owner's tunnel ───────────────────────────────────────────────────

/// Refuses any request that carries an `Origin`: the tunnel is for a Pimble
/// server, and a browser (which always sends one on a WebSocket, and would
/// bring the session cookie along by itself) has no business opening it.
/// An extractor so that it runs before the session is even looked at.
pub struct NotABrowser;

#[async_trait::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for NotABrowser {
    type Rejection = CloudError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if parts.headers.contains_key(ORIGIN) {
            return Err(CloudError::Forbidden("the relay tunnel is not opened from a browser".to_string()));
        }
        Ok(NotABrowser)
    }
}

/// `GET /api/v1/relay` — the owner's tunnel.
pub async fn owner_tunnel(_not_a_browser: NotABrowser, State(state): State<AppState>, authed: AuthedUser, ws: WebSocketUpgrade) -> Response {
    let limits = state.relay.limits().clone();
    // Twice the member limit: one oversized message for one member must end
    // that virtual connection, not trip the socket's own limit and take the
    // tunnel (and everybody on it) down with it.
    ws.max_message_size(limits.max_message_bytes * 2 + FRAME_HEADER_BYTES)
        .max_frame_size(limits.max_message_bytes * 2 + FRAME_HEADER_BYTES)
        .on_upgrade(move |socket| run_tunnel(state, authed.user.rid, socket))
}

#[derive(Deserialize)]
struct Serve {
    serve: Vec<String>,
}

/// Of the announced ids, the ones this account may serve: relay-tier, live,
/// and owned by it (whole-store `owner`, the only kind of owner there is).
/// Anything else — a hosted store, somebody else's, an id that is not one —
/// is left out without a word more than its absence from `serving`.
async fn servable(state: &AppState, user_rid: u64, announced: &[String]) -> CloudResult<Vec<String>> {
    let mut serving = Vec::new();
    for raw in announced.iter().take(MAX_ANNOUNCED_STORES) {
        let Ok(store_id) = StoreId::parse(raw.trim()) else { continue };
        let store_id = store_id.as_uuid().to_string();
        if serving.contains(&store_id) {
            continue;
        }
        let Some(store) = state.db.find_hosted_store(&store_id).await? else { continue };
        if store.deleted || !store.is_relayed() {
            continue;
        }
        if state.db.find_grant(user_rid, store.rid, "").await?.is_some_and(|grant| grant.role == "owner") {
            serving.push(store_id);
        }
    }
    Ok(serving)
}

async fn run_tunnel(state: AppState, user_rid: u64, socket: WebSocket) {
    let limits = state.relay.limits().clone();
    let (sink, mut stream) = socket.split();

    let Some((tunnel, out_rx)) = state.relay.register_tunnel(user_rid) else {
        tracing::info!("relay: tunnel refused, the account has as many as it may");
        refuse(sink, stream, Close::new(CLOSE_OVER_LIMIT, "too many tunnels"), &limits).await;
        return;
    };

    let writer = tokio::spawn(write_half(sink, out_rx, tunnel.close.clone(), limits.clone()));
    let mut close_rx = tunnel.close.subscribe();

    let mut announced = false;
    let mut idle_at = Instant::now() + limits.announce_timeout;
    let close = loop {
        match read_next(&mut stream, &mut close_rx, idle_at).await {
            Incoming::Closed(close) => break close,
            Incoming::Gone { .. } => break Close::new(CLOSE_NORMAL, "bye"),
            Incoming::Idle if announced => break Close::new(CLOSE_GOING_AWAY, "no answer to pings"),
            Incoming::Idle => break Close::new(CLOSE_BAD_ANNOUNCE, "expected {\"serve\": [...]}"),
            Incoming::Message(Message::Text(text)) => {
                // `serve`, the first time or again: each one is the whole set
                // this tunnel serves from now on.
                let Ok(serve) = serde_json::from_str::<Serve>(&text) else {
                    break Close::new(CLOSE_BAD_ANNOUNCE, "expected {\"serve\": [...]}");
                };
                let serving = match servable(&state, user_rid, &serve.serve).await {
                    Ok(serving) => serving,
                    Err(e) => {
                        tracing::warn!(error = %e, "relay: could not check an announce against the database");
                        break Close::new(CLOSE_INTERNAL, "try again");
                    }
                };
                state.relay.announce(&tunnel, serving.iter().cloned().collect());
                announced = true;
                tracing::info!(stores = ?serving, announced = serve.serve.len(), "relay: tunnel serving");
                if tunnel.out.send(Message::Text(json!({ "serving": serving }).to_string())).await.is_err() {
                    break Close::new(CLOSE_GOING_AWAY, "connection lost");
                }
            }
            Incoming::Message(Message::Binary(_)) if !announced => break Close::new(CLOSE_BAD_ANNOUNCE, "expected {\"serve\": [...]}"),
            Incoming::Message(Message::Binary(bytes)) => {
                if bytes.len() < FRAME_HEADER_BYTES {
                    break Close::new(CLOSE_PROTOCOL, "a frame is [conn u32][kind u8][payload]");
                }
                let conn = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let kind = bytes[4];
                let mut payload = bytes;
                payload.drain(..FRAME_HEADER_BYTES);
                from_owner(&tunnel, conn, kind, payload, &limits).await;
            }
            // Pings are answered by the socket itself; a pong, like anything
            // else that arrives, is the peer being alive.
            Incoming::Message(_) => {}
        }
        // Counted from here rather than from the read, so time spent handing
        // a message on (which is time spent not listening) is never held
        // against the peer. Before the announce the deadline stands.
        if announced {
            idle_at = Instant::now() + limits.idle_timeout;
        }
    };

    state.relay.remove_tunnel(&tunnel);
    signal_close(&tunnel.close, close);
    let _ = writer.await;
    linger(&mut stream).await;
}

/// One frame from the owner, for virtual connection `conn`. A connection the
/// relay no longer has is not an error: its `close` and the owner's last
/// frames for it simply crossed.
async fn from_owner(tunnel: &Tunnel, conn: u32, kind: u8, payload: Vec<u8>, limits: &RelayLimits) {
    let Some((texts, close)) = tunnel.conn(conn) else { return };
    let ended = match kind {
        KIND_TEXT if payload.len() > limits.max_message_bytes => Some(Close::new(CLOSE_TOO_BIG, "message too large")),
        KIND_TEXT => match String::from_utf8(payload) {
            // Waiting here is the backpressure on the owner: nothing more is
            // read from the tunnel until this member has room. It is bounded,
            // so one member who stops reading costs the others a pause, not
            // the tunnel.
            Ok(text) => match texts.send_timeout(Message::Text(text), limits.stall_timeout).await {
                Ok(()) => None,
                Err(mpsc::error::SendTimeoutError::Timeout(_)) => Some(Close::new(CLOSE_TOO_SLOW, "not reading")),
                Err(mpsc::error::SendTimeoutError::Closed(_)) => None, // already closing
            },
            Err(_) => Some(Close::new(CLOSE_INVALID_DATA, "text must be UTF-8")),
        },
        KIND_CLOSE => Some(Close::from_owner_side(CLOSE_NORMAL, "closed by the owner")),
        _ => Some(Close::new(CLOSE_PROTOCOL, "unknown frame kind")),
    };
    if let Some(ended) = ended {
        // Forgotten here and now, so nothing later in the tunnel reaches it;
        // the member's own task does the rest (its socket, and telling the
        // owner when the owner is not who ended it).
        tunnel.remove_conn(conn);
        signal_close(&close, ended);
    }
}

// ── A member's connection ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MemberQuery {
    /// The JWT, for a browser: a WebSocket there cannot set a header.
    #[serde(default)]
    access_token: Option<String>,
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(str::trim).filter(|token| !token.is_empty())
}

/// A browser may connect from this service's own public origin and no other
/// (one origin serves the app, the API and the relay); anything that is not a
/// browser sends no `Origin` at all and is judged by its token alone.
fn origin_allowed(headers: &HeaderMap, public_url: &str) -> bool {
    match headers.get(ORIGIN) {
        None => true,
        Some(origin) => origin.to_str().is_ok_and(|origin| origin.eq_ignore_ascii_case(public_url.trim_end_matches('/'))),
    }
}

/// `GET /api/v1/relay/<store id>` — a member's connection. Everything that
/// can refuse it for who is asking happens before the upgrade, as plain HTTP
/// statuses; what can only be known afterwards (is the owner there, is there
/// room) is answered with a close code.
pub async fn member_connection(
    State(state): State<AppState>,
    Path(store_id): Path<String>,
    Query(query): Query<MemberQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, CloudError> {
    if !origin_allowed(&headers, &state.config.public_url) {
        return Err(CloudError::Forbidden("this origin may not use the relay".to_string()));
    }
    let store_id = StoreId::parse(store_id.trim()).map_err(|e| CloudError::BadRequest(format!("not a store id: {e}")))?.as_uuid().to_string();
    let token = bearer(&headers)
        .map(str::to_string)
        .or(query.access_token.filter(|token| !token.is_empty()))
        .ok_or_else(|| CloudError::Unauthorized("no Authorization: Bearer <token> header or access_token parameter".to_string()))?;
    let verified = state.signer.verify(&token).await.map_err(|rejection| match rejection {
        JwtRejection::KeysUnavailable(why) => CloudError::Internal(format!("verifying a relay token: {why}")),
        other => CloudError::Unauthorized(other.to_string()),
    })?;
    if !verified.names_store(&store_id) {
        return Err(CloudError::Forbidden("this token grants nothing on this store".to_string()));
    }
    // The token travels on to the owner's machine. Only one that is good for
    // this store and nothing else may (`VerifiedToken::names_only`).
    if !verified.names_only(&store_id) {
        return Err(CloudError::Forbidden(
            "the relay takes the token minted for this store alone (`stores[].token` in the answer of POST /api/v1/token), not the account's general one".to_string(),
        ));
    }

    let limits = state.relay.limits().clone();
    Ok(ws
        .max_message_size(limits.max_message_bytes)
        .max_frame_size(limits.max_message_bytes)
        .on_upgrade(move |socket| run_member(state, store_id, token, verified.exp, socket)))
}

async fn run_member(state: AppState, store_id: String, token: String, exp: i64, socket: WebSocket) {
    let limits = state.relay.limits().clone();
    let (sink, mut stream) = socket.split();

    let OpenedConn { tunnel, conn, texts, close } = match state.relay.open_conn(&store_id) {
        Ok(opened) => opened,
        Err(refused) => {
            let close = match refused {
                OpenRefused::OwnerOffline => Close::OWNER_OFFLINE,
                OpenRefused::TunnelFull => Close::new(CLOSE_OVER_LIMIT, "too many connections"),
            };
            refuse(sink, stream, close, &limits).await;
            return;
        }
    };
    tracing::debug!(store_id = %store_id, "relay: member connected");

    let writer = tokio::spawn(write_half(sink, texts, close.clone(), limits.clone()));
    let mut close_rx = close.subscribe();

    // Handing a frame to the tunnel waits for room on it (that wait is the
    // backpressure on this member) but never past this connection's end.
    let to_owner = |bytes: Vec<u8>| {
        let out = tunnel.out.clone();
        let mut close_rx = close.subscribe();
        async move {
            tokio::select! {
                biased;
                close = closed(&mut close_rx) => Err(close),
                sent = out.send(Message::Binary(bytes)) => sent.map_err(|_| Close::OWNER_OFFLINE),
            }
        }
    };

    let expires = tokio::time::sleep(Duration::from_secs(u64::try_from(exp - chrono::Utc::now().timestamp()).unwrap_or(0)));
    tokio::pin!(expires);

    // `open` first, carrying the token: the owner's server is who decides
    // what this member may do, so it is shown what the relay was shown.
    let ended = match to_owner(frame(conn, KIND_OPEN, token.as_bytes())).await {
        Err(close) => close,
        Ok(()) => {
            drop(token);
            let mut idle_at = Instant::now() + limits.idle_timeout;
            loop {
                let incoming = tokio::select! {
                    biased;
                    _ = &mut expires => break Close::new(CLOSE_TOKEN_EXPIRED, "token expired"),
                    incoming = read_next(&mut stream, &mut close_rx, idle_at) => incoming,
                };
                match incoming {
                    Incoming::Closed(close) => break close,
                    Incoming::Gone { too_big: true } => break Close::new(CLOSE_TOO_BIG, "message too large"),
                    Incoming::Gone { too_big: false } => break Close::new(CLOSE_NORMAL, "bye"),
                    Incoming::Idle => break Close::new(CLOSE_GOING_AWAY, "no answer to pings"),
                    Incoming::Message(Message::Text(text)) if text.len() > limits.max_message_bytes => {
                        break Close::new(CLOSE_TOO_BIG, "message too large");
                    }
                    Incoming::Message(Message::Text(text)) => {
                        if let Err(close) = to_owner(frame(conn, KIND_TEXT, text.as_bytes())).await {
                            break close;
                        }
                    }
                    Incoming::Message(Message::Binary(_)) => break Close::new(CLOSE_UNSUPPORTED, "only text messages are relayed"),
                    Incoming::Message(_) => {}
                }
                idle_at = Instant::now() + limits.idle_timeout;
            }
        }
    };

    tunnel.remove_conn(conn);
    signal_close(&close, ended);
    // Whoever set the signal first is why this ended, and says whether the
    // owner already knows.
    let ended = (*close.borrow()).unwrap_or(ended);
    if ended.tell_owner {
        // Past the connection's own end, so not through `to_owner`; bounded
        // by the tunnel's writer, which gives up on a stuck owner by itself.
        let _ = tunnel.out.send(Message::Binary(frame(conn, KIND_CLOSE, &[]))).await;
    }
    tracing::debug!(store_id = %store_id, code = ended.code, "relay: member disconnected");
    let _ = writer.await;
    linger(&mut stream).await;
}

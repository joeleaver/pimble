//! The owner's end of Pimble Cloud's relay (docs/RELAY_CONTRACT.md, "The
//! tunnel client"; the wire is `pimble-cloud`'s `src/relay.rs`).
//!
//! One task per server while anything is shared from this computer. It opens
//! one outbound WebSocket to `<account url>/api/v1/relay` with the signed-in
//! account's session, says which stores it serves, and from then on is the
//! other end of every member's connection to them: for each `open` frame it
//! connects a loopback WebSocket to the relay face
//! (`crate::relay_face`) carrying the member's own JWT as `Authorization:
//! Bearer`, and pipes text both ways. The face verifies that token itself:
//! the relay keeps strangers off the tunnel and is trusted for nothing else,
//! least of all for who somebody is.
//!
//! The wire: the tunnel's only text traffic is `{"serve": [...]}` (from
//! here; each one replaces the set) and `{"serving": [...]}` (the answer).
//! Everything else is binary `[conn: u32 BE][kind: u8][payload]`, kinds
//! `1 open` (payload: the member's JWT), `2 text` (one JSON-RPC message),
//! `3 close`.
//!
//! Nothing is buffered beyond what is in flight. Every queue is a small
//! bounded channel; a side that cannot hand a message on stops reading, so
//! the sender is pushed back on, and a peer that stays stuck costs that one
//! connection, not the tunnel. What is piped is already end-to-end
//! encrypted (the same `PB` blobs a hosted store holds) inside JSON-RPC the
//! face answers.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use pimble_core::StoreId;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};
use url::Url;

use crate::handler::RpcHandler;
use crate::vault_link::{INITIAL_BACKOFF, MAX_BACKOFF};

const KIND_OPEN: u8 = 1;
const KIND_TEXT: u8 = 2;
const KIND_CLOSE: u8 = 3;
/// `[conn: u32 BE][kind: u8]`.
const FRAME_HEADER_BYTES: usize = 5;

/// What the relay itself allows one relayed message to be; a Pimble server
/// takes 10 MiB, so neither end of the pipe is the stricter one.
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// Frames waiting for the tunnel's socket, from every connection together.
const TUNNEL_QUEUE: usize = 8;
/// Messages waiting for one member's connection to the face.
const CONN_QUEUE: usize = 2;
/// The relay allows 64 connections a tunnel; one more than that is refused
/// here too, whatever the relay is.
const MAX_CONNS: usize = 64;
/// The platform edge reaps a connection after 600 s of silence; the relay
/// pings every 30 s and so does this end.
const PING_EVERY: Duration = Duration::from_secs(30);
/// A tunnel that has said nothing for this long, pings included, is gone.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// How long a peer may refuse to take a message before it is given up on.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The relay's close code for a tunnel whose every store a later tunnel took
/// over (`pimble-cloud`'s `relay::CLOSE_REPLACED`): this server's own
/// earlier socket after a restart, or another computer serving the same
/// stores.
const CLOSE_REPLACED: u16 = 4409;

/// The tunnel was closed because a later one serves everything it did.
#[derive(Debug)]
struct Replaced;

impl std::fmt::Display for Replaced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a later tunnel took over every store this one served")
    }
}

impl std::error::Error for Replaced {}

fn frame(conn: u32, kind: u8, payload: &[u8]) -> Message {
    let mut out = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
    out.extend_from_slice(&conn.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    Message::Binary(out)
}

fn serve_message(stores: &HashSet<StoreId>) -> Message {
    let ids: Vec<String> = stores.iter().map(|id| id.to_string()).collect();
    Message::Text(serde_json::json!({ "serve": ids }).to_string())
}

fn socket_config(max_message: usize) -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(max_message);
    config.max_frame_size = Some(max_message);
    config
}

fn bearer_request(url: &Url, token: &str) -> anyhow::Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
    let mut request = url.as_str().into_client_request()?;
    let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| anyhow::anyhow!("the credential is not a header value"))?;
    request.headers_mut().insert(AUTHORIZATION, value);
    Ok(request)
}

/// Run until aborted (`crate::relay_face::RelayHost` owns the task). While
/// `serve` names nothing there is no tunnel; while it names something the
/// tunnel is kept up, with the vault link's backoff between attempts.
pub(crate) async fn run(handler: RpcHandler, mut serve: watch::Receiver<HashSet<StoreId>>) {
    let mut backoff = INITIAL_BACKOFF;
    // A tunnel that cannot be opened is the ordinary state of a laptop with
    // no network: said once when it goes and once when it is back.
    let mut told_down = false;
    loop {
        while serve.borrow_and_update().is_empty() {
            if serve.changed().await.is_err() {
                return;
            }
        }
        let mut served = false;
        let result = serve_once(&handler, &mut serve, &mut served).await;
        if served {
            backoff = INITIAL_BACKOFF;
            told_down = false;
        }
        match result {
            // Nothing left to announce: no tunnel until there is.
            Ok(()) => {
                info!("Relay tunnel closed: nothing is shared from this computer right now");
                continue;
            }
            // Two computers that both share the same store (its directory
            // was copied) would take it from each other for ever at a
            // second's notice. The later one keeps it; this one asks again
            // only now and then.
            Err(e) if e.is::<Replaced>() => {
                warn!("Relay tunnel closed: {}. If another computer shares the same stores, only one of them can at a time", e);
                backoff = MAX_BACKOFF;
            }
            Err(e) if !told_down => {
                info!("Relay tunnel is down ({}); trying again, quietly, until it is back", e);
                told_down = true;
            }
            Err(e) => debug!("Relay tunnel still down: {}", e),
        }
        // The wait, cut short when what is served changes: a store relayed
        // a moment ago should not sit out the rest of a thirty-second wait.
        tokio::select! {
            _ = tokio::time::sleep(backoff) => backoff = (backoff * 2).min(MAX_BACKOFF),
            changed = serve.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// One tunnel, from connect to its end. `Ok` when it was closed from here
/// because there is nothing to serve any more.
async fn serve_once(handler: &RpcHandler, serve: &mut watch::Receiver<HashSet<StoreId>>, served: &mut bool) -> anyhow::Result<()> {
    let account = handler.keystore().account().await.ok_or_else(|| anyhow::anyhow!("no Pimble Cloud account is signed in"))?;
    let url = crate::cloud::relay_tunnel_url(&account.url).map_err(|e| anyhow::anyhow!(e))?;
    let request = bearer_request(&url, &account.session)?;
    // Twice a message and a header, as the relay's own end of the tunnel
    // reads: a frame wraps a whole member message.
    let config = socket_config(MAX_MESSAGE_BYTES * 2 + FRAME_HEADER_BYTES);
    let (socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async_with_config(request, Some(config), true))
        .await
        .map_err(|_| anyhow::anyhow!("connecting to {} timed out", url))?
        .map_err(|e| anyhow::anyhow!("connecting to {} failed: {}", url, e))?;
    let (mut sink, mut stream) = socket.split();

    // The write half is a task of its own, so reading never waits on
    // writing: two piped peers each stuck behind their own writer is how a
    // relay deadlocks.
    let (out, mut out_rx) = mpsc::channel::<Message>(TUNNEL_QUEUE);
    let mut writer = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if !matches!(tokio::time::timeout(STALL_TIMEOUT, sink.send(message)).await, Ok(Ok(()))) {
                break;
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), sink.close()).await;
    });

    let announced = serve.borrow_and_update().clone();
    if announced.is_empty() {
        return Ok(());
    }
    out.send(serve_message(&announced)).await.map_err(|_| anyhow::anyhow!("the tunnel closed before anything was said"))?;

    let mut conns: HashMap<u32, mpsc::Sender<String>> = HashMap::new();
    // Dropped with this call: every connection's task ends with its tunnel.
    let mut tasks: JoinSet<()> = JoinSet::new();
    let (done, mut done_rx) = mpsc::channel::<u32>(MAX_CONNS);
    let mut ping = tokio::time::interval_at(Instant::now() + PING_EVERY, PING_EVERY);
    let mut idle_at = Instant::now() + IDLE_TIMEOUT;

    loop {
        tokio::select! {
            incoming = stream.next() => {
                idle_at = Instant::now() + IDLE_TIMEOUT;
                match incoming {
                    None => return Err(anyhow::anyhow!("the relay closed the tunnel")),
                    Some(Err(e)) => return Err(anyhow::anyhow!("the tunnel broke: {}", e)),
                    Some(Ok(Message::Close(close))) => {
                        if close.as_ref().is_some_and(|c| u16::from(c.code) == CLOSE_REPLACED) {
                            return Err(anyhow::Error::new(Replaced));
                        }
                        let why = close.map(|c| format!("{} {}", u16::from(c.code), c.reason)).unwrap_or_else(|| "no reason given".to_string());
                        return Err(anyhow::anyhow!("the relay closed the tunnel ({})", why));
                    }
                    Some(Ok(Message::Text(text))) => {
                        // `{"serving": [...]}`: the announced stores the relay
                        // took. One it left out is not this account's to
                        // serve as far as the accounts service knows.
                        let serving = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v.get("serving").and_then(|s| s.as_array().map(|a| a.len())));
                        match serving {
                            Some(count) => {
                                if !*served {
                                    info!("Relay tunnel up: serving {} store(s) through {}", count, url);
                                }
                                *served = true;
                                let asked = serve.borrow().len();
                                if count < asked {
                                    warn!("Relay tunnel: the relay serves {} of the {} stores announced; the others are not recorded as shared from this account", count, asked);
                                }
                            }
                            None => debug!("Relay tunnel: unexpected text from the relay; ignoring it"),
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        if bytes.len() < FRAME_HEADER_BYTES {
                            return Err(anyhow::anyhow!("the relay sent a frame shorter than its header"));
                        }
                        let conn = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                        let kind = bytes[4];
                        let payload = &bytes[FRAME_HEADER_BYTES..];
                        match kind {
                            KIND_OPEN => {
                                let opened = match (std::str::from_utf8(payload), handler.relay().face_url()) {
                                    (Ok(jwt), Some(face_url)) if conns.len() < MAX_CONNS && !conns.contains_key(&conn) => {
                                        let (to_face, from_member) = mpsc::channel(CONN_QUEUE);
                                        conns.insert(conn, to_face);
                                        tasks.spawn(run_conn(conn, jwt.to_string(), face_url, from_member, out.clone(), done.clone()));
                                        true
                                    }
                                    _ => false,
                                };
                                if !opened {
                                    out.send(frame(conn, KIND_CLOSE, &[])).await.map_err(|_| anyhow::anyhow!("the tunnel's writer ended"))?;
                                }
                            }
                            KIND_TEXT => {
                                let Some(to_face) = conns.get(&conn) else { continue };
                                // Waiting here is the backpressure on the
                                // member: nothing more is read from the tunnel
                                // until this connection has room. Bounded, so
                                // one that stops taking costs the others a
                                // pause and not the tunnel.
                                let delivered = match String::from_utf8(payload.to_vec()) {
                                    Ok(text) => match to_face.send_timeout(text, STALL_TIMEOUT).await {
                                        Ok(()) => true,
                                        // Ended already; its own `close` is on its way.
                                        Err(mpsc::error::SendTimeoutError::Closed(_)) => {
                                            conns.remove(&conn);
                                            true
                                        }
                                        Err(mpsc::error::SendTimeoutError::Timeout(_)) => false,
                                    },
                                    Err(_) => false,
                                };
                                if !delivered {
                                    conns.remove(&conn);
                                    out.send(frame(conn, KIND_CLOSE, &[])).await.map_err(|_| anyhow::anyhow!("the tunnel's writer ended"))?;
                                }
                            }
                            // The member went (or the relay let go of them):
                            // dropping the sender ends the connection's task,
                            // which has nothing to tell the relay.
                            KIND_CLOSE => {
                                conns.remove(&conn);
                            }
                            other => debug!("Relay tunnel: a frame of unknown kind {} for connection {}; ignoring it", other, conn),
                        }
                    }
                    // A ping is answered by the socket itself; a pong, like
                    // anything else, is the relay being alive.
                    Some(Ok(_)) => {}
                }
            }
            changed = serve.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                let now = serve.borrow_and_update().clone();
                if now.is_empty() {
                    return Ok(());
                }
                // Each `serve` is the whole set from now on: a store left
                // out is withdrawn, and its members are told `owner offline`.
                out.send(serve_message(&now)).await.map_err(|_| anyhow::anyhow!("the tunnel's writer ended"))?;
            }
            Some(conn) = done_rx.recv() => {
                conns.remove(&conn);
            }
            Some(_) = tasks.join_next() => {}
            _ = ping.tick() => {
                out.send(Message::Ping(Vec::new())).await.map_err(|_| anyhow::anyhow!("the tunnel's writer ended"))?;
            }
            _ = tokio::time::sleep_until(idle_at) => return Err(anyhow::anyhow!("the relay has not answered for {} s", IDLE_TIMEOUT.as_secs())),
            _ = &mut writer => return Err(anyhow::anyhow!("the tunnel stopped taking what was sent")),
        }
    }
}

/// The relay's close code for a member's connection to a store nobody is
/// serving right now (`pimble-cloud`'s `relay::CLOSE_OWNER_OFFLINE`).
const CLOSE_OWNER_OFFLINE: u16 = 4404;

/// A member's side: whether the relay at `url` says the store's owner is
/// not there (close code 4404, `owner offline`). The JSON-RPC client a vault
/// link connects with does not pass a close frame's code on, so a link whose
/// connection to a relayed store just failed asks here, with the token it
/// had, to tell "the owner's computer is off" (an ordinary `Offline`) from
/// something worth a warning. Anything else, a refusal before the upgrade
/// included, is `false`.
pub(crate) async fn owner_is_offline(url: &Url, token: &str) -> bool {
    let Ok(request) = bearer_request(url, token) else { return false };
    let Ok(Ok((mut socket, _))) = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request)).await else { return false };
    // The refusal is the first thing the relay says, or it says nothing.
    let first = tokio::time::timeout(Duration::from_secs(2), socket.next()).await;
    let offline = matches!(&first, Ok(Some(Ok(Message::Close(Some(close))))) if u16::from(close.code) == CLOSE_OWNER_OFFLINE);
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.close(None)).await;
    offline
}

/// How one member's connection ended.
enum Ended {
    /// The face closed it, refused it (the token did not verify), or broke:
    /// the relay has to be told, so it lets the member go.
    Face,
    /// The relay said the member is gone, or the tunnel is: nobody to tell.
    Member,
}

/// One member's connection: a loopback WebSocket to the face with the
/// member's own token, piped to and from the tunnel.
async fn run_conn(conn: u32, jwt: String, face_url: Url, from_member: mpsc::Receiver<String>, out: mpsc::Sender<Message>, done: mpsc::Sender<u32>) {
    if let Ended::Face = pipe(conn, &jwt, &face_url, from_member, &out).await {
        let _ = out.send(frame(conn, KIND_CLOSE, &[])).await;
    }
    let _ = done.send(conn).await;
}

async fn pipe(conn: u32, jwt: &str, face_url: &Url, mut from_member: mpsc::Receiver<String>, out: &mpsc::Sender<Message>) -> Ended {
    let Ok(request) = bearer_request(face_url, jwt) else { return Ended::Face };
    let connected = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async_with_config(request, Some(socket_config(MAX_MESSAGE_BYTES)), true)).await;
    let socket = match connected {
        Ok(Ok((socket, _))) => socket,
        // A token the face does not take (expired, another issuer, no
        // grant it can read) is refused at the handshake.
        Ok(Err(e)) => {
            debug!("Relay tunnel: the face refused connection {}: {}", conn, e);
            return Ended::Face;
        }
        Err(_) => return Ended::Face,
    };
    let (mut to_face, mut from_face) = socket.split();

    // Two loops polled side by side, so a full tunnel never stops this
    // connection from taking what the member sent, nor the other way round.
    let member_to_face = async {
        while let Some(text) = from_member.recv().await {
            if !matches!(tokio::time::timeout(STALL_TIMEOUT, to_face.send(Message::Text(text))).await, Ok(Ok(()))) {
                return Ended::Face;
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), to_face.close()).await;
        Ended::Member
    };
    let face_to_member = async {
        while let Some(Ok(message)) = from_face.next().await {
            match message {
                Message::Text(text) => {
                    if out.send(frame(conn, KIND_TEXT, text.as_bytes())).await.is_err() {
                        return Ended::Member;
                    }
                }
                Message::Close(_) => break,
                // A Pimble server speaks JSON-RPC as text and nothing else.
                _ => {}
            }
        }
        Ended::Face
    };
    tokio::select! {
        ended = member_to_face => ended,
        ended = face_to_member => ended,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_conn_kind_payload() {
        let Message::Binary(bytes) = frame(0x0102_0304, KIND_TEXT, b"{}") else { panic!("frames are binary") };
        assert_eq!(bytes, vec![1, 2, 3, 4, KIND_TEXT, b'{', b'}']);
        let Message::Binary(bytes) = frame(7, KIND_CLOSE, &[]) else { panic!("frames are binary") };
        assert_eq!(bytes.len(), FRAME_HEADER_BYTES);
    }

    #[test]
    fn serve_names_every_store_by_its_id() {
        let (a, b) = (StoreId::new(), StoreId::new());
        let Message::Text(text) = serve_message(&[a, b].into_iter().collect()) else { panic!("serve is text") };
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut ids: Vec<String> = value["serve"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        ids.sort();
        let mut want = vec![a.to_string(), b.to_string()];
        want.sort();
        assert_eq!(ids, want);
    }
}

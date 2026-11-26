//! The relay stub: the owner's tunnel and members' connections, in memory.

use super::*;

// ── The relay stub ────────────────────────────────────────────────────────
//
// `pimble-cloud`'s `src/relay.rs`, as small as it can be and still be the
// same wire: session auth and no `Origin` for the tunnel, `serve`/`serving`,
// `[conn u32 BE][kind u8][payload]` frames, a member's token that must name
// this store and no other, 4404 `owner offline`. Nothing is kept: a message
// is in a channel or it is nowhere.

pub(crate) const KIND_OPEN: u8 = 1;
pub(crate) const KIND_TEXT: u8 = 2;
pub(crate) const KIND_CLOSE: u8 = 3;
pub(crate) const CLOSE_OWNER_OFFLINE: u16 = 4404;

pub(crate) type ServerSocket = WebSocketStream<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>>;

pub(crate) enum ToMember {
    Text(String),
    Close(u16, &'static str),
}

pub(crate) struct TunnelStub {
    pub(crate) out: mpsc::Sender<Message>,
    pub(crate) conns: Mutex<HashMap<u32, mpsc::Sender<ToMember>>>,
}

impl TunnelStub {
    pub(crate) fn close_all(&self, code: u16, reason: &'static str) {
        for (_, member) in self.conns.lock().unwrap().drain() {
            let _ = member.try_send(ToMember::Close(code, reason));
        }
    }
}

#[derive(Default)]
pub(crate) struct RelayStub {
    /// Store id to the tunnel it is behind.
    pub(crate) by_store: Mutex<HashMap<String, Arc<TunnelStub>>>,
    pub(crate) next_conn: AtomicU32,
    /// Members' connections that were piped to an owner.
    pub(crate) piped: AtomicUsize,
    /// Members' connections answered `owner offline`.
    pub(crate) owner_offline: AtomicUsize,
    /// Members' connections refused before the upgrade for a token that
    /// names another store too (the account's general token).
    pub(crate) wrong_token: AtomicUsize,
}

impl RelayStub {
    pub(crate) fn withdraw(&self, store_id: &str) {
        if let Some(tunnel) = self.by_store.lock().unwrap().remove(store_id) {
            tunnel.close_all(CLOSE_OWNER_OFFLINE, "owner offline");
        }
    }

    pub(crate) fn serves(&self, store_id: StoreId) -> bool {
        self.by_store.lock().unwrap().contains_key(&store_id.to_string())
    }

    /// The virtual connections in flight, over every tunnel.
    pub(crate) fn open_conns(&self) -> usize {
        let tunnels: Vec<Arc<TunnelStub>> = self.by_store.lock().unwrap().values().cloned().collect();
        let mut seen: Vec<*const TunnelStub> = Vec::new();
        let mut count = 0;
        for tunnel in tunnels {
            if !seen.contains(&Arc::as_ptr(&tunnel)) {
                seen.push(Arc::as_ptr(&tunnel));
                count += tunnel.conns.lock().unwrap().len();
            }
        }
        count
    }
}

pub(crate) fn frame(conn: u32, kind: u8, payload: &[u8]) -> Message {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.extend_from_slice(&conn.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    Message::Binary(out)
}

pub(crate) fn close_message(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.into() }))
}

/// Answer `101` and hand the upgraded socket to `run`.
pub(crate) fn upgrade<F, Fut>(mut request: Request, run: F) -> Response
where
    F: FnOnce(ServerSocket) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let Some(key) = request.headers().get("sec-websocket-key").cloned() else { return StatusCode::BAD_REQUEST.into_response() };
    let Some(on_upgrade) = request.extensions_mut().remove::<hyper::upgrade::OnUpgrade>() else { return StatusCode::BAD_REQUEST.into_response() };
    tokio::spawn(async move {
        if let Ok(upgraded) = on_upgrade.await {
            run(WebSocketStream::from_raw_socket(hyper_util::rt::TokioIo::new(upgraded), Role::Server, None).await).await;
        }
    });
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_ACCEPT, accept)
        .body(axum::body::Body::empty())
        .unwrap()
}

/// `GET /api/v1/relay`: the owner's tunnel. The account's session, and never
/// a browser.
pub(crate) async fn stub_owner_tunnel(State(state): State<StubState>, request: Request) -> Response {
    if request.headers().contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let user_id = {
        let s = state.inner.lock().unwrap();
        let session = session_of(request.headers());
        s.accounts.iter().find(|a| Some(a.session.as_str()) == session).map(|a| a.user_id.clone())
    };
    let Some(user_id) = user_id else { return StatusCode::UNAUTHORIZED.into_response() };
    upgrade(request, move |socket| run_tunnel(state, user_id, socket))
}

pub(crate) async fn run_tunnel(state: StubState, user_id: String, socket: ServerSocket) {
    let (mut sink, mut stream) = socket.split();
    let (out, mut out_rx) = mpsc::channel::<Message>(8);
    let tunnel = Arc::new(TunnelStub { out, conns: Mutex::new(HashMap::new()) });
    let writer = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(message)) = stream.next().await {
        match message {
            // Each `serve` is the whole set from now on.
            Message::Text(text) => {
                let Ok(serve) = serde_json::from_str::<serde_json::Value>(&text) else { break };
                let announced: Vec<String> = serve["serve"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
                let serving: Vec<String> = {
                    let s = state.inner.lock().unwrap();
                    announced.into_iter().filter(|id| s.tier_of(id) == "relay" && s.is_owner(&user_id, id)).collect()
                };
                {
                    let mut by_store = state.relay.by_store.lock().unwrap();
                    by_store.retain(|id, held| !Arc::ptr_eq(held, &tunnel) || serving.contains(id));
                    for id in &serving {
                        if let Some(earlier) = by_store.insert(id.clone(), tunnel.clone()).filter(|earlier| !Arc::ptr_eq(earlier, &tunnel)) {
                            earlier.close_all(CLOSE_OWNER_OFFLINE, "owner offline");
                        }
                    }
                }
                if tunnel.out.send(Message::Text(json!({ "serving": serving }).to_string())).await.is_err() {
                    break;
                }
            }
            Message::Binary(bytes) if bytes.len() >= 5 => {
                let conn = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let member = tunnel.conns.lock().unwrap().get(&conn).cloned();
                match (bytes[4], member) {
                    (KIND_TEXT, Some(member)) => {
                        if let Ok(text) = String::from_utf8(bytes[5..].to_vec()) {
                            let _ = member.send(ToMember::Text(text)).await;
                        }
                    }
                    (KIND_CLOSE, Some(member)) => {
                        tunnel.conns.lock().unwrap().remove(&conn);
                        let _ = member.send(ToMember::Close(1000, "closed by the owner")).await;
                    }
                    _ => {}
                }
            }
            Message::Binary(_) | Message::Close(_) => break,
            _ => {}
        }
    }

    // The tunnel is gone: its stores are behind nothing, and every member
    // through it is told so.
    state.relay.by_store.lock().unwrap().retain(|_, held| !Arc::ptr_eq(held, &tunnel));
    tunnel.close_all(CLOSE_OWNER_OFFLINE, "owner offline");
    writer.abort();
}

/// `GET /api/v1/relay/<store id>`: a member's connection. Its token is the
/// stub's own, and names this store and no other.
pub(crate) async fn stub_member_connection(State(state): State<StubState>, AxPath(store_id): AxPath<String>, request: Request) -> Response {
    let Some(token) = session_of(request.headers()).map(str::to_string) else { return StatusCode::UNAUTHORIZED.into_response() };
    let named = {
        let s = state.inner.lock().unwrap();
        verified_stores(&s.signing_key, &token)
    };
    let Some(named) = named else { return StatusCode::UNAUTHORIZED.into_response() };
    if named != vec![store_id.clone()] {
        state.relay.wrong_token.fetch_add(1, Ordering::SeqCst);
        return StatusCode::FORBIDDEN.into_response();
    }
    upgrade(request, move |socket| run_member(state, store_id, token, socket))
}

pub(crate) async fn run_member(state: StubState, store_id: String, token: String, mut socket: ServerSocket) {
    let tunnel = state.relay.by_store.lock().unwrap().get(&store_id).cloned();
    let Some(tunnel) = tunnel else {
        state.relay.owner_offline.fetch_add(1, Ordering::SeqCst);
        let _ = socket.send(close_message(CLOSE_OWNER_OFFLINE, "owner offline")).await;
        // Read until the peer has closed too, so the close frame is not
        // overtaken by a reset.
        let _ = tokio::time::timeout(Duration::from_secs(2), async { while let Some(Ok(_)) = socket.next().await {} }).await;
        return;
    };
    state.relay.piped.fetch_add(1, Ordering::SeqCst);
    let conn = state.relay.next_conn.fetch_add(1, Ordering::SeqCst) + 1;
    let (to_member, mut from_owner) = mpsc::channel::<ToMember>(2);
    tunnel.conns.lock().unwrap().insert(conn, to_member);
    let mut owner_knows = false;
    if tunnel.out.send(frame(conn, KIND_OPEN, token.as_bytes())).await.is_ok() {
        loop {
            tokio::select! {
                incoming = socket.next() => match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if tunnel.out.send(frame(conn, KIND_TEXT, text.as_bytes())).await.is_err() {
                            let _ = socket.send(close_message(CLOSE_OWNER_OFFLINE, "owner offline")).await;
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                outgoing = from_owner.recv() => match outgoing {
                    Some(ToMember::Text(text)) => {
                        if socket.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Some(ToMember::Close(code, reason)) => {
                        owner_knows = true;
                        let _ = socket.send(close_message(code, reason)).await;
                        let _ = tokio::time::timeout(Duration::from_secs(2), async { while let Some(Ok(_)) = socket.next().await {} }).await;
                        break;
                    }
                    None => break,
                },
            }
        }
    }
    tunnel.conns.lock().unwrap().remove(&conn);
    if !owner_knows {
        let _ = tunnel.out.send(frame(conn, KIND_CLOSE, &[])).await;
    }
}

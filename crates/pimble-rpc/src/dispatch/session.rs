//! A whole JSON-RPC connection with no transport and no runtime.
//!
//! [`serve`] reads JSON-RPC text from a stream, answers through the
//! dispatch and writes JSON-RPC text back: method calls (one at a time or
//! in a batch), subscriptions with their notifications and unsubscribes,
//! what jsonrpsee's client sends and expects. It runs every call of the
//! connection inside its own future (no `spawn`), so it needs nothing of
//! the host but something to poll it: a tokio task natively, a
//! `spawn_local` in a browser's worker.
//!
//! [`channel`] makes an in-process connection (`PimbleClient::in_process`
//! is its client half); docs/ONE_CODEBASE_CONTRACT.md wave 3 puts a
//! `MessagePort` where the channel is.

use std::collections::HashMap;
use std::sync::Arc;

use futures::stream::{FuturesUnordered, SelectAll};
use futures::{FutureExt, Stream, StreamExt};
use jsonrpsee::types::error::ErrorCode;
use jsonrpsee::types::response::{SubscriptionPayload, SubscriptionPayloadError, SubscriptionResponse};
use jsonrpsee::types::{ErrorObject, ErrorObjectOwned, Id, Notification, Params, Request, Response, ResponsePayload, SubscriptionId};
use serde_json::value::RawValue;
use tokio::sync::{mpsc, oneshot};

use super::{BoxFuture, Decision, Extensions, MethodKind, SubscriptionMessage, SubscriptionResult};
use crate::PimbleApiServer;

/// The client's half of an in-process connection: what it writes to the
/// server and what it reads back, JSON-RPC text both ways.
#[derive(Debug)]
pub struct ClientEnd {
    pub to_server: mpsc::UnboundedSender<String>,
    pub from_server: mpsc::UnboundedReceiver<String>,
}

/// The server's half: hand it to [`serve`].
#[derive(Debug)]
pub struct ServerEnd {
    pub from_client: mpsc::UnboundedReceiver<String>,
    pub to_client: mpsc::UnboundedSender<String>,
}

/// An in-process connection's two ends.
pub fn channel() -> (ClientEnd, ServerEnd) {
    let (to_server, from_client) = mpsc::unbounded_channel();
    let (to_client, from_server) = mpsc::unbounded_channel();
    (ClientEnd { to_server, from_server }, ServerEnd { from_client, to_client })
}

/// Serve one connection on `end` until the client drops its end. Every call
/// carries `ext` (the connection's principal, attached by the caller).
pub async fn serve<S: PimbleApiServer>(server: Arc<S>, ext: Extensions, end: ServerEnd) {
    let ServerEnd { from_client, to_client } = end;
    let incoming = futures::stream::unfold(from_client, |mut rx| async move { rx.recv().await.map(|text| (text, rx)) });
    serve_with(server, ext, Box::pin(incoming), move |text| to_client.send(text).is_ok()).await
}

/// Serve one connection: JSON-RPC text from `incoming`, answers to `send`
/// (which says `false` once the client has gone). Ends when either does.
pub async fn serve_with<S: PimbleApiServer>(
    server: Arc<S>,
    ext: Extensions,
    incoming: impl Stream<Item = String> + Unpin,
    mut send: impl FnMut(String) -> bool,
) {
    let mut incoming = incoming.fuse();
    let mut connection = Connection { server, ext, tasks: FuturesUnordered::new(), streams: SelectAll::new(), subscriptions: HashMap::new(), next_key: 0 };
    loop {
        let out = futures::select! {
            text = incoming.next() => match text {
                Some(text) => connection.received(&text),
                None => break,
            },
            done = connection.tasks.select_next_some() => connection.finished(done),
            item = connection.streams.select_next_some() => connection.notify(item),
        };
        for text in out {
            if !send(text) {
                return;
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
type BoxStream<T> = futures::stream::BoxStream<'static, T>;
#[cfg(target_arch = "wasm32")]
type BoxStream<T> = futures::stream::LocalBoxStream<'static, T>;

/// What one of a connection's futures finished with.
enum Done {
    /// Text to send (a call's or a batch's answer).
    Text(Option<String>),
    /// A subscribe method's decision.
    Decided { key: u64, id: Id<'static>, decision: Option<Decision>, messages: mpsc::Receiver<SubscriptionMessage> },
    /// A subscribe method finished.
    Ran { key: u64, result: SubscriptionResult },
}

struct Live {
    id: SubscriptionId<'static>,
    notification: &'static str,
    unsubscribe: &'static str,
    /// Dropped to end the subscription's stream; `None` until the server
    /// has accepted it.
    cancel: Option<oneshot::Sender<()>>,
}

struct Connection<S> {
    server: Arc<S>,
    ext: Extensions,
    tasks: FuturesUnordered<BoxFuture<'static, Done>>,
    streams: SelectAll<BoxStream<(u64, Option<SubscriptionMessage>)>>,
    /// Subscriptions by key (from the subscribe call; accepted once `cancel`
    /// is set); the subscription id on the wire is the key.
    subscriptions: HashMap<u64, Live>,
    next_key: u64,
}

fn text_of<T: serde::Serialize>(value: &T) -> Option<String> {
    serde_json::to_string(value).ok()
}

fn response(id: Id<'_>, answer: Result<Box<RawValue>, ErrorObjectOwned>) -> Option<String> {
    let payload = match answer {
        Ok(result) => ResponsePayload::success(result),
        Err(e) => ResponsePayload::error(e),
    };
    text_of(&Response::new(payload, id))
}

fn error_response(id: Id<'_>, code: ErrorCode) -> Option<String> {
    response(id, Err(ErrorObject::from(code)))
}

impl<S: PimbleApiServer> Connection<S> {
    fn received(&mut self, text: &str) -> Vec<String> {
        if text.trim_start().starts_with('[') {
            self.batch(text);
            return Vec::new();
        }
        match serde_json::from_str::<Request>(text) {
            Ok(request) => self.request(request).into_iter().collect(),
            // A notification asks for no answer, and the API has none.
            Err(_) if serde_json::from_str::<Notification<Option<&RawValue>>>(text).is_ok() => Vec::new(),
            Err(e) => {
                let code = if e.is_syntax() || e.is_eof() { ErrorCode::ParseError } else { ErrorCode::InvalidRequest };
                error_response(Id::Null, code).into_iter().collect()
            }
        }
    }

    /// One request. Answered now (an unsubscribe, an unknown method), or
    /// later by one of `tasks`.
    fn request(&mut self, request: Request) -> Option<String> {
        let id = request.id.into_owned();
        let method = request.method.into_owned();
        let params = request.params.map(|p| p.get().to_owned());
        if let Some(api) = super::method(&method) {
            match api.kind {
                MethodKind::Call => {
                    let (server, ext) = (Arc::clone(&self.server), self.ext.clone());
                    self.tasks.push(Box::pin(async move {
                        let answer = super::call(&*server, &ext, &method, params.as_deref()).await;
                        Done::Text(response(id, answer))
                    }));
                }
                MethodKind::Subscription { .. } => self.subscribe(id, &method, params),
            }
            return None;
        }
        if let Some(api) = super::unsubscribe_of(&method) {
            return self.unsubscribe(id, api.name, params.as_deref());
        }
        error_response(id, ErrorCode::MethodNotFound)
    }

    fn subscribe(&mut self, id: Id<'static>, method: &str, params: Option<String>) {
        let Some(subscribing) = super::subscribe(Arc::clone(&self.server), self.ext.clone(), method, params) else { return };
        let key = self.next_key;
        self.next_key += 1;
        let super::Subscribing { notification, unsubscribe, run, opening } = subscribing;
        self.tasks.push(Box::pin(run.map(move |result| Done::Ran { key, result })));
        let super::Opening { decision, messages } = opening;
        self.tasks.push(Box::pin(async move { Done::Decided { key, id, decision: decision.await.ok(), messages } }));
        // Remembered with the key until the decision comes.
        self.subscriptions.insert(
            key,
            Live { id: SubscriptionId::Num(key), notification, unsubscribe, cancel: None },
        );
    }

    fn unsubscribe(&mut self, id: Id<'static>, subscribe: &str, params: Option<&str>) -> Option<String> {
        let params = Params::new(params);
        let sub_id = match params.one::<SubscriptionId>() {
            Ok(sub_id) => sub_id.into_owned(),
            Err(e) => return response(id, Err(e)),
        };
        let unsubscribe_name = super::method(subscribe).and_then(|m| match m.kind {
            MethodKind::Subscription { unsubscribe, .. } => Some(unsubscribe),
            MethodKind::Call => None,
        });
        let key = self
            .subscriptions
            .iter()
            .find(|(_, live)| live.id == sub_id && Some(live.unsubscribe) == unsubscribe_name)
            .map(|(key, _)| *key);
        // Dropping it ends the stream, which drops the receiver: the
        // server's sink reads closed.
        let found = key.and_then(|key| self.subscriptions.remove(&key)).is_some();
        response(id, super::answer(Ok(found)))
    }

    fn finished(&mut self, done: Done) -> Vec<String> {
        match done {
            Done::Text(text) => text.into_iter().collect(),
            Done::Decided { key, id, decision, messages } => match decision {
                Some(Decision::Accept(reply)) => {
                    let Some(live) = self.subscriptions.get_mut(&key) else {
                        let _ = reply.send(false);
                        return Vec::new();
                    };
                    let (cancel, cancelled) = oneshot::channel();
                    live.cancel = Some(cancel);
                    let answer = response(id, super::answer(Ok(&live.id)));
                    let stream = futures::stream::unfold(messages, |mut rx| async move { rx.recv().await.map(|m| (m, rx)) })
                        .take_until(cancelled)
                        .map(move |m| (key, Some(m)))
                        .chain(futures::stream::once(async move { (key, None) }));
                    self.streams.push(Box::pin(stream));
                    // The answer is written before the server hears it went,
                    // so no notification can come ahead of it.
                    let _ = reply.send(true);
                    answer.into_iter().collect()
                }
                Some(Decision::Reject(err)) => {
                    self.subscriptions.remove(&key);
                    response(id, Err(err)).into_iter().collect()
                }
                // Dropped undecided: jsonrpsee answers so.
                None => {
                    self.subscriptions.remove(&key);
                    error_response(id, ErrorCode::InternalError).into_iter().collect()
                }
            },
            // A subscription that ends with an error says so in a last
            // notification, as jsonrpsee's does; one never accepted has
            // been answered already.
            Done::Ran { key, result: Err(e) } => match self.subscriptions.get(&key) {
                Some(live) if live.cancel.is_some() => {
                    let payload = SubscriptionPayloadError { subscription: live.id.clone(), error: e.0 };
                    text_of(&Notification::new(live.notification.into(), payload)).into_iter().collect()
                }
                _ => Vec::new(),
            },
            Done::Ran { .. } => Vec::new(),
        }
    }

    fn notify(&mut self, (key, msg): (u64, Option<SubscriptionMessage>)) -> Vec<String> {
        let Some(msg) = msg else {
            // The server dropped every sink, or the client unsubscribed.
            self.subscriptions.remove(&key);
            return Vec::new();
        };
        let Some(live) = self.subscriptions.get(&key) else { return Vec::new() };
        let payload = SubscriptionPayload { subscription: live.id.clone(), result: msg.into_raw() };
        text_of(&SubscriptionResponse::new(live.notification.into(), payload)).into_iter().collect()
    }

    /// A batch of calls, answered together. Subscriptions are not taken in a
    /// batch.
    fn batch(&mut self, text: &str) {
        let items: Vec<&RawValue> = match serde_json::from_str(text) {
            Ok(items) => items,
            Err(_) => {
                let answer = error_response(Id::Null, ErrorCode::ParseError);
                self.tasks.push(Box::pin(async move { Done::Text(answer) }));
                return;
            }
        };
        if items.is_empty() {
            let answer = error_response(Id::Null, ErrorCode::InvalidRequest);
            self.tasks.push(Box::pin(async move { Done::Text(answer) }));
            return;
        }
        let mut answers: Vec<BoxFuture<'static, Option<String>>> = Vec::new();
        for item in items {
            let Ok(request) = serde_json::from_str::<Request>(item.get()) else {
                if serde_json::from_str::<Notification<Option<&RawValue>>>(item.get()).is_err() {
                    let answer = error_response(Id::Null, ErrorCode::InvalidRequest);
                    answers.push(Box::pin(async move { answer }));
                }
                continue;
            };
            let id = request.id.into_owned();
            let method = request.method.into_owned();
            let params = request.params.map(|p| p.get().to_owned());
            match super::method(&method).map(|m| m.kind) {
                Some(MethodKind::Call) => {
                    let (server, ext) = (Arc::clone(&self.server), self.ext.clone());
                    answers.push(Box::pin(async move {
                        let answer = super::call(&*server, &ext, &method, params.as_deref()).await;
                        response(id, answer)
                    }));
                }
                Some(MethodKind::Subscription { .. }) => {
                    let answer = error_response(id, ErrorCode::InvalidRequest);
                    answers.push(Box::pin(async move { answer }));
                }
                None => {
                    let answer = match super::unsubscribe_of(&method) {
                        Some(api) => self.unsubscribe(id, api.name, params.as_deref()),
                        None => error_response(id, ErrorCode::MethodNotFound),
                    };
                    answers.push(Box::pin(async move { answer }));
                }
            }
        }
        self.tasks.push(Box::pin(async move {
            let answers: Vec<String> = futures::future::join_all(answers).await.into_iter().flatten().collect();
            Done::Text((!answers.is_empty()).then(|| format!("[{}]", answers.join(","))))
        }));
    }
}

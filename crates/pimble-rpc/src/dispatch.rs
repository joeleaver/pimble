//! The API apart from its transport (docs/ONE_CODEBASE_CONTRACT.md, seam A).
//!
//! A server implements [`crate::PimbleApiServer`] (generated with the client
//! from the one trait in `methods.rs`). Anything that carries JSON-RPC
//! reaches it here: a wire name and its params in, an answer or a
//! subscription out, with the caller's [`Extensions`] (the principal the
//! transport resolved) attached by the caller.
//!
//! - [`call`]: a method's answer, as the bytes of the wire's `result`, or its
//!   error.
//! - [`subscribe`]: a subscription being opened. The server decides through
//!   the [`PendingSubscriptionSink`] it is handed (accept or reject); the
//!   transport sees the decision on [`Opening::decision`] and the
//!   notifications on [`Opening::messages`].
//! - [`native::into_rpc`]: jsonrpsee's HTTP/WebSocket server fed from this
//!   dispatch (native only).
//! - [`session::serve`]: a whole JSON-RPC connection over a pair of string
//!   channels, with no runtime of its own (no `spawn`): what an in-process
//!   client talks to, and what the browser's worker will run over its
//!   `MessagePort`.
//!
//! Nothing here spawns a task or reads a clock; jsonrpsee-core's `server`
//! feature (which spawns with `tokio::spawn`) is used by [`native`] only.

use std::sync::Arc;

use jsonrpsee::types::error::ErrorCode;
use jsonrpsee::types::{ErrorObject, ErrorObjectOwned, Params};
use serde::Serialize;
use serde_json::value::RawValue;
use tokio::sync::{mpsc, oneshot};

pub use async_trait::async_trait;
/// What a transport attaches to each call: on a Pimble server, the
/// connection's `Principal` (and an agent's connection). The same type
/// jsonrpsee's server hands its methods (`jsonrpsee::Extensions`).
pub use http::Extensions;

pub use crate::methods::METHODS;

#[cfg(not(target_arch = "wasm32"))]
pub mod native;
pub mod session;

/// One wire name of the API (`METHODS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiMethod {
    /// The method's or subscription's name on the wire (`pimble_applyEdit`).
    pub name: &'static str,
    pub kind: MethodKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    /// Answered once.
    Call,
    /// Answered with a subscription id, then `notification`s until the
    /// client calls `unsubscribe` or the server closes it.
    Subscription { notification: &'static str, unsubscribe: &'static str },
}

/// The method of the API called `name`, if any.
pub fn method(name: &str) -> Option<&'static ApiMethod> {
    METHODS.iter().find(|m| m.name == name)
}

/// The subscription whose unsubscribe is called `name`, if any.
pub fn unsubscribe_of(name: &str) -> Option<&'static ApiMethod> {
    METHODS.iter().find(|m| matches!(m.kind, MethodKind::Subscription { unsubscribe, .. } if unsubscribe == name))
}

/// A future the dispatch hands a transport: `Send` natively, where the
/// transport may move it between threads, and not in a browser's worker.
#[cfg(not(target_arch = "wasm32"))]
pub type BoxFuture<'a, T> = futures::future::BoxFuture<'a, T>;
#[cfg(target_arch = "wasm32")]
pub type BoxFuture<'a, T> = futures::future::LocalBoxFuture<'a, T>;

#[cfg(not(target_arch = "wasm32"))]
fn boxed<'a, T>(f: impl std::future::Future<Output = T> + Send + 'a) -> BoxFuture<'a, T> {
    Box::pin(f)
}
#[cfg(target_arch = "wasm32")]
fn boxed<'a, T>(f: impl std::future::Future<Output = T> + 'a) -> BoxFuture<'a, T> {
    Box::pin(f)
}

/// Answer the method `method` (a wire name) with `params` (the request's
/// raw `params`, `None` when it had none). A name the API does not answer
/// to is the wire's "Method not found" (`-32601`), as jsonrpsee answers it.
pub async fn call<S: crate::PimbleApiServer>(
    server: &S,
    ext: &Extensions,
    method: &str,
    params: Option<&str>,
) -> Result<Box<RawValue>, ErrorObjectOwned> {
    match crate::methods::__call(server, ext, method, Params::new(params)).await {
        Some(answer) => answer,
        None => Err(ErrorObject::from(ErrorCode::MethodNotFound)),
    }
}

/// A method's result as the bytes of the wire's `result`: what jsonrpsee
/// writes for it (serde_json, compact), so a transport that writes these
/// bytes as they are sends what jsonrpsee's server sends.
#[doc(hidden)]
pub fn answer<T: Serialize>(result: Result<T, ErrorObjectOwned>) -> Result<Box<RawValue>, ErrorObjectOwned> {
    let value = result?;
    serde_json::value::to_raw_value(&value).map_err(|e| {
        tracing::error!("Error serializing response: {:?}", e);
        ErrorObject::from(ErrorCode::InternalError)
    })
}

/// A subscription being opened (see [`subscribe`]).
pub struct Subscribing {
    /// The subscription's notification name on the wire.
    pub notification: &'static str,
    /// Its unsubscribe method's name on the wire.
    pub unsubscribe: &'static str,
    /// The server's subscribe method. It must be polled for the server to
    /// decide (and may go on after it); it finishes with what jsonrpsee
    /// calls the close response (an error closes the subscription with a
    /// last notification carrying it).
    pub run: BoxFuture<'static, SubscriptionResult>,
    /// The decision and the notifications.
    pub opening: Opening,
}

/// Open the subscription `method` (a wire name) with `params`. `None`: no
/// subscription of the API is called that.
pub fn subscribe<S: crate::PimbleApiServer>(server: Arc<S>, ext: Extensions, method: &str, params: Option<String>) -> Option<Subscribing> {
    let ApiMethod { name, kind: MethodKind::Subscription { notification, unsubscribe } } = *self::method(method)? else {
        return None;
    };
    let (pending, opening) = pending_subscription();
    let run = boxed(async move {
        let params = Params::new(params.as_deref());
        crate::methods::__subscribe(&*server, &ext, name, params, pending).await.unwrap_or(Ok(()))
    });
    Some(Subscribing { notification, unsubscribe, run, opening })
}

// ── Subscriptions ───────────────────────────────────────────────────────

/// How many notifications wait between the server and the transport before
/// a send waits; the transport drains them as fast as its connection takes
/// them (and jsonrpsee's own connection buffer is behind that).
const SINK_BUFFER: usize = 16;

/// One notification's `result`, already serialized (what jsonrpsee's
/// `SubscriptionMessage` is).
#[derive(Debug, Clone)]
pub struct SubscriptionMessage(Box<RawValue>);

impl SubscriptionMessage {
    /// `value` serialized as a notification's `result`.
    pub fn from_json(value: &impl Serialize) -> Result<Self, serde_json::Error> {
        serde_json::value::to_raw_value(value).map(Self)
    }

    /// The `result`'s JSON text.
    pub fn as_raw(&self) -> &RawValue {
        &self.0
    }

    pub fn into_raw(self) -> Box<RawValue> {
        self.0
    }
}

/// The subscriber has gone: unsubscribed, or its connection closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Disconnected;

impl std::fmt::Display for Disconnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The subscriber has gone")
    }
}

impl std::error::Error for Disconnected {}

/// Accepting failed: the subscriber's connection went before the
/// subscription could be answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptError;

impl std::fmt::Display for AcceptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The remote peer closed the connection")
    }
}

impl std::error::Error for AcceptError {}

/// Why a subscribe method ended with an error (after accepting, the
/// transport closes the subscription with it). As jsonrpsee's
/// `StringError`: anything that displays converts into it, so `?` works on
/// [`PendingSubscriptionSink::accept`].
#[derive(Debug)]
pub struct SubscriptionError(pub String);

impl<T: std::fmt::Display> From<T> for SubscriptionError {
    fn from(value: T) -> Self {
        Self(value.to_string())
    }
}

/// What a subscribe method returns.
pub type SubscriptionResult = Result<(), SubscriptionError>;

/// The server's decision on a subscription, as its transport sees it.
#[derive(Debug)]
pub enum Decision {
    /// Answer the subscribe call with a subscription id, then say on `reply`
    /// whether that worked (`false`: the subscriber has gone).
    Accept(oneshot::Sender<bool>),
    /// Answer the subscribe call with this error.
    Reject(ErrorObjectOwned),
}

/// A subscription the server has not decided on yet, handed to its
/// subscribe method. Dropped undecided, the transport answers the
/// subscribe call with an internal error, as jsonrpsee does.
#[derive(Debug)]
pub struct PendingSubscriptionSink {
    decide: oneshot::Sender<Decision>,
    tx: mpsc::Sender<SubscriptionMessage>,
}

/// The transport's half of a [`PendingSubscriptionSink`].
#[derive(Debug)]
pub struct Opening {
    pub decision: oneshot::Receiver<Decision>,
    /// Every notification the server sends once it has accepted. Dropping
    /// it is what closes the subscription for the server
    /// ([`SubscriptionSink::closed`]).
    pub messages: mpsc::Receiver<SubscriptionMessage>,
}

/// A subscription's two halves: what its subscribe method is handed, and
/// what the transport reads.
pub fn pending_subscription() -> (PendingSubscriptionSink, Opening) {
    let (decide, decision) = oneshot::channel();
    let (tx, messages) = mpsc::channel(SINK_BUFFER);
    (PendingSubscriptionSink { decide, tx }, Opening { decision, messages })
}

impl PendingSubscriptionSink {
    /// Answer the subscribe call with a subscription id. Resolves once the
    /// answer has gone to the transport; an error means the subscriber has
    /// gone.
    pub async fn accept(self) -> Result<SubscriptionSink, AcceptError> {
        let (reply, accepted) = oneshot::channel();
        self.decide.send(Decision::Accept(reply)).map_err(|_| AcceptError)?;
        match accepted.await {
            Ok(true) => Ok(SubscriptionSink { tx: self.tx }),
            _ => Err(AcceptError),
        }
    }

    /// Answer the subscribe call with `err`.
    pub async fn reject(self, err: impl Into<ErrorObjectOwned>) {
        let _ = self.decide.send(Decision::Reject(err.into()));
    }
}

/// An accepted subscription: where the server sends its notifications.
/// Clones send to the same subscriber.
#[derive(Debug, Clone)]
pub struct SubscriptionSink {
    tx: mpsc::Sender<SubscriptionMessage>,
}

impl SubscriptionSink {
    /// Send one notification, waiting while the transport is behind.
    pub async fn send(&self, msg: SubscriptionMessage) -> Result<(), Disconnected> {
        self.tx.send(msg).await.map_err(|_| Disconnected)
    }

    /// Whether the subscriber has gone.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Resolves once the subscriber has gone.
    pub async fn closed(&self) {
        self.tx.closed().await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// The names `methods.rs` itself spells out, read from its text rather
    /// than from the macro: if the macro ever skipped an item (or read one
    /// differently) the dispatch would be missing what the trait lists.
    fn names_in_methods_rs() -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
        let text = include_str!("methods.rs");
        let ns = "pimble_";
        let quoted = |s: &str| s.trim().trim_matches('"').to_string();
        let (mut calls, mut subs, mut unsubs) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        for line in text.lines().map(str::trim) {
            if let Some(rest) = line.strip_prefix("#[method(name = ") {
                let name = rest.split(&[',', ')'][..]).next().unwrap();
                calls.insert(format!("{ns}{}", quoted(name)));
            } else if let Some(rest) = line.strip_prefix("#[subscription(name = ") {
                let name = rest.split("=>").next().unwrap();
                subs.insert(format!("{ns}{}", quoted(name)));
                let unsub = rest.split("unsubscribe = ").nth(1).unwrap().split(',').next().unwrap();
                unsubs.insert(format!("{ns}{}", quoted(unsub)));
            }
        }
        (calls, subs, unsubs)
    }

    #[test]
    fn every_method_of_the_trait_is_dispatched() {
        let (calls, subs, unsubs) = names_in_methods_rs();
        assert!(calls.len() > 50 && subs.len() >= 5, "methods.rs read wrongly: {} methods, {} subscriptions", calls.len(), subs.len());
        let listed_calls: BTreeSet<String> =
            METHODS.iter().filter(|m| m.kind == MethodKind::Call).map(|m| m.name.to_string()).collect();
        let listed_subs: BTreeSet<String> =
            METHODS.iter().filter(|m| matches!(m.kind, MethodKind::Subscription { .. })).map(|m| m.name.to_string()).collect();
        let listed_unsubs: BTreeSet<String> = METHODS
            .iter()
            .filter_map(|m| match m.kind {
                MethodKind::Subscription { unsubscribe, .. } => Some(unsubscribe.to_string()),
                MethodKind::Call => None,
            })
            .collect();
        assert_eq!(calls, listed_calls, "a method of the trait is missing from the dispatch (or the other way round)");
        assert_eq!(subs, listed_subs, "a subscription of the trait is missing from the dispatch");
        assert_eq!(unsubs, listed_unsubs);
        // One wire name, one meaning.
        let mut all = BTreeSet::new();
        for m in METHODS {
            assert!(all.insert(m.name), "{} twice", m.name);
            if let MethodKind::Subscription { unsubscribe, .. } = m.kind {
                assert!(all.insert(unsubscribe), "{unsubscribe} twice");
            }
        }
    }

    #[test]
    fn subscriptions_keep_their_wire_names() {
        // What existing clients send and listen for.
        let store = method("pimble_subscribeStoreChanges").unwrap();
        assert_eq!(
            store.kind,
            MethodKind::Subscription { notification: "pimble_storeChanged", unsubscribe: "pimble_unsubscribeStoreChanges" }
        );
        assert_eq!(unsubscribe_of("pimble_unsubscribePresence").unwrap().name, "pimble_subscribePresence");
        assert_eq!(method("pimble_applyEdit").unwrap().kind, MethodKind::Call);
        assert!(method("applyEdit").is_none());
    }

    #[test]
    fn a_message_is_its_json() {
        let msg = SubscriptionMessage::from_json(&serde_json::json!({ "a": 1 })).unwrap();
        assert_eq!(msg.as_raw().get(), r#"{"a":1}"#);
    }
}
